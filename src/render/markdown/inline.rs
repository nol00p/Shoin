//! Pass B — single left-to-right inline scan. SPEC.md §5.2.
//!
//! Delimiter matching is deliberately naive-but-safe: an opener is honored only
//! if a matching closer exists later on the SAME line. Unbalanced markers render
//! as plain text — which is exactly right while the user is mid-word typing
//! `**` before the closing pair exists.
//!
//! Markers are DIMMED, never concealed. Concealment would break SPEC.md §2.

use std::ops::Range;

use crate::config::schema::MarkdownConfig;
use crate::render::theme::{Attrs, Color, Style, Theme};
use crate::render::StyledSpan;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inline {
    Bold,
    Italic,
    BoldItalic,
    Strikethrough,
    Highlight,
    Code,
    Link,
    WikiLink,
    Image,
    Tag,
    Autolink,
    /// `\*` — the backslash is dimmed, the next char is literal.
    Escape,
}

pub struct InlineSpan {
    pub kind: Inline,
    /// The whole construct including markers.
    pub outer: Range<usize>,
    /// The rendered body, excluding markers.
    pub inner: Range<usize>,
}

/// Scan one line. Char-indexed ranges.
///
/// Guarantees, enforced by tests and fuzzing:
///   - never panics on arbitrary input
///   - spans never overlap and never exceed the line's char length
pub fn scan(line: &str, cfg: &MarkdownConfig) -> Vec<InlineSpan> {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < len {
        match match_at(&chars, i, cfg) {
            // A match always starts at `i` and ends past it, so `i` advances and
            // the emitted spans stay ordered and non-overlapping.
            Some(span) => {
                i = span.outer.end.max(i + 1);
                spans.push(span);
            }
            None => i += 1,
        }
    }
    spans
}

/// Try every construct that can begin at `i`, in priority order.
fn match_at(chars: &[char], i: usize, cfg: &MarkdownConfig) -> Option<InlineSpan> {
    match chars[i] {
        '\\' => escape_at(chars, i),
        '`' => code_at(chars, i),
        '!' if chars.get(i + 1) == Some(&'[') => bracket_link(chars, i, true),
        '[' if cfg.wiki_links && chars.get(i + 1) == Some(&'[') => wiki_at(chars, i),
        '[' => bracket_link(chars, i, false),
        '*' => emphasis_at(chars, i),
        '~' => pair_at(chars, i, '~', Inline::Strikethrough),
        '=' if cfg.highlight => pair_at(chars, i, '=', Inline::Highlight),
        '#' if cfg.tags => tag_at(chars, i),
        'h' => autolink_at(chars, i),
        _ => None,
    }
}

// ---------------------------------------------------------------- scan helpers

/// Run length of `ch` starting at `i`.
fn run_len(chars: &[char], i: usize, ch: char) -> usize {
    let mut n = 0;
    while chars.get(i + n) == Some(&ch) {
        n += 1;
    }
    n
}

/// First index at/after `from` beginning a run of at least `n` of `ch`.
fn find_delim(chars: &[char], from: usize, ch: char, n: usize) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        if chars[j] == ch && run_len(chars, j, ch) >= n {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn all_blank(chars: &[char], range: Range<usize>) -> bool {
    chars[range].iter().all(|c| c.is_whitespace())
}

fn starts_with(chars: &[char], i: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(k, pc)| chars.get(i + k) == Some(&pc))
}

fn escape_at(chars: &[char], i: usize) -> Option<InlineSpan> {
    let next = *chars.get(i + 1)?;
    if next.is_ascii_punctuation() {
        Some(InlineSpan {
            kind: Inline::Escape,
            outer: i..i + 2,
            inner: i + 1..i + 2,
        })
    } else {
        None
    }
}

fn code_at(chars: &[char], i: usize) -> Option<InlineSpan> {
    let r = run_len(chars, i, '`');
    let open_end = i + r;
    let close = find_delim(chars, open_end, '`', r)?;
    if close <= open_end {
        return None;
    }
    Some(InlineSpan {
        kind: Inline::Code,
        outer: i..close + r,
        inner: open_end..close,
    })
}

