//! Text objects — the `iw` / `ap` / `i(` half of the operator grammar.
//! SPEC.md §7.2.
//!
//! A motion answers "where does the cursor go"; an object answers "what is the
//! thing under the cursor". Both feed the same operators, so everything here
//! returns a plain absolute char range and lets `app.rs` apply `d`/`c`/`y` to
//! it exactly as it applies them to a resolved motion.
//!
//! Paragraph and sentence bounds live here rather than in the render layer
//! because they are properties of the TEXT: focus mode (`render::focus`) and
//! the `ip`/`is` objects are two readers of one definition.

use super::buffer::Buffer;
use super::cursor::Cursor;
use super::motion::char_class;

/// A resolved object: an absolute char range, END EXCLUSIVE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectRange {
    pub start: usize,
    pub end: usize,
    /// Whole lines — `ip`/`ap`, which delete their newlines with them.
    pub linewise: bool,
}

impl ObjectRange {
    fn chars(start: usize, end: usize) -> Option<Self> {
        (end > start).then_some(ObjectRange {
            start,
            end,
            linewise: false,
        })
    }
}

/// Resolve the object named by `key` around the cursor. `around` is the `a`
/// form (include the delimiters/trailing space); otherwise it is `i` (inside).
///
/// Counts are not applied: `d2iw` behaves as `diw`. Vim's counted objects are
/// rare in prose and the grammar has nowhere to put the count today.
pub fn resolve(buffer: &Buffer, key: char, around: bool) -> Option<ObjectRange> {
    let cursor = buffer.cursor;
    match key {
        'w' => word(buffer, cursor, around, false),
        'W' => word(buffer, cursor, around, true),
        'p' => paragraph_object(buffer, cursor.line, around),
        's' => sentence_object(buffer, cursor, around),
        '"' | '\'' | '`' => quote(buffer, cursor, key, around),
        '(' | ')' | 'b' => pair(buffer, cursor, '(', ')', around),
        '[' | ']' => pair(buffer, cursor, '[', ']', around),
        '{' | '}' | 'B' => pair(buffer, cursor, '{', '}', around),
        '<' | '>' => pair(buffer, cursor, '<', '>', around),
        _ => None,
    }
}

/// `iw`/`aw` and their `W` forms. A word is a run of one character class —
/// which makes a run of punctuation, and a run of spaces, objects in their own
/// right, exactly as in vim. Never crosses a line.
fn word(buffer: &Buffer, cursor: Cursor, around: bool, big: bool) -> Option<ObjectRange> {
    let chars: Vec<char> = buffer.line_text(cursor.line).chars().collect();
    if chars.is_empty() {
        return None;
    }
    let base = buffer.rope.line_to_char(cursor.line);
    let class = |c: char| {
        if big {
            u8::from(!c.is_whitespace())
        } else {
            char_class(c)
        }
    };

    let col = cursor.col.min(chars.len() - 1);
    let k = class(chars[col]);
    let mut s = col;
    while s > 0 && class(chars[s - 1]) == k {
        s -= 1;
    }
    let mut e = col + 1;
    while e < chars.len() && class(chars[e]) == k {
        e += 1;
    }

    if around {
        if k == 0 {
            // The object IS whitespace: `aw` swallows the word after it.
            let next = class(*chars.get(e)?);
            while e < chars.len() && class(chars[e]) == next {
                e += 1;
            }
        } else {
            let mut trailing = e;
            while trailing < chars.len() && chars[trailing].is_whitespace() {
                trailing += 1;
            }
            if trailing > e {
                e = trailing;
            } else {
                // Nothing after it — take the leading whitespace instead, so
                // `daw` on the last word of a line does not leave a gap.
                while s > 0 && chars[s - 1].is_whitespace() {
                    s -= 1;
                }
            }
        }
    }
    ObjectRange::chars(base + s, base + e)
}

/// The contiguous run of non-blank lines around `line`. A run of blank lines is
/// its own paragraph, so `ip` works on the gap between two blocks too.
pub fn paragraph_bounds(buffer: &Buffer, line: usize) -> (usize, usize) {
    let blank = |l: usize| buffer.line_text(l).trim().is_empty();
    if blank(line) {
        return (line, line);
    }
    let mut s = line;
    while s > 0 && !blank(s - 1) {
        s -= 1;
    }
    let last = buffer.line_count().saturating_sub(1);
    let mut e = line;
    while e < last && !blank(e + 1) {
        e += 1;
    }
    (s, e)
}

