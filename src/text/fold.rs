//! Folds: a list item with its children, or a heading's section, shown as its
//! first line.
//!
//! What can fold is read off the text each time (`candidates`, `all`); what
//! IS folded is a `Folds`, one per pane and document (vim's are per window),
//! holding the closed ones. They live
//! for the session only and never touch the text: a fold is a pair of line
//! numbers, END INCLUSIVE like every range in `list`.
//!
//! A fold follows the edits around it the way a mark would. `Folds::sync`
//! finds the lines an edit touched by comparing the text with the one the
//! folds were last placed in (`shared_ends`, as the render cache does), moves
//! the folds below them, and drops a fold whose first line went. It then
//! measures every fold again from its first line, so a fold whose item gained
//! a child grows with it, and one whose heading stopped being a heading goes.
//!
//! Like `list`, this reads the block cache's line kinds rather than parse
//! headings and list markers again.

use std::ops::Range;

use ropey::Rope;

use super::buffer::{shared_ends, Buffer};
use super::list::Lists;
use crate::render::markdown::block::BlockKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FoldKind {
    /// A list item and its children: `Lists::item_bounds`.
    Item,
    /// A heading and everything to the next heading of the same or a higher
    /// level, or the end of the file.
    Section,
}

/// Lines `start..=end`, shown as `start` while closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    pub start: usize,
    pub end: usize,
    pub kind: FoldKind,
}

impl Fold {
    pub fn contains(&self, line: usize) -> bool {
        (self.start..=self.end).contains(&line)
    }

    /// Whether `line` is one of the lines this fold hides while closed.
    pub fn hides(&self, line: usize) -> bool {
        line > self.start && line <= self.end
    }
}

/// The last line of the section that the heading on `start` opens, with the
/// blank lines before the next heading left out: they separate the sections
/// rather than belong to one. `None` when `start` is not a heading.
pub fn section_end(buffer: &Buffer, kinds: &[BlockKind], start: usize) -> Option<usize> {
    let Some(BlockKind::Heading(level)) = kinds.get(start) else { return None };
    let last = buffer.last_line().min(kinds.len().saturating_sub(1));
    let next = (start + 1..=last).find(|&l| matches!(kinds[l], BlockKind::Heading(n) if n <= *level));
    Some(trim_blanks(kinds, start, next.map_or(last, |n| n - 1)))
}

fn trim_blanks(kinds: &[BlockKind], start: usize, mut end: usize) -> usize {
    while end > start && matches!(kinds.get(end), Some(BlockKind::Blank)) {
        end -= 1;
    }
    end
}

/// The fold of `kind` that starts on `start`, if the text still has one there
/// that hides at least one line.
fn measure(buffer: &Buffer, kinds: &[BlockKind], lists: &Lists, kind: FoldKind, start: usize) -> Option<Fold> {
    let end = match kind {
        FoldKind::Section => section_end(buffer, kinds, start)?,
        FoldKind::Item => {
            if !matches!(kinds.get(start), Some(BlockKind::ListItem { .. })) {
                return None;
            }
            let (s, e) = lists.item_bounds(start)?;
            if s != start {
                return None;
            }
            e
        }
    };
    (end > start).then_some(Fold { start, end, kind })
}

/// Every fold around `line`, innermost first: the items that contain it, then
/// the sections, ordered by where they start (the later the start, the
/// further in). Only folds that would hide something.
pub fn candidates(buffer: &Buffer, kinds: &[BlockKind], tab_width: usize, line: usize) -> Vec<Fold> {
    let lists = Lists::new(buffer, kinds, tab_width);
    let mut out = Vec::new();
    let mut item = lists.item_bounds(line).map(|(s, _)| s);
    while let Some(s) = item {
        out.extend(measure(buffer, kinds, &lists, FoldKind::Item, s));
        item = lists.parent(s);
    }
    // The nearest heading at or above the line, then each shallower one
    // above that: the sections it sits in, inside out.
    let mut below = 7u8;
    let mut l = line.min(kinds.len().saturating_sub(1));
    loop {
        if let Some(BlockKind::Heading(n)) = kinds.get(l) {
            if *n < below {
                below = *n;
                if let Some(f) = measure(buffer, kinds, &lists, FoldKind::Section, l) {
                    if f.contains(line) {
                        out.push(f);
                    }
                }
            }
        }
        if l == 0 || below == 1 {
            break;
        }
        l -= 1;
    }
    out.sort_by(|a, b| b.start.cmp(&a.start).then(a.end.cmp(&b.end)));
    out.dedup();
    out
}

