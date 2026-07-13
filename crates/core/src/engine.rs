//! Text-generation engines. `Engine` abstracts over the real llama.cpp
//! backend (feature `llama`) and a deterministic extractive stub used when no
//! model is installed and in tests.

use anyhow::Result;

use crate::index::Passage;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String, // "system" | "user" | "assistant"
    pub content: String,
}

/// Streaming token callback. Return `false` to stop generation (client gone).
pub type TokenSink<'a> = &'a mut (dyn FnMut(&str) -> bool + Send);

pub trait Engine: Send + Sync {
    fn name(&self) -> String;
    /// Generate a reply to the chat, streaming pieces into `sink`, emitting
    /// at most `max_new_tokens` tokens.
    fn generate(
        &self,
        messages: &[ChatMessage],
        sink: TokenSink,
        max_new_tokens: usize,
    ) -> Result<String>;
    /// Whether this engine is capable enough to plan retrieval queries.
    fn can_plan(&self) -> bool {
        false
    }
}

/// Build the grounded prompt: system rules + numbered sources + conversation.
pub fn build_messages(
    history: &[ChatMessage],
    question: &str,
    passages: &[Passage],
) -> Vec<ChatMessage> {
    let mut sources = String::new();
    for (i, p) in passages.iter().enumerate() {
        sources.push_str(&format!(
            "[{n}] \"{title}\" ({book})\n{text}\n\n",
            n = i + 1,
            title = p.title,
            book = p.book,
            text = p.text
        ));
    }
    let system = format!(
        "You are a knowledgeable, friendly librarian. Use the conversation to understand what \
         the user needs, resolving follow-ups and references to earlier turns.\n\
         Ground every factual claim in the numbered sources below, placing the citation \
         immediately after the claim it supports, like [1] or [2][3]. Only cite numbers that \
         exist in the source list, and never put citations on text a source does not support. \
         Do not append lists of citations at the end of the answer.\n\
         Not every retrieved source is relevant — silently ignore the ones that are not. \
         If the sources do not actually contain what the user needs, say plainly that their \
         library does not seem to cover this topic (mentioning the closest thing it does \
         have, if anything) and stop — do not answer from your own general knowledge, and do \
         not force an answer out of irrelevant sources.\n\
         If the user asks you to rephrase, shorten, expand or continue, work from the \
         conversation and keep the citations that still apply.\n\
         Keep answers concise and factual.\n\nSOURCES:\n{sources}"
    );
    let mut msgs = vec![ChatMessage { role: "system".into(), content: system }];
    // Keep a generous window of prior turns for conversational context —
    // budgeted by size, not a fixed count, so long chats stay coherent.
    msgs.extend(history_window(history, ANSWER_HISTORY_CHARS, 16));
    msgs.push(ChatMessage { role: "user".into(), content: question.to_string() });
    msgs
}

/// Character budget for conversation history in the answer prompt. With the
/// default 8192-token context and chars ≈ 3× tokens, this leaves ample room
/// for the system rules + sources block and the generation budget.
const ANSWER_HISTORY_CHARS: usize = 8000;
/// Smaller budget for the retrieval planner: it only needs enough of the
/// conversation to resolve references, not every word of every answer.
const PLANNER_HISTORY_CHARS: usize = 4000;
/// One rambling turn must not evict the rest of the window.
const PER_MESSAGE_CHAR_CAP: usize = 2000;

/// Select the most recent history messages that fit a character budget
/// (newest kept first, returned in chronological order). Oversized messages
/// are clipped instead of evicting everything before them.
pub fn history_window(
    history: &[ChatMessage],
    char_budget: usize,
    max_msgs: usize,
) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::new();
    let mut used = 0usize;
    for m in history.iter().rev() {
        if out.len() >= max_msgs || used >= char_budget {
            break;
        }
        let cap = PER_MESSAGE_CHAR_CAP.min(char_budget - used);
        let mut content = m.content.clone();
        if content.chars().count() > cap {
            content = content.chars().take(cap).collect();
            content.push('…');
        }
        used += content.chars().count();
        out.push(ChatMessage { role: m.role.clone(), content });
    }
    out.reverse();
    out
}

