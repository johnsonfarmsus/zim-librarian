//! HTTP layer shared by the desktop shells: JSON API, SSE chat streaming,
//! ZIM content serving (with citation highlighting) and the embedded UI.
//! Binds to 127.0.0.1 only — nothing is ever exposed to the network.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::Stream;
use librarian_core::{App, ChatMessage};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

#[derive(RustEmbed)]
#[folder = "../../ui"]
struct Assets;

type Shared = Arc<App>;

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/api/library", get(list_books).post(add_book))
        .route("/api/library/scan", post(scan_books))
        .route("/api/library/:id", delete(remove_book))
        .route("/api/fs", get(fs_list))
        .route("/api/status", get(status))
        .route("/api/search", get(search_debug))
        .route("/api/models", get(models).post(select_model))
        .route("/api/models/catalog", get(model_catalog))
        .route("/api/models/download", post(model_download))
        .route("/api/chats", get(list_chats).post(new_chat))
        .route("/api/chats/:id", get(get_chat).delete(delete_chat))
        .route("/api/chat", post(chat))
        .route("/content/:zim/*path", get(content))
        .route("/home/:zim", get(book_home))
        .fallback(static_asset)
        .with_state(app)
}

/// Serve on a localhost port (0 = OS-assigned); returns the bound address.
pub async fn serve(
    app: Shared,
    port: u16,
) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let r = router(app);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, r).await;
    });
    Ok((addr, handle))
}

// ---------- static UI ----------

async fn static_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string())], f.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

// ---------- library ----------

async fn list_books(State(app): State<Shared>) -> Json<Value> {
    let books: Vec<Value> = app
        .library
        .books()
        .into_iter()
        .map(|b| {
            let missing = !b.path.exists();
            let mut v = serde_json::to_value(&b).unwrap_or_default();
            v["missing"] = json!(missing);
            v
        })
        .collect();
    Json(json!({
        "books": books,
        "books_dir": app.library.books_dir().to_string_lossy(),
    }))
}

async fn scan_books(State(app): State<Shared>) -> Json<Value> {
    let lib = app.library.clone();
    let added = tokio::task::spawn_blocking(move || lib.scan_books_dir())
        .await
        .unwrap_or(0);
    Json(json!({ "added": added }))
}

// Minimal directory listing for the "add book" file browser. Localhost-only
// single-user app: this lists what the user can already see in Finder, and
// only directories plus .zim files are returned.
#[derive(Deserialize)]
struct FsQuery {
    path: Option<String>,
}

