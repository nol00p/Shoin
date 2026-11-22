//! Cursor and selection.
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
    /// [`Cursor::LINE_END`] after `$`: the end of every line.
    pub goal_col: usize,

    /// The same goal as a SCREEN cell, once a vertical motion has measured it:
    /// `j` keeps the cell, not the char offset, so it lands under the char it
    /// left from across a tab or a wide glyph. `None` until then, meaning
    /// `goal_col` was set on this line and is measured here
    /// (`motion::resolve`).
    pub goal_cells: Option<usize>,
}

impl Cursor {
    /// The goal `$` leaves, Neovim's `curswant` of MAXCOL: `j`, `k`, `G`
    /// and the other motions that keep the goal land on the last character
    /// of each line, however long, until a horizontal motion sets a column.
    /// As a screen cell it is past every line's end, so the clamp to the
    /// last character is all it takes.
    pub const LINE_END: usize = usize::MAX;

    pub fn new(line: usize, col: usize) -> Self {
        Cursor {
            line,
            col,
            goal_col: col,
            goal_cells: None,
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
        self.goal_cells = None;
    }
}