/// Drop tokens from the middle of an over-budget prompt, preserving the head
/// (system rules + sources) and the tail (recent turns + the question). A
/// blind end-truncation would cut off the question itself.
pub fn keep_head_tail<T: Copy>(tokens: &mut Vec<T>, budget: usize) {
    if tokens.len() <= budget {
        return;
    }
    let head = budget * 2 / 3;
    let tail = budget - head;
    let cut = tokens.len() - tail;
    tokens.drain(head..cut);
}

/// Build the retrieval query for a question, folding in conversation context
/// when the question refers back to it ("how do I configure it?", "what
/// about IPv6?"). Keyword search can't resolve pronouns, so anaphoric or
/// very short questions are augmented with the previous user turns.
pub fn contextual_question(history: &[ChatMessage], question: &str) -> String {
    const ANAPHORA: &[&str] = &[
        "it", "its", "that", "this", "those", "these", "they", "them", "their", "he", "she",
        "him", "her", "one", "there", "same", "also", "again", "more",
    ];
    let lower = question.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let is_followup = content_words(question).len() < 3
        || words.iter().any(|w| ANAPHORA.contains(w))
        || lower.starts_with("what about")
        || lower.starts_with("how about")
        || lower.starts_with("and ");
    if !is_followup {
        return question.to_string();
    }
    // Most recent user turns carry the topic; two is enough context without
    // drowning the new question in old keywords.
    let prior: Vec<&str> = history
        .iter()
        .rev()
        .filter(|m| m.role == "user")
        .take(2)
        .map(|m| m.content.as_str())
        .collect();
    if prior.is_empty() {
        return question.to_string();
    }
    let mut q = String::new();
    for p in prior.iter().rev() {
        q.push_str(p);
        q.push(' ');
    }
    q.push_str(question);
    q
}

/// How the next answer should be grounded, decided by the model itself when
/// it is capable, so the librarian uses conversational context intelligently.
#[derive(Debug, Clone, PartialEq)]
pub enum RetrievalPlan {
    /// Search the library with these alternative queries (results merged).
    Search(Vec<String>),
    /// The message is a refinement (rephrase/shorten/continue…): answer from
    /// the conversation, reusing the previously retrieved sources.
    ReusePrevious,
}

/// Decide what to search for, given the conversation. Capable engines read
/// the whole conversation and produce a self-contained query (or decide no
/// new search is needed); otherwise fall back to the keyword heuristic.
pub fn plan_retrieval(
    engine: &dyn Engine,
    history: &[ChatMessage],
    question: &str,
) -> RetrievalPlan {
    if !engine.can_plan() {
        return RetrievalPlan::Search(vec![contextual_question(history, question)]);
    }
    let queries_part = "Reply with ONE to THREE search queries, one per line, each 3-8 \
        keywords (not sentences). Imagine the how-to guides or encyclopedia articles that \
        would answer the message, and write each query like such an article's title or key \
        terms — standard technical names of the tools, protocols and concepts involved, not \
        the user's casual phrasing. Make the lines different from each other (synonyms, \
        alternative approaches), resolving pronouns and references from the conversation. \
        Output only the queries — no explanations, no numbering, no quotes.";
    let system = if history.is_empty() {
        format!(
            "You plan library searches for a librarian assistant. Read the user's message. \
             {queries_part}"
        )
    } else {
        format!(
            "You plan library searches for a librarian assistant. Read the conversation and \
             the user's newest message. {queries_part}\n\
             Exception: reply with the single word NONE if the newest message only asks to \
             rephrase, shorten, expand, summarize or otherwise rework the previous answer."
        )
    };
    let mut msgs = vec![ChatMessage { role: "system".into(), content: system }];
    msgs.extend(history_window(history, PLANNER_HISTORY_CHARS, 12));
    msgs.push(ChatMessage { role: "user".into(), content: question.to_string() });

    let mut sink = |_: &str| true;
    let out = match engine.generate(&msgs, &mut sink, 96) {
        Ok(o) => o,
        Err(_) => return RetrievalPlan::Search(vec![contextual_question(history, question)]),
    };
    eprintln!("[planner] {out:?}");
    // NONE is checked against the raw first line, before the query filter
    // (which rejects single words) can eat it.
    let first_raw = out.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if first_raw.trim_matches(['"', '`', '\'', '.', '*']).eq_ignore_ascii_case("none") {
        if history.is_empty() {
            return RetrievalPlan::Search(vec![question.to_string()]);
        }
        return RetrievalPlan::ReusePrevious;
    }
    // Small models often answer the question instead of writing queries;
    // keep only lines that actually look like keyword queries.
    let lines: Vec<String> = out.lines().filter_map(clean_query_line).take(3).collect();
    if lines.is_empty() {
        return RetrievalPlan::Search(vec![contextual_question(history, question)]);
    }
    // The user's own words stay in the mix: they often carry disambiguating
    // context (like a book or product name) the rewrites drop.
    let mut queries = lines;
    queries.push(contextual_question(history, question));
    RetrievalPlan::Search(queries)
}