/// `ip`/`ap`. Linewise: the run of lines matching the cursor line's blankness,
/// and for `ap` the opposite run that follows (or precedes, at the end of a
/// file) — vim's "paragraph plus its trailing blank line".
fn paragraph_object(buffer: &Buffer, line: usize, around: bool) -> Option<ObjectRange> {
    let blank = |l: usize| buffer.line_text(l).trim().is_empty();
    let last = buffer.line_count().saturating_sub(1);
    let on_blank = blank(line);

    let mut s = line;
    while s > 0 && blank(s - 1) == on_blank {
        s -= 1;
    }
    let mut e = line;
    while e < last && blank(e + 1) == on_blank {
        e += 1;
    }

    if around {
        let mut trailing = e;
        while trailing < last && blank(trailing + 1) != on_blank {
            trailing += 1;
        }
        if trailing > e {
            e = trailing;
        } else {
            while s > 0 && blank(s - 1) != on_blank {
                s -= 1;
            }
        }
    }

    let start = buffer.rope.line_to_char(s);
    let end = if e < last {
        buffer.rope.line_to_char(e + 1)
    } else {
        buffer.rope.len_chars()
    };
    (end > start).then_some(ObjectRange {
        start,
        end,
        linewise: true,
    })
}

/// The sentence containing the cursor, bounded by `. ! ?` (inclusive) and never
/// crossing a paragraph break. Shared with focus mode.
pub fn sentence_bounds(buffer: &Buffer, cursor: Cursor) -> (usize, usize) {
    let (ps, pe) = paragraph_bounds(buffer, cursor.line);
    let lo = buffer.rope.line_to_char(ps);
    let hi = if pe + 1 < buffer.line_count() {
        buffer.rope.line_to_char(pe + 1)
    } else {
        buffer.rope.len_chars()
    };
    // Only the paragraph is scanned, not the document.
    let chars: Vec<char> = buffer.rope.slice(lo..hi).chars().collect();
    let idx = buffer.char_index(cursor).clamp(lo, hi);
    let at = |i: usize| chars[i - lo];

    // Back up to just after the previous terminator, then skip leading space.
    let mut s = idx;
    while s > lo && !matches!(at(s - 1), '.' | '!' | '?') {
        s -= 1;
    }
    while s < hi && at(s).is_whitespace() {
        s += 1;
    }

    // Forward to the next terminator, inclusive.
    let mut e = idx;
    while e < hi {
        let terminator = matches!(at(e), '.' | '!' | '?');
        e += 1;
        if terminator {
            break;
        }
    }
    (s.max(lo).min(e), e)
}

/// `is`/`as`. `as` takes the whitespace up to the next sentence with it.
fn sentence_object(buffer: &Buffer, cursor: Cursor, around: bool) -> Option<ObjectRange> {
    let (s, mut e) = sentence_bounds(buffer, cursor);
    if around {
        let len = buffer.rope.len_chars();
        while e < len && buffer.rope.char(e) == ' ' {
            e += 1;
        }
    }
    ObjectRange::chars(s, e)
}

/// `i"` / `a"` and the other two quote characters. Quotes do not span lines —
/// an unclosed quote is far more often a typo than an object.
fn quote(buffer: &Buffer, cursor: Cursor, q: char, around: bool) -> Option<ObjectRange> {
    let chars: Vec<char> = buffer.line_text(cursor.line).chars().collect();
    let base = buffer.rope.line_to_char(cursor.line);

    // Unescaped quote positions, paired left to right.
    let marks: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|(i, c)| **c == q && (*i == 0 || chars[i - 1] != '\\'))
        .map(|(i, _)| i)
        .collect();

    // `as_chunks::<2>()` rather than `chunks_exact(2)`: the items are
    // `&[usize; 2]`, so `c[0]` and `c[1]` are checked when this compiles instead
    // of at each call. `.1` is the odd trailing quote, which has no partner and
    // so bounds nothing.
    let (open, close) = marks
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| (c[0], c[1]))
        .find(|(_, close)| cursor.col <= *close)?;

    if !around {
        return ObjectRange::chars(base + open + 1, base + close);
    }
    // `a"` takes the quotes, plus trailing whitespace when there is any.
    let mut e = close + 1;
    while e < chars.len() && chars[e].is_whitespace() {
        e += 1;
    }
    ObjectRange::chars(base + open, base + e)
}