/// `*` / `**` / `***`. The run length picks italic / bold / both.
fn emphasis_at(chars: &[char], i: usize) -> Option<InlineSpan> {
    let n = run_len(chars, i, '*').min(3);
    let kind = match n {
        3 => Inline::BoldItalic,
        2 => Inline::Bold,
        _ => Inline::Italic,
    };
    let open_end = i + n;
    let close = find_delim(chars, open_end, '*', n)?;
    if close <= open_end || all_blank(chars, open_end..close) {
        return None;
    }
    Some(InlineSpan {
        kind,
        outer: i..close + n,
        inner: open_end..close,
    })
}

/// A two-character symmetric fence: `~~x~~`, `==x==`.
fn pair_at(chars: &[char], i: usize, ch: char, kind: Inline) -> Option<InlineSpan> {
    if run_len(chars, i, ch) < 2 {
        return None;
    }
    let open_end = i + 2;
    let close = find_delim(chars, open_end, ch, 2)?;
    if close <= open_end || all_blank(chars, open_end..close) {
        return None;
    }
    Some(InlineSpan {
        kind,
        outer: i..close + 2,
        inner: open_end..close,
    })
}

/// `[text](url)` and `![text](url)`. The rendered body is the text; the URL and
/// brackets are markers. Naive: the first `]` closes the text.
fn bracket_link(chars: &[char], i: usize, image: bool) -> Option<InlineSpan> {
    let text_start = if image { i + 2 } else { i + 1 };
    let text_end = find_char(chars, text_start, ']')?;
    if chars.get(text_end + 1) != Some(&'(') {
        return None;
    }
    let url_end = find_char(chars, text_end + 2, ')')?;
    Some(InlineSpan {
        kind: if image { Inline::Image } else { Inline::Link },
        outer: i..url_end + 1,
        inner: text_start..text_end,
    })
}

/// `[[target]]`.
fn wiki_at(chars: &[char], i: usize) -> Option<InlineSpan> {
    let text_start = i + 2;
    let mut j = text_start;
    while j + 1 < chars.len() {
        if chars[j] == ']' && chars[j + 1] == ']' {
            return Some(InlineSpan {
                kind: Inline::WikiLink,
                outer: i..j + 2,
                inner: text_start..j,
            });
        }
        j += 1;
    }
    None
}

/// `#tag` at a word boundary. A purely numeric tag (`#123`) is rejected, as in
/// Obsidian.
fn tag_at(chars: &[char], i: usize) -> Option<InlineSpan> {
    if i > 0 && !chars[i - 1].is_whitespace() {
        return None;
    }
    let start = i + 1;
    let first = *chars.get(start)?;
    if !(first.is_alphanumeric() || first == '_') {
        return None;
    }
    let mut j = start;
    while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '_' | '-' | '/')) {
        j += 1;
    }
    if !chars[start..j].iter().any(|c| c.is_alphabetic() || *c == '_') {
        return None;
    }
    Some(InlineSpan {
        kind: Inline::Tag,
        outer: i..j,
        inner: start..j,
    })
}

/// A bare `http://` / `https://` URL, ending at whitespace or a closing
/// bracket, with trailing sentence punctuation trimmed off.
fn autolink_at(chars: &[char], i: usize) -> Option<InlineSpan> {
    let scheme = if starts_with(chars, i, "https://") {
        8
    } else if starts_with(chars, i, "http://") {
        7
    } else {
        return None;
    };
    let mut j = i + scheme;
    while j < chars.len()
        && !chars[j].is_whitespace()
        && !matches!(chars[j], '<' | '>' | '"' | '\'' | '`' | ')' | ']' | '}')
    {
        j += 1;
    }
    while j > i + scheme && matches!(chars[j - 1], '.' | ',' | ';' | ':' | '!' | '?') {
        j -= 1;
    }
    if j <= i + scheme {
        return None;
    }
    Some(InlineSpan {
        kind: Inline::Autolink,
        outer: i..j,
        inner: i..j,
    })
}

fn find_char(chars: &[char], from: usize, ch: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == ch)
}

// ---------------------------------------------------------------------- styling

