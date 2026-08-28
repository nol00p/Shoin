//! Render layer. Pure: takes `&Buffer` + `&Theme`, returns frame content.
//! Holds no mutable state except caches keyed by `Buffer::revision`.
//! SPEC.md §3, §9.

pub mod cache;
pub mod conceal;
pub mod focus;
pub mod frame;
pub mod indent;
pub mod layout;
pub mod pane;
pub mod splash;
pub mod markdown;
pub mod theme;

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
/// It carries a POSITION, not pixels: the bytes stay in the render cache, and
/// the painter looks them up by `line`/`index`. A frame with three big photos
/// on it would otherwise clone three photos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    /// The rope line holding the `![[…]]`, and which row of its expansion.
    pub line: usize,
    pub index: usize,
}

impl Placement {
    /// Whether the whole box is on screen.
    ///
    /// A picture that does not fit is not drawn AT ALL. Neither graphics
    /// protocol clips: a box running off the bottom is painted over whatever
    /// is down there, and iTerm2 scrolls the screen to make room for it, which
    /// shifts every row above. Scrolling a picture in from the bottom edge is
    /// exactly when that happens — so it waits until it fits.
    ///
    /// Running off the TOP needs no guard here: the frame only records a
    /// placement when the picture's first row is itself on screen.
    pub fn fits_in(&self, term: (u16, u16)) -> bool {
        let (cols, rows) = term;
        self.y + self.rows <= rows && self.x + self.cols <= cols
    }

    /// Whether this picture shares any cell with `r` — a spotlight box, which
    /// was summoned to be read and therefore outranks it.
    pub fn overlaps(&self, r: ratatui::layout::Rect) -> bool {
        self.x < r.x + r.width
            && r.x < self.x + self.cols
            && self.y < r.y + r.height
            && r.y < self.y + self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::Placement;

    fn at(x: u16, y: u16, cols: u16, rows: u16) -> Placement {
        Placement { x, y, cols, rows, line: 0, index: 0 }
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
}
