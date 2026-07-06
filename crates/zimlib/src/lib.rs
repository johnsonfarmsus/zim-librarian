//! Minimal pure-Rust reader for the OpenZIM file format.
//!
//! Supports ZIM major version 5 and 6, both the old (`A`/`I`/`M`/`X`) and the
//! new (`C`/`M`/`W`/`X`) namespace schemes, and uncompressed, XZ and Zstandard
//! clusters. See <https://wiki.openzim.org/wiki/ZIM_file_format>.

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, ensure, Context, Result};
use memmap2::Mmap;

const HEADER_LEN: usize = 80;
const ZIM_MAGIC: u32 = 0x044D_495A;

#[derive(Debug, Clone)]
pub struct Header {
    pub major_version: u16,
    pub minor_version: u16,
    pub uuid: [u8; 16],
    pub entry_count: u32,
    pub cluster_count: u32,
    pub url_ptr_pos: u64,
    pub title_ptr_pos: u64,
    pub cluster_ptr_pos: u64,
    pub mime_list_pos: u64,
    pub main_page: u32,
    pub checksum_pos: u64,
}

impl Header {
    /// True when the file uses the new namespace scheme ('C' for content).
    pub fn new_namespaces(&self) -> bool {
        self.minor_version >= 1
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    /// Content entry: (cluster index, blob index).
    Content { cluster: u32, blob: u32 },
    /// Redirect to another entry (index into the URL pointer list).
    Redirect { target: u32 },
    /// Deleted entry / link target placeholder.
    Other,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Index into the URL pointer list.
    pub index: u32,
    pub namespace: u8,
    pub path: String,
    pub title: String,
    pub mime: Option<String>,
    pub kind: EntryKind,
}

impl Entry {
    pub fn is_html(&self) -> bool {
        self.mime
            .as_deref()
            .map(|m| m.starts_with("text/html"))
            .unwrap_or(false)
    }
}

pub struct Zim {
    map: Mmap,
    pub header: Header,
    mime_types: Vec<String>,
    // Tiny LRU of decompressed clusters: (index, (extended, data)).
    cluster_cache: Mutex<Vec<(u32, Arc<(bool, Vec<u8>)>)>>,
}

const CLUSTER_CACHE_CAP: usize = 8;

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Zim {
    pub fn open(path: impl AsRef<Path>) -> Result<Zim> {
        let file = std::fs::File::open(path.as_ref())
            .with_context(|| format!("opening {}", path.as_ref().display()))?;
        // Safety: the map is read-only; concurrent external modification of a
        // ZIM file while it is open is outside our threat model.
        let map = unsafe { Mmap::map(&file)? };
        ensure!(map.len() >= HEADER_LEN, "file too small to be a ZIM file");
        let magic = rd_u32(&map, 0);
        ensure!(magic == ZIM_MAGIC, "not a ZIM file (bad magic {magic:#x})");

        let header = Header {
            major_version: rd_u16(&map, 4),
            minor_version: rd_u16(&map, 6),
            uuid: map[8..24].try_into().unwrap(),
            entry_count: rd_u32(&map, 24),
            cluster_count: rd_u32(&map, 28),
            url_ptr_pos: rd_u64(&map, 32),
            title_ptr_pos: rd_u64(&map, 40),
            cluster_ptr_pos: rd_u64(&map, 48),
            mime_list_pos: rd_u64(&map, 56),
            main_page: rd_u32(&map, 64),
            checksum_pos: rd_u64(&map, 72),
        };
        ensure!(
            header.major_version == 5 || header.major_version == 6,
            "unsupported ZIM major version {}",
            header.major_version
        );

        // MIME type list: zero-terminated strings, terminated by an empty one.
        let mut mime_types = Vec::new();
        let mut pos = header.mime_list_pos as usize;
        loop {
            let start = pos;
            while pos < map.len() && map[pos] != 0 {
                pos += 1;
            }
            ensure!(pos < map.len(), "unterminated MIME list");
            if pos == start {
                break;
            }
            mime_types.push(String::from_utf8_lossy(&map[start..pos]).into_owned());
            pos += 1;
        }

        Ok(Zim {
            map,
            header,
            mime_types,
            cluster_cache: Mutex::new(Vec::new()),
        })
    }

    pub fn uuid_hex(&self) -> String {
        self.header.uuid.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn entry_count(&self) -> u32 {
        self.header.entry_count
    }

    fn dirent_offset(&self, index: u32) -> Result<usize> {
        ensure!(index < self.header.entry_count, "entry index out of range");
        let p = self.header.url_ptr_pos as usize + index as usize * 8;
        ensure!(p + 8 <= self.map.len(), "URL pointer out of range");
        Ok(rd_u64(&self.map, p) as usize)
    }

    fn read_cstr(&self, pos: usize) -> Result<(String, usize)> {
        let mut end = pos;
        while end < self.map.len() && self.map[end] != 0 {
            end += 1;
        }
        ensure!(end < self.map.len(), "unterminated string in dirent");
        Ok((
            String::from_utf8_lossy(&self.map[pos..end]).into_owned(),
            end + 1,
        ))
    }

    /// Parse the directory entry at the given URL-pointer-list index.
    pub fn entry_at(&self, index: u32) -> Result<Entry> {
        let off = self.dirent_offset(index)?;
        ensure!(off + 12 <= self.map.len(), "dirent out of range");
        let mimetype = rd_u16(&self.map, off);
        let namespace = self.map[off + 3];
        let (kind, mime, strings_at) = match mimetype {
            0xffff => (
                EntryKind::Redirect {
                    target: rd_u32(&self.map, off + 8),
                },
                None,
                off + 12,
            ),
            0xfffe | 0xfffd => (EntryKind::Other, None, off + 12),
            m => (
                EntryKind::Content {
                    cluster: rd_u32(&self.map, off + 8),
                    blob: rd_u32(&self.map, off + 12),
                },
                self.mime_types.get(m as usize).cloned(),
                off + 16,
            ),
        };
        let (path, next) = self.read_cstr(strings_at)?;
        let (title, _) = self.read_cstr(next)?;
        let title = if title.is_empty() { path.clone() } else { title };
        Ok(Entry {
            index,
            namespace,
            path,
            title,
            mime,
            kind,
        })
    }

    /// Namespace holding user-visible articles for this file's scheme.
    pub fn article_namespace(&self) -> u8 {
        if self.header.new_namespaces() {
            b'C'
        } else {
            b'A'
        }
    }

    /// Binary search for an entry by (namespace, path) over the URL pointer
    /// list, which is sorted by full URL.
    pub fn find(&self, namespace: u8, path: &str) -> Result<Option<Entry>> {
        let (mut lo, mut hi) = (0i64, self.header.entry_count as i64 - 1);
        while lo <= hi {
            let mid = ((lo + hi) / 2) as u32;
            let off = self.dirent_offset(mid)?;
            let mimetype = rd_u16(&self.map, off);
            let ns = self.map[off + 3];
            let strings_at = if mimetype == 0xffff || mimetype == 0xfffe || mimetype == 0xfffd {
                off + 12
            } else {
                off + 16
            };
            let (p, _) = self.read_cstr(strings_at)?;
            let cmp = (ns, p.as_str()).cmp(&(namespace, path));
            match cmp {
                std::cmp::Ordering::Equal => return Ok(Some(self.entry_at(mid)?)),
                std::cmp::Ordering::Less => lo = mid as i64 + 1,
                std::cmp::Ordering::Greater => hi = mid as i64 - 1,
            }
        }
        Ok(None)
    }

    /// Follow redirects (bounded) until a content entry is reached.
    pub fn resolve(&self, mut entry: Entry) -> Result<Entry> {
        for _ in 0..16 {
            match entry.kind {
                EntryKind::Redirect { target } => entry = self.entry_at(target)?,
                _ => return Ok(entry),
            }
        }
        bail!("redirect chain too long for {}", entry.path)
    }

    fn cluster(&self, index: u32) -> Result<Arc<(bool, Vec<u8>)>> {
        {
            let cache = self.cluster_cache.lock().unwrap();
            if let Some((_, data)) = cache.iter().find(|(i, _)| *i == index) {
                return Ok(data.clone());
            }
        }
        ensure!(index < self.header.cluster_count, "cluster out of range");
        let p = self.header.cluster_ptr_pos as usize + index as usize * 8;
        let start = rd_u64(&self.map, p) as usize;
        let end = if index + 1 < self.header.cluster_count {
            rd_u64(&self.map, p + 8) as usize
        } else {
            self.header.checksum_pos as usize
        };
        ensure!(
            start < end && end <= self.map.len(),
            "cluster bounds invalid"
        );
        let info = self.map[start];
        let extended = info & 0x10 != 0;
        let raw = &self.map[start + 1..end];
        let data = match info & 0x0f {
            0 | 1 => raw.to_vec(),
            4 => {
                let mut out = Vec::new();
                xz2::read::XzDecoder::new(raw)
                    .read_to_end(&mut out)
                    .context("xz cluster")?;
                out
            }
            5 => zstd::stream::decode_all(raw).context("zstd cluster")?,
            c => bail!("unsupported cluster compression {c}"),
        };
        let arc = Arc::new((extended, data));
        let mut cache = self.cluster_cache.lock().unwrap();
        if cache.len() >= CLUSTER_CACHE_CAP {
            cache.remove(0);
        }
        cache.push((index, arc.clone()));
        Ok(arc)
    }

    /// Read the raw bytes of one blob.
    pub fn blob(&self, cluster: u32, blob: u32) -> Result<Vec<u8>> {
        let data = self.cluster(cluster)?;
        let (extended, bytes) = (&data.0, &data.1);
        let osize = if *extended { 8usize } else { 4usize };
        let read_off = |i: usize| -> Result<usize> {
            let o = i * osize;
            ensure!(o + osize <= bytes.len(), "blob offset out of range");
            Ok(if *extended {
                rd_u64(bytes, o) as usize
            } else {
                rd_u32(bytes, o) as usize
            })
        };
        let first = read_off(0)?;
        let n_offsets = first / osize;
        ensure!(
            (blob as usize) + 1 < n_offsets,
            "blob index out of range"
        );
        let s = read_off(blob as usize)?;
        let e = read_off(blob as usize + 1)?;
        ensure!(s <= e && e <= bytes.len(), "blob bounds invalid");
        Ok(bytes[s..e].to_vec())
    }

    /// Convenience: fetch a resolved entry's content.
    pub fn content(&self, entry: &Entry) -> Result<Vec<u8>> {
        match entry.kind {
            EntryKind::Content { cluster, blob } => self.blob(cluster, blob),
            _ => Err(anyhow!("entry {} has no content", entry.path)),
        }
    }

    /// Look up a metadata value (M namespace), e.g. "Title" or "Language".
    pub fn metadata(&self, name: &str) -> Option<String> {
        let e = self.find(b'M', name).ok().flatten()?;
        let e = self.resolve(e).ok()?;
        let bytes = self.content(&e).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn main_page(&self) -> Option<Entry> {
        if self.header.main_page == u32::MAX {
            return None;
        }
        let e = self.entry_at(self.header.main_page).ok()?;
        self.resolve(e).ok()
    }

    /// The illustration/favicon PNG, if present.
    pub fn illustration(&self) -> Option<Vec<u8>> {
        for (ns, name) in [
            (b'M', "Illustration_48x48@1"),
            (b'-', "favicon"),
            (b'I', "favicon.png"),
        ] {
            if let Ok(Some(e)) = self.find(ns, name) {
                if let Ok(e) = self.resolve(e) {
                    if let Ok(bytes) = self.content(&e) {
                        return Some(bytes);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn testdata(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata")
            .join(name)
    }

    #[test]
    fn opens_new_namespace_zim() {
        let z = Zim::open(testdata("small_nons.zim")).unwrap();
        assert_eq!(z.header.major_version, 6);
        assert!(z.header.new_namespaces());
        assert_eq!(z.entry_count(), 16);
        // Every entry must parse.
        for i in 0..z.entry_count() {
            let e = z.entry_at(i).unwrap();
            assert!(!e.path.is_empty() || e.namespace != 0);
        }
    }

    #[test]
    fn opens_old_namespace_zim() {
        let z = Zim::open(testdata("small.zim")).unwrap();
        for i in 0..z.entry_count() {
            z.entry_at(i).unwrap();
        }
        // Old scheme: articles live in 'A'.
        assert_eq!(z.article_namespace(), b'A');
    }

    #[test]
    fn finds_and_reads_main_page() {
        for f in ["small.zim", "small_nons.zim"] {
            let z = Zim::open(testdata(f)).unwrap();
            let main = z.main_page().expect("main page");
            let html = z.content(&main).unwrap();
            assert!(!html.is_empty(), "{f}: empty main page");
            // find() must locate the same entry by path.
            let found = z.find(main.namespace, &main.path).unwrap().unwrap();
            assert_eq!(found.index, main.index);
        }
    }

    #[test]
    fn reads_metadata() {
        let z = Zim::open(testdata("small_nons.zim")).unwrap();
        // Test file has at least a Title metadata entry.
        assert!(z.metadata("Title").is_some());
    }
}
