//! Library-wide full-text passage index built with tantivy (BM25).
//!
//! One global index shared by all books (so scores are comparable across
//! books); documents are passage chunks (~1 kB of plain text), so a search
//! hit *is* a citable passage. Books are removed via delete-by-term.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value, FAST, STORED, STRING, TEXT};
use tantivy::{Index, IndexReader, TantivyDocument};
use zimlib::{EntryKind, Zim};

use crate::text::{chunk_text, html_to_text};

pub const CHUNK_TARGET_CHARS: usize = 1100;

#[derive(Clone)]
struct Fields {
    zim: Field,
    path: Field,
    title: Field,
    body: Field,
    chunk: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let zim = b.add_text_field("zim", STRING | STORED);
    let path = b.add_text_field("path", STRING | STORED);
    let title = b.add_text_field("title", TEXT | STORED);
    let body = b.add_text_field("body", TEXT | STORED);
    let chunk = b.add_u64_field("chunk", STORED | FAST);
    (b.build(), Fields { zim, path, title, body, chunk })
}

/// Progress of a background indexing run, shared with the UI.
#[derive(Default)]
pub struct IndexProgress {
    pub total_entries: AtomicU64,
    pub done_entries: AtomicU64,
    pub chunks: AtomicU64,
    pub finished: AtomicBool,
    pub failed: AtomicBool,
    pub cancel: AtomicBool,
}

/// One search index shared by every book in the library. A single index (as
/// opposed to one per ZIM) gives global corpus statistics, so BM25 scores are
/// comparable across books — a huge wiki cannot drown out a small one just
/// because its index has different term statistics. Books are removed with
/// `delete_term` on their ZIM id.
pub struct GlobalIndex {
    index: Index,
    writer: std::sync::Mutex<tantivy::IndexWriter>,
    reader: IndexReader,
    fields: Fields,
}

impl GlobalIndex {
    pub fn open_or_create(dir: &Path) -> Result<GlobalIndex> {
        std::fs::create_dir_all(dir)?;
        let (schema, fields) = schema();
        let index = if dir.join("meta.json").exists() {
            Index::open_in_dir(dir).context("opening search index")?
        } else {
            Index::create_in_dir(dir, schema).context("creating search index")?
        };
        let writer = index.writer(64 * 1024 * 1024)?;
        let reader = index.reader()?;
        Ok(GlobalIndex { index, writer: std::sync::Mutex::new(writer), reader, fields })
    }

    /// Walk every HTML article in the ZIM, extract text, chunk it and index
    /// it. Blocking; run on a worker thread. Idempotent: any existing chunks
    /// for this ZIM id are removed first.
    pub fn index_zim(&self, zim: &Zim, zim_id: &str, progress: &IndexProgress) -> Result<u64> {
        let f = &self.fields;
        let mut writer = self.writer.lock().unwrap();
        writer.delete_term(tantivy::Term::from_field_text(f.zim, zim_id));

        let article_ns = zim.article_namespace();
        let n = zim.entry_count();
        progress.total_entries.store(n as u64, Ordering::Relaxed);
        let mut chunks_total = 0u64;

        for i in 0..n {
            if progress.cancel.load(Ordering::Relaxed) {
                writer.rollback()?;
                anyhow::bail!("indexing cancelled");
            }
            progress.done_entries.store(i as u64 + 1, Ordering::Relaxed);
            let Ok(entry) = zim.entry_at(i) else { continue };
            if entry.namespace != article_ns || !entry.is_html() {
                continue;
            }
            let EntryKind::Content { .. } = entry.kind else { continue };
            let Ok(bytes) = zim.content(&entry) else { continue };
            let html = String::from_utf8_lossy(&bytes);
            let extracted = html_to_text(&html);
            let title = extracted.title.unwrap_or_else(|| entry.title.clone());
            for (ci, chunk) in
                chunk_text(&extracted.text, CHUNK_TARGET_CHARS).into_iter().enumerate()
            {
                let mut doc = TantivyDocument::default();
                doc.add_text(f.zim, zim_id);
                doc.add_text(f.path, &entry.path);
                doc.add_text(f.title, &title);
                doc.add_text(f.body, &chunk);
                doc.add_u64(f.chunk, ci as u64);
                writer.add_document(doc)?;
                chunks_total += 1;
            }
        }
        writer.commit()?;
        progress.chunks.store(chunks_total, Ordering::Relaxed);
        progress.finished.store(true, Ordering::Relaxed);
        Ok(chunks_total)
    }

    /// Remove every chunk belonging to a book.
    pub fn remove_zim(&self, zim_id: &str) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.delete_term(tantivy::Term::from_field_text(self.fields.zim, zim_id));
        writer.commit()?;
        Ok(())
    }

    /// BM25 search across all books; scores are globally comparable.
    /// `book_names` maps ZIM id → display name for the returned passages.
    pub fn search(
        &self,
        query: &str,
        k: usize,
        book_names: &std::collections::HashMap<String, String>,
    ) -> Result<Vec<Passage>> {
        let f = &self.fields;
        let mut parser = QueryParser::for_index(&self.index, vec![f.title, f.body]);
        parser.set_field_boost(f.title, 2.0);
        let (q, _errors) = parser.parse_query_lenient(query);
        let searcher = self.reader.searcher();
        // Over-fetch so the per-article cap still leaves k results.
        let top = searcher.search(&q, &TopDocs::with_limit(k * 4))?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let get_str = |field: Field| -> String {
                doc.get_first(field)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let zim_id = get_str(f.zim);
            let book = book_names.get(&zim_id).cloned().unwrap_or_default();
            out.push(Passage {
                zim_id,
                path: get_str(f.path),
                title: get_str(f.title),
                text: get_str(f.body),
                chunk: doc.get_first(f.chunk).and_then(|v| v.as_u64()).unwrap_or(0),
                score,
                book,
            });
        }
        Ok(merge_passages(vec![out], k))
    }
}