/// Accept a planner output line only if it plausibly is a keyword query, not
/// prose, markdown or code the model produced while "answering" instead of
/// planning. Returns the cleaned query.
fn clean_query_line(line: &str) -> Option<String> {
    let s = line.trim();
    // Markdown, code and shell artifacts disqualify the line outright — a
    // model that emits them is answering, not planning.
    if s.contains("**")
        || s.starts_with('#')
        || s.contains("://")
        || s.contains(['`', '<', '>', '{', '}', '=', ';', '$', '|'])
        || s.starts_with("sudo ")
        || s.split_whitespace().any(|w| w.chars().count() > 24)
    {
        return None;
    }
    // Strip list numbering ("1.", "2)", "-", "*") and quoting.
    let s = s
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == ' ')
        .trim_matches(['"', '`', '\'', '-', '*'])
        .trim();
    if s.is_empty() || s.chars().count() > 120 {
        return None;
    }
    // Sentences aren't queries: reject prose-like lines.
    let words = s.split_whitespace().count();
    if !(2..=10).contains(&words) || s.ends_with(':') || s.contains(". ") {
        return None;
    }
    if words > 6 && s.ends_with('.') {
        return None;
    }
    Some(s.trim_end_matches('.').to_string())
}

/// Triage retrieved candidates the way a librarian skims books before
/// answering: the model judges each passage's relevance to the question in
/// isolation (far more reliable for small models than asking them to ignore
/// junk while writing the answer).
///
/// Returns `None` when triage isn't possible (no capable engine / call
/// failed / unparseable) — caller should fall back to using all candidates.
/// `Some(vec![])` means the model judged NOTHING relevant.
pub fn triage_sources(
    engine: &dyn Engine,
    history: &[ChatMessage],
    question: &str,
    candidates: &[Passage],
) -> Option<Vec<usize>> {
    if !engine.can_plan() || candidates.is_empty() {
        return None;
    }
    // Follow-ups like "what about children?" triage blind without the
    // conversation topic; fold it in the same way retrieval does.
    let question = contextual_question(history, question);
    let mut list = String::new();
    for (i, p) in candidates.iter().enumerate() {
        let snippet: String = p.text.chars().take(300).collect();
        list.push_str(&format!("[{}] \"{}\" ({}): {}\n\n", i + 1, p.title, p.book, snippet));
    }
    let system = "You are the triage step of a librarian's search system. This is a \
        reference-lookup task: you are only selecting which library passages to hand over \
        for answering — you are not answering or giving advice yourself. \
        Keep EVERY passage that contains information that helps answer the question — the \
        facts, steps or explanations themselves. Discard passages that merely mention \
        similar words, define a related term, or are about tagging/cataloguing the topic. \
        Test: could part of the answer be quoted from this passage? \
        Reply with only the numbers of the passages to keep, comma-separated (example: \
        1,2,5). If none qualify, reply NONE.";
    let user = format!("Question: {question}\n\nCANDIDATE PASSAGES:\n{list}");
    let msgs = vec![
        ChatMessage { role: "system".into(), content: system.into() },
        ChatMessage { role: "user".into(), content: user },
    ];
    let mut sink = |_: &str| true;
    let out = engine.generate(&msgs, &mut sink, 32).ok()?;
    eprintln!(
        "[triage] cands={:?} -> {out:?}",
        candidates.iter().map(|p| p.title.as_str()).collect::<Vec<_>>()
    );
    let first = out.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    // Pull index numbers out of the reply, in order, deduped.
    let mut keep: Vec<usize> = Vec::new();
    let mut cur = String::new();
    for c in first.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            if let Ok(n) = cur.parse::<usize>() {
                if n >= 1 && n <= candidates.len() && !keep.contains(&(n - 1)) {
                    keep.push(n - 1);
                }
            }
            cur.clear();
        }
    }
    if keep.is_empty() {
        if first.to_ascii_lowercase().contains("none") {
            return Some(Vec::new());
        }
        return None; // unparseable → caller falls back
    }
    Some(keep)
}

