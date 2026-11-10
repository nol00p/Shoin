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
    /// Inline LaTeX delimited by `$…$`
    Latex,
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

/// The link span the cursor sits in, if it sits in one.
///
/// `!` is not part of an unexpanded `![[…]]`'s span: `bracket_link` rejects it
/// (no `(` follows the `]`), the scan falls through one character, and
/// `wiki_at` matches the `[[…]]` alone. That is fine for styling — the bang is
/// one dim character — but a reader whose cursor is ON the bang still means the
/// embed, so the bang is accepted here as belonging to the span after it.
pub fn span_at(line: &str, col: usize, cfg: &MarkdownConfig) -> Option<InlineSpan> {
    let bang = line.chars().nth(col) == Some('!');
    scan(line, cfg)
        .into_iter()
        .find(|s| s.outer.contains(&col) || (bang && s.outer.start == col + 1))
}

/// What a link span points AT — the note name or URL, never the display text.
///
/// Recovered from the line rather than stored on the span. `InlineSpan` is
/// built for every construct on every visible line, and a destination is read
/// only when someone follows a link; a third `Range` on the struct would cost
/// that on every frame to serve one keystroke.
///
/// `None` for a span that is not a link, and for a link whose destination is
/// empty — `[text]()` points nowhere.
pub fn target_of(span: &InlineSpan, line: &str) -> Option<String> {
    let out = match span.kind {
        Inline::WikiLink => line
            .chars()
            .skip(span.inner.start)
            .take(span.inner.len())
            .collect::<String>(),
        _ => destination(span, line)?.0,
    };
    let out = out.trim().to_string();
    (!out.is_empty()).then_some(out)
}

/// The address and title of a `[text](…)` / `![alt](…)` link or an autolink:
/// `(url, Some(title))` for `[a](url "title")`, `(url, None)` without one.
///
/// The `(…)` part is read as CommonMark writes it: `url`, `url "title"`,
/// `url 'title'`, `url (title)`, or `<url>` with any of those after it; a
/// backslash before punctuation is dropped from both. What does not fit that
/// shape — `[a](my note.md)`, a path with spaces — is all address, as before.
/// An autolink's address is its body, without any `<…>`.
///
/// `None` for a span of another kind (a `[[wiki link]]` names a note, not an
/// address). The address may be empty: `[text]()`.
pub fn destination(span: &InlineSpan, line: &str) -> Option<(String, Option<String>)> {
    let chars: Vec<char> = line.chars().collect();
    match span.kind {
        // `](` closes the body; the last char of the span is the `)`.
        Inline::Link | Inline::Image => {
            let raw = chars.get(span.inner.end + 2..span.outer.end.checked_sub(1)?)?;
            Some(split_destination(raw))
        }
        Inline::Autolink => Some((chars.get(span.inner.clone())?.iter().collect(), None)),
        _ => None,
    }
}

/// `url "title"` → (url, title), for what sits between `](` and `)`.
fn split_destination(raw: &[char]) -> (String, Option<String>) {
    let whole = || (unescape(raw).trim().to_string(), None);
    let start = raw.iter().take_while(|c| c.is_whitespace()).count();
    let Some(url_end) = dest_end(raw, start) else {
        return whole();
    };
    let rest = raw[url_end..].iter().skip_while(|c| c.is_whitespace()).count();
    let rest = &raw[raw.len() - rest..];
    let title = if rest.is_empty() {
        None
    } else {
        match title_end(rest, 0) {
            Some(end) if rest[end..].iter().all(|c| c.is_whitespace()) => {
                Some(unescape(&rest[1..end - 1]))
            }
            _ => return whole(),
        }
    };
    let url = &raw[start..url_end];
    let url = match url.first() {
        Some('<') => &url[1..url.len() - 1],
        _ => url,
    };
    (unescape(url), title)
}

/// Where the address that starts at `from` ends (exclusive): past the `>` of
/// a `<…>` address, else at whitespace or at the `)` that would bring the
/// paren count below zero. `None` for a `<` with no `>`.
fn dest_end(chars: &[char], from: usize) -> Option<usize> {
    if chars.get(from) == Some(&'<') {
        let mut j = from + 1;
        while j < chars.len() {
            match chars[j] {
                '\\' => j += 1,
                '<' => return None,
                '>' => return Some(j + 1),
                _ => {}
            }
            j += 1;
        }
        return None;
    }
    let mut depth = 0usize;
    let mut j = from;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 1,
            '(' => depth += 1,
            ')' if depth == 0 => return Some(j),
            ')' => depth -= 1,
            c if c.is_whitespace() => return Some(j),
            _ => {}
        }
        j += 1;
    }
    Some(chars.len().min(j))
}

