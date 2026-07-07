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
        let settings: Settings = std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let app = Arc::new(App {
            chats: chats::ChatStore::open(&data_dir)?,
            library,
            settings: RwLock::new(settings),
            engine: RwLock::new(None),
        });
        app.reload_engine();
        // Pick up anything dropped into the books folder while we were not
        // running, and reindex books whose index is missing (e.g. after an
        // index-format change). Off-thread: startup stays instant.
        let lib = app.library.clone();
        std::thread::spawn(move || {
            lib.scan_books_dir();
            for id in lib.books_needing_index() {
                let _ = lib.start_indexing(&id);
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
        self.engine.read().unwrap().clone().expect("engine initialized")
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
}