/// Whether the text contains at least one citation marker like `[3]`.
pub fn has_citations(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i + 2 < b.len() {
        if b[i] == b'[' && b[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j < b.len() && b[j] == b']' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Post-process an answer's citations: strip citation numbers that don't
/// exist, drop trailing bare-citation spam, and align each substantive
/// uncited sentence to the source passage with the highest content-word
/// overlap (small models often forget the citation instruction). Text no
/// source supports deliberately stays uncited.
pub fn enforce_citations(answer: &str, passages: &[Passage]) -> String {
    let n_sources = passages.len();
    let passage_words: Vec<std::collections::HashSet<String>> =
        passages.iter().map(|p| content_words(&format!("{} {}", p.title, p.text))).collect();

    let mut out = String::with_capacity(answer.len() + 32);
    let mut in_code = false;
    for line in answer.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            out.push_str(line);
            continue;
        }
        if in_code || trimmed.is_empty() {
            out.push_str(line);
            continue;
        }
        out.push_str(&cite_line(line, n_sources, &passage_words));
    }
    // Drop trailing lines that are nothing but citation markers ("[1] [2]…")
    // — citation spam some models emit at the end of an answer.
    let mut s = out.trim_end().to_string();
    while let Some(last) = s.lines().last() {
        let residue: String = last.chars().filter(|c| !"[] 0123456789\t".contains(*c)).collect();
        let only_cites = residue.is_empty() && last.contains('[');
        if !only_cites {
            break;
        }
        s.truncate(s.len() - last.len());
        s = s.trim_end().to_string();
    }
    s
}

fn cite_line(line: &str, n_sources: usize, passage_words: &[std::collections::HashSet<String>]) -> String {
    // Drop out-of-range citations like [7] when only 6 sources exist.
    let mut cleaned = String::with_capacity(line.len());
    let mut has_valid_cite = false;
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        cleaned.push_str(&rest[..open]);
        let tail = &rest[open..];
        let close = tail.find(']');
        match close {
            Some(c) if c <= 3 && tail[1..c].chars().all(|ch| ch.is_ascii_digit()) && c > 1 => {
                let n: usize = tail[1..c].parse().unwrap_or(0);
                if n >= 1 && n <= n_sources {
                    cleaned.push_str(&tail[..=c]);
                    has_valid_cite = true;
                }
                rest = &tail[c + 1..];
            }
            _ => {
                cleaned.push('[');
                rest = &tail[1..];
            }
        }
    }
    cleaned.push_str(rest);

    if has_valid_cite {
        return cleaned;
    }
    // Substantive uncited line: align to the best-overlapping passage.
    let words = content_words(&cleaned);
    if words.len() < 5 {
        return cleaned;
    }
    let mut best = (0usize, 0usize); // (overlap, index)
    for (i, pw) in passage_words.iter().enumerate() {
        let overlap = words.iter().filter(|w| pw.contains(*w)).count();
        if overlap > best.0 {
            best = (overlap, i);
        }
    }
    // Require both an absolute and relative overlap so we don't stamp
    // citations on sentences the sources don't actually support.
    if best.0 >= 4 && best.0 * 2 >= words.len() {
        let trailing_ws: String =
            cleaned.chars().rev().take_while(|c| c.is_whitespace()).collect();
        let body = cleaned.trim_end();
        let body = body.strip_suffix(['.', ':', ';']).unwrap_or(body);
        let punct = cleaned.trim_end().chars().last().filter(|c| ".:;".contains(*c));
        let mut s = format!("{body} [{}]", best.1 + 1);
        if let Some(p) = punct {
            s.push(p);
        }
        s.extend(trailing_ws.chars().rev());
        return s;
    }
    cleaned
}

fn content_words(s: &str) -> std::collections::HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(|w| w.to_lowercase())
        .collect()
}

/// Extractive fallback engine: no model weights required. Quotes the top
/// sources with citations. Also used by the test-suite.
pub struct StubEngine;

impl Engine for StubEngine {
    fn name(&self) -> String {
        "extractive (no model installed)".into()
    }