async fn fs_list(State(app): State<Shared>, Query(q): Query<FsQuery>) -> Response {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    let path = q
        .path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.clone());
    let path = path.canonicalize().unwrap_or(path);
    if !path.is_dir() {
        return (StatusCode::BAD_REQUEST, "not a directory").into_response();
    }
    let mut dirs_out: Vec<String> = Vec::new();
    let mut files_out: Vec<Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&path) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                dirs_out.push(name);
            } else if p
                .extension()
                .map(|x| x.eq_ignore_ascii_case("zim"))
                .unwrap_or(false)
            {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                files_out.push(json!({ "name": name, "size": size }));
            }
        }
    }
    dirs_out.sort_by_key(|d| d.to_lowercase());
    files_out.sort_by_key(|f| f["name"].as_str().unwrap_or("").to_lowercase());

    let mut quick = vec![json!({ "label": "Home", "path": home.to_string_lossy() })];
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        quick.push(json!({ "label": "Downloads", "path": downloads.to_string_lossy() }));
    }
    quick.push(json!({
        "label": "Books folder",
        "path": app.library.books_dir().to_string_lossy()
    }));
    #[cfg(target_os = "macos")]
    if std::path::Path::new("/Volumes").is_dir() {
        quick.push(json!({ "label": "External drives", "path": "/Volumes" }));
    }

    Json(json!({
        "path": path.to_string_lossy(),
        "parent": path.parent().map(|p| p.to_string_lossy().into_owned()),
        "dirs": dirs_out,
        "files": files_out,
        "quick": quick,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct AddBook {
    path: String,
}

async fn add_book(State(app): State<Shared>, Json(req): Json<AddBook>) -> Response {
    let lib = app.library.clone();
    let res =
        tokio::task::spawn_blocking(move || lib.add_book(std::path::Path::new(&req.path))).await;
    match res {
        Ok(Ok(meta)) => Json(json!({ "book": meta })).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "task failed").into_response(),
    }
}

async fn remove_book(State(app): State<Shared>, AxPath(id): AxPath<String>) -> Response {
    match app.library.remove_book(&id) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

async fn status(State(app): State<Shared>) -> Json<Value> {
    let indexing: Vec<Value> = app
        .library
        .indexing
        .lock()
        .unwrap()
        .iter()
        .map(|(id, p)| {
            json!({
                "id": id,
                "total": p.total_entries.load(Ordering::Relaxed),
                "done": p.done_entries.load(Ordering::Relaxed),
                "finished": p.finished.load(Ordering::Relaxed),
                "failed": p.failed.load(Ordering::Relaxed),
            })
        })
        .collect();
    Json(json!({
        "indexing": indexing,
        "engine": app.engine().name(),
        "model": app.settings.read().unwrap().model,
    }))
}

/// Raw retrieval, for debugging what the index returns for a query.
#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search_debug(State(app): State<Shared>, Query(sq): Query<SearchQuery>) -> Response {
    let lib = app.library.clone();
    let res = tokio::task::spawn_blocking(move || lib.retrieve(&sq.q, 8)).await;
    match res {
        Ok(Ok(hits)) => Json(json!({ "hits": hits })).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "task failed").into_response(),
    }
}

// ---------- models ----------

async fn models(State(app): State<Shared>) -> Json<Value> {
    Json(json!({
        "available": app.available_models(),
        "selected": app.settings.read().unwrap().model,
        "models_dir": app.models_dir().to_string_lossy(),
        "llama_compiled": cfg!(feature = "llama"),
    }))
}

#[derive(Deserialize)]
struct SelectModel {
    model: Option<String>,
}

async fn select_model(State(app): State<Shared>, Json(req): Json<SelectModel>) -> Response {
    app.settings.write().unwrap().model = req.model;
    let _ = app.save_settings();
    let a = app.clone();
    // Model loading can take seconds; do it off the async runtime.
    let res = tokio::task::spawn_blocking(move || a.reload_engine()).await;
    match res {
        Ok(()) => Json(json!({ "engine": app.engine().name() })).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "reload failed").into_response(),
    }
}

// ---------- model catalog & downloads ----------
//
// The only network code in the product. Strictly user-initiated: nothing is
// fetched unless the user clicks Download, and everything else works with
// zero network access.

struct CatalogEntry {
    id: &'static str,
    label: &'static str,
    file: &'static str,
    url: &'static str,
    bytes: u64,
    notes: &'static str,
}