/// Every fold in the document, for `zM`.
pub fn all(buffer: &Buffer, kinds: &[BlockKind], tab_width: usize) -> Vec<Fold> {
    let lists = Lists::new(buffer, kinds, tab_width);
    let last = buffer.last_line().min(kinds.len().saturating_sub(1));
    let mut out = Vec::new();
    // Sections in one pass: a heading closes every open one at its level or
    // deeper, and the end of the file closes the rest.
    let mut open: Vec<(usize, u8)> = Vec::new();
    let close = |start: usize, end: usize, out: &mut Vec<Fold>| {
        let end = trim_blanks(kinds, start, end);
        if end > start {
            out.push(Fold { start, end, kind: FoldKind::Section });
        }
    };
    for (l, kind) in kinds.iter().enumerate().take(last + 1) {
        match kind {
            BlockKind::Heading(n) => {
                while open.last().is_some_and(|&(_, m)| m >= *n) {
                    let (s, _) = open.pop().unwrap();
                    close(s, l - 1, &mut out);
                }
                open.push((l, *n));
            }
            BlockKind::ListItem { .. } => out.extend(measure(buffer, kinds, &lists, FoldKind::Item, l)),
            _ => {}
        }
    }
    for (s, _) in open {
        close(s, last, &mut out);
    }
    out
}

/// The closed folds of one document in one pane.
#[derive(Clone, Debug, Default)]
pub struct Folds {
    /// Sorted by start, then by end. Nested folds stay here while the fold
    /// around them is closed, so opening that one shows them still closed.
    closed: Vec<Fold>,
    /// The text the folds were last placed in, and its revision: what the
    /// next edit is compared with. `None` while nothing is folded.
    seen: Option<(Rope, u64)>,
}

/// The fold of `kind` that starts on `start` as the text is now, if it still
/// hides a line: what a fold an edit went through is measured again with.
pub fn remeasure(buffer: &Buffer, kinds: &[BlockKind], tab_width: usize, kind: FoldKind, start: usize) -> Option<Fold> {
    let lists = Lists::new(buffer, kinds, tab_width);
    measure(buffer, kinds, &lists, kind, start)
}

impl Folds {
    /// No fold closed: what a pane that never folded this document has.
    pub const NONE: Folds = Folds { closed: Vec::new(), seen: None };

    pub fn is_empty(&self) -> bool {
        self.closed.is_empty()
    }

    pub fn closed(&self) -> &[Fold] {
        &self.closed
    }

