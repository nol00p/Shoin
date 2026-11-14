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
///
/// `justify` stretches every wrapped row of a paragraph but its last to fill
/// `measure` exactly — the same math (`render::align::justify_gaps`) and the
/// same word-wrap (`render::layout::wrap_line`) the live editor's own
/// `:align justified` draws with, reused here because exported text has no
/// terminal to render into. Left `false` (the default), a paragraph is
/// written exactly as it always was: one output line per source line,
/// unwrapped — Shoin stores a paragraph as one logical line and only wraps it
/// for display, so that is already the full text. A heading, quote, list item,
/// or table row is never justified; stretching a quote's `> ` prefix or a
/// table's `|` columns would break what they are.
pub fn render(text: &str, measure: u16, justify: bool, md: &MarkdownConfig) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut carry = Carry::default();
    let mut in_fence = false;

    // Classified up front: whether a line ends in a hard break depends on
    // whether the next line carries its paragraph on, and a table's header
    // row is only known by the delimiter row after it — two lines ahead.
    let lines: Vec<(&str, BlockKind)> = text
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let (kind, next) = classify(line, &carry, i == 0, md.math);
            carry = next;
            (line, kind)
        })
        .collect();
    // With `tables = false` a table is prose to the paragraph rule, as it is
    // to HTML; it is still written as the rows it is.
    let as_prose = |k: &BlockKind| match k {
        BlockKind::Table if !md.tables => BlockKind::Paragraph,
        other => other.clone(),
    };
    let mut held = super::Held::default();

    for (i, (line, kind)) in lines.iter().enumerate() {
        let before = std::mem::take(&mut held);
        held = before.next(line, &as_prose(kind), md.math);

        match kind {
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
            // LaTeX is written out as it is: its `*`, `_` and `\\` are math,
            // not Markdown, so nothing in it may be stripped.
            BlockKind::MathBlock => {
                out.push(line.to_string());
                continue;
            }
            _ => {}
        }
        if in_fence {
            out.push(format!("    {line}"));
            continue;
        }
        // Code is never touched, so this comes after the fence check.
        let line = match as_prose(kind) {
            BlockKind::Paragraph | BlockKind::Quote(_) | BlockKind::ListItem { .. } => {
                let joins = lines.get(i + 1).is_some_and(|(next, k)| {
                    held.joins(next, &as_prose(k), lines.get(i + 2).map(|(_, k)| k), md)
                });
                without_break(line, joins)
            }
            _ => line,
        };

        match kind {
            BlockKind::Heading(level) => {
                let title = strip_inline(line.trim_start().trim_start_matches('#').trim(), md);
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
                out.push(format!("{prefix}{}", strip_inline(body, md)));
            }
            BlockKind::ListItem { marker, checked, depth } => {
                let indent = " ".repeat(*depth as usize);
                let body = super::item_body(line);
                let bullet = match (marker, checked) {
                    // A checkbox has to survive: unticked and ticked are the
                    // whole point of the line.
                    (_, Some(true)) => "[x] ".to_string(),
                    (_, Some(false)) => "[ ] ".to_string(),
                    (Marker::Ordered, None) => format!("{} ", ordinal(line)),
                    (_, None) => "- ".to_string(),
                };
                out.push(format!("{indent}{bullet}{}", strip_inline(body, md)));
            }
            BlockKind::Paragraph if justify => {
                push_justified(&mut out, &strip_inline(line, md), measure)
            }
            _ => out.push(strip_inline(line, md)),
        }
    }

    let mut joined = out.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// `line` without its hard-break marker. Plain text breaks a line by breaking
/// it, so the two trailing spaces or the backslash that say so in Markdown
/// are noise here. Trailing whitespace goes always; a backslash only when
/// the next line carries the paragraph on (`Held::joins`), since one that ends a paragraph
/// breaks nothing and is the author's text. An even run of backslashes is
/// escaped backslashes, not a break — as `html::ParaLine` reads it.
fn without_break(line: &str, continues: bool) -> &str {
    let text = line.trim_end();
    if text.len() < line.len() || !continues {
        return text;
    }
    let slashes = text.len() - text.trim_end_matches('\\').len();
    match slashes % 2 {
        1 => &text[..text.len() - 1],
        _ => text,
    }
}

