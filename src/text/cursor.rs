//! Cursor and selection. SPEC.md §4.
//!
//! `col` is a CHAR offset within the line — not a byte offset, not a display
//! column. Display width is a render concern, computed on demand for visible
//! lines only.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub line: usize,
    pub col: usize,

    /// Remembered column for vertical motion, so `j` over a short line and back
    /// returns to the original column. Reset by any horizontal motion or edit.
    pub goal_col: usize,
}

impl Cursor {
    pub fn new(line: usize, col: usize) -> Self {
        Cursor {
            line,
            col,
            goal_col: col,
        }
    }

    /// Clamp into the line. In Normal mode the cursor sits ON a character, so
    /// max col is `len - 1`; in Insert it may sit past the end at `len`.
    pub fn clamp(&mut self, line_len: usize, past_end: bool) {
        let max = if past_end {
            line_len
        } else {
            line_len.saturating_sub(1)
        };
        if self.col > max {
            self.col = max;
        }
    }

    /// Set column and sync the goal column. Use for horizontal motion and edits.
    pub fn set_col(&mut self, col: usize) {
        self.col = col;
        self.goal_col = col;
    }
}
