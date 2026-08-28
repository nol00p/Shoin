//! Motions. SPEC.md §7.3.
//!
//! A motion resolves to a target `Cursor` plus whether it is inclusive,
//! exclusive, or linewise — which is what operators need to compute a range.

use super::buffer::Buffer;
use super::cursor::Cursor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,

    WordForward { big: bool },
    WordBack { big: bool },
    WordEnd { big: bool },

    LineStart,
    LineFirstNonBlank,
    LineEnd,

    BufferStart,
    BufferEnd,
    GotoLine(usize),

    ParagraphForward,
    ParagraphBack,

    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,

    ScreenTop,
    ScreenMiddle,
    ScreenBottom,

    /// `f` `F` `t` `T`
    FindChar {
        target: char,
        forward: bool,
        till: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionKind {
    /// Range excludes the target character (`w`, `0`).
    Exclusive,
    /// Range includes the target character (`e`, `f`).
    Inclusive,
    /// Whole lines (`j`, `G`, `{`).
    Linewise,
}

pub struct MotionResult {
    pub target: Cursor,
    pub kind: MotionKind,
}

pub fn char_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Resolve a motion from the buffer's current cursor, applying `count`.
///
/// Returns `None` if the motion cannot move (e.g. `k` on line 0), which
/// operators treat as a no-op rather than an error. `page` is the viewport
/// height, needed by the scrolling motions.
pub fn resolve(buffer: &Buffer, motion: Motion, count: usize, page: usize) -> Option<MotionResult> {
    let count = count.max(1);
    let cur = buffer.cursor;
    let last_line = buffer.line_count().saturating_sub(1);

    use Motion::*;
    use MotionKind::*;

    let (target, kind) = match motion {
        Left => {
            if cur.col == 0 {
                return None;
            }
            (Cursor::new(cur.line, cur.col.saturating_sub(count)), Exclusive)
        }
        Right => {
            let len = buffer.line_len(cur.line);
            if len == 0 || cur.col + 1 >= len {
                return None;
            }
            (Cursor::new(cur.line, (cur.col + count).min(len - 1)), Exclusive)
        }
        Up => {
            if cur.line == 0 {
                return None;
            }
            let line = cur.line.saturating_sub(count);
            let mut c = cur;
            c.line = line;
            c.col = cur.goal_col.min(buffer.line_len(line).saturating_sub(1));
            (c, Linewise)
        }
        Down => {
            if cur.line >= last_line {
                return None;
            }
            let line = (cur.line + count).min(last_line);
            let mut c = cur;
            c.line = line;
            c.col = cur.goal_col.min(buffer.line_len(line).saturating_sub(1));
            (c, Linewise)
        }

        LineStart => (Cursor::new(cur.line, 0), Exclusive),
        LineFirstNonBlank => {
            let text = buffer.line_text(cur.line);
            let col = text.chars().take_while(|c| c.is_whitespace()).count();
            (Cursor::new(cur.line, col), Exclusive)
        }
        LineEnd => {
            // `3$` is the end of the line two below, as in vim. The count
            // reached here and was dropped, so every `N$` acted as `$`.
            let line = (cur.line + count - 1).min(last_line);
            let len = buffer.line_len(line);
            (Cursor::new(line, len.saturating_sub(1)), Inclusive)
        }

        BufferStart => (Cursor::new(0, 0), Linewise),
        BufferEnd => (Cursor::new(last_line, 0), Linewise),
        GotoLine(n) => (Cursor::new(n.min(last_line), 0), Linewise),

        WordForward { big } => {
            let mut line = cur.line;
            let mut col = cur.col;
            for _ in 0..count {
                let (l, c) = word_forward(buffer, line, col, big, last_line);
                line = l;
                col = c;
            }
            (Cursor::new(line, col), Exclusive)
        }
        WordBack { big } => {
            let mut line = cur.line;
            let mut col = cur.col;
            for _ in 0..count {
                let (l, c) = word_back(buffer, line, col, big);
                line = l;
                col = c;
            }
            (Cursor::new(line, col), Exclusive)
        }

        WordEnd { big } => {
            let mut line = cur.line;
            let mut col = cur.col;
            for _ in 0..count {
                let (l, c) = word_end(buffer, line, col, big, last_line);
                line = l;
                col = c;
            }
            // Inclusive: `de` deletes through the last char of the word.
            (Cursor::new(line, col), Inclusive)
        }

        ParagraphForward => {
            let mut line = cur.line;
            for _ in 0..count {
                line = (line + 1).min(last_line);
                while line < last_line && !buffer.line_text(line).trim().is_empty() {
                    line += 1;
                }
            }
            (Cursor::new(line, 0), Linewise)
        }
        ParagraphBack => {
            let mut line = cur.line;
            for _ in 0..count {
                line = line.saturating_sub(1);
                while line > 0 && !buffer.line_text(line).trim().is_empty() {
                    line -= 1;
                }
            }
            (Cursor::new(line, 0), Linewise)
        }

        HalfPageDown => {
            let line = (cur.line + page / 2).min(last_line);
            (Cursor::new(line, cur.goal_col), Linewise)
        }
        HalfPageUp => {
            let line = cur.line.saturating_sub(page / 2);
            (Cursor::new(line, cur.goal_col), Linewise)
        }
        PageDown => {
            let line = (cur.line + page).min(last_line);
            (Cursor::new(line, cur.goal_col), Linewise)
        }
        PageUp => {
            let line = cur.line.saturating_sub(page);
            (Cursor::new(line, cur.goal_col), Linewise)
        }

        FindChar {
            target,
            forward,
            till,
        } => {
            let text: Vec<char> = buffer.line_text(cur.line).chars().collect();
            // `3fx` is the THIRD x, not the first: a count multiplies a find
            // the way it multiplies a word motion. `Pending` collects it and
            // `repeat_find` passes it on, and it was dropped here.
            let nth = count - 1;
            let found = if forward {
                (cur.col + 1..text.len())
                    .filter(|&i| text[i] == target)
                    .nth(nth)
            } else {
                (0..cur.col)
                    .rev()
                    .filter(|&i| text[i] == target)
                    .nth(nth)
            }?;
            let col = if till {
                if forward {
                    found.saturating_sub(1)
                } else {
                    found + 1
                }
            } else {
                found
            };
            (Cursor::new(cur.line, col), Inclusive)
        }

        // Deliberately unanswerable here: `H`/`M`/`L` are about the VIEWPORT,
        // which this module cannot see. `App::resolve_screen_motion` turns them
        // into a `GotoLine` before a motion is ever asked for one.
        ScreenTop | ScreenMiddle | ScreenBottom => return None,
    };

    let mut target = target;
    // Vertical motions keep the goal column; everything else resets it.
    match motion {
        Up | Down | HalfPageDown | HalfPageUp | PageDown | PageUp => {
            target.goal_col = cur.goal_col;
            target.col = target.col.min(buffer.line_len(target.line).saturating_sub(1));
        }
        _ => target.goal_col = target.col,
    }

    if target == cur {
        return None;
    }
    Some(MotionResult { target, kind })
}

fn word_forward(
    buffer: &Buffer,
    mut line: usize,
    mut col: usize,
    big: bool,
    last_line: usize,
) -> (usize, usize) {
    let chars: Vec<char> = buffer.line_text(line).chars().collect();
    if col >= chars.len() {
        if line < last_line {
            return (line + 1, 0);
        }
        return (line, col);
    }
    let start_class = if big {
        if chars[col].is_whitespace() {
            0
        } else {
            1
        }
    } else {
        char_class(chars[col])
    };

    // Step off the current run.
    while col < chars.len() {
        let cls = if big {
            if chars[col].is_whitespace() {
                0
            } else {
                1
            }
        } else {
            char_class(chars[col])
        };
        if cls != start_class {
            break;
        }
        col += 1;
    }
    // Then skip whitespace.
    while col < chars.len() && chars[col].is_whitespace() {
        col += 1;
    }

    if col >= chars.len() {
        if line < last_line {
            line += 1;
            let next: Vec<char> = buffer.line_text(line).chars().collect();
            col = next.iter().take_while(|c| c.is_whitespace()).count();
            col = col.min(next.len().saturating_sub(1));
        } else {
            col = chars.len().saturating_sub(1);
        }
    }
    (line, col)
}

fn class_of(c: char, big: bool) -> u8 {
    if big {
        if c.is_whitespace() {
            0
        } else {
            1
        }
    } else {
        char_class(c)
    }
}

/// `e` / `E`: land on the last character of the current or next word.
fn word_end(
    buffer: &Buffer,
    mut line: usize,
    mut col: usize,
    big: bool,
    last_line: usize,
) -> (usize, usize) {
    let mut chars: Vec<char> = buffer.line_text(line).chars().collect();

    // Step one char forward, crossing a line boundary if needed.
    if col + 1 < chars.len() {
        col += 1;
    } else if line < last_line {
        line += 1;
        col = 0;
        chars = buffer.line_text(line).chars().collect();
    } else {
        return (line, chars.len().saturating_sub(1));
    }

    // Skip whitespace (and blank lines) to the start of the next word.
    loop {
        while col < chars.len() && chars[col].is_whitespace() {
            col += 1;
        }
        if col < chars.len() {
            break;
        }
        if line < last_line {
            line += 1;
            col = 0;
            chars = buffer.line_text(line).chars().collect();
        } else {
            return (line, chars.len().saturating_sub(1));
        }
    }

    // Run to the last char of this word.
    let cls = class_of(chars[col], big);
    while col + 1 < chars.len() && class_of(chars[col + 1], big) == cls {
        col += 1;
    }
    (line, col)
}

fn word_back(buffer: &Buffer, mut line: usize, mut col: usize, big: bool) -> (usize, usize) {
    if col == 0 {
        if line == 0 {
            return (0, 0);
        }
        line -= 1;
        col = buffer.line_len(line);
    }
    let chars: Vec<char> = buffer.line_text(line).chars().collect();
    if chars.is_empty() {
        return (line, 0);
    }
    col = col.min(chars.len());

    // Skip whitespace to the left.
    while col > 0 && chars[col - 1].is_whitespace() {
        col -= 1;
    }
    if col == 0 {
        return (line, 0);
    }
    let cls = if big {
        1
    } else {
        char_class(chars[col - 1])
    };
    while col > 0 {
        let c = chars[col - 1];
        let this = if big {
            if c.is_whitespace() {
                0
            } else {
                1
            }
        } else {
            char_class(c)
        };
        if this != cls {
            break;
        }
        col -= 1;
    }
    (line, col)
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

    /// Resolve and report `(line, col)`, or `None` when the motion cannot move.
    fn go(b: &Buffer, m: Motion, count: usize) -> Option<(usize, usize)> {
        resolve(b, m, count, 10).map(|r| (r.target.line, r.target.col))
    }

    fn kind(b: &Buffer, m: Motion) -> MotionKind {
        resolve(b, m, 1, 10).expect("a motion").kind
    }

    #[test]
    fn word_motions_cross_lines_and_classes() {
        let b = buf("foo bar.baz\nnext line\n", 0, 0);
        assert_eq!(go(&b, Motion::WordForward { big: false }, 1), Some((0, 4)));
        // `.` is its own class, so `w` stops on it; `W` runs past it.
        assert_eq!(go(&b, Motion::WordForward { big: false }, 2), Some((0, 7)));
        // `W` ignores the `.`, so `bar.baz` is one big word.
        assert_eq!(go(&b, Motion::WordForward { big: true }, 1), Some((0, 4)));
        assert_eq!(go(&b, Motion::WordForward { big: true }, 2), Some((1, 0)));

        let b = buf("foo bar\nnext\n", 0, 5);
        assert_eq!(go(&b, Motion::WordBack { big: false }, 1), Some((0, 4)));
        assert_eq!(go(&b, Motion::WordBack { big: false }, 2), Some((0, 0)));

        // `e` lands ON the last character, which is why it is inclusive.
        let b = buf("foo bar\n", 0, 0);
        assert_eq!(go(&b, Motion::WordEnd { big: false }, 1), Some((0, 2)));
        assert_eq!(kind(&b, Motion::WordEnd { big: false }), MotionKind::Inclusive);
    }

    #[test]
    fn line_motions_land_where_vim_puts_them() {
        let b = buf("    indented text\n", 0, 10);
        assert_eq!(go(&b, Motion::LineStart, 1), Some((0, 0)));
        assert_eq!(go(&b, Motion::LineFirstNonBlank, 1), Some((0, 4)));
        // `$` is the last character, not one past it.
        assert_eq!(go(&b, Motion::LineEnd, 1), Some((0, 16)));
        assert_eq!(kind(&b, Motion::LineEnd), MotionKind::Inclusive);

        // An empty line has nowhere to go.
        let b = buf("\nx\n", 0, 0);
        assert_eq!(go(&b, Motion::LineEnd, 1), None);
    }

    /// A count multiplies a find: `3fx` is the THIRD x on the line.
    #[test]
    fn a_count_multiplies_a_character_find() {
        let b = buf("axbxcxd\n", 0, 0);
        let f = |n| {
            go(
                &b,
                Motion::FindChar { target: 'x', forward: true, till: false },
                n,
            )
        };
        assert_eq!(f(1), Some((0, 1)));
        assert_eq!(f(2), Some((0, 3)));
        assert_eq!(f(3), Some((0, 5)));
        assert_eq!(f(4), None, "there is no fourth x");

        // Backwards, and `t` stopping one short in each direction.
        let b = buf("axbxcxd\n", 0, 6);
        let back = |n, till| {
            go(
                &b,
                Motion::FindChar { target: 'x', forward: false, till },
                n,
            )
        };
        assert_eq!(back(1, false), Some((0, 5)));
        assert_eq!(back(2, false), Some((0, 3)));
        assert_eq!(back(2, true), Some((0, 4)), "`T` stops one past it");

        let b = buf("axbxcxd\n", 0, 0);
        assert_eq!(
            go(&b, Motion::FindChar { target: 'x', forward: true, till: true }, 2),
            Some((0, 2)),
            "`2tx` stops before the second x"
        );
    }

    /// A count multiplies `$` too: `3$` is the end of the second line below.
    #[test]
    fn a_count_multiplies_the_end_of_line() {
        let b = buf("one\ntwo two\nthree\n", 0, 0);
        assert_eq!(go(&b, Motion::LineEnd, 1), Some((0, 2)));
        assert_eq!(go(&b, Motion::LineEnd, 2), Some((1, 6)));
        // Clamped to the last line — which, for a document ending in a
        // newline, is the empty one after `three`.
        assert_eq!(go(&b, Motion::LineEnd, 9), Some((3, 0)));
        assert_eq!(go(&b, Motion::LineEnd, 3), Some((2, 4)));
    }

    #[test]
    fn vertical_motions_keep_the_goal_column() {
        let b = buf("a long first line\nshort\nanother long line\n", 0, 15);
        // Down onto a short line clamps, but the GOAL is remembered.
        let r = resolve(&b, Motion::Down, 1, 10).unwrap();
        assert_eq!((r.target.line, r.target.col), (1, 4));
        assert_eq!(r.target.goal_col, 15, "the goal survives the clamp");
        assert_eq!(r.kind, MotionKind::Linewise);

        // Two lines down it can be honoured again.
        let mut b2 = buf("a long first line\nshort\nanother long line\n", 1, 4);
        b2.cursor.goal_col = 15;
        assert_eq!(go(&b2, Motion::Down, 1), Some((2, 15)));
    }

    #[test]
    fn edges_return_none_rather_than_moving_nowhere() {
        let b = buf("one\ntwo\n", 0, 0);
        assert_eq!(go(&b, Motion::Up, 1), None, "no line above the first");
        assert_eq!(go(&b, Motion::Left, 1), None);
        assert_eq!(go(&b, Motion::LineStart, 1), None, "already there");

        // A document ending in a newline has an empty last line to sit on —
        // ropey's model, and where an append would land — so there IS a line
        // below `two`. Line 2 is that one, and nothing is below it.
        let b = buf("one\ntwo\n", 1, 2);
        assert_eq!(go(&b, Motion::Down, 1), Some((2, 0)));
        assert_eq!(go(&b, Motion::Right, 1), None, "col 2 is the last char of `two`");
        let b = buf("one\ntwo\n", 2, 0);
        assert_eq!(go(&b, Motion::Down, 1), None, "nothing below the last line");

        // `H`/`M`/`L` need a viewport this module cannot see.
        assert_eq!(go(&b, Motion::ScreenTop, 1), None);
    }

    #[test]
    fn buffer_and_paragraph_motions_are_linewise() {
        let b = buf("one\n\ntwo\nthree\n\nfour\n", 0, 0);
        assert_eq!(kind(&b, Motion::BufferEnd), MotionKind::Linewise);
        assert_eq!(go(&b, Motion::ParagraphForward, 1), Some((1, 0)));
        assert_eq!(go(&b, Motion::ParagraphForward, 2), Some((4, 0)));

        let b = buf("one\n\ntwo\nthree\n\nfour\n", 5, 0);
        assert_eq!(go(&b, Motion::ParagraphBack, 1), Some((4, 0)));
        assert_eq!(go(&b, Motion::GotoLine(2), 1), Some((2, 0)));
        assert_eq!(go(&b, Motion::GotoLine(999), 1), Some((6, 0)), "clamped");
    }

    #[test]
    fn char_classes_separate_words_punctuation_and_space() {
        assert_eq!(char_class(' '), 0);
        assert_eq!(char_class('\t'), 0);
        assert_eq!(char_class('a'), 1);
        assert_eq!(char_class('_'), 1);
        assert_eq!(char_class('7'), 1);
        assert_eq!(char_class('é'), 1, "letters outside ASCII are still letters");
        assert_eq!(char_class('.'), 2);
    }
}
