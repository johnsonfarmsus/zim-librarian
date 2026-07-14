//! librarian-core: ZIM library management, passage indexing/retrieval, and
//! local LLM inference behind a small `Engine` trait.

pub mod chats;
pub mod engine;
pub mod index;
pub mod library;
pub mod text;

#[cfg(feature = "llama")]
pub mod llama;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use chats::{Chat, ChatMeta, ChatStore, StoredMessage};
pub use engine::{
    build_messages, contextual_question, enforce_citations, has_citations, plan_retrieval,
    triage_sources, ChatMessage, Engine, RetrievalPlan, StubEngine,
};
pub use index::Passage;
pub use library::{BookMeta, Library};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// GGUF file name inside `<data>/models`, or an absolute path.
    pub model: Option<String>,
    pub context_tokens: u32,
    pub retrieval_passages: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { model: None, context_tokens: 8192, retrieval_passages: 6 }
    }
}

pub fn default_data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ZIM_LIBRARIAN_DATA") {
        return PathBuf::from(d);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("zim-librarian")
}

/// The model a fresh install should start with: the bundled OLMo when
/// present, otherwise a lone .gguf someone pre-placed, otherwise none.
fn preferred_model(models_dir: &std::path::Path) -> Option<String> {
    let ggufs: Vec<String> = std::fs::read_dir(models_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_ascii_lowercase().ends_with(".gguf"))
        .collect();
    ggufs
        .iter()
        .find(|n| n.to_ascii_lowercase().contains("olmo"))
        .cloned()
        .or_else(|| (ggufs.len() == 1).then(|| ggufs[0].clone()))
}

/// Top-level application state shared by every frontend (HTTP server, Tauri).
pub struct App {
    pub library: Arc<Library>,
    pub chats: chats::ChatStore,
    pub settings: RwLock<Settings>,
    engine: RwLock<Option<Arc<dyn Engine>>>,
}

impl App {
    pub fn open(data_dir: PathBuf) -> Result<Arc<App>> {
        let library = Library::open(data_dir.clone())?;
        let settings_path = data_dir.join("settings.json");
        let fresh_install = !settings_path.exists();
        let mut settings: Settings = std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // First run: when an installer pre-placed a model (the shell copies
        // bundled resources into <data>/models before opening the app),
        // select it so chat works out of the box. An existing settings.json
        // with model=null is a deliberate choice and stays untouched.
        if fresh_install && settings.model.is_none() {
            settings.model = preferred_model(&data_dir.join("models"));
        }
        let app = Arc::new(App {
            chats: chats::ChatStore::open(&data_dir)?,
            library,
            settings: RwLock::new(settings),
            engine: RwLock::new(None),
        });
        if fresh_install && app.settings.read().unwrap().model.is_some() {
            let _ = app.save_settings();
        }
        // Engine loading takes seconds (a GGUF read + GPU upload) — do it in
        // the background so the server and UI come up instantly; the stub
        // engine answers extractively until the model is in. Same thread then
        // picks up new books and reindexes stale ones.
        let bg = app.clone();
        std::thread::spawn(move || {
            bg.reload_engine();
            bg.library.scan_books_dir();
            for id in bg.library.books_needing_index() {
                let _ = bg.library.start_indexing(&id);
            }
        });
        Ok(app)
    }

    pub fn save_settings(&self) -> Result<()> {
        let s = self.settings.read().unwrap();
        std::fs::write(
            self.library.data_dir.join("settings.json"),
            serde_json::to_string_pretty(&*s)?,
        )?;
        Ok(())
    }

    pub fn models_dir(&self) -> PathBuf {
        self.library.data_dir.join("models")
    }

