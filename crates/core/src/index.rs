//! Per-ZIM full-text passage index built with tantivy (BM25).
//!
//! One index directory per ZIM file. Documents are passage chunks (~1 kB of
//! plain text) so a search hit *is* a citable passage.

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
    path: Field,
    title: Field,
    body: Field,
    chunk: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let path = b.add_text_field("path", STRING | STORED);
    let title = b.add_text_field("title", TEXT | STORED);
    let body = b.add_text_field("body", TEXT | STORED);
    let chunk = b.add_u64_field("chunk", STORED | FAST);
    (b.build(), Fields { path, title, body, chunk })
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

/// Walk every HTML article in the ZIM, extract text, chunk it and index it.
/// Blocking; run on a worker thread. Returns the number of chunks indexed.
pub fn build_index(zim: &Zim, dir: &Path, progress: &IndexProgress) -> Result<u64> {
    std::fs::create_dir_all(dir)?;
    let (schema, f) = schema();
    // Recreate from scratch: indexing is idempotent per ZIM file.
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let index = Index::create_in_dir(dir, schema).context("creating tantivy index")?;
    let mut writer = index.writer(64 * 1024 * 1024)?;

    let article_ns = zim.article_namespace();
    let n = zim.entry_count();
    progress.total_entries.store(n as u64, Ordering::Relaxed);
    let mut chunks_total = 0u64;

    for i in 0..n {
        if progress.cancel.load(Ordering::Relaxed) {
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
        for (ci, chunk) in chunk_text(&extracted.text, CHUNK_TARGET_CHARS).into_iter().enumerate() {
            let mut doc = TantivyDocument::default();
            doc.add_text(f.path, &entry.path);
            doc.add_text(f.title, &title);
            doc.add_text(f.body, &chunk);
            doc.add_u64(f.chunk, ci as u64);
            writer.add_document(doc)?;
            chunks_total += 1;
        }
        if i % 5000 == 4999 {
            writer.commit()?;
        }
    }
    writer.commit()?;
    progress.chunks.store(chunks_total, Ordering::Relaxed);
    progress.finished.store(true, Ordering::Relaxed);
    Ok(chunks_total)
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

pub struct SearchIndex {
    reader: IndexReader,
    parser: QueryParser,
    fields: Fields,
}

impl SearchIndex {
    pub fn open(dir: &Path) -> Result<SearchIndex> {
        let index = Index::open_in_dir(dir)?;
        let (_, fields) = schema();
        let reader = index.reader()?;
        let mut parser =
            QueryParser::for_index(&index, vec![fields.title, fields.body]);
        parser.set_field_boost(fields.title, 2.0);
        Ok(SearchIndex { reader, parser, fields })
    }

    pub fn search(&self, query: &str, k: usize, zim_id: &str, book: &str) -> Result<Vec<Passage>> {
        let (q, _errors) = self.parser.parse_query_lenient(query);
        let searcher = self.reader.searcher();
        let top = searcher.search(&q, &TopDocs::with_limit(k))?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let get_str = |field: Field| -> String {
                doc.get_first(field)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            out.push(Passage {
                zim_id: zim_id.to_string(),
                path: get_str(self.fields.path),
                title: get_str(self.fields.title),
                text: get_str(self.fields.body),
                chunk: doc
                    .get_first(self.fields.chunk)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                score,
                book: book.to_string(),
            });
        }
        Ok(out)
    }
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

/// Merge per-book result lists into one ranked list, with diversity rules so
/// the model sees a spread of evidence rather than one dominant article:
/// - at most `PER_ARTICLE_CAP` passages from any single article;
/// - every book whose best hit is competitive (≥ 60% of the global top
///   score) is guaranteed at least one slot, so multi-book topics draw on
///   multiple books instead of whichever index scored marginally higher.
pub fn merge_passages(mut lists: Vec<Vec<Passage>>, k: usize) -> Vec<Passage> {
    const PER_ARTICLE_CAP: usize = 2;
    let by_score =
        |a: &Passage, b: &Passage| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal);
    let mut all: Vec<Passage> = lists.drain(..).flatten().collect();
    all.sort_by(by_score);
    // De-duplicate near-identical passages (same article + chunk).
    let mut seen = std::collections::HashSet::new();
    all.retain(|p| seen.insert((p.zim_id.clone(), p.path.clone(), p.chunk)));

    let top_score = all.first().map(|p| p.score).unwrap_or(0.0);
    let mut picked: Vec<Passage> = Vec::new();
    let mut used = vec![false; all.len()];
    let mut per_article: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();

    // Pass 1: best competitive passage from each book. (BM25 scores from
    // different indexes are only roughly comparable, so books whose best hit
    // is far below the leader don't earn a slot just for existing.)
    let mut books_seen = std::collections::HashSet::new();
    for (i, p) in all.iter().enumerate() {
        if picked.len() >= k {
            break;
        }
        if p.score >= top_score * 0.6 && books_seen.insert(p.zim_id.clone()) {
            *per_article.entry((p.zim_id.clone(), p.path.clone())).or_default() += 1;
            picked.push(p.clone());
            used[i] = true;
        }
    }
    // Pass 2: fill remaining slots by score, capped per article.
    for (i, p) in all.iter().enumerate() {
        if picked.len() >= k {
            break;
        }
        if used[i] {
            continue;
        }
        let n = per_article.entry((p.zim_id.clone(), p.path.clone())).or_default();
        if *n >= PER_ARTICLE_CAP {
            continue;
        }
        *n += 1;
        picked.push(p.clone());
    }
    picked.sort_by(by_score);
    picked
}

pub fn index_dir_for(data_dir: &Path, zim_id: &str) -> PathBuf {
    data_dir.join("index").join(zim_id)
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
    fn merge_guarantees_competitive_books_a_slot() {
        // Book z2 scores slightly lower everywhere but is clearly relevant:
        // it must still appear among the sources.
        let a = vec![
            p("z1", "A1", 0, 30.0),
            p("z1", "A2", 0, 29.0),
            p("z1", "A3", 0, 28.0),
            p("z1", "A4", 0, 27.0),
        ];
        let b = vec![p("z2", "B1", 0, 22.0), p("z2", "B2", 0, 21.0)];
        // An irrelevant book far below the leader earns nothing.
        let c = vec![p("z3", "C1", 0, 3.0)];
        let merged = merge_passages(vec![a, b, c], 4);
        assert!(merged.iter().any(|x| x.zim_id == "z2"), "book z2 missing: {merged:#?}");
        assert!(!merged.iter().any(|x| x.zim_id == "z3"), "noise book included");
        assert_eq!(merged.len(), 4);
    }

    #[test]
    fn merge_dedupes_and_keeps_score_order() {
        let a = vec![p("z1", "A", 0, 10.0), p("z1", "A", 0, 10.0), p("z1", "B", 0, 12.0)];
        let merged = merge_passages(vec![a], 6);
        assert_eq!(merged.len(), 2);
        assert!(merged[0].score >= merged[1].score);
    }
}
