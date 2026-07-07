//! Persistent chat history: one JSON file per conversation under
//! `<data>/chats/`. Assistant messages keep the passages they cited so the
//! UI can re-render clickable citations after a reload.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::index::Passage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String, // "user" | "assistant"
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Passage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub title: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// Starred chats pin to the top and are exempt from auto-pruning.
    #[serde(default)]
    pub starred: bool,
    pub messages: Vec<StoredMessage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMeta {
    pub id: String,
    pub title: String,
    pub updated_ms: u64,
    pub messages: usize,
    pub starred: bool,
}

/// Auto-prune threshold: newest unstarred chats beyond this total go away.
pub const MAX_CHATS: usize = 15;

pub struct ChatStore {
    dir: PathBuf,
    // Serializes read-modify-write cycles (append) across threads.
    lock: Mutex<()>,
    counter: AtomicU32,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

impl ChatStore {
    pub fn open(data_dir: &std::path::Path) -> Result<ChatStore> {
        let dir = data_dir.join("chats");
        std::fs::create_dir_all(&dir)?;
        let store = ChatStore { dir, lock: Mutex::new(()), counter: AtomicU32::new(0) };
        // Enforce the cap immediately, not just on the next message.
        store.prune(MAX_CHATS);
        Ok(store)
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            bail!("invalid chat id");
        }
        Ok(self.dir.join(format!("{id}.json")))
    }

    pub fn create(&self, title: &str) -> Result<Chat> {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let now = now_ms();
        let chat = Chat {
            id: format!("c{now}-{n}"),
            title: clip_title(title),
            created_ms: now,
            updated_ms: now,
            starred: false,
            messages: Vec::new(),
        };
        self.write(&chat)?;
        Ok(chat)
    }

    fn write(&self, chat: &Chat) -> Result<()> {
        let path = self.path(&chat.id)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(chat)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Chat> {
        let raw = std::fs::read_to_string(self.path(id)?).context("no such chat")?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        std::fs::remove_file(self.path(id)?)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<ChatMeta> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().map(|x| x == "json") != Some(true) {
                    continue;
                }
                if let Ok(raw) = std::fs::read_to_string(&p) {
                    if let Ok(c) = serde_json::from_str::<Chat>(&raw) {
                        out.push(ChatMeta {
                            id: c.id,
                            title: c.title,
                            updated_ms: c.updated_ms,
                            messages: c.messages.len(),
                            starred: c.starred,
                        });
                    }
                }
            }
        }
        // Starred chats pin to the top; both groups newest-first.
        out.sort_by(|a, b| b.starred.cmp(&a.starred).then(b.updated_ms.cmp(&a.updated_ms)));
        out
    }

    pub fn set_starred(&self, id: &str, starred: bool) -> Result<Chat> {
        let _guard = self.lock.lock().unwrap();
        let mut chat = self.get(id)?;
        chat.starred = starred;
        self.write(&chat)?;
        Ok(chat)
    }

    /// Keep at most `max_total` chats: starred chats always survive; the
    /// oldest unstarred chats beyond the limit are deleted.
    pub fn prune(&self, max_total: usize) -> usize {
        let metas = self.list();
        let starred = metas.iter().filter(|m| m.starred).count();
        let allowed_unstarred = max_total.saturating_sub(starred);
        let mut deleted = 0;
        for m in metas.iter().filter(|m| !m.starred).skip(allowed_unstarred) {
            if self.delete(&m.id).is_ok() {
                deleted += 1;
            }
        }
        deleted
    }

    /// Append a message; sets the chat title from the first user message.
    pub fn append(&self, id: &str, msg: StoredMessage) -> Result<Chat> {
        let _guard = self.lock.lock().unwrap();
        let mut chat = self.get(id)?;
        if chat.messages.is_empty() && msg.role == "user" {
            chat.title = clip_title(&msg.content);
        }
        chat.messages.push(msg);
        chat.updated_ms = now_ms();
        self.write(&chat)?;
        drop(_guard);
        self.prune(MAX_CHATS);
        Ok(chat)
    }
}

fn clip_title(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = if s.is_empty() { "New chat".to_string() } else { s };
    if s.chars().count() <= 60 {
        s
    } else {
        let cut: String = s.chars().take(57).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_store_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChatStore::open(tmp.path()).unwrap();
        let chat = store.create("").unwrap();
        assert_eq!(chat.title, "New chat");

        store
            .append(&chat.id, StoredMessage {
                role: "user".into(),
                content: "How do bees fly and why does it matter for gardens?".into(),
                sources: vec![],
            })
            .unwrap();
        let after = store
            .append(&chat.id, StoredMessage {
                role: "assistant".into(),
                content: "They beat their wings fast [1].".into(),
                sources: vec![Passage {
                    zim_id: "z".into(),
                    path: "Bee".into(),
                    title: "Bee".into(),
                    text: "wings".into(),
                    chunk: 0,
                    score: 1.0,
                    book: "B".into(),
                }],
            })
            .unwrap();
        // Title adopted from first user message.
        assert!(after.title.starts_with("How do bees fly"));

        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].messages, 2);

        let loaded = store.get(&chat.id).unwrap();
        assert_eq!(loaded.messages[1].sources.len(), 1);

        store.delete(&chat.id).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn starring_pins_protects_and_prune_caps_total() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChatStore::open(tmp.path()).unwrap();
        // Star the first (oldest) chat, then flood the store: appends
        // auto-prune, and the starred chat must survive.
        let first = store.create("").unwrap();
        store
            .append(&first.id, StoredMessage {
                role: "user".into(),
                content: "keep me".into(),
                sources: vec![],
            })
            .unwrap();
        store.set_starred(&first.id, true).unwrap();
        for i in 0..20 {
            let c = store.create("").unwrap();
            store
                .append(&c.id, StoredMessage {
                    role: "user".into(),
                    content: format!("question {i}"),
                    sources: vec![],
                })
                .unwrap();
        }
        let metas = store.list();
        assert!(metas.len() <= MAX_CHATS, "{} chats left", metas.len());
        assert_eq!(metas[0].id, first.id, "starred chat not pinned to top");
        assert!(metas[0].starred);
        // Unstarred remainder is newest-first.
        assert!(metas[1].updated_ms >= metas[2].updated_ms);
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChatStore::open(tmp.path()).unwrap();
        assert!(store.get("../../etc/passwd").is_err());
        assert!(store.get("a/b").is_err());
    }
}
