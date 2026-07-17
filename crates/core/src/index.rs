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

/// How many ZIM entries to process between durable commits. Small enough that
/// an interrupted index loses only seconds of work, large enough to avoid
/// tantivy segment churn. ~11 commits for the 108 k-entry OSM wiki.
pub const CHECKPOINT_ENTRIES: u32 = 10_000;

/// How an indexing run ended.
pub enum IndexOutcome {
    /// Reached the end of the ZIM — the book is fully indexed.
    Done { chunks: u64 },
    /// Stopped early (cancel requested, e.g. the app is being backgrounded);
    /// committed progress up to `next_entry`, to resume there next time.
    Paused { next_entry: u32, chunks: u64 },
}

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

    /// Walk every HTML article in the ZIM, extract text, chunk it and index it.
    /// Blocking; run on a worker thread.
    ///
    /// Resumable: pass the last checkpoint's `resume_from` entry index and
    /// `chunks_so_far`; indexing continues from there and commits every
    /// `CHECKPOINT_ENTRIES`, invoking `checkpoint(next_entry, chunks_total)`
    /// after each commit so the caller can persist a durable resume point. This
    /// is what lets a large index survive the app being suspended/killed on iOS
    /// (each app session picks up where the last left off) rather than
    /// restarting from zero. When `progress.cancel` is set the run commits what
    /// it has and returns `Paused` at the current entry.
    ///
    /// A crash between a commit and the caller persisting the checkpoint can
    /// re-process at most one batch on resume; the duplicate chunks are
    /// harmless because retrieval dedups by `(zim_id, path, chunk)`. Only a
    /// fresh run (`resume_from == 0`) clears the book's existing chunks.
    pub fn index_zim(
        &self,
        zim: &Zim,
        zim_id: &str,
        resume_from: u32,
        chunks_so_far: u64,
        progress: &IndexProgress,
        mut checkpoint: impl FnMut(u32, u64),
    ) -> Result<IndexOutcome> {
        let f = &self.fields;
        let mut writer = self.writer.lock().unwrap();
        if resume_from == 0 {
            writer.delete_term(tantivy::Term::from_field_text(f.zim, zim_id));
        }

        let article_ns = zim.article_namespace();
        let n = zim.entry_count();
        progress.total_entries.store(n as u64, Ordering::Relaxed);
        progress.done_entries.store(resume_from as u64, Ordering::Relaxed);
        let mut chunks_total = chunks_so_far;
        progress.chunks.store(chunks_total, Ordering::Relaxed);
        let mut since_commit = 0u32;

        let mut i = resume_from;
        while i < n {
            if progress.cancel.load(Ordering::Relaxed) {
                // Checkpoint and stop: commit what we have so a later run
                // resumes from exactly here instead of losing this session.
                writer.commit()?;
                checkpoint(i, chunks_total);
                return Ok(IndexOutcome::Paused { next_entry: i, chunks: chunks_total });
            }
            progress.done_entries.store(i as u64 + 1, Ordering::Relaxed);
            if let Ok(entry) = zim.entry_at(i) {
                let is_pdf = entry.mime.as_deref() == Some("application/pdf");
                if entry.namespace == article_ns && (entry.is_html() || is_pdf) {
                    if let (EntryKind::Content { .. }, Ok(bytes)) = (&entry.kind, zim.content(&entry))
                    {
                        let extracted = if is_pdf {
                            // Some ZIMs (e.g. the "zimgit" collections) are
                            // bundles of PDF documents rather than HTML articles.
                            pdf_to_text(&bytes).map(|text| {
                                let name = entry
                                    .title
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(&entry.title)
                                    .trim_end_matches(".pdf")
                                    .trim()
                                    .to_string();
                                (name, text)
                            })
                        } else {
                            let html = String::from_utf8_lossy(&bytes);
                            let e = html_to_text(&html);
                            Some((e.title.unwrap_or_else(|| entry.title.clone()), e.text))
                        };
                        if let Some((title, text)) = extracted {
                            for (ci, chunk) in
                                chunk_text(&text, CHUNK_TARGET_CHARS).into_iter().enumerate()
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
                    }
                }
            }
            since_commit += 1;
            i += 1;
            if since_commit >= CHECKPOINT_ENTRIES {
                writer.commit()?;
                checkpoint(i, chunks_total);
                progress.chunks.store(chunks_total, Ordering::Relaxed);
                since_commit = 0;
            }
        }
        writer.commit()?;
        // Make the commit visible to searches immediately (the default
        // reload policy is delayed, which races tests and first queries).
        self.reader.reload()?;
        progress.chunks.store(chunks_total, Ordering::Relaxed);
        Ok(IndexOutcome::Done { chunks: chunks_total })
    }

    /// Version of the indexing pipeline; bump when indexing gains new
    /// capabilities (so existing books get reindexed on upgrade).
    pub const PIPELINE_VERSION: u32 = 3; // v3: clean PDF titles

    /// Remove every chunk belonging to a book.
    pub fn remove_zim(&self, zim_id: &str) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.delete_term(tantivy::Term::from_field_text(self.fields.zim, zim_id));
        writer.commit()?;
        self.reader.reload()?;
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
        Ok(merge_passages(vec![self.search_raw(query, k * 4, book_names)?], k))
    }

    /// Raw ranked hits without the cap/floor post-processing.
    pub fn search_raw(
        &self,
        query: &str,
        limit: usize,
        book_names: &std::collections::HashMap<String, String>,
    ) -> Result<Vec<Passage>> {
        let f = &self.fields;
        let mut parser = QueryParser::for_index(&self.index, vec![f.title, f.body]);
        parser.set_field_boost(f.title, 1.4);
        let (q, _errors) = parser.parse_query_lenient(query);
        let searcher = self.reader.searcher();
        let top = searcher.search(&q, &TopDocs::with_limit(limit.max(1)))?;
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
        Ok(out)
    }
}

/// Extract plain text from a PDF. Returns None when the PDF has no usable
/// text layer (scanned images) or the parser chokes — PDF parsing is wild
/// territory, so panics are contained here.
fn pdf_to_text(bytes: &[u8]) -> Option<String> {
    let result = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes));
    let text = result.ok()?.ok()?;
    // Collapse the frequently-broken line layout into paragraphs: blank
    // lines separate paragraphs, single newlines are soft wraps.
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push(if blank > 0 { '\n' } else { ' ' });
        }
        out.push_str(l);
        blank = 0;
    }
    (out.len() > 100).then_some(out)
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
