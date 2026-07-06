//! The user's library: a set of ZIM files plus their search indexes and
//! background indexing state. Persisted as a small JSON manifest in the app
//! data directory; the ZIM files themselves stay wherever the user put them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use zimlib::Zim;

use crate::index::{
    build_index, index_dir_for, merge_passages, query_from_question, IndexProgress, Passage,
    SearchIndex, SharedProgress,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookMeta {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub description: String,
    pub language: String,
    pub entry_count: u32,
    pub indexed: bool,
    pub chunks: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    books: Vec<BookMeta>,
}

struct OpenBook {
    zim: Arc<Zim>,
    search: Option<Arc<SearchIndex>>,
}

pub struct Library {
    pub data_dir: PathBuf,
    manifest: RwLock<Manifest>,
    open: Mutex<HashMap<String, OpenBook>>,
    pub indexing: Mutex<HashMap<String, SharedProgress>>,
}

impl Library {
    pub fn open(data_dir: PathBuf) -> Result<Arc<Library>> {
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(data_dir.join("models"))?;
        std::fs::create_dir_all(data_dir.join("books"))?;
        let manifest_path = data_dir.join("library.json");
        let manifest = if manifest_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
                .context("parsing library.json")?
        } else {
            Manifest::default()
        };
        Ok(Arc::new(Library {
            data_dir,
            manifest: RwLock::new(manifest),
            open: Mutex::new(HashMap::new()),
            indexing: Mutex::new(HashMap::new()),
        }))
    }

    fn save(&self) -> Result<()> {
        let manifest = self.manifest.read().unwrap();
        let json = serde_json::to_string_pretty(&*manifest)?;
        std::fs::write(self.data_dir.join("library.json"), json)?;
        Ok(())
    }

    pub fn books(&self) -> Vec<BookMeta> {
        self.manifest.read().unwrap().books.clone()
    }

    pub fn book(&self, id: &str) -> Option<BookMeta> {
        self.manifest
            .read()
            .unwrap()
            .books
            .iter()
            .find(|b| b.id == id)
            .cloned()
    }

    /// Get (and cache) the opened ZIM for a book.
    pub fn zim(&self, id: &str) -> Result<Arc<Zim>> {
        if let Some(ob) = self.open.lock().unwrap().get(id) {
            return Ok(ob.zim.clone());
        }
        let meta = self.book(id).context("unknown book")?;
        let zim = Arc::new(Zim::open(&meta.path)?);
        self.open
            .lock()
            .unwrap()
            .insert(id.to_string(), OpenBook { zim: zim.clone(), search: None });
        Ok(zim)
    }

    fn search_index(&self, id: &str) -> Result<Arc<SearchIndex>> {
        if let Some(ob) = self.open.lock().unwrap().get(id) {
            if let Some(s) = &ob.search {
                return Ok(s.clone());
            }
        }
        let dir = index_dir_for(&self.data_dir, id);
        let s = Arc::new(SearchIndex::open(&dir)?);
        self.zim(id)?; // ensure OpenBook exists
        if let Some(ob) = self.open.lock().unwrap().get_mut(id) {
            ob.search = Some(s.clone());
        }
        Ok(s)
    }

    /// Add a ZIM file to the library and start indexing it in the background.
    pub fn add_book(self: &Arc<Self>, path: &Path) -> Result<BookMeta> {
        let zim = Zim::open(path).context("not a readable ZIM file")?;
        let id = zim.uuid_hex();
        if self.book(&id).is_some() {
            bail!("this ZIM file is already in the library");
        }
        let meta = BookMeta {
            id: id.clone(),
            path: path.to_path_buf(),
            title: zim
                .metadata("Title")
                .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into_owned()),
            description: zim.metadata("Description").unwrap_or_default(),
            language: zim.metadata("Language").unwrap_or_default(),
            entry_count: zim.entry_count(),
            indexed: false,
            chunks: 0,
        };
        self.manifest.write().unwrap().books.push(meta.clone());
        self.save()?;
        self.start_indexing(&id)?;
        Ok(meta)
    }

    pub fn start_indexing(self: &Arc<Self>, id: &str) -> Result<SharedProgress> {
        let progress: SharedProgress = Arc::new(IndexProgress::default());
        self.indexing
            .lock()
            .unwrap()
            .insert(id.to_string(), progress.clone());
        let lib = self.clone();
        let id = id.to_string();
        let p = progress.clone();
        std::thread::spawn(move || {
            let run = || -> Result<u64> {
                let zim = lib.zim(&id)?;
                let dir = index_dir_for(&lib.data_dir, &id);
                build_index(&zim, &dir, &p)
            };
            // A panic in the indexer must not leave the UI stuck on
            // "indexing…" forever.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("indexer panicked")));
            match result {
                Ok(chunks) => {
                    {
                        let mut m = lib.manifest.write().unwrap();
                        if let Some(b) = m.books.iter_mut().find(|b| b.id == id) {
                            b.indexed = true;
                            b.chunks = chunks;
                        }
                    }
                    let _ = lib.save();
                    // Invalidate any cached (stale) search reader.
                    if let Some(ob) = lib.open.lock().unwrap().get_mut(&id) {
                        ob.search = None;
                    }
                }
                Err(e) => {
                    eprintln!("indexing {id} failed: {e:#}");
                    p.failed.store(true, Ordering::Relaxed);
                    p.finished.store(true, Ordering::Relaxed);
                }
            }
        });
        Ok(progress)
    }

    pub fn remove_book(&self, id: &str) -> Result<()> {
        if let Some(p) = self.indexing.lock().unwrap().get(id) {
            p.cancel.store(true, Ordering::Relaxed);
        }
        self.open.lock().unwrap().remove(id);
        self.manifest.write().unwrap().books.retain(|b| b.id != id);
        self.save()?;
        let _ = std::fs::remove_dir_all(index_dir_for(&self.data_dir, id));
        Ok(())
    }

    /// The managed drop folder: any .zim placed here is added automatically
    /// by `scan_books_dir` (run at startup and via the UI's rescan button).
    pub fn books_dir(&self) -> PathBuf {
        self.data_dir.join("books")
    }

    /// Add every .zim in the books folder that isn't in the library yet.
    /// Returns how many books were added.
    pub fn scan_books_dir(self: &Arc<Self>) -> usize {
        let known: std::collections::HashSet<PathBuf> =
            self.books().iter().map(|b| b.path.clone()).collect();
        let mut added = 0;
        let Ok(rd) = std::fs::read_dir(self.books_dir()) else { return 0 };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            let is_zim = path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("zim"))
                .unwrap_or(false);
            if !is_zim || known.contains(&path) {
                continue;
            }
            // add_book rejects duplicates (same ZIM UUID) and non-ZIM files.
            if self.add_book(&path).is_ok() {
                added += 1;
            }
        }
        added
    }

    /// Retrieve the top passages for a question across all indexed books.
    pub fn retrieve(&self, question: &str, k: usize) -> Result<Vec<Passage>> {
        let query = query_from_question(question);
        let mut lists = Vec::new();
        for b in self.books().iter().filter(|b| b.indexed) {
            match self.search_index(&b.id) {
                Ok(s) => match s.search(&query, k, &b.id, &b.title) {
                    Ok(hits) => lists.push(hits),
                    Err(_) => continue,
                },
                Err(_) => continue,
            }
        }
        Ok(merge_passages(lists, k))
    }
}
