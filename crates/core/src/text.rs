//! Small, dependency-free HTML → plain text extraction, tuned for indexing
//! ZIM article content. Not a general-purpose HTML parser: it strips tags,
//! drops script/style/head content, decodes common entities and inserts
//! paragraph breaks at block-level boundaries.

/// Tags whose entire content is dropped.
const DROP: &[&str] = &["script", "style", "head", "noscript", "svg", "math"];
/// Tags that imply a paragraph/line break in the extracted text.
const BLOCK: &[&str] = &[
    "p", "div", "br", "li", "ul", "ol", "table", "tr", "td", "th", "h1", "h2", "h3", "h4", "h5",
    "h6", "section", "article", "blockquote", "pre", "dd", "dt", "figcaption", "caption",
];

pub struct Extracted {
    pub title: Option<String>,
    pub text: String,
}

pub fn html_to_text(html: &str) -> Extracted {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len() / 4);
    let mut title: Option<String> = None;
    let mut i = 0usize;
    let mut drop_until: Option<String> = None;
    let mut in_title = false;
    let mut title_buf = String::new();

    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Find the end of the tag.
            let Some(rel_end) = html[i..].find('>') else { break };
            let tag_body = &html[i + 1..i + rel_end];
            let closing = tag_body.starts_with('/');
            let name: String = tag_body
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();

            if let Some(ref d) = drop_until {
                if closing && name == *d {
                    drop_until = None;
                }
            } else if !closing && DROP.contains(&name.as_str()) && !tag_body.ends_with('/') {
                drop_until = Some(name.clone());
            }

            if name == "title" {
                in_title = !closing;
                if closing && title.is_none() {
                    let t = collapse_ws(&title_buf);
                    if !t.is_empty() {
                        title = Some(t);
                    }
                }
            }
            if BLOCK.contains(&name.as_str()) && !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            i += rel_end + 1;
            continue;
        }
        // Text content.
        let next_tag = html[i..].find('<').map(|p| i + p).unwrap_or(bytes.len());
        let chunk = &html[i..next_tag];
        if in_title {
            // <title> lives inside <head> (a dropped tag): capture it anyway.
            title_buf.push_str(&decode_entities(chunk));
        } else if drop_until.is_none() {
            push_collapsed(&mut out, &decode_entities(chunk));
        }
        i = next_tag;
    }

    // Normalize: collapse blank-line runs, trim lines.
    let mut text = String::with_capacity(out.len());
    for line in out.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(l);
    }
    Extracted { title, text }
}

fn push_collapsed(out: &mut String, s: &str) {
    let mut last_ws = out.ends_with(' ') || out.ends_with('\n') || out.is_empty();
    for c in s.chars() {
        if c == '\n' || c == '\r' || c == '\t' || c == ' ' {
            if !last_ws {
                out.push(' ');
                last_ws = true;
            }
        } else {
            out.push(c);
            last_ws = false;
        }
    }
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        // Byte-based scan: entity names are ASCII, but arbitrary multibyte
        // text may follow the '&', so no string slicing before the ';'.
        let semi = rest.as_bytes().iter().take(12).position(|&b| b == b';');
        match semi {
            Some(sp) => {
                let ent = &rest[1..sp];
                let decoded: Option<char> = match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    "ndash" => Some('–'),
                    "mdash" => Some('—'),
                    "hellip" => Some('…'),
                    _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                        u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
                    }
                    _ if ent.starts_with('#') => {
                        ent[1..].parse::<u32>().ok().and_then(char::from_u32)
                    }
                    _ => None,
                };
                match decoded {
                    Some(c) => {
                        out.push(c);
                        rest = &rest[sp + 1..];
                    }
                    None => {
                        out.push('&');
                        rest = &rest[1..];
                    }
                }
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Split extracted text into retrieval chunks of roughly `target` characters,
/// breaking only at paragraph (line) boundaries. Oversized single paragraphs
/// are split at sentence-ish boundaries.
pub fn chunk_text(text: &str, target: usize) -> Vec<String> {
    let max = target * 2;
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let push_cur = |cur: &mut String, chunks: &mut Vec<String>| {
        let t = cur.trim();
        // Skip degenerate fragments (nav crumbs, single links...).
        if t.chars().count() >= 80 {
            chunks.push(t.to_string());
        }
        cur.clear();
    };
    for para in text.lines() {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if cur.len() + para.len() + 1 > max && !cur.is_empty() {
            push_cur(&mut cur, &mut chunks);
        }
        if para.len() > max {
            // Split long paragraph on sentence boundaries.
            let mut piece = String::new();
            for sent in split_sentences(para) {
                if piece.len() + sent.len() > max && !piece.is_empty() {
                    cur.push_str(&piece);
                    push_cur(&mut cur, &mut chunks);
                    piece.clear();
                }
                piece.push_str(sent);
            }
            cur.push_str(&piece);
        } else {
            if !cur.is_empty() {
                cur.push('\n');
            }
            cur.push_str(para);
        }
        if cur.len() >= target {
            push_cur(&mut cur, &mut chunks);
        }
    }
    push_cur(&mut cur, &mut chunks);
    chunks
}

fn split_sentences(s: &str) -> impl Iterator<Item = &str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if (c == b'.' || c == b'!' || c == b'?') && i + 1 < b.len() && b[i + 1] == b' ' {
            parts.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        parts.push(&s[start..]);
    }
    parts.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_and_title() {
        let html = r#"<html><head><title>My &amp; Page</title><style>p{color:red}</style></head>
            <body><h1>Heading</h1><p>First para with <b>bold</b> text.</p>
            <script>var x = "<p>ignored</p>";</script>
            <p>Second&nbsp;para &#8212; done.</p></body></html>"#;
        let e = html_to_text(html);
        assert_eq!(e.title.as_deref(), Some("My & Page"));
        assert!(e.text.contains("First para with bold text."));
        assert!(e.text.contains("Second para — done."));
        assert!(!e.text.contains("ignored"));
        assert!(!e.text.contains("color:red"));
    }

    #[test]
    fn chunks_respect_target() {
        let para = "This is a sentence that repeats itself for testing purposes. ".repeat(60);
        let chunks = chunk_text(&para, 1000);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().all(|c| c.len() <= 2100));
        assert!(chunks.iter().all(|c| c.chars().count() >= 80));
    }
}
