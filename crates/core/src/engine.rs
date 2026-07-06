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
    /// Generate a reply to the chat, streaming pieces into `sink`.
    fn generate(&self, messages: &[ChatMessage], sink: TokenSink) -> Result<String>;
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
        "You are a librarian assistant. Answer the user's question using ONLY the numbered \
         sources below. After every claim, cite the source(s) it came from using bracketed \
         numbers like [1] or [2][3]. Example: \"Alpine Linux uses apk to manage \
         packages [1]. It supports both wired and wireless networking [2][3].\" \
         Never cite a number that is not in the source list. \
         If the sources do not contain the answer, say so plainly and do not guess. \
         Keep answers concise and factual.\n\nSOURCES:\n{sources}"
    );
    let mut msgs = vec![ChatMessage { role: "system".into(), content: system }];
    // Keep a short window of prior turns for conversational context.
    let tail = history.len().saturating_sub(6);
    msgs.extend(history[tail..].iter().cloned());
    msgs.push(ChatMessage { role: "user".into(), content: question.to_string() });
    msgs
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

/// Guarantee inline citations even when the model ignores the citation
/// instruction (small models often do): strip citation numbers that don't
/// exist, and align each substantive uncited sentence to the source passage
/// with the highest content-word overlap, appending its [n].
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
    // Hard guarantee: an answer never ships without clickable citations.
    // If alignment found nothing to attach inline (heavily paraphrased
    // output), list the consulted sources explicitly at the end.
    if n_sources > 0 && !has_citation(&out) {
        let list: String = (1..=n_sources.min(6)).map(|n| format!("[{n}]")).collect();
        out = format!("{}\n\nSources consulted: {list}", out.trim_end());
    }
    out
}

fn has_citation(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while let Some(open) = s[i..].find('[') {
        let p = i + open + 1;
        let mut j = p;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > p && j < b.len() && b[j] == b']' {
            return true;
        }
        i = p;
    }
    false
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
    if best.0 >= 3 && best.0 * 5 >= words.len() * 2 {
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

    fn generate(&self, messages: &[ChatMessage], sink: TokenSink) -> Result<String> {
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
    fn enforce_citations_never_ships_citation_free_answers() {
        let passages = vec![passage("T", "totally unrelated source text here")];
        let cited = enforce_citations(
            "A heavily paraphrased answer sharing no vocabulary with sources at all.",
            &passages,
        );
        assert!(cited.contains("Sources consulted: [1]"), "{cited}");
        // But an answer that already cites gets no appendix.
        let cited2 = enforce_citations("A claim [1].", &passages);
        assert!(!cited2.contains("Sources consulted"));
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
        let full = StubEngine.generate(&msgs, &mut sink).unwrap();
        assert_eq!(full, streamed);
        assert!(full.contains("[1]"));
    }
}