const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "gemma-4-e2b",
        label: "Gemma 4 E2B (recommended)",
        file: "gemma-4-E2B-it-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/gemma-4-E2B-it-GGUF/resolve/main/gemma-4-E2B-it-Q4_K_M.gguf",
        bytes: 3_106_736_256,
        notes: "Latest on-device Gemma; best quality tested. Needs ~4 GB free RAM.",
    },
    CatalogEntry {
        id: "gemma-3n-e2b",
        label: "Gemma 3n E2B",
        file: "gemma-3n-E2B-it-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/gemma-3n-E2B-it-GGUF/resolve/main/gemma-3n-E2B-it-Q4_K_M.gguf",
        bytes: 3_026_881_888,
        notes: "Previous-generation on-device Gemma. Needs ~4 GB free RAM.",
    },
    CatalogEntry {
        id: "olmo-2-1b",
        label: "OLMo 2 1B (fully open)",
        file: "OLMo-2-0425-1B-Instruct-Q4_K_M.gguf",
        url: "https://huggingface.co/allenai/OLMo-2-0425-1B-Instruct-GGUF/resolve/main/OLMo-2-0425-1B-Instruct-Q4_K_M.gguf",
        bytes: 935_515_296,
        notes: "Open weights, data and training code. Good on 8 GB machines.",
    },
    CatalogEntry {
        id: "olmo-2-7b",
        label: "OLMo 2 7B (fully open, larger)",
        file: "olmo-2-1124-7B-instruct-Q4_K_M.gguf",
        url: "https://huggingface.co/allenai/OLMo-2-1124-7B-Instruct-GGUF/resolve/main/olmo-2-1124-7B-instruct-Q4_K_M.gguf",
        bytes: 4_472_020_256,
        notes: "Strongest open option; best on 16 GB+ machines.",
    },
    CatalogEntry {
        id: "smollm2-360m",
        label: "SmolLM2 360M (tiny)",
        file: "SmolLM2-360M-Instruct-Q8_0.gguf",
        url: "https://huggingface.co/HuggingFaceTB/SmolLM2-360M-Instruct-GGUF/resolve/main/smollm2-360m-instruct-q8_0.gguf",
        bytes: 386_404_992,
        notes: "For very old or low-memory hardware. Treat answers as search summaries.",
    },
];

#[derive(Default, Clone)]
struct DlProgress {
    done: u64,
    total: u64,
    error: Option<String>,
}

static DOWNLOADS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, DlProgress>>> =
    std::sync::LazyLock::new(Default::default);

async fn model_catalog(State(app): State<Shared>) -> Json<Value> {
    let installed = app.available_models();
    let dls = DOWNLOADS.lock().unwrap().clone();
    let entries: Vec<Value> = CATALOG
        .iter()
        .map(|e| {
            let status = if installed.iter().any(|m| m == e.file) {
                json!({ "state": "installed" })
            } else if let Some(p) = dls.get(e.id) {
                match &p.error {
                    Some(err) => json!({ "state": "error", "message": err }),
                    None => json!({ "state": "downloading", "done": p.done, "total": p.total }),
                }
            } else {
                json!({ "state": "absent" })
            };
            json!({
                "id": e.id, "label": e.label, "file": e.file,
                "bytes": e.bytes, "notes": e.notes, "status": status,
            })
        })
        .collect();
    Json(json!({ "models": entries }))
}

#[derive(Deserialize)]
struct DownloadReq {
    id: String,
}

async fn model_download(State(app): State<Shared>, Json(req): Json<DownloadReq>) -> Response {
    let Some(entry) = CATALOG.iter().find(|e| e.id == req.id) else {
        return (StatusCode::NOT_FOUND, "unknown model").into_response();
    };
    {
        let mut dls = DOWNLOADS.lock().unwrap();
        if dls.get(entry.id).map(|p| p.error.is_none()).unwrap_or(false) {
            return Json(json!({ "ok": true })).into_response(); // already running
        }
        dls.insert(entry.id.to_string(), DlProgress { total: entry.bytes, ..Default::default() });
    }
    let dest = app.models_dir().join(entry.file);
    let (id, url) = (entry.id.to_string(), entry.url.to_string());
    tokio::task::spawn_blocking(move || {
        let result = download_file(&url, &dest, &id);
        let mut dls = DOWNLOADS.lock().unwrap();
        match result {
            Ok(()) => {
                dls.remove(&id);
            }
            Err(e) => {
                if let Some(p) = dls.get_mut(&id) {
                    p.error = Some(e.to_string());
                }
                let _ = std::fs::remove_file(dest.with_extension("part"));
            }
        }
    });
    Json(json!({ "ok": true })).into_response()
}