/// `12.` from `  12. text`.
fn ordinal(line: &str) -> String {
    let t = line.trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    let delim = t.chars().nth(digits.len()).unwrap_or('.');
    format!("{digits}{delim}")
}

/// Word-wrap `body` to `measure` and stretch every row but the last to fill it
/// exactly, pushing each as its own output line.
fn push_justified(out: &mut Vec<String>, body: &str, measure: u16) {
    let chars: Vec<char> = body.chars().collect();
    let ranges = crate::render::layout::wrap_line(body, measure);
    let last = ranges.len() - 1;
    for (i, (start, end)) in ranges.into_iter().enumerate() {
        let row: String = chars[start..end].iter().collect();
        if i == last {
            out.push(row.trim_end().to_string());
        } else {
            let gaps = crate::render::align::justify_gaps(&row, measure);
            out.push(stretch(&row, &gaps));
        }
    }
}

/// Insert the extra spaces `align::justify_gaps` computed at each gap.
fn stretch(row: &str, gaps: &[(usize, u16)]) -> String {
    let trimmed = row.trim_end();
    if gaps.is_empty() {
        return trimmed.to_string();
    }
    let mut out = String::new();
    for (i, ch) in trimmed.chars().enumerate() {
        out.push(ch);
        if let Some(&(_, extra)) = gaps.iter().find(|g| g.0 == i) {
            out.push_str(&" ".repeat(extra as usize));
        }
    }
    out
}

/// Drop inline markers, keeping what they wrapped.
///
/// A LINK keeps its target in parentheses: the whole reason to write a link is
/// the address, and a plain-text reader has no way to follow one that has been
/// thrown away.
fn strip_inline(line: &str, md: &MarkdownConfig) -> String {
    strip_in(line, md, false)
}