/// A title starting at `from` — `"…"`, `'…'` or `(…)` — and the index just
/// past its closing mark. `None` if `from` opens no title or it never closes.
fn title_end(chars: &[char], from: usize) -> Option<usize> {
    let close = match chars.get(from)? {
        '"' => '"',
        '\'' => '\'',
        '(' => ')',
        _ => return None,
    };
    let mut j = from + 1;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 1,
            '(' if close == ')' => return None,
            c if c == close => return Some(j + 1),
            _ => {}
        }
        j += 1;
    }
    None
}

/// Drop the backslash from `\(` and every other escaped ASCII punctuation.
fn unescape(chars: &[char]) -> String {
    let mut out = String::new();
    let mut j = 0;
    while j < chars.len() {
        if chars[j] == '\\' && chars.get(j + 1).is_some_and(|c| c.is_ascii_punctuation()) {
            j += 1;
        }
        out.push(chars[j]);
        j += 1;
    }
    out
}

/// Try every construct that can begin at `i`, in priority order.
fn match_at(chars: &[char], i: usize, cfg: &MarkdownConfig) -> Option<InlineSpan> {
    match chars[i] {
        '\\' => escape_at(chars, i),
        '`' => code_at(chars, i),
        '!' if chars.get(i + 1) == Some(&'[') => bracket_link(chars, i, true),
        '[' if cfg.wiki_links && chars.get(i + 1) == Some(&'[') => wiki_at(chars, i),
        '[' => bracket_link(chars, i, false),
        '*' | '_' => emphasis_at(chars, i, chars[i]),
        '~' => pair_at(chars, i, '~', Inline::Strikethrough),
        '=' if cfg.highlight => pair_at(chars, i, '=', Inline::Highlight),
        '#' if cfg.tags => tag_at(chars, i),
        'h' => autolink_at(chars, i),
        '<' => angle_autolink_at(chars, i),
        '$' if cfg.math => {
            if chars.get(i + 1) == Some(&'$') {
                latex_delimited_at(chars, i)
            } else {
                latex_at(chars, i)
            }
        }
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

/// The first closing run of at least `n` of `ch` at or after `from`.
///
/// A closer may not follow whitespace or be escaped, and a `_` closer may not
/// be followed by a letter or digit (so `snake_case` stays plain). A run that
/// fails is skipped whole: `*a **` must not close on the second star alone.
fn find_closer(chars: &[char], from: usize, ch: char, n: usize) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        if chars[j] != ch {
            j += 1;
            continue;
        }
        let run = run_len(chars, j, ch);
        let after = chars.get(j + run);
        let ok = run >= n
            && j > 0
            && !chars[j - 1].is_whitespace()
            && !is_escaped(chars, j)
            && !(ch == '_' && after.is_some_and(|c| c.is_alphanumeric()));
        if ok {
            return Some(j);
        }
        j += run;
    }
    None
}

/// Whether the run of `ch` at `i`, `run` long, may open a span: something
/// other than whitespace follows it, and a `_` run is not inside a word
/// (`snake_case`, `a__b__`).
fn can_open(chars: &[char], i: usize, run: usize, ch: char) -> bool {
    let Some(next) = chars.get(i + run) else {
        return false;
    };
    if next.is_whitespace() {
        return false;
    }
    if ch == '_' && i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
        return false;
    }
    true
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

fn latex_at(chars: &[char], i: usize) -> Option<InlineSpan> {
    // Pandoc rule: opening $ must be immediately followed by a non-whitespace character.
    let next_char = chars.get(i + 1)?;
    if next_char.is_whitespace() || *next_char == '$' {
        return None;
    }

    let mut j = i + 1;
    while j < chars.len() {
        // `\$` is a dollar sign inside the math, not its end.
        if chars[j] == '$' && !is_escaped(chars, j) {
            // Pandoc rule: closing $ must be preceded by a non-whitespace character
            // and must NOT be followed by an ASCII digit.
            let prev_char = chars[j - 1];
            if prev_char.is_whitespace() {
                return None;
            }
            if let Some(after) = chars.get(j + 1) {
                if after.is_ascii_digit() {
                    return None;
                }
            }
            return (j > i + 1).then_some(InlineSpan {
                kind: Inline::Latex,
                outer: i..j + 1,
                inner: i + 1..j,
            });
        }
        j += 1;
    }
    None
}

