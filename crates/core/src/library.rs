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
    global_index_dir, merge_passages, query_from_question, GlobalIndex, IndexOutcome,
    IndexProgress, Passage, SharedProgress,
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
    /// Indexing pipeline version this book was last indexed with.
    #[serde(default)]
    pub index_version: u32,
    /// Resumable-indexing checkpoint: how many ZIM entries have been indexed so
    /// far. A partially-indexed book (indexed == false, indexed_upto > 0)
    /// resumes from here instead of restarting, so progress survives the app
    /// being suspended or killed mid-index on mobile.
    #[serde(default)]
    pub indexed_upto: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    books: Vec<BookMeta>,
}

struct OpenBook {
    zim: Arc<Zim>,
}

pub struct Library {
    pub data_dir: PathBuf,
    manifest: RwLock<Manifest>,
    open: Mutex<HashMap<String, OpenBook>>,
    pub indexing: Mutex<HashMap<String, SharedProgress>>,
    index: GlobalIndex,
}

impl Library {
    pub fn open(data_dir: PathBuf) -> Result<Arc<Library>> {
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(data_dir.join("models"))?;
        std::fs::create_dir_all(data_dir.join("books"))?;
        let manifest_path = data_dir.join("library.json");
        let mut manifest: Manifest = if manifest_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
                .context("parsing library.json")?
        } else {
            Manifest::default()
        };
        // Migration from the old one-index-per-book layout: if the global
        // index doesn't exist yet, previously indexed books need reindexing
        // (App::open kicks that off), and the orphaned per-book dirs go away.
        let global_dir = global_index_dir(&data_dir);
        let fresh_index = !global_dir.join("meta.json").exists();
        if fresh_index {
            for b in &mut manifest.books {
                b.indexed = false;
                b.chunks = 0;
                b.indexed_upto = 0;
            }
            if let Ok(rd) = std::fs::read_dir(data_dir.join("index")) {
                for e in rd.filter_map(|e| e.ok()) {
                    if e.path().is_dir() && e.file_name() != "global" {
                        let _ = std::fs::remove_dir_all(e.path());
                    }
                }
            }
        }
        // Books indexed by an older pipeline (e.g. before PDF support) get
        // reindexed at startup.
        for b in &mut manifest.books {
            if b.indexed && b.index_version < GlobalIndex::PIPELINE_VERSION {
                b.indexed = false;
                b.chunks = 0;
                b.indexed_upto = 0;
            }
        }
        // iOS reassigns the app's data-container path across reinstalls: it
        // migrates the files but the absolute paths stored here go stale, so
        // every book reads as "missing" and the chat composer stays locked.
        // Re-root any book whose file vanished but whose filename is present in
        // the managed books dir. This is synchronous (done before the first UI
        // query, unlike the background scan) and matches by filename rather than
        // opening the ZIM — opening a multi-GB book here would stall startup and
        // risk OOM on a memory-constrained phone.
        let books_dir = data_dir.join("books");
        let mut healed = false;
        for b in &mut manifest.books {
            if b.path.exists() {
                continue;
            }
            if let Some(name) = b.path.file_name() {
                let candidate = books_dir.join(name);
                if candidate.exists() {
                    b.path = candidate;
                    healed = true;
                }
            }
        }
        let index = GlobalIndex::open_or_create(&global_dir)?;
        let lib = Arc::new(Library {
            data_dir,
            manifest: RwLock::new(manifest),
            open: Mutex::new(HashMap::new()),
            indexing: Mutex::new(HashMap::new()),
            index,
        });
        if fresh_index || healed {
            lib.save()?;
        }
        Ok(lib)
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
            .insert(id.to_string(), OpenBook { zim: zim.clone() });
        Ok(zim)
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
            index_version: 0,
            indexed_upto: 0,
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
            // Resume from the last durable checkpoint rather than restarting.
            let (resume_from, chunks_so_far) = lib
                .book(&id)
                .map(|b| (b.indexed_upto, b.chunks))
                .unwrap_or((0, 0));
            let run = || -> Result<IndexOutcome> {
                let zim = lib.zim(&id)?;
                let ckpt_lib = lib.clone();
                let ckpt_id = id.clone();
                lib.index.index_zim(
                    &zim,
                    &id,
                    resume_from,
                    chunks_so_far,
                    &p,
                    // Persist the resume point after every committed batch so an
                    // interruption (iOS suspending/killing the app) costs at most
                    // one batch of re-work, never the whole book.
                    move |next_entry, chunks| {
                        {
                            let mut m = ckpt_lib.manifest.write().unwrap();
                            if let Some(b) = m.books.iter_mut().find(|b| b.id == ckpt_id) {
                                b.indexed_upto = next_entry;
                                b.chunks = chunks;
                            }
                        }
                        let _ = ckpt_lib.save();
                    },
                )
            };
            // A panic in the indexer must not leave the UI stuck on
            // "indexing…" forever.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("indexer panicked")));
            match result {
                Ok(IndexOutcome::Done { chunks }) => {
                    {
                        let mut m = lib.manifest.write().unwrap();
                        if let Some(b) = m.books.iter_mut().find(|b| b.id == id) {
                            b.indexed = true;
                            b.chunks = chunks;
                            b.indexed_upto = b.entry_count;
                            b.index_version = GlobalIndex::PIPELINE_VERSION;
                        }
                    }
                    let _ = lib.save();
                }
                Ok(IndexOutcome::Paused { next_entry, chunks }) => {
                    // Not finished — just this session ended. The checkpoint is
                    // already saved; a later run (next launch / background task)
                    // resumes from here.
                    let mut m = lib.manifest.write().unwrap();
                    if let Some(b) = m.books.iter_mut().find(|b| b.id == id) {
                        b.indexed_upto = next_entry;
                        b.chunks = chunks;
                    }
                    drop(m);
                    let _ = lib.save();
                }
                Err(e) => {
                    eprintln!("indexing {id} failed: {e:#}");
                    p.failed.store(true, Ordering::Relaxed);
                }
            }
            // This indexing run has ended (done, paused, or failed); the UI and
            // wait_for_indexing key off `finished` to know no run is active.
            p.finished.store(true, Ordering::Relaxed);
        });
        Ok(progress)
    }

    /// Ask every in-flight indexing run to checkpoint and stop. Used when the
    /// app is about to be suspended (iOS background/expiration) so progress is
    /// saved durably; the next run resumes from the checkpoint.
    pub fn pause_all_indexing(&self) {
        for p in self.indexing.lock().unwrap().values() {
            p.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn remove_book(&self, id: &str) -> Result<()> {
        if let Some(p) = self.indexing.lock().unwrap().get(id) {
            p.cancel.store(true, Ordering::Relaxed);
        }
        self.open.lock().unwrap().remove(id);
        self.manifest.write().unwrap().books.retain(|b| b.id != id);
        self.save()?;
        self.index.remove_zim(id)?;
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
            // A .zim at an unrecognised path is either a brand-new book or an
            // existing one whose file moved (e.g. the app data container changed
            // across a reinstall, leaving the stored path "missing"). Open it to
            // read its UUID so we can tell the two cases apart — otherwise a
            // moved file trips add_book's UUID-duplicate check and the book is
            // stuck "missing" forever, with no way to recover.
            let zim = match Zim::open(&path) {
                Ok(z) => z,
                Err(e) => {
                    eprintln!("skipping {}: {e:#}", path.display());
                    continue;
                }
            };
            let id = zim.uuid_hex();
            if self.book(&id).is_some() {
                // Same book, new location: heal the stored path (the index is
                // still valid — identical content and UUID) and drop any stale
                // open handle so the next read uses the new path.
                if let Some(b) = self
                    .manifest
                    .write()
                    .unwrap()
                    .books
                    .iter_mut()
                    .find(|b| b.id == id)
                {
                    b.path = path.clone();
                }
                self.open.lock().unwrap().remove(&id);
                let _ = self.save();
            } else {
                match self.add_book(&path) {
                    Ok(_) => added += 1,
                    Err(e) => eprintln!("skipping {}: {e:#}", path.display()),
                }
            }
        }
        added
    }

    /// Retrieve the top passages for a question across all indexed books.
    pub fn retrieve(&self, question: &str, k: usize) -> Result<Vec<Passage>> {
        self.retrieve_multi(std::slice::from_ref(&question.to_string()), k)
    }

    /// Retrieve using several alternative queries: each query's scores are
    /// normalized by its own top hit (so vocabulary-lucky queries don't
    /// dominate), a passage keeps its best normalized score across queries,
    /// then the usual cap/floor/top-k merge applies.
    pub fn retrieve_multi(&self, queries: &[String], k: usize) -> Result<Vec<Passage>> {
        let names: HashMap<String, String> =
            self.books().into_iter().map(|b| (b.id, b.title)).collect();
        let mut best: HashMap<(String, String, u64), Passage> = HashMap::new();
        for q in queries {
            let hits = self.index.search_raw(&query_from_question(q), k * 3, &names)?;
            let top = hits.first().map(|h| h.score).unwrap_or(0.0);
            if top <= 0.0 {
                continue;
            }
            for mut h in hits {
                h.score /= top;
                let key = (h.zim_id.clone(), h.path.clone(), h.chunk);
                match best.get(&key) {
                    Some(prev) if prev.score >= h.score => {}
                    _ => {
                        best.insert(key, h);
                    }
                }
            }
        }
        Ok(merge_passages(vec![best.into_values().collect()], k))
    }

    /// Block until no book is actively indexing. Used at startup to hold the
    /// (memory-hungry) LLM load until the initial index pass finishes, so a
    /// large book isn't competing with the ~1 GB model for memory on a small
    /// device — the concurrency, not the book's size on disk, is what jetsam-
    /// kills a 3 GB phone.
    pub fn wait_for_indexing(&self) {
        loop {
            let pending = {
                let idx = self.indexing.lock().unwrap();
                idx.values().any(|p| !p.finished.load(Ordering::Relaxed))
            };
            if !pending {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    /// Books that claim a search index but don't have one (e.g. after the
    /// index format changed) — App::open reindexes these at startup.
    pub fn books_needing_index(&self) -> Vec<String> {
        self.books()
            .iter()
            .filter(|b| !b.indexed && b.path.exists())
            .map(|b| b.id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn testdata(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata")
            .join(name)
    }

    fn wait(p: &SharedProgress) {
        for _ in 0..1800 {
            if p.finished.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("indexing timed out");
    }

    /// A book whose index was interrupted partway (app killed/suspended on iOS)
    /// resumes from its checkpoint on the next run and ends fully indexed, with
    /// its content still retrievable — no restart-from-zero, no lost data.
    #[test]
    fn resumes_index_from_checkpoint_without_losing_data() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::open(tmp.path().to_path_buf()).unwrap();
        let file = lib.books_dir().join("a.zim");
        std::fs::copy(testdata("alpinelinux.zim"), &file).unwrap();
        let id = lib.add_book(&file).unwrap().id;
        wait(&lib.indexing.lock().unwrap().get(&id).unwrap().clone());
        assert!(lib.book(&id).unwrap().indexed, "baseline index completes");
        assert!(
            !lib.retrieve("wireless network", 6).unwrap().is_empty(),
            "baseline retrieval works"
        );

        // Simulate an index that only got halfway before the app was killed:
        // not indexed, checkpoint at the midpoint.
        let n = lib.book(&id).unwrap().entry_count;
        {
            let mut m = lib.manifest.write().unwrap();
            let b = m.books.iter_mut().find(|b| b.id == id).unwrap();
            b.indexed = false;
            b.indexed_upto = n / 2;
        }
        assert!(lib.books_needing_index().contains(&id), "resume is pending");

        let p = lib.start_indexing(&id).unwrap();
        wait(&p);
        assert!(lib.book(&id).unwrap().indexed, "resume completes the index");
        assert!(
            !lib.retrieve("wireless network", 6).unwrap().is_empty(),
            "content still retrievable after resuming"
        );
    }
}