/// Flatten inline spans into a gap-free, non-overlapping cover of `base.range`.
///
/// Each span contributes up to three pieces: a dimmed leading marker, the
/// kind-styled body, and a dimmed trailing marker. Gaps between spans keep the
/// block `base` style. Spans not fully inside the region are ignored.
pub fn to_styled(
    line: &str,
    spans: &[InlineSpan],
    base: &StyledSpan,
    theme: &Theme,
) -> Vec<StyledSpan> {
    let len = line.chars().count();
    let region_start = base.range.start;
    let region_end = base.range.end.min(len);
    let marker = marker_style(theme);

    let mut out = Vec::new();
    let mut cursor = region_start;

    for sp in spans {
        if sp.outer.start < cursor || sp.outer.start < region_start || sp.outer.end > region_end {
            continue;
        }
        if sp.outer.start > cursor {
            out.push(StyledSpan {
                range: cursor..sp.outer.start,
                style: base.style,
            });
        }
        if sp.inner.start > sp.outer.start {
            out.push(StyledSpan {
                range: sp.outer.start..sp.inner.start,
                style: marker,
            });
        }
        if sp.inner.end > sp.inner.start {
            out.push(StyledSpan {
                range: sp.inner.clone(),
                style: body_style(sp.kind, base.style, theme),
            });
        }
        if sp.outer.end > sp.inner.end {
            out.push(StyledSpan {
                range: sp.inner.end..sp.outer.end,
                style: marker,
            });
        }
        cursor = sp.outer.end;
    }

    if cursor < region_end {
        out.push(StyledSpan {
            range: cursor..region_end,
            style: base.style,
        });
    }
    out
}

fn marker_style(theme: &Theme) -> Style {
    Style {
        fg: theme.text_dim,
        bg: Color::None,
        attrs: Attrs {
            dim: theme.dim_markers,
            ..Attrs::default()
        },
    }
}

