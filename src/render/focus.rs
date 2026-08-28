//! Focus mode. SPEC.md §6.
//!
//! Non-focused text is drawn at `theme.text_dim`. Bounds are recomputed only
//! when the cursor leaves the current region — not every frame.

use crate::text::buffer::Buffer;
use crate::text::cursor::Cursor;
use crate::text::object;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusMode {
    Off,
    Paragraph,
    Sentence,
}

/// The region kept at full brightness, as (line, col) bounds.
pub struct FocusRegion {
    pub start: Cursor,
    pub end: Cursor,
    /// Buffer revision this was computed for; a change re-derives it.
    pub revision: u64,
}

impl FocusRegion {
    /// The region kept bright for the cursor's current position, or `None` in
    /// `Off`. Paragraph = the contiguous non-blank block; Sentence = the run
    /// between sentence terminators, clamped to the paragraph.
    pub fn compute(buffer: &Buffer, mode: FocusMode) -> Option<FocusRegion> {
        if mode == FocusMode::Off {
            return None;
        }
        let cursor = buffer.cursor;
        let on_blank = buffer.line_text(cursor.line).trim().is_empty();

        // Sitting on a blank line between paragraphs must NOT dim the whole
        // document. Anchor to the nearest paragraph instead, and always keep the
        // cursor line inside the bright region.
        let (mut start, mut end) = if on_blank || mode == FocusMode::Paragraph {
            let anchor = nearest_paragraph_line(buffer, cursor.line);
            let (s, e) = object::paragraph_bounds(buffer, anchor);
            (Cursor::new(s, 0), Cursor::new(e, buffer.line_len(e)))
        } else {
            let (s, e) = object::sentence_bounds(buffer, cursor);
            (cursor_of(buffer, s), cursor_of(buffer, e))
        };

        if (cursor.line, cursor.col) < (start.line, start.col) {
            start = Cursor::new(cursor.line, 0);
        }
        if (cursor.line, cursor.col) > (end.line, end.col) {
            end = cursor;
        }

        Some(FocusRegion {
            start,
            end,
            revision: buffer.revision,
        })
    }

    /// Cheap check to avoid recomputing every frame: still current if the buffer
    /// hasn't changed and the cursor is still inside the region.
    pub fn still_valid(&self, buffer: &Buffer) -> bool {
        self.revision == buffer.revision && self.contains(buffer.cursor)
    }

    pub fn contains(&self, c: Cursor) -> bool {
        (c.line, c.col) >= (self.start.line, self.start.col)
            && (c.line, c.col) <= (self.end.line, self.end.col)
    }
}

/// Resolve `line` to a non-blank line to anchor focus on: itself if non-blank,
/// otherwise the nearest paragraph below, then above, else `line` (empty doc).
fn nearest_paragraph_line(buffer: &Buffer, line: usize) -> usize {
    let blank = |l: usize| buffer.line_text(l).trim().is_empty();
    if !blank(line) {
        return line;
    }
    let last = buffer.line_count().saturating_sub(1);
    if let Some(l) = (line + 1..=last).find(|&l| !blank(l)) {
        return l;
    }
    (0..line).rev().find(|&l| !blank(l)).unwrap_or(line)
}

fn cursor_of(buffer: &Buffer, idx: usize) -> Cursor {
    let idx = idx.min(buffer.rope.len_chars());
    let line = buffer.rope.char_to_line(idx);
    let col = idx - buffer.rope.line_to_char(line);
    Cursor::new(line, col)
}

impl FocusMode {
    pub fn next(self) -> Self {
        match self {
            FocusMode::Off => FocusMode::Paragraph,
            FocusMode::Paragraph => FocusMode::Sentence,
            FocusMode::Sentence => FocusMode::Off,
        }
    }

    /// `None` for a word this does not know, so a typo in `layout.focus` is
    /// reported rather than silently read as `off`.
    pub fn parse(s: &str) -> Option<FocusMode> {
        Some(match s.trim() {
            "off" | "none" => FocusMode::Off,
            "paragraph" | "para" => FocusMode::Paragraph,
            "sentence" | "sent" => FocusMode::Sentence,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            FocusMode::Off => "off",
            FocusMode::Paragraph => "paragraph",
            FocusMode::Sentence => "sentence",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::cursor::Cursor;

    fn buf(text: &str, line: usize, col: usize) -> Buffer {
        let mut b = Buffer::empty();
        b.insert_str(Cursor::new(0, 0), text);
        b.cursor = Cursor::new(line, col);
        b
    }

    #[test]
    fn paragraph_region_spans_the_block() {
        let b = buf("a\nb\nc\n\nd\n", 1, 0); // cursor in first paragraph (a,b,c)
        let r = FocusRegion::compute(&b, FocusMode::Paragraph).unwrap();
        assert_eq!(r.start.line, 0);
        assert_eq!(r.end.line, 2);
        assert!(r.contains(Cursor::new(2, 0)));
        assert!(!r.contains(Cursor::new(4, 0)));
    }

    #[test]
    fn paragraph_region_survives_moves_within_it() {
        let mut b = buf("one two\nthree\n\nfar\n", 0, 0);
        let r = FocusRegion::compute(&b, FocusMode::Paragraph).unwrap();
        b.cursor = Cursor::new(1, 2); // still in the same paragraph
        assert!(r.still_valid(&b));
        b.cursor = Cursor::new(3, 0); // crossed the blank line
        assert!(!r.still_valid(&b));
    }

    #[test]
    fn blank_line_attaches_to_the_next_paragraph_not_everything() {
        // Cursor on the blank line 1, between a heading and a paragraph below.
        let b = buf("# Title\n\nbody one\nbody two\n\nfar\n", 1, 0);
        let r = FocusRegion::compute(&b, FocusMode::Paragraph).unwrap();
        // The region covers the blank line plus the paragraph below it — NOT
        // just the (empty) blank line, which would dim the whole document.
        assert!(r.contains(Cursor::new(1, 0)), "cursor line stays bright");
        assert!(r.contains(Cursor::new(2, 0)), "adjacent paragraph is bright");
        assert!(r.contains(Cursor::new(3, 0)));
        assert!(!r.contains(Cursor::new(5, 0)), "distant paragraph is dimmed");
    }

    #[test]
    fn sentence_region_stops_at_terminators() {
        // "Hello world. Second one." — cursor in the first sentence.
        let b = buf("Hello world. Second one.\n", 0, 3);
        let r = FocusRegion::compute(&b, FocusMode::Sentence).unwrap();
        assert_eq!(r.start.col, 0);
        assert_eq!(r.end.col, 12); // through the first '.'
    }
}