/// Whether the character at `i` is escaped: an odd run of backslashes sits
/// right before it (`\$` is, `\\$` is not).
pub(crate) fn is_escaped(chars: &[char], i: usize) -> bool {
    chars[..i].iter().rev().take_while(|c| **c == '\\').count() % 2 == 1
}

fn latex_delimited_at(chars: &[char], i: usize) -> Option<InlineSpan> {
    let mut j = i + 2;
    while j < chars.len() {
        if chars[j] == '$' && chars.get(j + 1) == Some(&'$') && !is_escaped(chars, j) {
            return (j > i + 2).then_some(InlineSpan {
                kind: Inline::Latex,
                outer: i..j + 2,
                inner: i + 2..j,
            });
        }
        j += 1;
    }
    None
}

/// `*` / `**` / `***`, and the same with `_`. The run length picks italic /
/// bold / both. The opener must be followed, and the closer preceded, by a
/// non-space, so `2 * 3 * 4` stays plain; `_` also stays plain inside a word.
fn emphasis_at(chars: &[char], i: usize, ch: char) -> Option<InlineSpan> {
    let run = run_len(chars, i, ch);
    if !can_open(chars, i, run, ch) {
        return None;
    }
    let n = run.min(3);
    let kind = match n {
        3 => Inline::BoldItalic,
        2 => Inline::Bold,
        _ => Inline::Italic,
    };
    let open_end = i + n;
    let close = find_closer(chars, open_end, ch, n)?;
    if close <= open_end || all_blank(chars, open_end..close) {
        return None;
    }
    Some(InlineSpan {
        kind,
        outer: i..close + n,
        inner: open_end..close,
    })
}

/// A two-character symmetric fence: `~~x~~`, `==x==`. Spaced like emphasis:
/// `a == b == c` stays plain.
fn pair_at(chars: &[char], i: usize, ch: char, kind: Inline) -> Option<InlineSpan> {
    let run = run_len(chars, i, ch);
    if run < 2 || !can_open(chars, i, run, ch) {
        return None;
    }
    let open_end = i + 2;
    let close = find_closer(chars, open_end, ch, 2)?;
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
///
/// The `)` that closes the construct is found as CommonMark finds it: the
/// address may hold balanced parentheses (`wiki/Rust_(language)`), may be
/// written `<…>`, and may be followed by a `"title"`. When the `(…)` does not
/// have that shape — `[a](my note.md)`, an unbalanced `(` — the first
/// balancing `)`, then simply the first `)`, closes it, so a link still
/// forms wherever it formed before.
fn bracket_link(chars: &[char], i: usize, image: bool) -> Option<InlineSpan> {
    let text_start = if image { i + 2 } else { i + 1 };
    let text_end = find_char(chars, text_start, ']')?;
    if chars.get(text_end + 1) != Some(&'(') {
        return None;
    }
    let from = text_end + 2;
    let url_end = link_close(chars, from)
        .or_else(|| balanced_close(chars, from))
        .or_else(|| find_char(chars, from, ')'))?;
    Some(InlineSpan {
        kind: if image { Inline::Image } else { Inline::Link },
        outer: i..url_end + 1,
        inner: text_start..text_end,
    })
}

/// The closing `)` of a well-formed `(url "title")` starting at `from`.
fn link_close(chars: &[char], from: usize) -> Option<usize> {
    let skip_ws = |mut j: usize| {
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        j
    };
    let j = dest_end(chars, skip_ws(from))?;
    let mut j = skip_ws(j);
    if chars.get(j) != Some(&')') {
        j = skip_ws(title_end(chars, j)?);
    }
    (chars.get(j) == Some(&')')).then_some(j)
}

/// The `)` that brings the paren count below zero, whitespace and quotes
/// notwithstanding.
fn balanced_close(chars: &[char], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = from;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 1,
            '(' => depth += 1,
            ')' if depth == 0 => return Some(j),
            ')' => depth -= 1,
            _ => {}
        }
        j += 1;
    }
    None
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