/// A construct's body style, layered over the block's base style.
fn body_style(kind: Inline, base: Style, theme: &Theme) -> Style {
    let mut s = base;
    match kind {
        Inline::Bold => {
            s.fg = theme.bold;
            s.attrs.bold = theme.bold_attr;
        }
        Inline::Italic => {
            s.fg = theme.italic;
            s.attrs.italic = theme.italic_attr;
        }
        Inline::BoldItalic => {
            s.fg = theme.bold;
            s.attrs.bold = theme.bold_attr;
            s.attrs.italic = theme.italic_attr;
        }
        Inline::Strikethrough => {
            s.fg = theme.strikethrough;
            s.attrs.strikethrough = true;
        }
        Inline::Highlight => s.bg = theme.highlight_bg,
        Inline::Code => {
            s.fg = theme.code;
            s.bg = theme.code_bg;
        }
        Inline::Link | Inline::Image | Inline::Autolink => {
            s.fg = theme.link;
            s.attrs.underline = theme.link_underline;
        }
        Inline::WikiLink => {
            s.fg = theme.wiki_link;
            s.attrs.underline = theme.link_underline;
        }
        Inline::Tag => {
            s.fg = theme.tag;
            s.bg = theme.tag_bg;
        }
        // The backslash is the marker; the escaped char shows in the base style.
        Inline::Escape => {}
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md() -> MarkdownConfig {
        MarkdownConfig::default()
    }

    /// Char-slice of a span's outer/inner, for readable assertions.
    fn text(line: &str, r: &Range<usize>) -> String {
        line.chars().skip(r.start).take(r.end - r.start).collect()
    }

    fn only(line: &str) -> InlineSpan {
        let mut v = scan(line, &md());
        assert_eq!(v.len(), 1, "expected exactly one span in {line:?}");
        v.pop().unwrap()
    }

    #[test]
    fn emphasis_variants() {
        let s = only("a **bold** b");
        assert_eq!(s.kind, Inline::Bold);
        assert_eq!(text("a **bold** b", &s.inner), "bold");
        assert_eq!(text("a **bold** b", &s.outer), "**bold**");

        assert_eq!(only("*i*").kind, Inline::Italic);
        assert_eq!(only("***x***").kind, Inline::BoldItalic);
        assert_eq!(only("~~gone~~").kind, Inline::Strikethrough);
        assert_eq!(only("==hi==").kind, Inline::Highlight);
    }

    #[test]
    fn unbalanced_markers_are_plain() {
        // The whole point: half-typed constructs never produce a span.
        assert!(scan("a **bold", &md()).is_empty());
        assert!(scan("just * one", &md()).is_empty());
        assert!(scan("`unclosed", &md()).is_empty());
        assert!(scan("** **", &md()).is_empty()); // blank inner
    }

    #[test]
    fn code_is_literal_inside() {
        let s = only("call `f(*x*)` now");
        assert_eq!(s.kind, Inline::Code);
        assert_eq!(text("call `f(*x*)` now", &s.inner), "f(*x*)");
        // The `*x*` inside the code span is not separately parsed.
        assert_eq!(scan("call `f(*x*)` now", &md()).len(), 1);
    }

    #[test]
    fn links_images_wikis() {
        let s = only("see [text](http://x.y)");
        assert_eq!(s.kind, Inline::Link);
        assert_eq!(text("see [text](http://x.y)", &s.inner), "text");

        let img = only("![alt](p.png)");
        assert_eq!(img.kind, Inline::Image);
        assert_eq!(text("![alt](p.png)", &img.inner), "alt");

        let w = only("a [[Note Name]] b");
        assert_eq!(w.kind, Inline::WikiLink);
        assert_eq!(text("a [[Note Name]] b", &w.inner), "Note Name");
    }

    #[test]
    fn tags_need_boundaries() {
        let s = only("a #project tag");
        assert_eq!(s.kind, Inline::Tag);
        assert_eq!(text("a #project tag", &s.inner), "project");
        // Mid-word `#` is not a tag; a numeric-only tag is rejected.
        assert!(scan("foo#bar", &md()).is_empty());
        assert!(scan("#123", &md()).is_empty());
    }

    #[test]
    fn autolink_trims_trailing_punctuation() {
        let s = only("visit https://example.com/path.");
        assert_eq!(s.kind, Inline::Autolink);
        assert_eq!(
            text("visit https://example.com/path.", &s.outer),
            "https://example.com/path"
        );
    }

    #[test]
    fn escape_dims_backslash() {
        let s = only(r"a \* b");
        assert_eq!(s.kind, Inline::Escape);
        assert_eq!(text(r"a \* b", &s.outer), r"\*");
        assert_eq!(text(r"a \* b", &s.inner), "*");
    }

    #[test]
    fn config_gates() {
        let mut cfg = md();
        cfg.wiki_links = false;
        cfg.tags = false;
        cfg.highlight = false;
        assert!(scan("[[note]] #tag ==hi==", &cfg).is_empty());
    }

    #[test]
    fn spans_never_overlap_and_stay_in_bounds() {
        let line = "**a** `b` [c](d) #e ~~f~~ *g* \\!";
        let spans = scan(line, &md());
        let len = line.chars().count();
        let mut prev_end = 0;
        for s in &spans {
            assert!(s.outer.start >= prev_end, "overlap at {:?}", s.outer);
            assert!(s.inner.start >= s.outer.start && s.inner.end <= s.outer.end);
            assert!(s.outer.end <= len);
            prev_end = s.outer.end;
        }
    }

    #[test]
    fn to_styled_covers_region_without_gaps() {
        let line = "a **b** c";
        let theme = Theme::default();
        let base = StyledSpan {
            range: 0..line.chars().count(),
            style: Style::fg(theme.text),
        };
        let out = to_styled(line, &scan(line, &md()), &base, &theme);
        // Contiguous cover of the whole region.
        assert_eq!(out.first().unwrap().range.start, 0);
        assert_eq!(out.last().unwrap().range.end, line.chars().count());
        for w in out.windows(2) {
            assert_eq!(w[0].range.end, w[1].range.start);
        }
        // The `**` markers are dimmed; the body carries the bold color.
        assert!(out.iter().any(|s| s.style.fg == theme.text_dim && s.style.attrs.dim));
        assert!(out.iter().any(|s| s.style.fg == theme.bold));
    }

    #[test]
    fn never_panics_on_odd_input() {
        for line in [
            "", "*", "**", "***", "~", "==", "`", "[", "](", "![]", "[[", "]]",
            "\\", "#", "# ", "http://", "https://", "*******", "``````", "[a](b",
            "😀 **bold** 🎉", "\t\t- x", "a*b*c*d*e",
        ] {
            let _ = scan(line, &md()); // must not panic
        }
    }
}