    /// The lines the closed folds hide, one range per fold (they overlap
    /// where folds nest).
    pub fn ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.closed.iter().map(|f| f.start + 1..f.end + 1)
    }

    /// Close `fold`. The text is remembered so the next edit can move it.
    pub fn close(&mut self, fold: Fold, buffer: &Buffer) {
        if !self.closed.contains(&fold) {
            self.closed.push(fold);
            self.closed.sort_by_key(|f| (f.start, f.end));
        }
        self.remember(buffer);
    }

    /// Close every fold in `folds`, as `zM` does.
    pub fn close_all(&mut self, folds: Vec<Fold>, buffer: &Buffer) {
        self.closed = folds;
        self.closed.sort_by_key(|f| (f.start, f.end));
        self.closed.dedup();
        self.remember(buffer);
    }

    pub fn open_all(&mut self) {
        self.closed.clear();
        self.seen = None;
    }

    /// Whether a closed fold starts on `line`.
    pub fn is_head(&self, line: usize) -> bool {
        self.closed.iter().any(|f| f.start == line)
    }

    /// Open the folds that start on `line`. False when none did.
    pub fn open_at(&mut self, line: usize) -> bool {
        self.retain(|f| f.start != line)
    }

    /// Open every fold that hides `line`, so it is shown. False when none
    /// did.
    pub fn reveal(&mut self, line: usize) -> bool {
        self.retain(|f| !f.hides(line))
    }

    fn retain(&mut self, keep: impl Fn(&Fold) -> bool) -> bool {
        let before = self.closed.len();
        self.closed.retain(keep);
        if self.closed.is_empty() {
            self.seen = None;
        }
        self.closed.len() != before
    }

    fn remember(&mut self, buffer: &Buffer) {
        self.seen = Some((buffer.rope.clone(), buffer.revision));
    }

    /// Follow whatever edits were made since the folds were last placed.
    /// `kinds` must describe the text as it is now: until they do, the folds
    /// wait where they are.
    pub fn sync(&mut self, buffer: &Buffer, kinds: &[BlockKind], kinds_revision: u64, tab_width: usize) {
        if self.closed.is_empty() {
            self.seen = None;
            return;
        }
        if self.seen.as_ref().is_some_and(|(_, rev)| *rev == buffer.revision) {
            return;
        }
        if kinds_revision != buffer.revision || kinds.len() != buffer.line_count() {
            return;
        }
        if let Some((old, _)) = self.seen.take() {
            self.shift(&old, &buffer.rope);
        }
        let lists = Lists::new(buffer, kinds, tab_width);
        let mut kept: Vec<Fold> = self
            .closed
            .iter()
            .filter_map(|f| measure(buffer, kinds, &lists, f.kind, f.start))
            .collect();
        kept.sort_by_key(|f| (f.start, f.end));
        kept.dedup();
        self.closed = kept;
        if !self.closed.is_empty() {
            self.remember(buffer);
        }
    }

    /// Move the folds below an edit by the lines it added or removed, and
    /// drop those whose first line it changed while adding or removing lines
    /// (`dd` on a fold takes the fold with it). An edit inside one line keeps
    /// the fold that starts there.
    fn shift(&mut self, old: &Rope, new: &Rope) {
        let (head, tail) = shared_ends(old, new);
        // The touched lines are `first..old_end` in the old text and
        // `first..new_end` in the new one. A shared end that starts at the
        // start of a line in BOTH texts holds that whole line; otherwise the
        // line holding its first byte is touched, in both. Deciding it for
        // each text apart would count lines differently on the two sides:
        // `-` typed on an empty line above a fold starts the shared end at a
        // line start in the old text and just after the `-` in the new one,
        // and the fold would move down a line nobody added.
        let line_start = |r: &Rope, at: usize| at == 0 || r.byte(at - 1) == b'\n';
        let (old_at, new_at) = (old.len_bytes() - tail, new.len_bytes() - tail);
        let whole = usize::from(!(line_start(old, old_at) && line_start(new, new_at)));
        let first = new.byte_to_line(head);
        let old_end = old.byte_to_line(old_at) + whole;
        let new_end = new.byte_to_line(new_at) + whole;
        if old_end == new_end {
            return;
        }
        self.closed.retain_mut(|f| {
            if f.start < first {
                true
            } else if f.start >= old_end {
                f.start = f.start + new_end - old_end;
                f.end = f.end + new_end - old_end;
                true
            } else {
                false
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::markdown::block::BlockCache;

    fn buf(text: &str) -> (Buffer, BlockCache) {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str(text);
        let cache = BlockCache::build(&b, true, 4);
        (b, cache)
    }

    const DOC: &str = "# A\n\
intro\n\
- one\n  - one.a\n  - one.b\n- two\n\
\n\
## B\n\
text\n\
\n\
## C\n\
- x\n\
# D\n";

    #[test]
    fn sections_end_at_the_next_heading_as_high_and_skip_trailing_blanks() {
        let (b, c) = buf(DOC);
        assert_eq!(section_end(&b, &c.kinds, 0), Some(11));
        assert_eq!(section_end(&b, &c.kinds, 7), Some(8));
        assert_eq!(section_end(&b, &c.kinds, 10), Some(11));
        assert_eq!(section_end(&b, &c.kinds, 12), Some(12));
        assert_eq!(section_end(&b, &c.kinds, 1), None);
    }

    #[test]
    fn candidates_run_from_the_innermost_out() {
        let (b, c) = buf(DOC);
        let starts = |line| candidates(&b, &c.kinds, 4, line).iter().map(|f| (f.start, f.end, f.kind)).collect::<Vec<_>>();
        use FoldKind::*;
        // `one.a` has no children: its own item hides nothing.
        assert_eq!(starts(3), [(2, 4, Item), (0, 11, Section)]);
        assert_eq!(starts(8), [(7, 8, Section), (0, 11, Section)]);
        // A lone `- x` and the empty section D fold nothing of their own.
        assert_eq!(starts(11), [(10, 11, Section), (0, 11, Section)]);
        assert_eq!(starts(12), []);
    }

    /// Folds of `text` after `edit` turned it into `edited`, by first line.
    fn shifted(text: &str, edited: &str, at: usize) -> Vec<usize> {
        let (b, _) = buf(text);
        let mut folds = Folds::default();
        folds.close(Fold { start: at, end: at + 1, kind: FoldKind::Item }, &b);
        let (mut b2, c2) = buf(edited);
        b2.revision = b.revision + 1;
        folds.sync(&b2, &c2.kinds, b2.revision, 4);
        folds.closed().iter().map(|f| f.start).collect()
    }

    #[test]
    fn edits_above_a_fold_move_it_by_the_lines_they_add() {
        let doc = "intro\n\n- one\n  - a\n";
        assert_eq!(shifted(doc, "intro\n-\n- one\n  - a\n", 2), [2], "typed on the empty line above: no line added");
        assert_eq!(shifted(doc, "intro\n\n\n- one\n  - a\n", 2), [3], "a line opened above");
        assert_eq!(shifted(doc, "intro\n- one\n  - a\n", 2), [1], "a line deleted above");
        assert_eq!(shifted(doc, "intro\n\n- one\n  - a\nmore\n", 2), [2], "a line added below");
    }

    #[test]
    fn all_finds_every_item_and_section() {
        let (b, c) = buf(DOC);
        let mut got: Vec<_> = all(&b, &c.kinds, 4).iter().map(|f| (f.start, f.end)).collect();
        got.sort();
        assert_eq!(got, [(0, 11), (2, 4), (7, 8), (10, 11)]);
    }
}
