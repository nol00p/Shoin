//! Lines that take no screen rows.
//!
//! Every line used to take at least one row. A closed fold breaks that: its
//! first line stands for the rest, which are still in the text but drawn
//! nowhere. The row index (`RenderCache::index`) reads a `Hidden` set and gives
//! each line in it zero rows, so drawing, scrolling, `locate` and mouse
//! hit-testing all skip it without knowing why it is hidden.
//!
//! The set is built by `of` from every source of hidden lines: closed folds,
//! and display math blocks laid out over several rows (#77). A laid-out block
//! draws its layout as rows of its opening `$$` line, the way an embed draws
//! its target, and the lines after it take zero rows. `math_blocks` and
//! `math_block_at` find the blocks that can be laid out: the CLOSED ones.

use std::ops::Range;

use crate::render::markdown::block::{classify, BlockCache, BlockKind, Open};
use crate::text::buffer::Buffer;
use crate::text::fold::Folds;

/// Line ranges that take zero rows: sorted, disjoint, never touching.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hidden {
    ranges: Vec<Range<usize>>,
}

impl Hidden {
    /// The union of `ranges`, in any order, overlapping or not. Empty ranges
    /// are dropped.
    pub fn from_ranges(ranges: impl IntoIterator<Item = Range<usize>>) -> Hidden {
        let mut all: Vec<Range<usize>> = ranges.into_iter().filter(|r| r.start < r.end).collect();
        all.sort_by_key(|r| r.start);
        let mut out: Vec<Range<usize>> = Vec::with_capacity(all.len());
        for r in all {
            match out.last_mut() {
                // Touching runs merge too: a run is then always followed by a
                // line that is shown, which is what `run_after` relies on.
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => out.push(r),
            }
        }
        Hidden { ranges: out }
    }

    /// The runs, top to bottom.
    pub fn ranges(&self) -> &[Range<usize>] {
        &self.ranges
    }

    /// The run holding `line`, if it is hidden.
    pub fn run(&self, line: usize) -> Option<Range<usize>> {
        let i = self.ranges.partition_point(|r| r.end <= line);
        self.ranges.get(i).filter(|r| r.start <= line).cloned()
    }

    /// The line shown in place of `line`: itself when it is shown, else the
    /// line above its run (the fold's first line). A run starting at line 0
    /// has nothing above it, and answers with the line below it instead.
    pub fn shown(&self, line: usize) -> usize {
        match self.run(line) {
            Some(r) if r.start > 0 => r.start - 1,
            Some(r) => r.end,
            None => line,
        }
    }

    /// How many lines of `lines` are hidden.
    pub fn count_in(&self, lines: Range<usize>) -> usize {
        let from = self.ranges.partition_point(|r| r.end <= lines.start);
        self.ranges[from..]
            .iter()
            .take_while(|r| r.start < lines.end)
            .map(|r| r.end.min(lines.end) - r.start.max(lines.start))
            .sum()
    }

    /// `a..=b` grown to whole runs: from the line shown for `a` to the last
    /// line the one shown for `b` stands for.
    pub fn widen(&self, a: usize, b: usize) -> (usize, usize) {
        (self.shown(a), self.last_of(self.shown(b)))
    }

    /// The last line `line` stands for on screen: the end of the run right
    /// after it, or `line` itself when the next line is shown.
    pub fn last_of(&self, line: usize) -> usize {
        match self.run(line + 1) {
            Some(r) => r.end - 1,
            None => line,
        }
    }
}

/// Every line that takes no rows this frame: the lines closed folds hide, and
/// the lines after the opening line of each display block in `laid_out` (whole
/// blocks, as `math_blocks` gives them). Fold motions pass no blocks: `j`, `dd`
/// and `zc` see folds only.
pub fn of(folds: &Folds, laid_out: &[Range<usize>]) -> Hidden {
    Hidden::from_ranges(folds.ranges().chain(laid_out.iter().map(|b| b.start + 1..b.end)))
}

/// Every closed display math block in the document, top to bottom, as whole
/// line ranges: the opening `$$` line through the line that closes it. A
/// one-line `$$…$$` is a block of one line. A block left open (bounded by a
/// blank line or the end of the file) is not one: it keeps its per-line form.
///
/// Read off the block kinds and the carry state the classifier already
/// cached, so it costs a walk over the kinds and reads no text, except the last
/// line's when a block reaches it.
pub fn math_blocks(blocks: &BlockCache, buffer: &Buffer) -> Vec<Range<usize>> {
    let n = blocks.kinds.len().min(blocks.carry.len()).min(buffer.line_count());
    let mut out = Vec::new();
    let mut line = 0;
    while line < n {
        if opens_math(blocks, line) {
            match block_from(blocks, buffer, line, n) {
                Ok(b) => {
                    line = b.end;
                    out.push(b);
                }
                Err(past) => line = past,
            }
        } else {
            line += 1;
        }
    }
    out
}