    fn generate(
        &self,
        messages: &[ChatMessage],
        sink: TokenSink,
        _max_new_tokens: usize,
    ) -> Result<String> {
        // Recover the numbered sources from the system message.
        let system = messages.first().map(|m| m.content.as_str()).unwrap_or("");
        let mut out = String::from(
            "No language model is installed, so here are the most relevant passages \
             from your library:\n\n",
        );
        let sources = system
            .split_once("SOURCES:\n")
            .map(|(_, s)| s)
            .unwrap_or("");
        let mut n = 0;
        for block in sources.split("\n\n") {
            if !block.trim_start().starts_with('[') {
                continue;
            }
            n += 1;
            if n > 3 {
                break;
            }
            let mut lines = block.lines();
            let head = lines.next().unwrap_or("");
            let body: String = lines.collect::<Vec<_>>().join(" ");
            let snippet: String = body.chars().take(300).collect();
            out.push_str(&format!("{head}\n{snippet}… [{n}]\n\n"));
        }
        if n == 0 {
            out = "I could not find anything relevant in your library for that question.".into();
        }
        for piece in out.split_inclusive(' ') {
            if !sink(piece) {
                break;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passage(t: &str, text: &str) -> Passage {
        Passage {
            zim_id: "z".into(),
            path: "P".into(),
            title: t.into(),
            text: text.into(),
            chunk: 0,
            score: 1.0,
            book: "Book".into(),
        }
    }

    #[test]
    fn prompt_numbers_sources_in_order() {
        let msgs = build_messages(&[], "how do bees fly?", &[
            passage("Bee flight", "Bees beat their wings 230 times per second."),
            passage("Wings", "Wing stroke amplitude is small."),
        ]);
        assert_eq!(msgs.len(), 2);
        let sys = &msgs[0].content;
        assert!(sys.contains("[1] \"Bee flight\""));
        assert!(sys.contains("[2] \"Wings\""));
        assert_eq!(msgs[1].role, "user");
    }

    #[test]
    fn enforce_citations_aligns_uncited_claims() {
        let passages = vec![
            passage("Bee flight", "Bees beat their wings roughly 230 times every second during flight."),
            passage("Honey", "Honey is produced from flower nectar collected by worker bees."),
        ];
        let answer = "Bees beat their wings about 230 times per second during flight.\n\
                      Honey comes from nectar collected by worker bees.\n\
                      The moon is made of green cheese.\n";
        let cited = enforce_citations(answer, &passages);
        let lines: Vec<&str> = cited.lines().collect();
        assert!(lines[0].contains("[1]"), "line 0 uncited: {}", lines[0]);
        assert!(lines[1].contains("[2]"), "line 1 uncited: {}", lines[1]);
        // Unsupported claim must NOT get a citation stamped on it.
        assert!(!lines[2].contains('['), "unsupported line was cited: {}", lines[2]);
    }

    #[test]
    fn followup_questions_inherit_topic_context() {
        let history = vec![
            ChatMessage { role: "user".into(), content: "wireless networking on Alpine Linux".into() },
            ChatMessage { role: "assistant".into(), content: "Use wpa_supplicant [1].".into() },
        ];
        // Anaphoric follow-up gets augmented with the prior user turn…
        let q = contextual_question(&history, "how do I configure it at boot?");
        assert!(q.contains("Alpine Linux"), "{q}");
        assert!(q.contains("configure it at boot"));
        // …but a self-contained new question is left untouched.
        let q2 = contextual_question(&history, "explain the apk package manager format");
        assert_eq!(q2, "explain the apk package manager format");
        // No history → unchanged even when anaphoric.
        assert_eq!(contextual_question(&[], "what about it?"), "what about it?");
    }

    #[test]
    fn enforce_citations_leaves_unsupported_text_uncited() {
        // An answer the sources don't support (e.g. an honest refusal) must
        // NOT get citations stamped on it.
        let passages = vec![passage("T", "totally unrelated source text here")];
        let cited = enforce_citations(
            "Your library does not seem to cover skin conditions or rashes.",
            &passages,
        );
        assert!(!cited.contains('['), "{cited}");
    }

    #[test]
    fn enforce_citations_strips_trailing_citation_spam() {
        let passages = vec![passage("T", "some text")];
        let cited = enforce_citations("A real claim [1].\n\n[1] [1] [1]\n", &passages);
        assert_eq!(cited.trim_end(), "A real claim [1].");
    }

    struct PlanEngine(&'static str);
    impl Engine for PlanEngine {
        fn name(&self) -> String {
            "plan-test".into()
        }
        fn can_plan(&self) -> bool {
            true
        }
        fn generate(&self, _m: &[ChatMessage], _s: TokenSink, _max: usize) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    #[test]
    fn triage_parses_numbers_none_and_garbage() {
        let cands: Vec<Passage> = (0..6).map(|i| passage(&format!("T{i}"), "text")).collect();
        assert_eq!(
            triage_sources(&PlanEngine("2, 5"), &[], "q", &cands),
            Some(vec![1, 4])
        );
        assert_eq!(triage_sources(&PlanEngine("NONE"), &[], "q", &cands), Some(vec![]));
        // Out-of-range numbers are dropped; duplicates deduped.
        assert_eq!(
            triage_sources(&PlanEngine("1, 9, 1, 3"), &[], "q", &cands),
            Some(vec![0, 2])
        );
        // Unparseable output → None → caller falls back to all candidates.
        assert_eq!(triage_sources(&PlanEngine("hard to say!"), &[], "q", &cands), None);
        // Engines that can't plan don't triage.
        assert_eq!(triage_sources(&StubEngine, &[], "q", &cands), None);
    }

    #[test]
    fn plan_retrieval_uses_model_query_or_reuses() {
        let history = vec![ChatMessage { role: "user".into(), content: "alpine wifi".into() }];
        // Model rewrites the query…
        let p = plan_retrieval(&PlanEngine("alpine linux wireless boot"), &history, "at boot?");
        match p {
            RetrievalPlan::Search(qs) => {
                assert_eq!(qs[0], "alpine linux wireless boot");
                // The original question (context-expanded) rides along.
                assert!(qs.last().unwrap().contains("at boot"));
            }
            _ => panic!("expected search"),
        }
        // …or decides no new retrieval is needed.
        let p = plan_retrieval(&PlanEngine("NONE"), &history, "make it shorter");
        assert_eq!(p, RetrievalPlan::ReusePrevious);
        // First turn consults capable engines too (query rewriting), and
        // NONE with no history degrades to searching the raw question.
        let p = plan_retrieval(&PlanEngine("bee flight wing speed"), &[], "how do bees fly?");
        match p {
            RetrievalPlan::Search(qs) => assert_eq!(qs[0], "bee flight wing speed"),
            _ => panic!("expected search"),
        }
        let p = plan_retrieval(&PlanEngine("NONE"), &[], "how do bees fly?");
        assert_eq!(p, RetrievalPlan::Search(vec!["how do bees fly?".into()]));
        // Engines that can't plan fall back to the keyword heuristic.
        let p = plan_retrieval(&StubEngine, &history, "what about it at boot?");
        match p {
            RetrievalPlan::Search(qs) => assert!(qs[0].contains("alpine wifi"), "{qs:?}"),
            _ => panic!("expected search"),
        }
    }

    #[test]
    fn planner_rejects_prose_and_keeps_queries() {
        // Real junk observed from a 1B model asked for queries.
        assert_eq!(clean_query_line("Setting up a wireless access point on Alpine Linux involves several steps. Here are the key terms:"), None);
        assert_eq!(clean_query_line("3. **WPA2 Encryption**: Ensuring data security."), None);
        assert_eq!(clean_query_line("bash"), None);
        assert_eq!(clean_query_line("### Step 1: Bridge the interfaces"), None);
        assert_eq!(clean_query_line("sudo apt update"), None);
        assert_eq!(clean_query_line("sudo apt install wpa_supplicant-wlan0 -c/<YOUR_WPA_CONFIG_FILE> -i wlan0"), None);
        assert_eq!(clean_query_line("Replace `<WPA_CONFIG_FILE>` with the path to your `wpa_supp"), None);
        // Legitimate queries survive, cleaned.
        assert_eq!(clean_query_line("1. alpine linux hostapd setup"), Some("alpine linux hostapd setup".into()));
        assert_eq!(clean_query_line("\"bridge-utils hostapd\""), Some("bridge-utils hostapd".into()));
        assert_eq!(clean_query_line("- wireless access point configuration"), Some("wireless access point configuration".into()));
    }

    #[test]
    fn history_window_keeps_newest_within_budget() {
        let history: Vec<ChatMessage> = (0..30)
            .map(|i| ChatMessage {
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                content: format!("message number {i} {}", "x".repeat(500)),
            })
            .collect();
        let w = history_window(&history, 2000, 16);
        // Budget holds…
        let total: usize = w.iter().map(|m| m.content.chars().count()).sum();
        assert!(total <= 2000 + 1, "window over budget: {total}");
        // …the newest message is always present, and order is chronological.
        assert!(w.last().unwrap().content.contains("message number 29"));
        assert!(w.len() >= 2);
        for pair in w.windows(2) {
            let a: u32 = pair[0].content.split_whitespace().nth(2).unwrap().parse().unwrap();
            let b: u32 = pair[1].content.split_whitespace().nth(2).unwrap().parse().unwrap();
            assert!(a < b);
        }
        // A single oversized message is clipped, not dropped.
        let big = vec![ChatMessage { role: "user".into(), content: "y".repeat(9000) }];
        let w = history_window(&big, 8000, 16);
        assert_eq!(w.len(), 1);
        assert!(w[0].content.chars().count() <= PER_MESSAGE_CHAR_CAP + 1);
        // Message cap: 16 max even under budget.
        let many: Vec<ChatMessage> = (0..40)
            .map(|i| ChatMessage { role: "user".into(), content: format!("m{i}") })
            .collect();
        assert_eq!(history_window(&many, 100_000, 16).len(), 16);
    }

    #[test]
    fn keep_head_tail_preserves_question_end() {
        let mut v: Vec<u32> = (0..100).collect();
        keep_head_tail(&mut v, 30);
        assert_eq!(v.len(), 30);
        // Head survives (system + sources)…
        assert_eq!(v[0], 0);
        assert_eq!(v[19], 19);
        // …and so does the tail (the question).
        assert_eq!(*v.last().unwrap(), 99);
        assert_eq!(v[20], 90);
        // Under budget → untouched.
        let mut small: Vec<u32> = (0..10).collect();
        keep_head_tail(&mut small, 30);
        assert_eq!(small.len(), 10);
    }

    struct RecordingEngine(std::sync::Mutex<Vec<ChatMessage>>);
    impl Engine for RecordingEngine {
        fn name(&self) -> String {
            "recording".into()
        }
        fn can_plan(&self) -> bool {
            true
        }
        fn generate(&self, m: &[ChatMessage], _s: TokenSink, _max: usize) -> Result<String> {
            *self.0.lock().unwrap() = m.to_vec();
            Ok("1".into())
        }
    }

    #[test]
    fn triage_sees_conversation_context_for_followups() {
        let history = vec![
            ChatMessage { role: "user".into(), content: "treating burns with home remedies".into() },
            ChatMessage { role: "assistant".into(), content: "Cool water first [1].".into() },
        ];
        let cands = vec![passage("Burn care", "cool running water for ten minutes")];
        let eng = RecordingEngine(std::sync::Mutex::new(vec![]));
        triage_sources(&eng, &history, "what about for children?", &cands).unwrap();
        let seen = eng.0.lock().unwrap();
        let user = &seen.iter().find(|m| m.role == "user").unwrap().content;
        assert!(user.contains("treating burns"), "triage prompt lost the topic: {user}");
    }

    #[test]
    fn enforce_citations_strips_invalid_and_keeps_valid() {
        let passages = vec![passage("T", "some text")];
        let cited = enforce_citations("A valid claim [1]. A bogus one [9].", &passages);
        assert!(cited.contains("[1]"));
        assert!(!cited.contains("[9]"));
    }

    #[test]
    fn enforce_citations_leaves_code_blocks_alone() {
        let passages = vec![passage(
            "Networking",
            "install the hostapd access point software with apk add hostapd command line",
        )];
        let answer = "Install the hostapd access point software using the apk command:\n```\napk add hostapd access point software command line install\n```\n";
        let cited = enforce_citations(answer, &passages);
        assert!(cited.lines().next().unwrap().contains("[1]"));
        assert!(!cited.contains("install [1]"), "code line was modified: {cited}");
    }

    #[test]
    fn stub_engine_streams_and_cites() {
        let msgs = build_messages(&[], "q", &[passage("T", &"x".repeat(200))]);
        let mut streamed = String::new();
        let mut sink = |s: &str| {
            streamed.push_str(s);
            true
        };
        let full = StubEngine.generate(&msgs, &mut sink, 1024).unwrap();
        assert_eq!(full, streamed);
        assert!(full.contains("[1]"));
    }
}