/// A single retrieved passage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Passage {
    pub zim_id: String,
    pub path: String,
    pub title: String,
    pub text: String,
    pub chunk: u64,
    pub score: f32,
    /// Human-readable name of the library book this came from.
    pub book: String,
}

/// Prepare a user question for lexical retrieval: strip punctuation and
/// question boilerplate so the BM25 query is keyword-shaped.
pub fn query_from_question(q: &str) -> String {
    const STOP: &[&str] = &[
        "what", "who", "when", "where", "why", "how", "is", "are", "was", "were", "the", "a",
        "an", "do", "does", "did", "can", "could", "should", "would", "of", "to", "in", "on",
        "for", "about", "tell", "me", "explain", "please", "you", "i",
    ];
    let cleaned: String = q
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { ' ' })
        .collect();
    let words: Vec<&str> = cleaned
        .split_whitespace()
        .filter(|w| !STOP.contains(&w.to_ascii_lowercase().as_str()))
        .collect();
    if words.is_empty() {
        cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        words.join(" ")
    }
}

/// Merge per-book result lists into one ranked list.
///
/// Diversity comes from a per-article cap (one long article can't fill every
/// slot), and junk is trimmed by a relevance floor relative to the best hit.
/// There is deliberately NO forced per-book representation: guaranteeing
/// books a slot pulled in wildly irrelevant passages just for existing.
pub fn merge_passages(mut lists: Vec<Vec<Passage>>, k: usize) -> Vec<Passage> {
    const PER_ARTICLE_CAP: usize = 2;
    /// Passages scoring below this fraction of the top hit are dropped
    /// entirely — fewer, relevant sources beat a padded list.
    const SCORE_FLOOR: f32 = 0.35;
    let by_score =
        |a: &Passage, b: &Passage| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal);
    let mut all: Vec<Passage> = lists.drain(..).flatten().collect();
    all.sort_by(by_score);
    // De-duplicate near-identical passages (same article + chunk).
    let mut seen = std::collections::HashSet::new();
    all.retain(|p| seen.insert((p.zim_id.clone(), p.path.clone(), p.chunk)));

    let top_score = all.first().map(|p| p.score).unwrap_or(0.0);
    let mut picked: Vec<Passage> = Vec::new();
    let mut per_article: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for p in &all {
        if picked.len() >= k {
            break;
        }
        if p.score < top_score * SCORE_FLOOR {
            break; // sorted by score: everything after is worse
        }
        let n = per_article.entry((p.zim_id.clone(), p.path.clone())).or_default();
        if *n >= PER_ARTICLE_CAP {
            continue;
        }
        *n += 1;
        picked.push(p.clone());
    }
    picked
}

pub fn global_index_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("index").join("global")
}

pub type SharedProgress = Arc<IndexProgress>;

#[cfg(test)]
mod tests {
    use super::*;

    fn p(zim: &str, path: &str, chunk: u64, score: f32) -> Passage {
        Passage {
            zim_id: zim.into(),
            path: path.into(),
            title: path.into(),
            text: format!("text {path} {chunk}"),
            chunk,
            score,
            book: zim.into(),
        }
    }

    #[test]
    fn merge_caps_passages_per_article() {
        // One article with 5 top-scoring chunks must not fill every slot.
        let a = vec![
            p("z1", "Dominant", 0, 30.0),
            p("z1", "Dominant", 1, 29.0),
            p("z1", "Dominant", 2, 28.0),
            p("z1", "Dominant", 3, 27.0),
            p("z1", "Dominant", 4, 26.0),
            p("z1", "Other", 0, 20.0),
            p("z1", "Third", 0, 19.0),
        ];
        let merged = merge_passages(vec![a], 6);
        let dominant = merged.iter().filter(|x| x.path == "Dominant").count();
        assert_eq!(dominant, 2, "per-article cap not applied: {merged:#?}");
        assert!(merged.iter().any(|x| x.path == "Other"));
        assert!(merged.iter().any(|x| x.path == "Third"));
    }

    #[test]
    fn merge_drops_passages_far_below_the_best_hit() {
        // Junk passages must not be padded in just to fill k slots.
        let a = vec![
            p("z1", "Relevant", 0, 30.0),
            p("z1", "AlsoGood", 0, 24.0),
            p("z2", "Noise1", 0, 6.0),
            p("z2", "Noise2", 0, 4.0),
        ];
        let merged = merge_passages(vec![a], 6);
        assert_eq!(merged.len(), 2, "junk was included: {merged:#?}");
        assert!(merged.iter().all(|x| x.score >= 24.0));
    }

    #[test]
    fn merge_has_no_forced_book_representation() {
        // A second book with weak hits earns nothing just for existing.
        let a = vec![p("z1", "A1", 0, 30.0), p("z1", "A2", 0, 29.0)];
        let b = vec![p("z2", "B1", 0, 9.0)];
        let merged = merge_passages(vec![a, b], 3);
        assert!(merged.iter().all(|x| x.zim_id == "z1"), "{merged:#?}");
    }

    #[test]
    fn merge_dedupes_and_keeps_score_order() {
        let a = vec![p("z1", "A", 0, 10.0), p("z1", "A", 0, 10.0), p("z1", "B", 0, 12.0)];
        let merged = merge_passages(vec![a], 6);
        assert_eq!(merged.len(), 2);
        assert!(merged[0].score >= merged[1].score);
    }
}