/// `i(` / `a{` and friends. Brackets DO span lines, matched with nesting, so
/// `di(` works on a multi-line call or a wrapped link target.
fn pair(
    buffer: &Buffer,
    cursor: Cursor,
    open: char,
    close: char,
    around: bool,
) -> Option<ObjectRange> {
    let rope = &buffer.rope;
    let len = rope.len_chars();
    let idx = buffer.char_index(cursor).min(len.saturating_sub(1));
    if len == 0 {
        return None;
    }

    // Search back for the unmatched opener. Sitting ON one counts as inside it.
    let start = if rope.char(idx) == open {
        idx
    } else {
        let mut depth = 0usize;
        let mut i = idx;
        loop {
            if i == 0 {
                return None;
            }
            i -= 1;
            let c = rope.char(i);
            if c == close {
                depth += 1;
            } else if c == open {
                match depth.checked_sub(1) {
                    Some(d) => depth = d,
                    None => break i,
                }
            }
        }
    };

    // Then forward for its partner.
    let mut depth = 0usize;
    let mut j = start + 1;
    let end = loop {
        if j >= len {
            return None;
        }
        let c = rope.char(j);
        if c == open {
            depth += 1;
        } else if c == close {
            match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => break j,
            }
        }
        j += 1;
    };

    if around {
        ObjectRange::chars(start, end + 1)
    } else {
        ObjectRange::chars(start + 1, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn buf(text: &str, line: usize, col: usize) -> Buffer {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str(text);
        b.cursor = Cursor::new(line, col);
        b
    }

    fn text_of(b: &Buffer, r: ObjectRange) -> String {
        b.rope.slice(r.start..r.end).to_string()
    }

    #[test]
    fn inner_and_around_word() {
        let b = buf("the quick fox\n", 0, 6); // inside "quick"
        assert_eq!(text_of(&b, resolve(&b, 'w', false).unwrap()), "quick");
        assert_eq!(text_of(&b, resolve(&b, 'w', true).unwrap()), "quick ");
    }

    /// On the last word there is no trailing space, so `aw` eats the leading one.
    #[test]
    fn around_word_at_end_of_line_takes_leading_space() {
        let b = buf("the quick fox\n", 0, 11);
        assert_eq!(text_of(&b, resolve(&b, 'w', true).unwrap()), " fox");
    }

    /// Punctuation is its own class: `iw` on `,` is the comma, not the word.
    #[test]
    fn word_objects_follow_character_class() {
        let b = buf("one, two\n", 0, 3);
        assert_eq!(text_of(&b, resolve(&b, 'w', false).unwrap()), ",");
        // `iW` is whitespace-delimited, so it takes the comma with the word.
        let b = buf("one, two\n", 0, 1);
        assert_eq!(text_of(&b, resolve(&b, 'W', false).unwrap()), "one,");
    }

    #[test]
    fn paragraph_is_linewise_and_around_takes_the_blank_line() {
        let b = buf("a one\na two\n\nb one\n", 0, 2);
        let inner = resolve(&b, 'p', false).unwrap();
        assert!(inner.linewise);
        assert_eq!(text_of(&b, inner), "a one\na two\n");
        assert_eq!(text_of(&b, resolve(&b, 'p', true).unwrap()), "a one\na two\n\n");
    }

    #[test]
    fn sentence_stops_at_the_terminator() {
        let b = buf("One thing. Two things. Three.\n", 0, 12);
        assert_eq!(text_of(&b, resolve(&b, 's', false).unwrap()), "Two things.");
        assert_eq!(text_of(&b, resolve(&b, 's', true).unwrap()), "Two things. ");
    }

    #[test]
    fn quotes_pair_left_to_right() {
        let b = buf("say \"hello there\" now\n", 0, 8);
        assert_eq!(text_of(&b, resolve(&b, '"', false).unwrap()), "hello there");
        assert_eq!(text_of(&b, resolve(&b, '"', true).unwrap()), "\"hello there\" ");
    }

    /// The cursor before the quotes still finds the first pair on the line.
    #[test]
    fn quotes_found_from_before_them() {
        let b = buf("say \"hi\" now\n", 0, 0);
        assert_eq!(text_of(&b, resolve(&b, '"', false).unwrap()), "hi");
    }

    #[test]
    fn brackets_nest_and_span_lines() {
        let b = buf("f(a, g(b),\n  c)\n", 0, 3);
        assert_eq!(text_of(&b, resolve(&b, '(', false).unwrap()), "a, g(b),\n  c");
        assert_eq!(text_of(&b, resolve(&b, '(', true).unwrap()), "(a, g(b),\n  c)");
        // Inside the nested pair, the inner one wins.
        let b = buf("f(a, g(b),\n  c)\n", 0, 7);
        assert_eq!(text_of(&b, resolve(&b, 'b', false).unwrap()), "b");
    }

    #[test]
    fn unmatched_bracket_is_no_object() {
        let b = buf("no brackets here\n", 0, 4);
        assert!(resolve(&b, '(', false).is_none());
    }

    /// An empty pair has an empty inside — no object, so the operator no-ops
    /// rather than deleting the brackets.
    #[test]
    fn empty_pair_has_no_inside() {
        let b = buf("f()\n", 0, 2);
        assert!(resolve(&b, '(', false).is_none());
        assert_eq!(text_of(&b, resolve(&b, '(', true).unwrap()), "()");
    }
}