/// `<https://…>`: the angle brackets are markers, the address is the body.
/// No whitespace and no `<` inside, as in CommonMark.
fn angle_autolink_at(chars: &[char], i: usize) -> Option<InlineSpan> {
    let start = i + 1;
    let scheme = if starts_with(chars, start, "https://") {
        8
    } else if starts_with(chars, start, "http://") {
        7
    } else {
        return None;
    };
    let mut j = start + scheme;
    while j < chars.len() && chars[j] != '>' {
        if chars[j].is_whitespace() || chars[j] == '<' {
            return None;
        }
        j += 1;
    }
    if j >= chars.len() || j == start + scheme {
        return None;
    }
    Some(InlineSpan {
        kind: Inline::Autolink,
        outer: i..j + 1,
        inner: start..j,
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
        Inline::Latex => {
            s.fg = theme.code;
            s.bg = theme.code_bg;
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

    /// F14: a delimiter run opens only when no space follows it and closes only
    /// when no space precedes it, so arithmetic stays plain.
    #[test]
    fn spaced_markers_do_not_emphasise() {
        assert!(scan("2 * 3 * 4 = 24", &md()).is_empty());
        assert!(scan("a ** b ** c", &md()).is_empty());
        assert!(scan("x *** y *** z", &md()).is_empty());
        assert!(scan("a ~~ b ~~ c", &md()).is_empty());
        assert!(scan("a == b == c", &md()).is_empty());
        assert!(scan("*a *", &md()).is_empty());
        assert!(scan("* a*", &md()).is_empty());
        // A closer that fails the rule is skipped whole, not split.
        assert!(scan("*a **", &md()).is_empty());
        // ... and the scan goes on to a closer that passes.
        let line = "*a * b*";
        let s = only(line);
        assert_eq!((s.kind, text(line, &s.inner)), (Inline::Italic, "a * b".into()));
        // Unchanged forms.
        assert_eq!(only("*a*").kind, Inline::Italic);
        assert_eq!(only("**a**").kind, Inline::Bold);
        let line = "a*b*c";
        let s = only(line);
        assert_eq!((s.kind, text(line, &s.inner)), (Inline::Italic, "b".into()));
        let line = "**a*b*c**";
        let s = only(line);
        assert_eq!((s.kind, text(line, &s.inner)), (Inline::Bold, "a*b*c".into()));
        let line = "***both*** and **x**";
        let v = scan(line, &md());
        assert_eq!(v.iter().map(|s| s.kind).collect::<Vec<_>>(), [Inline::BoldItalic, Inline::Bold]);
        // A list marker's `* ` never opens emphasis.
        let line = "* item with *it*";
        let s = only(line);
        assert_eq!(text(line, &s.outer), "*it*");
        assert!(scan("* item * more", &md()).is_empty());
    }

    /// F14: `_` and `__` emphasise like `*` and `**`, but never inside a word.
    #[test]
    fn underscore_emphasis() {
        let line = "an _italic_ and __bold__ word";
        let v = scan(line, &md());
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].kind, text(line, &v[0].inner)), (Inline::Italic, "italic".into()));
        assert_eq!((v[1].kind, text(line, &v[1].inner)), (Inline::Bold, "bold".into()));
        assert_eq!(only("_a_").kind, Inline::Italic);
        assert_eq!(only("__a__").kind, Inline::Bold);
        assert_eq!(only("___a___").kind, Inline::BoldItalic);
        assert_eq!(only("__init__").kind, Inline::Bold);
        let line = "(_a_), _b_.";
        let v = scan(line, &md());
        assert_eq!(v.iter().map(|s| text(line, &s.inner)).collect::<Vec<_>>(), ["a", "b"]);
        for plain in [
            "snake_case_name",
            "a_b_c",
            "a__b__c",
            "a__b__",
            "_a_b",
            "_ a _",
            "x _ y _ z",
            "file_name.md and other_name.md",
            "see example.com/a_b_c and x_y_z",
        ] {
            assert!(scan(plain, &md()).is_empty(), "{plain:?}");
        }
        // An escaped `_` does not close.
        let kinds: Vec<Inline> = scan("_a\\_", &md()).iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [Inline::Escape]);
        // The address of a URL is one span; its underscores open nothing.
        let line = "https://x.org/a_b_c_ and _d_";
        let v = scan(line, &md());
        assert_eq!(v.iter().map(|s| s.kind).collect::<Vec<_>>(), [Inline::Autolink, Inline::Italic]);
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

    /// The span under the cursor is what a follow command acts on — including
    /// from the `!` of an `![[…]]`, which is NOT inside the span the scanner
    /// emits for it.
    #[test]
    fn span_at_finds_the_link_the_cursor_is_in() {
        let cfg = MarkdownConfig::default();
        let line = "see ![[frag#Head]] and [x](y.md) and https://a.example/b";

        let bang = line.find('!').unwrap();
        assert_eq!(span_at(line, bang, &cfg).unwrap().kind, Inline::WikiLink);
        assert_eq!(span_at(line, bang + 3, &cfg).unwrap().kind, Inline::WikiLink);

        let md = line.find("[x]").unwrap();
        assert_eq!(span_at(line, md + 1, &cfg).unwrap().kind, Inline::Link);

        let url = line.find("https").unwrap();
        assert_eq!(span_at(line, url + 2, &cfg).unwrap().kind, Inline::Autolink);

        assert!(span_at(line, 1, &cfg).is_none(), "plain prose is not a link");
    }

    /// A destination is never the display text.
    #[test]
    fn target_of_reads_the_destination_not_the_label() {
        let cfg = MarkdownConfig::default();
        let cases = [
            ("[[frag#Head]]", "frag#Head"),
            ("![[frag]]", "frag"),
            ("[the label](notes/x.md)", "notes/x.md"),
            ("https://a.example/b", "https://a.example/b"),
        ];
        for (line, want) in cases {
            let span = span_at(line, line.len() / 2, &cfg).unwrap();
            assert_eq!(target_of(&span, line).as_deref(), Some(want), "{line}");
        }
        // A link that points nowhere has no destination to follow.
        let span = span_at("[label]()", 2, &cfg).unwrap();
        assert_eq!(target_of(&span, "[label]()"), None);
    }

    /// F13: an address may hold balanced parentheses, a title, or `<…>`.
    #[test]
    fn link_destinations_balance_parens_and_split_titles() {
        let wiki = "[Rust](https://en.wikipedia.org/wiki/Rust_(programming_language))";
        let s = only(wiki);
        assert_eq!(s.kind, Inline::Link);
        assert_eq!(s.outer, 0..wiki.chars().count(), "the closing `)` belongs to the link");
        assert_eq!(
            target_of(&s, wiki).as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(programming_language)")
        );

        let img = r#"![alt](pic.png "title")"#;
        let s = only(img);
        assert_eq!(s.kind, Inline::Image);
        assert_eq!(destination(&s, img), Some(("pic.png".into(), Some("title".into()))));
        assert_eq!(target_of(&s, img).as_deref(), Some("pic.png"));

        let line = "[a](b 'one') [c](d (two)) [e](f \"a (b\")";
        let spans = scan(line, &md());
        let got: Vec<_> = spans.iter().map(|s| destination(s, line).unwrap()).collect();
        assert_eq!(
            got,
            vec![
                ("b".into(), Some("one".into())),
                ("d".into(), Some("two".into())),
                ("f".into(), Some("a (b".into())),
            ]
        );

        // The first `)` still closes a link that has no `(` of its own.
        let line = "[a](b) and (c)";
        let s = only(line);
        assert_eq!(text(line, &s.outer), "[a](b)");
        assert_eq!(target_of(&s, line).as_deref(), Some("b"));

        // `<…>` holds spaces and parentheses; the brackets are not the address.
        let line = "[a](<b c>) x";
        let s = only(line);
        assert_eq!(text(line, &s.outer), "[a](<b c>)");
        assert_eq!(target_of(&s, line).as_deref(), Some("b c"));
        let line = "[a](<b) c> \"t\") x";
        let s = only(line);
        assert_eq!(destination(&s, line), Some(("b) c".into(), Some("t".into()))));

        // Escaped parentheses do not count, and lose their backslash.
        let line = r"[a](b\(c) d)";
        let s = only(line);
        assert_eq!(text(line, &s.outer), r"[a](b\(c)");
        assert_eq!(target_of(&s, line).as_deref(), Some("b(c"));

        // A path with spaces, as people write them, still reads whole.
        let line = "[n](my note.md)";
        assert_eq!(target_of(&only(line), line).as_deref(), Some("my note.md"));
    }

    #[test]
    fn angle_autolink_hides_its_brackets() {
        let line = "see <https://a.b/c_(d)> now";
        let s = only(line);
        assert_eq!(s.kind, Inline::Autolink);
        assert_eq!(text(line, &s.outer), "<https://a.b/c_(d)>");
        assert_eq!(text(line, &s.inner), "https://a.b/c_(d)");
        assert_eq!(target_of(&s, line).as_deref(), Some("https://a.b/c_(d)"));
        assert_eq!(destination(&s, line), Some(("https://a.b/c_(d)".into(), None)));
        // Not an autolink: no scheme, a space, or no closing `>`.
        assert!(scan("<b>x</b>", &md()).is_empty());
        assert_eq!(only("<https://a b>").outer, 1..10, "only the bare URL");
        assert_eq!(only("<https://a.b").outer, 1..12);
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
    fn inline_latex_is_detected_when_enabled() {
        let mut cfg = md();
        cfg.math = true;
        let spans = scan("area $x^2$", &cfg);
        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert_eq!(span.kind, Inline::Latex);
        assert_eq!(text("area $x^2$", &span.inner), "x^2");
        assert!(scan("$unclosed", &cfg).is_empty());
        let spans_display = scan(r"$$\nabla \cdot \vec{E}$$", &cfg);
        assert_eq!(spans_display.len(), 1);
        let display = &spans_display[0];
        assert_eq!(display.kind, Inline::Latex);
        assert_eq!(
            text(r"$$\nabla \cdot \vec{E}$$", &display.inner),
            r"\nabla \cdot \vec{E}"
        );
        // Pandoc rule tests:
        assert!(scan("costs $5 and $10", &cfg).is_empty());
        assert!(scan("costs $ 5$", &cfg).is_empty());
        assert!(scan("costs $5 $", &cfg).is_empty());
        // \( is an escaped parenthesis, not math:
        assert_eq!(scan(r"area \(x^2\)", &cfg)[0].kind, Inline::Escape);

        // When math is false, $...$ is plain text:
        let mut cfg_off = md();
        cfg_off.math = false;
        assert!(scan("area $x^2$", &cfg_off).is_empty());
    }

    #[test]
    fn spans_never_overlap_and_stay_in_bounds() {
        for line in [
            "**a** `b` [c](d) #e ~~f~~ *g* \\!",
            "[a](b(c)d) (e) [f](g)) ![h](i(j \"k)\")",
            "[a](<b) [c](d) <https://e(f)> <http://>",
            "[a](b\\) c) [d](( [e](f 'g) h",
            "((([x](y((z)))))) <https://a.b)>",
            "[a](b \"c) [d](e)",
            "2 * 3 * 4 _a_ __b__ snake_case *a **b** c*",
            "*a **  _ _x_ y_ __ ___z___ a_b_ _c_d",
        ] {
            let spans = scan(line, &md());
            let len = line.chars().count();
            let mut prev_end = 0;
            for s in &spans {
                assert!(s.outer.start >= prev_end, "overlap at {:?} in {line:?}", s.outer);
                assert!(s.inner.start >= s.outer.start && s.inner.end <= s.outer.end);
                assert!(s.outer.end <= len, "past the end in {line:?}");
                let _ = destination(s, line);
                let _ = target_of(s, line);
                prev_end = s.outer.end;
            }
        }
    }

    /// Random lines over the characters links are made of: the same
    /// guarantees hold, and reading a destination never panics.
    #[test]
    fn link_soup_keeps_the_scanner_guarantees() {
        let alphabet: Vec<char> = "[]()<>!\\\"' ahtps:/.é*_~=".chars().collect();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..3000 {
            let mut line = String::new();
            for _ in 0..(seed % 24) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                line.push(alphabet[(seed % alphabet.len() as u64) as usize]);
            }
            let len = line.chars().count();
            let mut prev_end = 0;
            for s in scan(&line, &md()) {
                assert!(s.outer.start >= prev_end && s.outer.end > s.outer.start, "{line:?}");
                assert!(s.inner.start >= s.outer.start && s.inner.end <= s.outer.end, "{line:?}");
                assert!(s.outer.end <= len, "{line:?}");
                let _ = (destination(&s, &line), target_of(&s, &line));
                prev_end = s.outer.end;
            }
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
            "[a](", "[a](<", "[a](<b", "[a](b \"", "[a](()", "[a](\\", "<", "<h", "<https://",
            "<https://>", "[a](b (c", "![a](<>)", "[a]( )",
            "_", "__", "___", "_ _", "* *", "** **", "a_", "_a", "__a_", "_a__", "~~ ~~",
        ] {
            for s in scan(line, &md()) {
                let _ = (destination(&s, line), target_of(&s, line)); // must not panic
            }
        }
    }
}
