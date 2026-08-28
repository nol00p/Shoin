//! Markdown to plain text.
//!
//! Not a renderer — a *stripper*. The job is to leave a file someone can read
//! in `less` or paste into an email, so every marker that exists to be parsed
//! rather than read comes out, and nothing that carries meaning does.
//!
//! Where a marker carries meaning, it is replaced rather than removed: a list
//! stays a list, a quote stays marked as quoted, and a heading keeps its rank
//! by being underlined the way plain-text documents have always done it.

use crate::config::schema::MarkdownConfig;
use crate::render::markdown::block::{classify, BlockKind, Carry, Marker};
use crate::render::markdown::inline;

/// Convert a Markdown document to plain text.
pub fn render(text: &str) -> String {
    let md = MarkdownConfig::default();
    let mut out: Vec<String> = Vec::new();
    let mut carry = Carry::None;
    let mut in_fence = false;

    for (i, line) in text.lines().enumerate() {
        let (kind, next) = classify(line, &carry, i == 0);
        carry = next;

        match &kind {
            // The fence delimiters go; the code between them is indented, which
            // is how plain text has always marked a block as verbatim.
            BlockKind::FenceOpen(_) => {
                in_fence = true;
                continue;
            }
            BlockKind::FenceClose => {
                in_fence = false;
                continue;
            }
            BlockKind::FenceBody { .. } => {
                out.push(format!("    {line}"));
                continue;
            }
            // Front matter is metadata for tools, not text for readers.
            BlockKind::FrontMatter => continue,
            _ => {}
        }
        if in_fence {
            out.push(format!("    {line}"));
            continue;
        }

        match &kind {
            BlockKind::Heading(level) => {
                let title = strip_inline(line.trim_start().trim_start_matches('#').trim(), &md);
                out.push(title.clone());
                // Underline the top two ranks, the convention `setext` headings
                // and every README have used forever. Deeper ranks would need
                // a third rule nobody recognises, so they stand on their own.
                let rule = match level {
                    1 => Some('='),
                    2 => Some('-'),
                    _ => None,
                };
                if let Some(c) = rule {
                    out.push(c.to_string().repeat(title.chars().count().max(1)));
                }
            }
            BlockKind::Rule => out.push("-".repeat(40)),
            BlockKind::Quote(depth) => {
                let body = line.trim_start().trim_start_matches(['>', ' ']);
                let prefix = "> ".repeat((*depth as usize).max(1));
                out.push(format!("{prefix}{}", strip_inline(body, &md)));
            }
            BlockKind::ListItem { marker, checked, depth } => {
                let indent = " ".repeat(*depth as usize);
                let body = list_body(line);
                let bullet = match (marker, checked) {
                    // A checkbox has to survive: unticked and ticked are the
                    // whole point of the line.
                    (_, Some(true)) => "[x] ".to_string(),
                    (_, Some(false)) => "[ ] ".to_string(),
                    (Marker::Ordered, None) => format!("{} ", ordinal(line)),
                    (_, None) => "- ".to_string(),
                };
                out.push(format!("{indent}{bullet}{}", strip_inline(body, &md)));
            }
            _ => out.push(strip_inline(line, &md)),
        }
    }

    let mut joined = out.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// The text of a list item, past its marker and any checkbox.
fn list_body(line: &str) -> &str {
    let t = line.trim_start();
    let past_marker = match t.find(' ') {
        Some(i) => &t[i + 1..],
        None => "",
    };
    let t = past_marker.trim_start();
    if (t.starts_with("[ ]") || t.starts_with("[x]") || t.starts_with("[X]")) && t.len() >= 3 {
        t[3..].trim_start()
    } else {
        t
    }
}

/// `12.` from `  12. text`.
fn ordinal(line: &str) -> String {
    let t = line.trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    let delim = t.chars().nth(digits.len()).unwrap_or('.');
    format!("{digits}{delim}")
}

/// Drop inline markers, keeping what they wrapped.
///
/// A LINK keeps its target in parentheses: the whole reason to write a link is
/// the address, and a plain-text reader has no way to follow one that has been
/// thrown away.
fn strip_inline(line: &str, md: &MarkdownConfig) -> String {
    let chars: Vec<char> = line.chars().collect();
    let spans = inline::scan(line, md);
    if spans.is_empty() {
        return line.to_string();
    }
    let mut out = String::new();
    let mut i = 0usize;
    for sp in &spans {
        if sp.outer.start < i {
            continue;
        }
        out.extend(&chars[i..sp.outer.start]);
        let inner: String = chars[sp.inner.clone()].iter().collect();
        match sp.kind {
            inline::Inline::Link => {
                let whole: String = chars[sp.outer.clone()].iter().collect();
                match url_of(&whole) {
                    Some(url) if url != inner => out.push_str(&format!("{inner} ({url})")),
                    _ => out.push_str(&inner),
                }
            }
            inline::Inline::Image => out.push_str(&format!("[image: {inner}]")),
            _ => out.push_str(&inner),
        }
        i = sp.outer.end;
    }
    out.extend(&chars[i.min(chars.len())..]);
    out
}

/// The `url` from `[text](url)`.
fn url_of(whole: &str) -> Option<&str> {
    let open = whole.rfind("](")?;
    let close = whole.rfind(')')?;
    (close > open + 2).then(|| whole[open + 2..close].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_keep_their_rank_by_being_underlined() {
        let got = render("# Title\n\n## Section\n\n### Deeper\n");
        assert!(got.contains("Title\n====="), "an H1 is ruled with =");
        assert!(got.contains("Section\n-------"), "an H2 with -");
        assert!(got.contains("Deeper"), "deeper ranks stand alone");
        assert!(!got.contains("###"));
    }

    #[test]
    fn inline_markers_go_and_their_text_stays() {
        let got = render("a **bold** and *italic* and `code` and ==hi== here\n");
        assert_eq!(got.trim(), "a bold and italic and code and hi here");
    }

    /// A link's address is the reason it was written; plain text has no other
    /// way to carry it.
    #[test]
    fn a_link_keeps_its_address() {
        let got = render("see [the docs](https://example.com/x) now\n");
        assert_eq!(got.trim(), "see the docs (https://example.com/x) now");
        // …unless the text already is the address.
        let got = render("[https://example.com](https://example.com)\n");
        assert_eq!(got.trim(), "https://example.com");
    }

    #[test]
    fn lists_stay_lists_and_checkboxes_survive() {
        let got = render("- one\n- two\n\n1. first\n2. second\n\n- [ ] todo\n- [x] done\n");
        assert!(got.contains("- one"));
        assert!(got.contains("1. first"));
        assert!(got.contains("2. second"));
        assert!(got.contains("[ ] todo"), "an unticked box is the point of the line");
        assert!(got.contains("[x] done"));
    }

    #[test]
    fn quotes_keep_their_mark_and_code_is_indented() {
        let got = render("> quoted words\n\n```rust\nlet x = 1;\n```\n");
        assert!(got.contains("> quoted words"));
        assert!(got.contains("    let x = 1;"), "verbatim text is indented");
        assert!(!got.contains("```"), "the fence itself goes");
        assert!(!got.contains("rust"), "…and so does its info string");
    }

    /// A `#` inside a fence is code, not a heading, and must not be underlined.
    #[test]
    fn a_hash_inside_a_fence_is_left_alone() {
        let got = render("```sh\n# a comment\n```\n");
        assert!(got.contains("    # a comment"));
        assert!(!got.contains("====="));
    }

    #[test]
    fn front_matter_is_metadata_and_goes() {
        let got = render("---\ntitle: x\n---\nbody\n");
        assert!(got.contains("body"));
        assert!(!got.contains("title: x"));
    }
}