fn download_file(url: &str, dest: &std::path::Path, id: &str) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    let resp = ureq::get(url).call()?;
    let total: u64 = resp
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if total > 0 {
        if let Some(p) = DOWNLOADS.lock().unwrap().get_mut(id) {
            p.total = total;
        }
    }
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp)?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut done: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(p) = DOWNLOADS.lock().unwrap().get_mut(id) {
            p.done = done;
        }
    }
    file.flush()?;
    drop(file);
    if total > 0 && done < total {
        anyhow::bail!("download truncated ({done} of {total} bytes)");
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

// ---------- chats (history) ----------

async fn list_chats(State(app): State<Shared>) -> Json<Value> {
    Json(json!({ "chats": app.chats.list() }))
}

async fn new_chat(State(app): State<Shared>) -> Response {
    match app.chats.create("") {
        Ok(c) => Json(json!({ "chat": c })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_chat(State(app): State<Shared>, AxPath(id): AxPath<String>) -> Response {
    match app.chats.get(&id) {
        Ok(c) => Json(json!({ "chat": c })).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

async fn delete_chat(State(app): State<Shared>, AxPath(id): AxPath<String>) -> Response {
    match app.chats.delete(&id) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

// ---------- chat turn (SSE) ----------

#[derive(Deserialize)]
struct ChatReq {
    /// Existing chat to continue; omitted → a new chat is created and its id
    /// is streamed back in a "chat" event.
    chat_id: Option<String>,
    message: String,
}

async fn chat(
    State(app): State<Shared>,
    Json(req): Json<ChatReq>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

    tokio::task::spawn_blocking(move || {
        let err = |e: String| {
            let _ = tx.send(Event::default().event("error").data(e));
        };
        // Load or create the conversation.
        let chat = match &req.chat_id {
            Some(id) => app.chats.get(id),
            None => app.chats.create(""),
        };
        let chat = match chat {
            Ok(c) => c,
            Err(e) => return err(e.to_string()),
        };
        let history: Vec<ChatMessage> = chat
            .messages
            .iter()
            .map(|m| ChatMessage { role: m.role.clone(), content: m.content.clone() })
            .collect();
        let question = req.message;

        let chat = match app.chats.append(
            &chat.id,
            librarian_core::StoredMessage {
                role: "user".into(),
                content: question.clone(),
                sources: vec![],
            },
        ) {
            Ok(c) => c,
            Err(e) => return err(e.to_string()),
        };
        // Tell the UI which chat this turn belongs to (and its title, which
        // is adopted from the first question).
        let _ = tx.send(
            Event::default()
                .event("chat")
                .data(json!({ "id": chat.id, "title": chat.title }).to_string()),
        );

        // Let the model plan retrieval from the conversation: rewrite the
        // question into a self-contained query, or decide the message is a
        // refinement that should reuse the previous sources.
        let engine = app.engine();
        let plan = librarian_core::plan_retrieval(engine.as_ref(), &history, &question);
        let k = app.settings.read().unwrap().retrieval_passages;
        let (passages, plan_info) = match &plan {
            librarian_core::RetrievalPlan::ReusePrevious => {
                let prev: Vec<librarian_core::Passage> = chat
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant" && !m.sources.is_empty())
                    .map(|m| m.sources.clone())
                    .unwrap_or_default();
                if prev.is_empty() {
                    match app.library.retrieve(&question, k) {
                        Ok(p) => (p, json!({ "query": question })),
                        Err(e) => return err(e.to_string()),
                    }
                } else {
                    (prev, json!({ "reused": true }))
                }
            }
            librarian_core::RetrievalPlan::Search(qs) => {
                // Over-fetch candidates; the triage step below curates them.
                match app.library.retrieve_multi(qs, (k * 2).max(10)) {
                    Ok(p) => (p, json!({ "query": qs.join("  |  ") })),
                    Err(e) => return err(e.to_string()),
                }
            }
        };
        let _ = tx.send(Event::default().event("plan").data(plan_info.to_string()));

        // Triage: the model reads each candidate and keeps only passages that
        // actually address the question. If nothing survives, reply with a
        // deterministic "not in your library" message — no generation, no
        // opportunity to weave junk sources into fiction.
        let reused = plan_info.get("reused").is_some();
        let passages = if reused {
            passages // previously curated
        } else {
            match librarian_core::triage_sources(engine.as_ref(), &question, &passages) {
                None => passages.into_iter().take(k).collect(),
                Some(keep) if keep.is_empty() => {
                    let near: Vec<librarian_core::Passage> =
                        passages.into_iter().take(3).collect();
                    let mut msg = String::from(
                        "I searched your library, but nothing in it actually covers this \
                         question.",
                    );
                    if !near.is_empty() {
                        msg.push_str("\n\nThe closest things I found were:\n");
                        for (i, p) in near.iter().enumerate() {
                            msg.push_str(&format!("- \"{}\" ({}) [{}]\n", p.title, p.book, i + 1));
                        }
                    }
                    msg.push_str(
                        "\nIf you'd like me to answer questions on this topic, add a ZIM \
                         book that covers it (library.kiwix.org has free ones).",
                    );
                    let _ = tx.send(
                        Event::default()
                            .event("sources")
                            .data(serde_json::to_string(&near).unwrap_or_else(|_| "[]".into())),
                    );
                    let _ = app.chats.append(
                        &chat.id,
                        librarian_core::StoredMessage {
                            role: "assistant".into(),
                            content: msg.clone(),
                            sources: near,
                        },
                    );
                    let _ = tx.send(Event::default().event("done").data(msg));
                    return;
                }
                Some(keep) => keep.into_iter().take(k).map(|i| passages[i].clone()).collect(),
            }
        };
        let _ = tx.send(
            Event::default()
                .event("sources")
                .data(serde_json::to_string(&passages).unwrap_or_else(|_| "[]".into())),
        );
        let messages = librarian_core::build_messages(&history, &question, &passages);
        let mut sink =
            |piece: &str| tx.send(Event::default().event("token").data(piece)).is_ok();
        match engine.generate(&messages, &mut sink, 1024) {
            Ok(full) => {
                let mut full = librarian_core::enforce_citations(&full, &passages);
                // A citation-free answer means no source supported it: label
                // it clearly instead of letting it pass as library-grounded.
                if !passages.is_empty() && !librarian_core::has_citations(&full) {
                    full = format!(
                        "⚠ Nothing in your library supported this answer — what follows is \
the model's own general knowledge, not your books.\n\n{full}"
                    );
                }
                let _ = app.chats.append(
                    &chat.id,
                    librarian_core::StoredMessage {
                        role: "assistant".into(),
                        content: full.clone(),
                        sources: passages,
                    },
                );
                let _ = tx.send(Event::default().event("done").data(full));
            }
            Err(e) => err(e.to_string()),
        }
    });

    let stream = UnboundedReceiverStream::new(rx).map(Ok);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------- ZIM content ----------

#[derive(Deserialize)]
struct ContentQuery {
    /// Text snippet to highlight and scroll to (citation click-through).
    hl: Option<String>,
}

enum ContentResult {
    Found { mime: String, body: Vec<u8> },
    Redirect(String),
    NotFound,
}

async fn content(
    State(app): State<Shared>,
    AxPath((zim_id, path)): AxPath<(String, String)>,
    Query(q): Query<ContentQuery>,
) -> Response {
    let lib = app.library.clone();
    let zid = zim_id.clone();
    let p = path.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<ContentResult> {
        let zim = lib.zim(&zid)?;
        let ns = zim.article_namespace();
        let mut entry = zim.find(ns, &p)?;
        // Kiwix HTML sometimes links with an explicit namespace prefix
        // ("A/Foo") or into other namespaces (old-scheme images in 'I').
        if entry.is_none() {
            if let Some((nsc, rest)) = p.split_once('/') {
                if nsc.len() == 1 {
                    entry = zim.find(nsc.as_bytes()[0], rest)?;
                }
            }
        }
        if entry.is_none() {
            for alt in [b'I', b'-', b'W', b'M'] {
                entry = zim.find(alt, &p)?;
                if entry.is_some() {
                    break;
                }
            }
        }
        let Some(entry) = entry else {
            return Ok(ContentResult::NotFound);
        };
        if let zimlib::EntryKind::Redirect { target } = entry.kind {
            let t = zim.entry_at(target)?;
            return Ok(ContentResult::Redirect(t.path));
        }
        let mime = entry
            .mime
            .clone()
            .unwrap_or_else(|| "application/octet-stream".into());
        let body = zim.content(&entry)?;
        Ok(ContentResult::Found { mime, body })
    })
    .await;

    match res {
        Ok(Ok(ContentResult::Found { mime, mut body })) => {
            if mime.starts_with("text/html") {
                if let Some(hl) = q.hl.as_deref() {
                    body = inject_highlighter(body, hl);
                }
            }
            ([(header::CONTENT_TYPE, mime)], body).into_response()
        }
        Ok(Ok(ContentResult::Redirect(target))) => {
            let hl = q
                .hl
                .as_deref()
                .map(|h| format!("?hl={}", urlencode(h)))
                .unwrap_or_default();
            Redirect::temporary(&format!("/content/{zim_id}/{target}{hl}")).into_response()
        }
        Ok(Ok(ContentResult::NotFound)) => {
            (StatusCode::NOT_FOUND, "no such article").into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "task failed").into_response(),
    }
}

/// Open a book for browsing: redirect to its main page.
async fn book_home(State(app): State<Shared>, AxPath(zim_id): AxPath<String>) -> Response {
    let lib = app.library.clone();
    let zid = zim_id.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<String>> {
        let zim = lib.zim(&zid)?;
        Ok(zim.main_page().map(|e| e.path))
    })
    .await;
    match res {
        Ok(Ok(Some(path))) => {
            let path = path.split('/').map(urlencode).collect::<Vec<_>>().join("/");
            Redirect::temporary(&format!("/content/{zim_id}/{path}")).into_response()
        }
        Ok(Ok(None)) => (StatusCode::NOT_FOUND, "this book has no main page").into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "task failed").into_response(),
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Append a small script that finds the cited passage text, wraps it in a
/// <mark> and scrolls it into view.
fn inject_highlighter(mut body: Vec<u8>, needle: &str) -> Vec<u8> {
    let needle_json = serde_json::to_string(needle).unwrap_or_else(|_| "\"\"".into());
    let script = format!(
        r#"<style>#__librarian_hl{{background:#ffe08a;padding:1px 2px;border-radius:3px}}</style>
<script>(function(){{
var needle = {needle_json};
function norm(s) {{ return s.replace(/\s+/g, ' ').toLowerCase(); }}
var target = norm(needle).trim();
if (!target) return;
function attempt(len) {{
  var frag = target.slice(0, len).trim();
  if (!frag) return false;
  var walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  var node;
  while ((node = walker.nextNode())) {{
    if (norm(node.textContent).indexOf(frag) === -1) continue;
    var raw = node.textContent, lower = raw.toLowerCase();
    var probe = frag.slice(0, 40);
    var rawIdx = lower.indexOf(probe);
    if (rawIdx === -1) rawIdx = 0;
    try {{
      var range = document.createRange();
      range.setStart(node, rawIdx);
      range.setEnd(node, Math.min(raw.length, rawIdx + frag.length));
      var mark = document.createElement('mark');
      mark.id = '__librarian_hl';
      range.surroundContents(mark);
      mark.scrollIntoView({{block: 'center'}});
    }} catch (e) {{
      node.parentElement && node.parentElement.scrollIntoView({{block: 'center'}});
    }}
    return true;
  }}
  return false;
}}
window.addEventListener('load', function () {{
  if (!attempt(120)) if (!attempt(60)) attempt(30);
}});
}})();</script>"#
    );
    // Byte-wise case-insensitive search for the last "</body>" so the insert
    // offset is exact even with multibyte content.
    let needle_tag = b"</body>";
    let insert_at = body
        .windows(needle_tag.len())
        .rposition(|w| w.eq_ignore_ascii_case(needle_tag));
    match insert_at {
        Some(pos) => {
            body.splice(pos..pos, script.into_bytes());
        }
        None => body.extend_from_slice(script.as_bytes()),
    }
    body
}
