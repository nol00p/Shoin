//! Render layer. Pure: takes `&Buffer` + `&Theme`, returns frame content.
//! Holds no mutable state except caches keyed by `Buffer::revision`.

pub mod align;
pub mod cache;
pub mod conceal;
pub mod focus;
pub mod frame;
pub mod hidden;
pub mod hints;
pub mod indent;
pub mod layout;
pub mod math;
pub(crate) mod mermaid;
pub mod numbers;
pub mod pane;
pub mod print;
pub mod quote;
pub mod splash;
pub mod markdown;
pub mod theme;
#[cfg(test)]
mod indexed_tests;
#[cfg(test)]
mod perf_long_line;

use std::ops::Range;

use theme::Style;

/// A styled run of SOURCE characters. `range` is a char range within its line.
///
/// Spans cover exactly the line's characters, in order, without overlap —
/// asserted in tests and fuzzed. What actually reaches the frame is this set
/// filtered through the line's `ConcealMap` (empty for active lines).
#[derive(Clone, Debug, PartialEq)]
pub struct StyledSpan {
    pub range: Range<usize>,
    pub style: Style,
}

/// Where a picture goes on screen, recorded while the frame is built and read
/// back afterwards by the painter.
///
/// It carries a position, with the picture bytes registered once by ID.
///
/// A box cut off by the top or bottom of the text area is recorded too, at
/// its first VISIBLE row: `y` is that row, `skip` how many of the box's rows
/// are above it, and `shown` how many are on screen from there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: u16,
    pub y: u16,
    /// The whole box, in cells, cut off or not.
    pub cols: u16,
    pub rows: u16,
    /// Rows of the box above the screen, and rows of it on screen.
    pub skip: u16,
    pub shown: u16,
    /// The source line and the first row of its preview holding the picture.
    pub line: usize,
    pub index: usize,
    /// The picture's `image::kitty_id`, so the painter knows which pictures a
    /// frame still shows without the cache that held them.
    pub id: u32,
}

impl Placement {
    /// Whether every row of the box is on screen.
    pub fn whole(&self) -> bool {
        self.skip == 0 && self.shown == self.rows
    }

    /// Whether the visible part of the box is inside the terminal.
    ///
    /// A picture cut off at the SIDE is not drawn at all, nor is one whose
    /// rows run past the terminal's last row: neither protocol clips, and
    /// iTerm2 scrolls the screen to make room, which shifts every row above.
    /// The frame already cuts `shown` at the bottom of the text area, so the
    /// second case is a guard, not the rule.
    pub fn fits_in(&self, term: (u16, u16)) -> bool {
        let (cols, rows) = term;
        self.y + self.shown <= rows && self.x + self.cols <= cols
    }

    /// Whether this picture shares any cell with `r` — a spotlight box, which
    /// was summoned to be read and therefore outranks it. Only the rows on
    /// screen count: a box cut off at the bottom does not reach past it.
    pub fn overlaps(&self, r: ratatui::layout::Rect) -> bool {
        self.x < r.x + r.width
            && r.x < self.x + self.cols
            && self.y < r.y + r.height
            && r.y < self.y + self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::Placement;

    fn at(x: u16, y: u16, cols: u16, rows: u16) -> Placement {
        Placement { x, y, cols, rows, skip: 0, shown: rows, line: 0, index: 0, id: 1 }
    }

    /// A spotlight box is summoned to be read, so a picture under it stands
    /// down — otherwise the overlay is behind the thing it was opened over.
    #[test]
    fn a_picture_yields_to_an_overlay_it_touches() {
        use ratatui::layout::Rect;
        let pic = at(10, 5, 40, 10); // columns 10..50, rows 5..15
        assert!(pic.overlaps(Rect { x: 20, y: 8, width: 30, height: 3 }), "straddling it");
        assert!(pic.overlaps(Rect { x: 0, y: 0, width: 80, height: 24 }), "covering it");
        assert!(pic.overlaps(Rect { x: 49, y: 14, width: 4, height: 3 }), "one shared cell");

        assert!(!pic.overlaps(Rect { x: 50, y: 5, width: 10, height: 10 }), "just to the right");
        assert!(!pic.overlaps(Rect { x: 10, y: 15, width: 40, height: 3 }), "just below");
        assert!(!pic.overlaps(Rect { x: 10, y: 0, width: 40, height: 5 }), "just above");

        // Cut off after three rows: what lies below those is not its.
        let cut = Placement { shown: 3, ..pic };
        assert!(!cut.overlaps(Rect { x: 20, y: 8, width: 30, height: 3 }), "past its last row");
        assert!(cut.overlaps(Rect { x: 20, y: 7, width: 30, height: 3 }));
    }

    #[test]
    fn a_picture_that_would_run_off_the_screen_does_not_fit() {
        let term = (80, 24);
        assert!(at(0, 0, 80, 24).fits_in(term), "exactly filling it is fitting");
        assert!(at(4, 4, 60, 18).fits_in(term));

        assert!(!at(4, 20, 60, 5).fits_in(term), "one row past the bottom");
        assert!(!at(40, 4, 60, 5).fits_in(term), "and past the right edge");
        // The bottom edge is the case that matters: iTerm2 SCROLLS to make
        // room, which moves every row above the picture.
        assert!(!at(0, 23, 10, 2).fits_in(term));
    }

    /// A box cut off at the top or bottom is not whole, but its visible part
    /// fits; one cut off at the side still does not fit.
    #[test]
    fn a_cut_off_picture_fits_by_its_visible_rows() {
        let term = (80, 24);
        let top = Placement { skip: 4, shown: 6, ..at(0, 0, 60, 10) };
        assert!(!top.whole() && top.fits_in(term));
        let bottom = Placement { shown: 3, ..at(0, 21, 60, 10) };
        assert!(!bottom.whole() && bottom.fits_in(term));
        assert!(at(0, 0, 60, 10).whole());
        assert!(!Placement { shown: 3, ..at(40, 21, 60, 10) }.fits_in(term), "cut at the side");
    }
}
