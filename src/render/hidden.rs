//! Lines that take no screen rows.
//!
//! Every line used to take at least one row. A closed fold breaks that: its
//! first line stands for the rest, which are still in the text but drawn
//! nowhere. The row index (`RenderCache::index`) reads a `Hidden` set and gives
//! each line in it zero rows, so drawing, scrolling, `locate` and mouse
//! hit-testing all skip it without knowing why it is hidden.
//!
//! The set is built by `of` from every source of hidden lines. Folds are the
//! only one today; a laid-out `$$…$$` block (#77) would add the lines after its
//! opening `$$` while the cursor is outside it, and draw the layout as rows of
//! the opening line, the way an embed draws its target.

use std::ops::Range;

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

/// Every line that takes no rows this frame: today, the lines closed folds
/// hide.
pub fn of(folds: &Folds) -> Hidden {
    Hidden::from_ranges(folds.ranges())
}

#[cfg(test)]
mod tests {
    use super::*;

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