    /// GGUF files available in the models directory.
    pub fn available_models(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.models_dir())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.to_ascii_lowercase().ends_with(".gguf"))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    pub fn resolve_model_path(&self) -> Option<PathBuf> {
        let name = self.settings.read().unwrap().model.clone()?;
        let p = PathBuf::from(&name);
        let p = if p.is_absolute() { p } else { self.models_dir().join(&name) };
        p.exists().then_some(p)
    }

    /// (Re)build the engine from current settings. Falls back to the
    /// extractive stub when no model is configured or loading fails.
    pub fn reload_engine(&self) {
        let engine: Arc<dyn Engine> = match self.resolve_model_path() {
            #[cfg(feature = "llama")]
            Some(path) => {
                let n_ctx = self.settings.read().unwrap().context_tokens;
                match llama::LlamaEngine::load(&path, n_ctx) {
                    Ok(e) => Arc::new(e),
                    Err(err) => {
                        eprintln!("failed to load model: {err:#}");
                        Arc::new(StubEngine)
                    }
                }
            }
            #[cfg(not(feature = "llama"))]
            Some(_) => Arc::new(StubEngine),
            None => Arc::new(StubEngine),
        };
        *self.engine.write().unwrap() = Some(engine);
    }

    pub fn engine(&self) -> Arc<dyn Engine> {
        // The stub answers extractively while the real model is still
        // loading in the startup background thread.
        self.engine.read().unwrap().clone().unwrap_or_else(|| Arc::new(StubEngine))
    }

    /// Retrieval for a conversation turn: folds history into the query when
    /// the question is a follow-up.
    pub fn retrieve_for(&self, history: &[ChatMessage], question: &str) -> Result<Vec<Passage>> {
        let k = self.settings.read().unwrap().retrieval_passages;
        let query = engine::contextual_question(history, question);
        self.library.retrieve(&query, k)
    }

    /// One full grounded-chat turn: retrieve → prompt → generate.
    /// Returns the retrieved passages; generation streams into `sink`.
    pub fn chat_turn(
        &self,
        history: &[ChatMessage],
        question: &str,
        sink: engine::TokenSink,
    ) -> Result<(Vec<Passage>, String)> {
        let passages = self.retrieve_for(history, question)?;
        let messages = build_messages(history, question, &passages);
        let answer = self.engine().generate(&messages, sink, 1024)?;
        let answer = enforce_citations(&answer, &passages);
        Ok((passages, answer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn testdata(name: &str) -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata")
            .join(name)
    }

    /// End-to-end on a real Kiwix ZIM: add → index → retrieve → grounded chat.
    #[test]
    fn add_index_retrieve_chat() {
        let tmp = tempfile::tempdir().unwrap();
        let app = App::open(tmp.path().to_path_buf()).unwrap();
        let book = app.library.add_book(&testdata("alpinelinux.zim")).unwrap();
        // Wait for background indexing to finish.
        let progress = app.library.indexing.lock().unwrap().get(&book.id).unwrap().clone();
        for _ in 0..1200 {
            if progress.finished.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(progress.finished.load(Ordering::Relaxed), "indexing timed out");
        assert!(!progress.failed.load(Ordering::Relaxed), "indexing failed");
        let books = app.library.books();
        assert!(books[0].indexed);
        assert!(books[0].chunks > 100, "expected many chunks, got {}", books[0].chunks);

        let passages = app.library.retrieve("How do I set up a wireless network?", 6).unwrap();
        assert!(!passages.is_empty());
        assert!(passages.iter().any(|p| {
            let t = format!("{} {}", p.title, p.text).to_lowercase();
            t.contains("wireless") || t.contains("wifi") || t.contains("wlan")
        }), "no wireless-related passage in top results: {:#?}",
            passages.iter().map(|p| &p.title).collect::<Vec<_>>());

        // Full chat turn with the stub engine.
        let mut streamed = String::new();
        let mut sink = |s: &str| { streamed.push_str(s); true };
        let (sources, answer) = app
            .chat_turn(&[], "How do I set up a wireless network?", &mut sink)
            .unwrap();
        assert!(!sources.is_empty());
        assert!(answer.contains("[1]"));
        // The final answer is the streamed text after citation post-processing.
        assert!(!streamed.is_empty());
        assert!(streamed.starts_with(answer.split('[').next().unwrap()));
    }

    /// A book whose file moved (new path, same UUID) must be healed by a rescan
    /// rather than getting stuck "missing". This is the mobile drop-in /
    /// reinstall case: the app data container path changes, so the stored path
    /// no longer exists even though the .zim is present under a new path.
    #[test]
    fn scan_heals_moved_book() {
        let tmp = tempfile::tempdir().unwrap();
        let app = App::open(tmp.path().to_path_buf()).unwrap();
        let books_dir = app.library.books_dir();
        std::fs::create_dir_all(&books_dir).unwrap();

        let first = books_dir.join("first.zim");
        std::fs::copy(testdata("alpinelinux.zim"), &first).unwrap();
        let id = app.library.add_book(&first).unwrap().id;

        // Simulate the file moving: same bytes at a new path, old path gone.
        let moved = books_dir.join("moved.zim");
        std::fs::rename(&first, &moved).unwrap();
        assert!(
            !app.library.book(&id).unwrap().path.exists(),
            "precondition: stored path is now missing"
        );

        let added = app.library.scan_books_dir();
        assert_eq!(added, 0, "a moved book is healed, not added as new");
        assert_eq!(app.library.books().len(), 1, "no duplicate book created");
        let healed = app.library.book(&id).unwrap();
        assert_eq!(healed.path, moved, "stored path healed to the new location");
        assert!(healed.path.exists());
    }
}