/// The closed display math block holding `line`, if there is one.
pub fn math_block_at(blocks: &BlockCache, buffer: &Buffer, line: usize) -> Option<Range<usize>> {
    let n = blocks.kinds.len().min(blocks.carry.len()).min(buffer.line_count());
    if line >= n || blocks.kinds[line] != BlockKind::MathBlock {
        return None;
    }
    // A line the block entering it is still open in is not the opener.
    let mut start = line;
    while start > 0 && in_open_math(blocks, start) {
        start -= 1;
    }
    let b = block_from(blocks, buffer, start, n).ok()?;
    b.contains(&line).then_some(b)
}

/// Whether `line` opens a display block: a math line no block is open into.
fn opens_math(blocks: &BlockCache, line: usize) -> bool {
    blocks.kinds[line] == BlockKind::MathBlock && !in_open_math(blocks, line)
}

/// Whether a display block is open entering `line`.
fn in_open_math(blocks: &BlockCache, line: usize) -> bool {
    matches!(blocks.carry[line].open, Open::InMath)
}

/// Whether a display block is still open leaving `line`. The carry entering
/// the next line says; the last line has none, so it is classified again.
fn leaves_open(blocks: &BlockCache, buffer: &Buffer, line: usize, n: usize) -> bool {
    if line + 1 < n {
        return in_open_math(blocks, line + 1);
    }
    let (_, next) = classify(&buffer.line_text(line), &blocks.carry[line], line == 0, blocks.math, blocks.tab_width);
    matches!(next.open, Open::InMath)
}

/// The block opened at `start`: `Ok` with its lines when a line closes it,
/// `Err` with the first line past it when nothing does.
fn block_from(blocks: &BlockCache, buffer: &Buffer, start: usize, n: usize) -> Result<Range<usize>, usize> {
    let mut line = start;
    loop {
        if !leaves_open(blocks, buffer, line, n) {
            return Ok(start..line + 1);
        }
        line += 1;
        // Still open where the next line is not math (the blank line that
        // bounds it) or at the end of the file: unclosed.
        if line >= n || blocks.kinds[line] != BlockKind::MathBlock {
            return Err(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(text: &str) -> (Vec<Range<usize>>, BlockCache, Buffer) {
        let mut b = Buffer::empty();
        b.insert_str(crate::text::cursor::Cursor::new(0, 0), text);
        let c = BlockCache::build(&b, true, 4);
        (math_blocks(&c, &b), c, b)
    }

    /// Closed blocks, one-line blocks and blocks that run into each other are
    /// found; an unclosed one (bounded by a blank line, or the end of the
    /// file) is not.
    #[test]
    fn closed_math_blocks_are_found_and_unclosed_ones_are_not() {
        let (found, c, b) = blocks("p\n$$\nx\n$$\n$$y$$\n$$ a\nb $$\n\n$$\nopen\n\n- i\n  $$\n  z\n  $$\n$$\nlast");
        assert_eq!(found, vec![1..4, 4..5, 5..7, 12..15]);
        assert_eq!(math_block_at(&c, &b, 2), Some(1..4));
        assert_eq!(math_block_at(&c, &b, 3), Some(1..4));
        assert_eq!(math_block_at(&c, &b, 4), Some(4..5));
        assert_eq!(math_block_at(&c, &b, 6), Some(5..7));
        assert_eq!(math_block_at(&c, &b, 9), None, "unclosed");
        assert_eq!(math_block_at(&c, &b, 0), None);
        assert_eq!(math_block_at(&c, &b, 16), None, "open at the end of the file");
        // A block closed on the file's last line, with no newline after it.
        let (found, ..) = blocks("$$\nx\n$$");
        assert_eq!(found, vec![0..3]);
        let (found, ..) = blocks("$$\nx");
        assert!(found.is_empty());
    }

    #[test]
    fn laid_out_blocks_hide_all_but_their_opening_line() {
        let h = of(&Folds::default(), &[2..5, 7..8]);
        assert_eq!(h.run(3), Some(3..5));
        assert_eq!(h.ranges().len(), 1);
        assert_eq!(h.shown(4), 2);
        assert_eq!(h.last_of(2), 4);
    }

    #[test]
    fn ranges_merge_and_answer_by_line() {
        let h = Hidden::from_ranges([5..8, 2..4, 3..5, 10..10, 12..13]);
        assert_eq!(h.ranges(), &[2..8, 12..13]);
        assert!(h.run(1).is_none());
        assert!(h.run(2).is_some() && h.run(7).is_some());
        assert!(h.run(8).is_none() && h.run(11).is_none());
        assert_eq!(h.run(12), Some(12..13));
        assert_eq!(h.shown(4), 1);
        assert_eq!(h.shown(9), 9);
        assert_eq!(h.last_of(1), 7);
        assert_eq!(h.last_of(8), 8);
        assert_eq!(Hidden::from_ranges(std::iter::once(0..2)).shown(1), 2);
        assert_eq!(h.widen(1, 1), (1, 7));
        assert_eq!(h.count_in(0..20), 7);
        assert_eq!(h.count_in(5..13), 4);
        assert_eq!(h.count_in(8..12), 0);
        assert_eq!(h.widen(5, 9), (1, 9));
        assert_eq!(h.widen(9, 11), (9, 12));
    }
}