/// `strip_inline`, with `in_link` set inside a link's text, where a nested
/// link is only its text. Prose bodies (emphasis, a link's text) are
/// stripped again, because `inline::scan` returns flat spans.
fn strip_in(line: &str, md: &MarkdownConfig, in_link: bool) -> String {
    use inline::Inline;
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
            Inline::Bold | Inline::Italic | Inline::BoldItalic | Inline::Strikethrough | Inline::Highlight => {
                out.push_str(&strip_in(&inner, md, in_link))
            }
            Inline::Link => {
                let text = strip_in(&inner, md, true);
                match inline::destination(sp, line) {
                    Some((url, _)) if !in_link && !url.is_empty() && url != text => {
                        out.push_str(&format!("{text} ({url})"))
                    }
                    _ => out.push_str(&text),
                }
            }
            // Alt text is plain: `![a [b](c)](d)` reads "a b". Flattened as
            // HTML export's alt is, which does not recurse into a picture in
            // it: pictures nest, and `![![![…` must not run the stack out.
            Inline::Image => out.push_str(&format!("[image: {}]", super::html::plain_text(&inner, md))),
            // What the reader sees in the editor: the label after `|`, or
            // the target when there is none.
            Inline::WikiLink => match inner.split_once('|') {
                Some((_, label)) => out.push_str(label.trim()),
                None => out.push_str(inner.trim()),
            },
            _ => out.push_str(&inner),
        }
        i = sp.outer.end;
    }
    out.extend(&chars[i.min(chars.len())..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture's alt text is plain, and a badge in a link names its picture.
    #[test]
    fn pictures_name_their_plain_alt_text() {
        let md = MarkdownConfig::default();
        assert_eq!(strip_inline("![a [b](c) *d*](p.png)", &md), "[image: a b d]");
        assert_eq!(strip_inline("[![ci](b.svg)](https://x.y)", &md), "[image: ci] (https://x.y)");
        assert_eq!(strip_inline("![see #todo](p.png)", &md), "[image: see #todo]");
        // Pictures nested deep: no recursion per level.
        let n = 50_000;
        let deep = format!("{}a{}", "![".repeat(n), "](u)".repeat(n));
        let t = std::time::Instant::now();
        assert!(strip_inline(&deep, &md).starts_with("[image: "));
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
    }

    /// LaTeX is not Markdown: nothing in a display block is stripped.
    #[test]
    fn math_blocks_are_written_as_they_are() {
        let src = "$$\na*b*c \\\\ d\n\\{x\\}\n$$\n";
        assert_eq!(render(src, 72, false, &MarkdownConfig::default()), src);
    }

    #[test]
    fn headings_keep_their_rank_by_being_underlined() {
        let got = render("# Title\n\n## Section\n\n### Deeper\n", 72, false, &MarkdownConfig::default());
        assert!(got.contains("Title\n====="), "an H1 is ruled with =");
        assert!(got.contains("Section\n-------"), "an H2 with -");
        assert!(got.contains("Deeper"), "deeper ranks stand alone");
        assert!(!got.contains("###"));
    }

    #[test]
    fn inline_markers_go_and_their_text_stays() {
        let got = render("a **bold** and *italic* and `code` and ==hi== here\n", 72, false, &MarkdownConfig::default());
        assert_eq!(got.trim(), "a bold and italic and code and hi here");
    }

    /// A link's address is the reason it was written; plain text has no other
    /// way to carry it.
    #[test]
    fn a_link_keeps_its_address() {
        let got = render("see [the docs](https://example.com/x) now\n", 72, false, &MarkdownConfig::default());
        assert_eq!(got.trim(), "see the docs (https://example.com/x) now");
        // …unless the text already is the address.
        let got = render("[https://example.com](https://example.com)\n", 72, false, &MarkdownConfig::default());
        assert_eq!(got.trim(), "https://example.com");
    }

    /// Markup inside bold or a link's text goes too.
    #[test]
    fn nested_markers_go_as_well() {
        let md = MarkdownConfig::default();
        assert_eq!(render("**a [b](u)**\n", 72, false, &md), "a b (u)\n");
        assert_eq!(render("[**bold** `c`](https://x.y)\n", 72, false, &md), "bold c (https://x.y)\n");
        assert_eq!(render("*a ~~b~~ ==c==*\n", 72, false, &md), "a b c\n");
        assert_eq!(render("`**kept**`\n", 72, false, &md), "**kept**\n");
    }

    /// A wiki link reads as its label; a title is not part of an address.
    #[test]
    fn wiki_links_read_as_their_labels_and_titles_go() {
        let md = MarkdownConfig::default();
        assert_eq!(render("see [[note|the note]] and [[other]]\n", 72, false, &md), "see the note and other\n");
        assert_eq!(render("[docs](<a b.md> \"T\")\n", 72, false, &md), "docs (a b.md)\n");
        assert_eq!(render("[w](https://x.y/a_(b))\n", 72, false, &md), "w (https://x.y/a_(b))\n");
    }

    #[test]
    fn lists_stay_lists_and_checkboxes_survive() {
        let got = render("- one\n- two\n\n1. first\n2. second\n\n- [ ] todo\n- [x] done\n", 72, false, &MarkdownConfig::default());
        assert!(got.contains("- one"));
        assert!(got.contains("1. first"));
        assert!(got.contains("2. second"));
        assert!(got.contains("[ ] todo"), "an unticked box is the point of the line");
        assert!(got.contains("[x] done"));
    }

    /// A tab after the marker is whitespace like a space; the first word
    /// after it is text, not part of the marker.
    #[test]
    fn a_tab_after_the_marker_keeps_the_first_word() {
        let md = MarkdownConfig::default();
        assert_eq!(render("-\tfoo bar\n", 72, false, &md), "- foo bar\n");
        assert_eq!(render("1.\tone two\n", 72, false, &md), "1. one two\n");
        assert_eq!(render("- [x]\tdone\n", 72, false, &md), "[x] done\n");
        assert_eq!(render("*\t[ ]\tto do\n", 72, false, &md), "[ ] to do\n");
    }

    /// Plain text has line breaks of its own: a hard break's two spaces or
    /// backslash are Markdown for one, and go. A backslash that ends the
    /// paragraph breaks nothing and is text.
    #[test]
    fn hard_break_markers_go() {
        let md = MarkdownConfig::default();
        assert_eq!(render("one  \ntwo\n", 72, false, &md), "one\ntwo\n");
        assert_eq!(render("one\\\ntwo\n", 72, false, &md), "one\ntwo\n");
        assert_eq!(render("> one\\\n> two\n", 72, false, &md), "> one\n> two\n");
        assert_eq!(render("> one  \n> two\n", 72, false, &md), "> one\n> two\n");
        assert_eq!(render("- one\\\n  two\n", 72, false, &md), "- one\n  two\n");
        assert_eq!(render("one\\\n", 72, false, &md), "one\\\n");
        assert_eq!(render("one\\\n\ntwo\n", 72, false, &md), "one\\\n\ntwo\n");
        assert_eq!(render("> one\\\n>\n> two\n", 72, false, &md), "> one\\\n> \n> two\n");
    }

    /// Code keeps its trailing spaces and backslashes: they are not breaks.
    #[test]
    fn code_keeps_its_line_ends() {
        let md = MarkdownConfig::default();
        assert_eq!(render("```\na  \nb\\\nc\n```\n", 72, false, &md), "    a  \n    b\\\n    c\n");
        assert_eq!(render("- item\n  ```\n  a\\\n  b\n  ```\n", 72, false, &md), "- item\n      a\\\n      b\n");
    }

    #[test]
    fn quotes_keep_their_mark_and_code_is_indented() {
        let got = render("> quoted words\n\n```rust\nlet x = 1;\n```\n", 72, false, &MarkdownConfig::default());
        assert!(got.contains("> quoted words"));
        assert!(got.contains("    let x = 1;"), "verbatim text is indented");
        assert!(!got.contains("```"), "the fence itself goes");
        assert!(!got.contains("rust"), "…and so does its info string");
    }

    /// A `#` inside a fence is code, not a heading, and must not be underlined.
    #[test]
    fn a_hash_inside_a_fence_is_left_alone() {
        let got = render("```sh\n# a comment\n```\n", 72, false, &MarkdownConfig::default());
        assert!(got.contains("    # a comment"));
        assert!(!got.contains("====="));
    }

    #[test]
    fn front_matter_is_metadata_and_goes() {
        let got = render("---\ntitle: x\n---\nbody\n", 72, false, &MarkdownConfig::default());
        assert!(got.contains("body"));
        assert!(!got.contains("title: x"));
    }

    /// `justify: true` wraps a paragraph to `measure` and stretches every row
    /// but its last to fill it exactly — the same values `render::frame`'s
    /// own `justified_stretches_every_row_but_the_last` test draws on screen,
    /// since this reuses the very same `wrap_line`/`justify_gaps` math.
    #[test]
    fn justify_wraps_and_stretches_a_paragraph_but_its_last_row() {
        let got = render("aaa bbb ccc ddd\n", 10, true, &MarkdownConfig::default());
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(lines, vec!["aaa    bbb", "ccc ddd"], "{got:?}");
    }

    /// A paragraph that never wraps has no "last row" distinct from its only
    /// row, so it is left exactly as `justify: false` would write it.
    #[test]
    fn justify_leaves_an_unwrapped_paragraph_ragged() {
        let got = render("aaa bbb\n", 20, true, &MarkdownConfig::default());
        assert_eq!(got.trim_end(), "aaa bbb");
    }

    /// `justify` never touches a heading, quote, list item or table row: a
    /// quote's `> ` prefix and a table's `|` columns are the author's own
    /// layout, and wrapping them the way a paragraph wraps would break it.
    #[test]
    fn justify_leaves_headings_quotes_lists_and_tables_alone() {
        let got = render(
            "# A heading much longer than the measure itself\n\n\
             > a quoted line that is also much longer than the measure\n\n\
             - a list item that likewise runs well past the measure\n\n\
             | a | b |\n|---|---|\n| longer than the measure | cell |\n",
            10,
            true,
            &MarkdownConfig::default(),
        );
        assert!(got.contains("A heading much longer than the measure itself"));
        assert!(got.contains("> a quoted line that is also much longer than the measure"));
        assert!(got.contains("- a list item that likewise runs well past the measure"));
        assert!(got.contains("| longer than the measure | cell |"));
    }
}
