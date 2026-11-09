//! List items as units — the foundation of structural list editing (#29, #30).
//!
//! An ITEM is an item line plus everything that belongs to it: wrapped and
//! lazy continuation lines, nested paragraphs, fenced blocks, and child items
//! with their own subtrees. The `al`/`aL` objects, item moves and renumbering
//! are all built on the bounds computed here.
//!
//! Nesting follows the rule HTML export uses (`export/html.rs`
//! `adjust_lists`): an item's parent is the nearest item above it with a
//! smaller indent, not CommonMark's content column. Editing and export have to
//! agree on what is nested under what.
//!
//! Lines are classified by the block cache rather than re-parsed here, which is
//! the one place the text layer reads a render type: `BlockKind` already knows
//! fences, headings and list markers, and a second parser would drift from it.
//! Indents, though, are measured here, in columns with a tab counting as
//! `tab_width` — the cache's `depth` counts a tab as one.
//!
//! Every function is pure over the buffer text and the kinds, and every range
//! is a pair of line numbers, END INCLUSIVE, like `object::paragraph_bounds`.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

use ropey::Rope;

use super::buffer::Buffer;
use crate::render::markdown::block::{leading_ws_cols, BlockCache, BlockKind, Marker};

/// Leading whitespace in columns, a tab counting as `tab_width`. Matches how
/// the renderer lays out a tabbed list (`render::layout::content_column`).
pub fn indent_cols(line: &str, tab_width: usize) -> usize {
    line.chars()
        .map_while(|c| match c {
            ' ' => Some(1),
            '\t' => Some(tab_width.max(1)),
            _ => None,
        })
        .sum()
}

/// One ordinal to rewrite: the digits at `cols` (chars) on `line` become
/// `number`. The delimiter after them is left as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Renumber {
    pub line: usize,
    pub cols: Range<usize>,
    pub number: String,
}

/// Replace the leading spaces and tabs of `line` with `cols` spaces: how
/// a `Realign` line is applied.
pub fn reindent(rope: &mut Rope, line: usize, cols: usize) {
    let start = rope.line_to_char(line);
    let ws = leading_ws_cols(&rope.line(line).to_string());
    rope.remove(start..start + ws);
    rope.insert(start, &" ".repeat(cols));
}

/// Leading whitespace in columns as CommonMark counts it: tab stops every 4,
/// whatever `tab_width` says.
fn commonmark_cols(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .fold(0, |col, c| if c == '\t' { col + 4 - col % 4 } else { col + 1 })
}

/// What `Lists::realign` would change: the new indent, in columns, of each
/// line that moves, and the item lines it leaves as written because moving
/// them would change how Shoin shows the list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Realign {
    pub lines: Vec<(usize, usize)>,
    pub skipped: Vec<usize>,
}

/// Why lines touch no run of sibling items to move as one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunError {
    /// A touched line, blank lines aside, is in no list item.
    NotInList,
    /// The items touched are not siblings in one list.
    TwoLists,
    /// Only blank lines that touch no item, such as one before a sub-item.
    NoItem,
}

/// A view of one document's lists. Cheap to build. `kinds` must be the block
/// cache for `buffer`'s current text.
///
/// It remembers the indents, extents and parents it has worked out: a
/// command asks for the same ones many times (every sibling of a long list
/// asks for the same parent and its extent), and the text cannot change
/// while a `Lists` borrows it. Build a new one after an edit.
pub struct Lists<'a> {
    buffer: &'a Buffer,
    kinds: &'a [BlockKind],
    tab_width: usize,
    indents: RefCell<HashMap<usize, usize>>,
    extents: RefCell<HashMap<usize, (usize, usize)>>,
    parents: RefCell<HashMap<usize, Option<usize>>>,
    /// `shallower_above` of every item line, in one pass on first use.
    shallower: OnceCell<HashMap<usize, usize>>,
}

impl<'a> Lists<'a> {
    pub fn new(buffer: &'a Buffer, kinds: &'a [BlockKind], tab_width: usize) -> Self {
        Lists {
            buffer,
            kinds,
            tab_width: tab_width.max(1),
            indents: RefCell::default(),
            extents: RefCell::default(),
            parents: RefCell::default(),
            shallower: OnceCell::new(),
        }
    }

    /// The item containing `line`, children included — the `al` range. On a
    /// continuation line or a child paragraph it is the innermost item around
    /// it; outside any list, `None`.
    pub fn item_bounds(&self, line: usize) -> Option<(usize, usize)> {
        self.item_at(line).map(|s| self.bounds(s))
    }

    /// The item containing `line` without its children — the `il` range: its
    /// own line, its continuation lines, and anything else of its own before
    /// the first child item. Never ends on a blank line.
    pub fn own_bounds(&self, line: usize) -> Option<(usize, usize)> {
        let start = self.item_at(line)?;
        let mut e = match self.first_child(start) {
            Some(child) => child - 1,
            None => self.bounds(start).1,
        };
        while e > start && self.blank(e) {
            e -= 1;
        }
        Some((start, e))
    }

    /// The item line of the first child of the item containing `line`.
    pub fn first_child(&self, line: usize) -> Option<usize> {
        let start = self.item_at(line)?;
        let (_, end) = self.bounds(start);
        let d = self.indent(start);
        (start + 1..=end).find(|&l| self.is_item(l) && self.indent(l) > d)
    }

    /// The first line of the parent of the item containing `line`. `None` for
    /// a top-level item, or outside a list.
    pub fn parent(&self, line: usize) -> Option<usize> {
        self.parent_of(self.item_at(line)?)
    }

    /// The previous sibling of the item containing `line`: same parent, same
    /// kind of list. `None` for a first child.
    pub fn prev_sibling_bounds(&self, line: usize) -> Option<(usize, usize)> {
        self.prev_sibling(self.item_at(line)?).map(|s| self.bounds(s))
    }

    /// The next sibling of the item containing `line`. `None` for a last child.
    pub fn next_sibling_bounds(&self, line: usize) -> Option<(usize, usize)> {
        self.next_sibling(self.item_at(line)?).map(|s| self.bounds(s))
    }

    /// Every sibling of the item containing `line`, first through last, each
    /// with its children — the `aL` range.
    pub fn level_bounds(&self, line: usize) -> Option<(usize, usize)> {
        let items = self.level_items(line);
        Some((*items.first()?, self.bounds(*items.last()?).1))
    }

    /// The run of siblings that lines `top..=bot` touch, as the item lines
    /// of its first and last item — what Visual `Alt-j`/`Alt-k` moves.
    ///
    /// A blank line touches an item only when it sits between that item's own
    /// text, the next line being more of it; one before a sub-item or
    /// between items touches nothing.
    /// Each other line touches its innermost item,
    /// and an item nested deeper than the shallowest one touched stands for
    /// its ancestor at that level, so a run never splits a parent from its
    /// children. Nesting is counted in levels, not columns: siblings may sit
    /// at different indents.
    pub fn sibling_run(&self, top: usize, bot: usize) -> Result<(usize, usize), RunError> {
        let bot = bot.min(self.buffer.last_line());
        // Each item once: a long item's lines all touch the same one.
        let mut touched: Vec<usize> = Vec::new();
        for l in top..=bot {
            let item = if self.blank(l) {
                // Between two lines of one item's own text, the blank line is
                // that item's; after a sub-item, or before one, it is no one's.
                let prev = (0..l).rev().find(|&k| !self.blank(k));
                let next = (l + 1..=self.buffer.last_line()).find(|&k| !self.blank(k));
                let own = |k: usize| self.item_at(k).filter(|&x| x == k || !self.is_item(k));
                match (self.item_at(l), prev.and_then(own), next.and_then(own)) {
                    (Some(x), Some(p), Some(n)) if p == x && n == x => x,
                    _ => continue,
                }
            } else {
                self.item_at(l).ok_or(RunError::NotInList)?
            };
            if !touched.contains(&item) {
                touched.push(item);
            }
        }
        if touched.is_empty() {
            // Blank lines alone: inside a list they touch no item, outside
            // one they are simply not in a list, as Normal mode says.
            let in_list = (top..=bot).any(|l| self.item_at(l).is_some());
            return Err(if in_list { RunError::NoItem } else { RunError::NotInList });
        }
        // Each touched item with its ancestors, nearest first: its level is
        // the number of ancestors, and lifting it to a level is a lookup.
        let chains: Vec<Vec<usize>> = touched
            .iter()
            .map(|&item| {
                let mut chain = vec![item];
                while let Some(p) = chain.last().and_then(|&k| self.parent_of(k)) {
                    chain.push(p);
                }
                chain
            })
            .collect();
        let shallowest = chains.iter().map(|c| c.len()).min().unwrap_or(1);
        let lift = |chain: &Vec<usize>| chain[chain.len() - shallowest];
        let first = lift(&chains[0]);
        let last = lift(&chains[chains.len() - 1]);
        // Siblings in one list are reached from the first by `next_sibling`;
        // anything else (another parent, another list) is passed over.
        let mut k = first;
        while k < last {
            k = self.next_sibling(k).ok_or(RunError::TwoLists)?;
        }
        if k == last {
            Ok((first, last))
        } else {
            Err(RunError::TwoLists)
        }
    }

    /// The item lines of every sibling of the item containing `line`, in
    /// order. Empty outside a list.
    pub fn level_items(&self, line: usize) -> Vec<usize> {
        let Some(item) = self.item_at(line) else {
            return Vec::new();
        };
        let mut items = vec![item];
        while let Some(p) = self.prev_sibling(items[0]) {
            items.insert(0, p);
        }
        while let Some(n) = self.next_sibling(*items.last().unwrap_or(&item)) {
            items.push(n);
        }
        items
    }

    /// How far this document nests a child inside its parent, in columns: the
    /// parent→child step of the nesting nearest to `line`, so a file that
    /// nests by 2 keeps nesting by 2 whatever `tab_width` says. `None` when no
    /// list in the document nests. Items in `skip` are not measured: a level
    /// being shifted would otherwise count its own distance from its parent.
    pub fn step(&self, line: usize, skip: &[usize]) -> Option<usize> {
        let last = self.buffer.last_line();
        // Outward from `line`, alternating below and above.
        let near = (0..=last).flat_map(|d| {
            let below = (d > 0).then(|| line.checked_add(d)).flatten();
            [line.checked_sub(d), below]
        });
        near.flatten()
            .filter(|&l| l <= last && self.is_item(l) && !skip.contains(&l))
            .find_map(|l| {
                let parent = self.parent_of(l)?;
                Some(self.indent(l) - self.indent(parent))
            })
    }

    /// The column the item containing `line` sits at.
    pub fn item_indent(&self, line: usize) -> Option<usize> {
        self.item_at(line).map(|s| self.indent(s))
    }

    /// The number of the ordered item on `line`, and the chars its digits
    /// span. `None` for a bullet, a line that is not an item, or a number too
    /// long to count with.
    pub fn ordinal(&self, line: usize) -> Option<(u64, Range<usize>)> {
        if !self.ordered(line) {
            return None;
        }
        let text = self.buffer.line_text(line);
        let ws = text.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let digits: String = text.chars().skip(ws).take_while(char::is_ascii_digit).collect();
        Some((digits.parse().ok()?, ws..ws + digits.len()))
    }

    /// The edits that number the ordered level containing `line` in sequence,
    /// counting up from `start` — or, with `None`, from its first item's own
    /// number, so a list that starts at `3.` keeps starting there. Each item
    /// keeps its own `.` or `)`. Nothing for a bullet list, or one already in
    /// sequence.
    ///
    /// The edits are returned, not made, so the caller can put them in the
    /// same undo step as whatever disturbed the numbering.
    pub fn renumber(&self, line: usize, start: Option<u64>) -> Vec<Renumber> {
        let ordinals: Option<Vec<(usize, u64, Range<usize>)>> = self
            .level_items(line)
            .into_iter()
            .map(|l| self.ordinal(l).map(|(n, cols)| (l, n, cols)))
            .collect();
        // A bullet list, or an ordinal too long to parse: leave it as written.
        let Some(ordinals) = ordinals.filter(|o| !o.is_empty()) else {
            return Vec::new();
        };
        let first = start.unwrap_or(ordinals[0].1);
        ordinals
            .into_iter()
            .zip(first..)
            .filter(|((_, n, _), want)| n != want)
            .map(|((line, _, cols), want)| Renumber {
                line,
                cols,
                number: want.to_string(),
            })
            .collect()
    }

    /// The fix-list command: re-indent the whole list around `line` so that
    /// CommonMark tools (GitHub, pandoc, export) read it the way Shoin shows
    /// it. `None` outside a list; a blank line between two items of one list
    /// is inside it. `math` is the `[markdown] math` the kinds were built
    /// with.
    ///
    /// Shoin's structure decides what belongs to what; nothing here guesses
    /// at CommonMark beyond its one rule for items: what sits under an item
    /// belongs to it when it starts at the item's text column T, or up to 3
    /// columns past it. A sub-item or block that already does stays where it
    /// is; one that doesn't moves to T, and takes everything under it along
    /// by the same amount. Top-level items, lazy lines and blank lines never
    /// move. A line whose tabs CommonMark counts differently from Shoin is
    /// rewritten with spaces at the column Shoin shows it at.
    ///
    /// The result is checked by classifying the new text: an item whose move
    /// would change any line's kind, or any item's extent or parent (a fence
    /// at 4+ columns reads as prose here), is left as written with its
    /// subtree, and so is one whose move would push code left of column 0.
    /// The rest is tried again.
    pub fn realign(&self, line: usize, math: bool) -> Option<Realign> {
        let mut top = self.item_at(line).or_else(|| self.blank_inside(line))?;
        while let Some(p) = self.parent_of(top) {
            top = p;
        }
        let items = self.level_items(top);
        let first = *items.first()?;
        let end = self.bounds(*items.last()?).1;
        let mut skip: BTreeSet<usize> = BTreeSet::new();
        loop {
            let mut moves = BTreeMap::new();
            let mut blamed = BTreeSet::new();
            for &item in &items {
                self.place(item, 0, &skip, &mut moves, &mut blamed);
            }
            moves.retain(|&l, cols| *cols != self.indent(l) || self.tabs_differ(l));
            if blamed.is_empty() {
                blamed = self.changes(first, end, &moves, math);
            }
            if blamed.is_empty() {
                let skipped = skip
                    .iter()
                    .copied()
                    .filter(|&s| !self.ancestors(s).any(|a| skip.contains(&a)))
                    .collect();
                return Some(Realign {
                    lines: moves.into_iter().collect(),
                    skipped,
                });
            }
            // Each blamed item, or its nearest ancestor that still moves.
            let fresh: Vec<usize> = blamed
                .into_iter()
                .filter_map(|k| std::iter::once(k).chain(self.ancestors(k)).find(|a| !skip.contains(a)))
                .collect();
            if fresh.is_empty() {
                // Every item up to the top already stays: nothing is safe.
                return Some(Realign {
                    lines: Vec::new(),
                    skipped: items,
                });
            }
            skip.extend(fresh);
        }
    }

    fn kind(&self, line: usize) -> Option<&BlockKind> {
        self.kinds.get(line)
    }

    fn is_item(&self, line: usize) -> bool {
        matches!(self.kind(line), Some(BlockKind::ListItem { .. }))
    }

    /// Ordered or bullet. Switching between the two at one depth starts a new
    /// list; `-`, `*` and `+` are all one kind, as in export.
    fn ordered(&self, line: usize) -> bool {
        matches!(
            self.kind(line),
            Some(BlockKind::ListItem {
                marker: Marker::Ordered,
                ..
            })
        )
    }

    fn blank(&self, line: usize) -> bool {
        matches!(self.kind(line), Some(BlockKind::Blank))
    }

    fn indent(&self, line: usize) -> usize {
        if let Some(&d) = self.indents.borrow().get(&line) {
            return d;
        }
        let d = indent_cols(&self.buffer.line_text(line), self.tab_width);
        self.indents.borrow_mut().insert(line, d);
        d
    }

    /// Where `item` and its subtree go when its parent moved by `delta`
    /// columns: each line's new indent into `moves`. An item in `skip` stays
    /// as written, subtree and all; one whose move would push a code line
    /// left of column 0 goes into `blamed`.
    fn place(
        &self,
        item: usize,
        delta: isize,
        skip: &BTreeSet<usize>,
        moves: &mut BTreeMap<usize, usize>,
        blamed: &mut BTreeSet<usize>,
    ) {
        if skip.contains(&item) {
            return;
        }
        let shift = |l: usize, by: isize| self.indent(l) as isize + by;
        moves.insert(item, shift(item, delta).max(0) as usize);
        let t = self.text_col(item) as isize + delta;
        // Where a child's first line goes: kept within T..=T+3, else to T.
        let fit = |l: usize| {
            let at = shift(l, delta);
            if (t..=t + 3).contains(&at) {
                delta
            } else {
                delta + t - at
            }
        };
        let own = self.indent(item);
        let (_, end) = self.bounds(item);
        // The shift of the block the next line would continue, if any (the
        // item's own text first), and whether that block is paragraph text,
        // which a paragraph or `$$` line right after it continues.
        let mut follow = Some(delta);
        let mut para = true;
        let mut i = item + 1;
        while i <= end {
            let Some(kind) = self.kind(i) else { break };
            if self.buffer.line_text(i).trim().is_empty() {
                // Nothing to indent. An empty line in a fence keeps it open.
                if !matches!(kind, BlockKind::FenceBody { .. }) {
                    follow = None;
                    para = false;
                }
                i += 1;
                continue;
            }
            if self.is_item(i) {
                // A direct child: deeper ones are inside its own bounds.
                self.place(i, fit(i), skip, moves, blamed);
                i = self.bounds(i).1 + 1;
                follow = None;
                para = false;
                continue;
            }
            let prev = self.kind(i - 1);
            let code = matches!(kind, BlockKind::FenceBody { .. } | BlockKind::FenceClose);
            let continues = match kind {
                _ if code => true,
                // `$$` is paragraph text to CommonMark.
                BlockKind::Paragraph | BlockKind::MathBlock => para,
                BlockKind::Table => matches!(prev, Some(BlockKind::Table)),
                BlockKind::Quote(_) => matches!(prev, Some(BlockKind::Quote(_))),
                _ => false,
            };
            let lazy = matches!(kind, BlockKind::Paragraph | BlockKind::MathBlock) && self.indent(i) <= own;
            match follow.filter(|_| continues) {
                // A lazy line is read as part of the paragraph wherever it
                // starts, by Shoin and CommonMark alike: leave it.
                Some(_) if lazy => {}
                Some(by) => {
                    let to = shift(i, by);
                    if code && to < 0 {
                        blamed.insert(item);
                    }
                    moves.insert(i, to.max(0) as usize);
                }
                None => {
                    let by = fit(i);
                    moves.insert(i, shift(i, by).max(0) as usize);
                    follow = Some(by);
                }
            }
            para = matches!(kind, BlockKind::Paragraph | BlockKind::MathBlock);
            i += 1;
        }
    }

    /// The column an item's text starts at, CommonMark's way: past the marker
    /// and the 1 to 4 spaces after it. With none (an empty item) or 5 or
    /// more, one column past the marker. Columns are counted as `indent`
    /// counts them, so T and the lines under the item compare.
    fn text_col(&self, item: usize) -> usize {
        let text = self.buffer.line_text(item);
        let ws = |c: char| match c {
            ' ' => Some(1),
            '\t' => Some(self.tab_width),
            _ => None,
        };
        let mut chars = text.chars().peekable();
        let mut col = 0;
        while let Some(w) = chars.peek().copied().and_then(ws) {
            col += w;
            chars.next();
        }
        while chars.next_if(char::is_ascii_digit).is_some() {
            col += 1;
        }
        // The bullet, or the ordinal's `.` / `)`.
        chars.next();
        col += 1;
        let marker_end = col;
        while let Some(w) = chars.peek().copied().and_then(ws) {
            col += w;
            chars.next();
        }
        let gap = col - marker_end;
        if chars.peek().is_none() || gap == 0 || gap > 4 {
            marker_end + 1
        } else {
            col
        }
    }

    /// Whether `line`'s indent has a tab that CommonMark, with its tab stops
    /// every 4, counts to another column than Shoin does. Code keeps its
    /// tabs: there they are text.
    fn tabs_differ(&self, line: usize) -> bool {
        let text = self.buffer.line_text(line);
        !matches!(self.kind(line), Some(BlockKind::FenceBody { .. } | BlockKind::FenceClose))
            && text.chars().take_while(|c| c.is_whitespace()).any(|c| c == '\t')
            && commonmark_cols(&text) != self.indent(line)
    }

    /// A blank `line` between two items of one list: the item above it.
    fn blank_inside(&self, line: usize) -> Option<usize> {
        if !self.blank(line) {
            return None;
        }
        let above = (0..line).rev().find(|&l| !self.blank(l))?;
        let below = (line + 1..=self.buffer.last_line()).find(|&l| !self.blank(l))?;
        let top = |l: usize| {
            let item = self.item_at(l)?;
            Some(self.ancestors(item).last().unwrap_or(item))
        };
        let (a, b) = (top(above)?, top(below)?);
        self.level_items(a).contains(&b).then(|| self.item_at(above)).flatten()
    }

    fn ancestors(&self, item: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.parent_of(item), move |&p| self.parent_of(p))
    }

    /// The items whose moves make Shoin show the list `first..=end`
    /// differently: the item around a line that changes kind, and any item
    /// whose extent or parent changes. Empty when it would look the same.
    ///
    /// Only the list and two lines past it are classified again, from a copy
    /// of the rope (so its lines are numbered exactly as the buffer's). A
    /// list starts at an item line, which no carried state can be open
    /// across.
    fn changes(&self, first: usize, end: usize, moves: &BTreeMap<usize, usize>, math: bool) -> BTreeSet<usize> {
        let mut blamed = BTreeSet::new();
        if moves.is_empty() {
            return blamed;
        }
        let rope = &self.buffer.rope;
        let hi = (end + 2).min(self.buffer.last_line());
        let to = if hi + 1 < rope.len_lines() { rope.line_to_char(hi + 1) } else { rope.len_chars() };
        let mut copy = Rope::from(rope.slice(rope.line_to_char(first)..to));
        for (&l, &cols) in moves.iter().rev() {
            reindent(&mut copy, l - first, cols);
        }
        let mut buffer = Buffer::empty();
        buffer.rope = copy;
        let cache = BlockCache::build(&buffer, math);
        let after = Lists::new(&buffer, &cache.kinds, self.tab_width);
        let same = |a: Option<&BlockKind>, b: Option<&BlockKind>| match (a, b) {
            (Some(BlockKind::ListItem { marker: m, .. }), Some(BlockKind::ListItem { marker: n, .. })) => m == n,
            (Some(a), Some(b)) => std::mem::discriminant(a) == std::mem::discriminant(b),
            (a, b) => a.is_none() == b.is_none(),
        };
        // The innermost item of this list around `l`.
        let around = |l: usize| {
            (first..=l.min(end))
                .rev()
                .find_map(|k| self.item_at(k))
                .unwrap_or(first)
        };
        for l in first..=hi {
            let n = l - first;
            if !same(self.kind(l), after.kind(n)) {
                blamed.insert(around(l));
            } else if self.is_item(l)
                && (self.bounds(l).1 != after.bounds(n).1 + first
                    || self.parent_of(l) != after.parent_of(n).map(|p| p + first))
            {
                blamed.insert(l);
            }
        }
        blamed
    }

    /// The extent of the item whose item line is `start`.
    fn bounds(&self, start: usize) -> (usize, usize) {
        if let Some(&b) = self.extents.borrow().get(&start) {
            return b;
        }
        let b = self.measure(start);
        self.extents.borrow_mut().insert(start, b);
        b
    }

    /// `bounds`, worked out.
    fn measure(&self, start: usize) -> (usize, usize) {
        let last = self.buffer.last_line();
        let d = self.indent(start);
        let mut end = start;
        let mut in_fence = false;
        let mut i = start + 1;
        while i <= last {
            let Some(kind) = self.kind(i) else { break };
            match kind {
                // A fence that opened inside the item keeps every line up to
                // its close, whatever their indentation: they are text, not
                // structure.
                BlockKind::FenceBody { .. } | BlockKind::FenceClose if in_fence => {
                    in_fence = !matches!(kind, BlockKind::FenceClose);
                }
                // A blank run belongs only if what follows it is deeper — a
                // loose item's child. Trailing blank lines never do.
                BlockKind::Blank => {
                    let mut j = i + 1;
                    while j <= last && self.blank(j) {
                        j += 1;
                    }
                    if j > last || self.indent(j) <= d {
                        break;
                    }
                    i = j;
                    continue;
                }
                _ if self.indent(i) > d => {
                    in_fence = matches!(kind, BlockKind::FenceOpen(_));
                }
                // Lazy continuation: a paragraph line may drop its indent when
                // it directly follows paragraph text (`- one\ntwo`).
                BlockKind::Paragraph
                    if matches!(
                        self.kind(i - 1),
                        Some(BlockKind::Paragraph | BlockKind::ListItem { .. })
                    ) => {}
                // A display block keeps every line up to its close, as a fence
                // does, and may follow the item's text the way a lazy
                // paragraph line does.
                BlockKind::MathBlock
                    if matches!(
                        self.kind(i - 1),
                        Some(
                            BlockKind::Paragraph
                                | BlockKind::ListItem { .. }
                                | BlockKind::MathBlock
                        )
                    ) => {}
                _ => break,
            }
            end = i;
            i += 1;
        }
        (start, end)
    }

    /// The innermost item whose bounds contain `line`, as its item line.
    fn item_at(&self, line: usize) -> Option<usize> {
        let mut k = (0..=line).rev().find(|&l| self.is_item(l))?;
        // Nothing above the nearest item line can contain `line` without
        // containing that item line too — so climb its ancestors.
        while self.bounds(k).1 < line {
            k = self.shallower_above(k)?;
        }
        Some(k)
    }

    /// The nearest item line above `item` (an item line) with a smaller
    /// indent than it.
    ///
    /// Worked out for every item at once, top to bottom, with a stack of the
    /// items that could still be the answer for one further down: each with
    /// a smaller indent than the one above it. An item pops those at least
    /// as deep as itself; what is left on top is its answer. Asking each item
    /// in turn would scan back over the whole list every time.
    fn shallower_above(&self, item: usize) -> Option<usize> {
        debug_assert!(self.is_item(item), "shallower_above({item}) on a line that is no item");
        // A top-level item has nothing shallower: no need for the pass.
        if self.indent(item) == 0 {
            return None;
        }
        let found = self.shallower.get_or_init(|| {
            let mut found = HashMap::new();
            let mut stack: Vec<(usize, usize)> = Vec::new();
            for l in (0..self.kinds.len()).filter(|&l| self.is_item(l)) {
                let d = self.indent(l);
                while stack.last().is_some_and(|&(_, e)| e >= d) {
                    stack.pop();
                }
                if let Some(&(k, _)) = stack.last() {
                    found.insert(l, k);
                }
                stack.push((l, d));
            }
            found
        });
        found.get(&item).copied()
    }

    /// Nearest shallower item above `item` whose bounds contain it.
    fn parent_of(&self, item: usize) -> Option<usize> {
        if let Some(&p) = self.parents.borrow().get(&item) {
            return p;
        }
        let p = self.find_parent(item);
        self.parents.borrow_mut().insert(item, p);
        p
    }

    /// `parent_of`, worked out.
    fn find_parent(&self, item: usize) -> Option<usize> {
        let mut k = self.shallower_above(item)?;
        while self.bounds(k).1 < item {
            k = self.shallower_above(k)?;
        }
        Some(k)
    }

    fn same_list(&self, a: usize, b: usize) -> bool {
        self.ordered(a) == self.ordered(b) && self.parent_of(a) == self.parent_of(b)
    }

    fn next_sibling(&self, item: usize) -> Option<usize> {
        let last = self.buffer.last_line();
        let mut j = self.bounds(item).1 + 1;
        while j <= last && self.blank(j) {
            j += 1;
        }
        (j <= last && self.is_item(j) && self.same_list(item, j)).then_some(j)
    }

    /// Found from the line just above `item` (past any blank run): the
    /// previous sibling's subtree ends there, so it is that line's innermost
    /// item or one of its ancestors.
    fn prev_sibling(&self, item: usize) -> Option<usize> {
        let mut p = item.checked_sub(1)?;
        while self.blank(p) {
            p = p.checked_sub(1)?;
        }
        let parent = self.parent_of(item);
        let mut x = self.item_at(p)?;
        while self.parent_of(x) != parent {
            if Some(x) == parent {
                return None;
            }
            x = self.parent_of(x)?;
        }
        (self.next_sibling(x) == Some(item)).then_some(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::markdown::block::BlockCache;
    use ropey::Rope;

    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str(text);
        b
    }

    /// Runs `f` against the lists of `text` with a tab width of 4.
    fn with<T>(text: &str, f: impl FnOnce(&Lists) -> T) -> T {
        let b = buf(text);
        let cache = BlockCache::build(&b, true);
        f(&Lists::new(&b, &cache.kinds, 4))
    }

    #[test]
    fn indent_counts_a_tab_as_tab_width() {
        assert_eq!(indent_cols("  - a", 4), 2);
        assert_eq!(indent_cols("\t- a", 4), 4);
        assert_eq!(indent_cols(" \t- a", 2), 3);
        assert_eq!(indent_cols("- a", 4), 0);
    }

    #[test]
    fn tight_list_items_carry_their_children() {
        let t = "- a\n  - a1\n  - a2\n    - a2x\n- b\n";
        with(t, |l| {
            assert_eq!(l.item_bounds(0), Some((0, 3)));
            assert_eq!(l.item_bounds(1), Some((1, 1)));
            assert_eq!(l.item_bounds(2), Some((2, 3)));
            assert_eq!(l.item_bounds(4), Some((4, 4)));
        });
    }

    #[test]
    fn loose_items_keep_inner_blanks_but_never_trailing_ones() {
        let t = "- a\n\n  - a1\n\n- b\n\nafter\n";
        with(t, |l| {
            assert_eq!(l.item_bounds(0), Some((0, 2)));
            // A blank inside a loose item belongs to it.
            assert_eq!(l.item_bounds(1), Some((0, 2)));
            // A blank between two top-level items belongs to neither.
            assert_eq!(l.item_bounds(3), None);
            assert_eq!(l.item_bounds(4), Some((4, 4)));
            assert_eq!(l.item_bounds(6), None);
        });
    }

    #[test]
    fn lazy_continuation_under_a_nested_item() {
        let t = "- a\n  - b\nlazy\n- c\n";
        with(t, |l| {
            assert_eq!(l.item_bounds(1), Some((1, 2)));
            assert_eq!(l.item_bounds(0), Some((0, 2)));
            // The cursor on the lazy line is in the innermost item.
            assert_eq!(l.item_bounds(2), Some((1, 2)));
        });
    }

    #[test]
    fn math_block_body_less_indented_than_the_item_stays_in_it() {
        let t = "- a\n  $$\n\\frac{a}{b}\n  $$\n- b\n";
        with(t, |l| {
            assert_eq!(l.item_bounds(0), Some((0, 3)));
            assert_eq!(l.item_bounds(2), Some((0, 3)));
            assert_eq!(l.next_sibling_bounds(0), Some((4, 4)));
        });
    }

    #[test]
    fn a_sibling_run_takes_the_shallowest_items_touched() {
        let t = "- a\n  - a1\n- b\n\n- c\n  - c1\n";
        with(t, |l| {
            assert_eq!(l.sibling_run(0, 0), Ok((0, 0)));
            // a1 stands for a once b, a level up, is touched too.
            assert_eq!(l.sibling_run(1, 2), Ok((0, 2)));
            assert_eq!(l.sibling_run(1, 1), Ok((1, 1)));
            // The blank line touches nothing.
            assert_eq!(l.sibling_run(2, 3), Ok((2, 2)));
            assert_eq!(l.sibling_run(3, 3), Err(RunError::NotInList));
            assert_eq!(l.sibling_run(1, 5), Ok((0, 4)));
            // The blank line between b and c touches neither: b and c move.
            assert_eq!(l.sibling_run(2, 4), Ok((2, 4)));
        });
        // Siblings at uneven indents are one level.
        with("- a\n   - b\n  - c\n", |l| assert_eq!(l.sibling_run(1, 2), Ok((1, 2))));
        with("- a\n1. b\n", |l| assert_eq!(l.sibling_run(0, 1), Err(RunError::TwoLists)));
        with("- a\n  - x\n- b\n  - y\n", |l| {
            assert_eq!(l.sibling_run(1, 1), Ok((1, 1)));
            assert_eq!(l.sibling_run(3, 3), Ok((3, 3)));
        });
        with("text\n- a\n", |l| assert_eq!(l.sibling_run(0, 1), Err(RunError::NotInList)));
        // A loose item's inner blank line alone is that item.
        with("- a\n\n  para\n- b\n", |l| assert_eq!(l.sibling_run(1, 1), Ok((0, 0))));
        // A blank line before a sub-item touches nothing, alone or not, so a
        // bigger selection never moves less.
        with("- a\n\n  - a1\n- b\n", |l| {
            assert_eq!(l.sibling_run(1, 1), Err(RunError::NoItem));
            assert_eq!(l.sibling_run(1, 2), Ok((2, 2)));
        });
        // Between a parent's own paragraphs after its sub-items, too.
        with("- a\n  - a1\n\n  more\n\n  even more\n- b\n", |l| {
            assert_eq!(l.sibling_run(4, 4), Ok((0, 0)));
        });
        // After a sub-item it is no one's: running a selection one line past
        // a2 moves a2, not its parent.
        with("- a\n  - a1\n  - a2\n\n  more\n- b\n", |l| {
            assert_eq!(l.sibling_run(2, 3), Ok((2, 2)));
        });
        // Blank lines outside any list are not in a list.
        with("para\n\nmore\n", |l| assert_eq!(l.sibling_run(1, 1), Err(RunError::NotInList)));
        // Deeply nested lines share their levels; the run is still right.
        with("- a\n  - b\n    - c\n    - d\n  - e\n", |l| {
            assert_eq!(l.sibling_run(2, 3), Ok((2, 3)));
            assert_eq!(l.sibling_run(3, 4), Ok((1, 4)));
        })
    }

    #[test]
    fn fence_body_less_indented_than_the_item_stays_in_it() {
        let t = "- a\n  ```\nbody\n  ```\n- b\n";
        with(t, |l| {
            assert_eq!(l.item_bounds(0), Some((0, 3)));
            assert_eq!(l.item_bounds(2), Some((0, 3)));
            assert_eq!(l.next_sibling_bounds(0), Some((4, 4)));
        });
    }

    #[test]
    fn child_paragraph_and_continuation_lines() {
        let t = "- a\n  wrapped\n\n  para\n- b\n";
        with(t, |l| {
            assert_eq!(l.item_bounds(0), Some((0, 3)));
            assert_eq!(l.item_bounds(1), Some((0, 3)));
            assert_eq!(l.item_bounds(3), Some((0, 3)));
        });
    }

    #[test]
    fn a_list_ends_at_a_heading_rule_or_table_without_a_blank() {
        for t in [
            "- a\n# h\n",
            "- a\n---\n",
            "- a\n| x | y |\n|---|---|\n",
        ] {
            with(t, |l| assert_eq!(l.item_bounds(0), Some((0, 0)), "{t:?}"));
        }
    }

    #[test]
    fn outside_any_list_there_is_no_item() {
        with("para\n\n# h\n", |l| {
            assert_eq!(l.item_bounds(0), None);
            assert_eq!(l.parent(0), None);
            assert_eq!(l.level_bounds(2), None);
        });
    }

    #[test]
    fn last_item_without_a_trailing_newline() {
        with("- a\n- b\n  - c", |l| {
            assert_eq!(l.item_bounds(1), Some((1, 2)));
            assert_eq!(l.prev_sibling_bounds(2), None);
            assert_eq!(l.prev_sibling_bounds(1), Some((0, 0)));
        });
    }

    /// The spec's example: `b` at 3 columns and `c` at 2 are both children of
    /// `a`, so they are siblings.
    #[test]
    fn inconsistent_child_indents_are_still_siblings() {
        let t = "- a\n   - b\n  - c\n";
        with(t, |l| {
            assert_eq!(l.parent(1), Some(0));
            assert_eq!(l.parent(2), Some(0));
            assert_eq!(l.next_sibling_bounds(1), Some((2, 2)));
            assert_eq!(l.prev_sibling_bounds(2), Some((1, 1)));
            assert_eq!(l.item_bounds(0), Some((0, 2)));
        });
    }

    #[test]
    fn tabs_and_spaces_nest_by_columns() {
        // A tab is 4 columns: `\t- b` is as deep as `    - c`, and deeper
        // than `  - x`.
        let t = "- a\n  - x\n\t- b\n    - c\n";
        with(t, |l| {
            assert_eq!(l.parent(2), Some(1));
            assert_eq!(l.next_sibling_bounds(2), Some((3, 3)));
            assert_eq!(l.item_bounds(1), Some((1, 3)));
        });
    }

    #[test]
    fn siblings_skip_children_and_stop_at_the_ends() {
        let t = "- a\n  - a1\n- b\n- c\n  - c1\n";
        with(t, |l| {
            assert_eq!(l.next_sibling_bounds(0), Some((2, 2)));
            assert_eq!(l.prev_sibling_bounds(3), Some((2, 2)));
            assert_eq!(l.prev_sibling_bounds(2), Some((0, 1)));
            assert_eq!(l.prev_sibling_bounds(0), None);
            assert_eq!(l.next_sibling_bounds(3), None);
            // A first child's previous line is its parent, not a sibling.
            assert_eq!(l.prev_sibling_bounds(4), None);
            assert_eq!(l.next_sibling_bounds(1), None);
        });
    }

    #[test]
    fn loose_siblings_across_blank_lines() {
        with("- a\n\n- b\n\n\n- c\n", |l| {
            assert_eq!(l.next_sibling_bounds(0), Some((2, 2)));
            assert_eq!(l.next_sibling_bounds(2), Some((5, 5)));
            assert_eq!(l.prev_sibling_bounds(5), Some((2, 2)));
        });
    }

    #[test]
    fn bullet_and_ordered_at_one_depth_are_different_lists() {
        let t = "- a\n- b\n1. c\n2. d\n";
        with(t, |l| {
            assert_eq!(l.next_sibling_bounds(1), None);
            assert_eq!(l.prev_sibling_bounds(2), None);
            assert_eq!(l.level_bounds(0), Some((0, 1)));
            assert_eq!(l.level_bounds(3), Some((2, 3)));
        });
    }

    #[test]
    fn mixed_bullet_markers_are_one_list() {
        with("- a\n* b\n+ c\n", |l| assert_eq!(l.level_bounds(1), Some((0, 2))));
    }

    #[test]
    fn ordered_inside_bullet_and_the_reverse() {
        let t = "- a\n  1. x\n  2) y\n- b\n  - z\n";
        with(t, |l| {
            assert_eq!(l.parent(2), Some(0));
            assert_eq!(l.level_bounds(1), Some((1, 2)));
            assert_eq!(l.level_bounds(4), Some((4, 4)));
        });
        let t = "1. a\n   - x\n   - y\n2. b\n";
        with(t, |l| {
            assert_eq!(l.parent(2), Some(0));
            assert_eq!(l.next_sibling_bounds(0), Some((3, 3)));
        });
    }

    #[test]
    fn level_takes_every_sibling_with_children() {
        let t = "- p\n  - a\n    - a1\n  - b\n  - c\n    - c1\n- q\n";
        with(t, |l| {
            assert_eq!(l.level_bounds(3), Some((1, 5)));
            assert_eq!(l.level_bounds(0), Some((0, 6)));
        });
    }

    #[test]
    fn task_items_are_items() {
        let t = "- [ ] a\n  - [x] b\n- [ ] c\n";
        with(t, |l| {
            assert_eq!(l.item_bounds(0), Some((0, 1)));
            assert_eq!(l.parent(1), Some(0));
            assert_eq!(l.next_sibling_bounds(0), Some((2, 2)));
        });
    }

    #[test]
    fn step_is_the_nearest_nesting() {
        // Its own list nests by 2.
        assert_eq!(with("- a\n  - b\n- c\n", |l| l.step(2, &[])), Some(2));
        // A flat list takes the step of the nearest list that nests.
        let t = "- x\n   - y\n\np\n\n- a\n- b\n\np\n\n- m\n  - n\n";
        assert_eq!(with(t, |l| l.step(6, &[])), Some(3));
        assert_eq!(with(t, |l| l.step(10, &[])), Some(2));
        // A tab is `tab_width` columns.
        assert_eq!(with("- a\n\t- b\n", |l| l.step(0, &[])), Some(4));
        // Nothing nests.
        assert_eq!(with("- a\n- b\n", |l| l.step(0, &[])), None);
    }

    #[test]
    fn own_bounds_stop_before_the_first_child() {
        let t = "- a\n  wrapped\n\n  para\n\n  - b\n    - c\n- d\n";
        with(t, |l| {
            assert_eq!(l.own_bounds(0), Some((0, 3)));
            assert_eq!(l.own_bounds(3), Some((0, 3)));
            assert_eq!(l.own_bounds(5), Some((5, 5)));
            assert_eq!(l.own_bounds(7), Some((7, 7)));
        });
    }

    /// The numbers `renumber` would write, applied to the text.
    fn renumbered(text: &str, line: usize) -> String {
        with(text, |l| {
            let mut lines: Vec<String> = text.lines().map(String::from).collect();
            for r in l.renumber(line, None) {
                let s = &mut lines[r.line];
                let chars: Vec<char> = s.chars().collect();
                *s = chars[..r.cols.start].iter().collect::<String>()
                    + &r.number
                    + &chars[r.cols.end..].iter().collect::<String>();
            }
            lines.join("\n") + "\n"
        })
    }

    #[test]
    fn renumber_counts_up_from_the_first_number() {
        assert_eq!(renumbered("1. a\n1. b\n5. c\n", 0), "1. a\n2. b\n3. c\n");
        assert_eq!(renumbered("3. a\n1. b\n", 1), "3. a\n4. b\n");
        // Each item keeps its own delimiter.
        assert_eq!(renumbered("1) a\n1. b\n", 0), "1) a\n2. b\n");
        // Already in sequence: nothing to do.
        with("1. a\n2. b\n", |l| assert!(l.renumber(0, None).is_empty()));
    }

    /// Only the level asked for: its sub-lists, and lists elsewhere, are left
    /// as written.
    #[test]
    fn renumber_touches_one_level() {
        let t = "1. a\n   1. x\n   1. y\n1. b\n\n# h\n\n1. p\n1. q\n";
        assert_eq!(
            renumbered(t, 0),
            "1. a\n   1. x\n   1. y\n2. b\n\n# h\n\n1. p\n1. q\n"
        );
        assert_eq!(
            renumbered(t, 2),
            "1. a\n   1. x\n   2. y\n1. b\n\n# h\n\n1. p\n1. q\n"
        );
    }

    #[test]
    fn renumber_handles_a_change_of_width() {
        let t: String = (0..10).map(|i| format!("1. i{i}\n")).collect();
        let want: String = (1..=10).map(|i| format!("{i}. i{}\n", i - 1)).collect();
        assert_eq!(renumbered(&t, 0), want);
    }

    #[test]
    fn renumber_from_a_given_start() {
        with("4. a\n4. b\n", |l| {
            let r = l.renumber(0, Some(1));
            assert_eq!(r.iter().map(|r| r.number.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        });
    }

    #[test]
    fn bullet_lists_are_never_renumbered() {
        with("- a\n- b\n", |l| assert!(l.renumber(0, None).is_empty()));
        with("prose\n", |l| assert!(l.renumber(0, None).is_empty()));
    }

    /// Two lists separated by a heading never see each other.
    #[test]
    fn lists_do_not_cross_a_heading() {
        let t = "- a\n\n# h\n\n- b\n";
        with(t, |l| {
            assert_eq!(l.next_sibling_bounds(0), None);
            assert_eq!(l.prev_sibling_bounds(4), None);
        });
    }

    /// The text after `realign` at `line`, and the item lines it skipped.
    fn realigned(text: &str, line: usize) -> (String, Vec<usize>) {
        realigned_tw(text, line, 4)
    }

    const NINE: &str = "1. a\n2. b\n3. c\n4. d\n5. e\n6. f\n7. g\n8. h\n";

    #[test]
    fn realign_moves_a_sub_list_to_the_wider_text_column() {
        let t = format!("{NINE}9. i\n   - x\n10. j\n   - y\n     - z\n");
        let (out, skipped) = realigned(&t, 0);
        assert_eq!(out, format!("{NINE}9. i\n   - x\n10. j\n    - y\n      - z\n"));
        assert!(skipped.is_empty());
    }

    #[test]
    fn realign_keeps_what_is_already_inside_the_item() {
        // Nested by 4 is within T..=T+3 for `9.` and `10.` alike.
        let t = format!("{NINE}9. i\n    - x\n10. j\n    - y\n");
        assert_eq!(realigned(&t, 9).0, t);
        let t = "- a\n  - b\n    - c\n- d\n   - e\n";
        assert_eq!(realigned(t, 0).0, t);
    }

    #[test]
    fn realign_is_idempotent() {
        let t = format!("{NINE}9. i\n   - x\n10. j\n   - y\n\n   more\n   text\n");
        let (once, _) = realigned(&t, 0);
        assert_eq!(once, format!("{NINE}9. i\n   - x\n10. j\n    - y\n\n    more\n    text\n"));
        assert_eq!(realigned(&once, 0).0, once);
    }

    #[test]
    fn realign_pulls_in_a_block_past_the_text_column() {
        let t = "- a\n\n       para\n- b\n";
        assert_eq!(realigned(t, 3).0, "- a\n\n  para\n- b\n");
    }

    #[test]
    fn realign_moves_a_fence_whole() {
        let t = "- a\n\n ```\n code\n   more\n ```\n";
        assert_eq!(realigned(t, 0).0, "- a\n\n  ```\n  code\n    more\n  ```\n");
    }

    #[test]
    fn realign_skips_a_fence_it_would_put_4_columns_in() {
        let t = format!("{NINE}9. i\n10. j\n\n   ```\n   x\n   ```\n   - y\n11. k\n   - z\n");
        let (out, skipped) = realigned(&t, 0);
        assert_eq!(skipped, vec![9]);
        assert_eq!(out, format!("{NINE}9. i\n10. j\n\n   ```\n   x\n   ```\n   - y\n11. k\n    - z\n"));
    }

    #[test]
    fn realign_skips_display_math_it_would_put_4_columns_in() {
        let t = format!("{NINE}9. i\n10. j\n\n   $$\n   x\n   $$\n");
        let (out, skipped) = realigned(&t, 0);
        assert_eq!((out, skipped), (t.clone(), vec![9]));
    }

    #[test]
    fn realign_leaves_lazy_lines_and_blank_lines() {
        let t = format!("{NINE}9. i\n10. j\nlazy\n   - y\n\n\n11. k\n");
        assert_eq!(realigned(&t, 0).0, format!("{NINE}9. i\n10. j\nlazy\n    - y\n\n\n11. k\n"));
    }

    #[test]
    fn realign_works_from_a_nested_line_and_covers_the_whole_list() {
        let t = format!("{NINE}9. i\n   - x\n10. j\n   - y\n");
        assert_eq!(realigned(&t, 11).0, realigned(&t, 0).0);
    }

    #[test]
    fn realign_reads_a_tab_as_tab_width() {
        let t = format!("{NINE}9. i\n10. j\n\t- y\n");
        assert_eq!(realigned(&t, 0).0, t);
    }

    #[test]
    fn realign_moves_a_table_whole() {
        let t = format!("{NINE}9. i\n10. j\n\n   | a | b |\n   |---|---|\n   | 1 | 2 |\n");
        let want = format!("{NINE}9. i\n10. j\n\n    | a | b |\n    |---|---|\n    | 1 | 2 |\n");
        assert_eq!(realigned(&t, 0).0, want);
    }

    #[test]
    fn realign_takes_a_tab_indented_child_along() {
        // `- y` (a tab past 2 columns) is `- x`'s child, and moves with it
        // rather than becoming its sibling. A moved line gets spaces.
        let t = format!("{NINE}9. i\n10. j\n   - x\n  \t- y\n");
        let (out, skipped) = realigned(&t, 0);
        assert_eq!(out, format!("{NINE}9. i\n10. j\n    - x\n       - y\n"));
        assert!(skipped.is_empty());
    }

    #[test]
    fn realign_pulls_a_sub_item_shoin_shows_back_into_reach() {
        // 4+ past `9.`'s text column CommonMark reads `- x` as more of `i`;
        // Shoin shows a sub-item, and after the move both do.
        let t = format!("{NINE}9. i\n       - x\n");
        assert_eq!(realigned(&t, 0).0, format!("{NINE}9. i\n   - x\n"));
        // An item whose text opens a list, and a task box: already in reach.
        let t = "9. - x\n     - y\n10. [x] done\n       - z\n";
        assert_eq!(realigned(t, 0).0, t);
    }

    #[test]
    fn realign_steps_over_whitespace_only_lines() {
        let t = format!("{NINE}9. i\n10. j\n   - x\n\t\n   - y\n");
        assert_eq!(realigned(&t, 0).0, format!("{NINE}9. i\n10. j\n    - x\n\t\n    - y\n"));
    }

    /// `realigned` with another tab width.
    fn realigned_tw(text: &str, line: usize, tab_width: usize) -> (String, Vec<usize>) {
        let b = buf(text);
        let cache = BlockCache::build(&b, true);
        let r = Lists::new(&b, &cache.kinds, tab_width).realign(line, true).expect("in a list");
        let mut rope = b.rope.clone();
        for &(l, cols) in r.lines.iter().rev() {
            reindent(&mut rope, l, cols);
        }
        (rope.to_string(), r.skipped)
    }

    // Review findings, each reproduced before it was fixed.

    #[test]
    fn realign_starts_a_new_block_after_a_fence_heading_or_rule() {
        let t = "- a\n\n ```\n x\n ```\n     para\n";
        assert_eq!(realigned(t, 0).0, "- a\n\n  ```\n  x\n  ```\n     para\n");
        let t = "- a\n\n # H\n     para\n";
        assert_eq!(realigned(t, 0).0, "- a\n\n  # H\n     para\n");
    }

    #[test]
    fn realign_survives_a_tab_width_of_0() {
        realigned_tw("- a\n\t- b\n", 0, 0);
    }

    #[test]
    fn realign_never_flattens_code_it_would_pull_left() {
        let t = "- a\n\n\t\t```\n    y\n  z\nw\n\t\t```\n";
        let (out, skipped) = realigned_tw(t, 0, 4);
        assert_eq!((out, skipped), (t.to_string(), vec![0]));
    }

    #[test]
    fn realign_measures_an_item_and_its_children_alike() {
        let t = "- a\n  \t- b\n          - c\n";
        let (once, _) = realigned_tw(t, 0, 4);
        assert_eq!(realigned_tw(&once, 0, 4).0, once);
    }

    #[test]
    fn realign_rewrites_a_tab_other_tools_count_differently() {
        // Shoin counts 6 columns at tab_width 2; CommonMark, 12.
        let (out, _) = realigned_tw("10. j\n\t\t\t- y\n", 0, 2);
        assert_eq!(out, "10. j\n      - y\n");
    }

    #[test]
    fn realign_adds_nothing_to_an_empty_line_in_a_fence() {
        let t = "- a\n\n ```\n x\n\n y\n ```\n";
        assert_eq!(realigned(t, 0).0, "- a\n\n  ```\n  x\n\n  y\n  ```\n");
    }

    #[test]
    fn realign_blames_the_item_that_moved() {
        // Pulling `x` to 4 would swallow `para`: `x` stays, `w` is fixed.
        let t = "10. a\n         - x\n\n      para\n   - w\n";
        let (out, skipped) = realigned(t, 0);
        assert_eq!(skipped, vec![1]);
        assert_eq!(out, "10. a\n         - x\n\n      para\n    - w\n");
    }

    #[test]
    fn realign_reads_lines_ropey_splits_on_its_own() {
        let t = "x\u{2028}y\n\n- a\n\n       para\n- b\n\n       more\n";
        let (out, skipped) = realigned_tw(t, 3, 4);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(out, "x\u{2028}y\n\n- a\n\n  para\n- b\n\n  more\n");
    }

    #[test]
    fn realign_works_from_a_blank_line_inside_a_list() {
        assert!(with("- a\n\n- b\n", |l| l.realign(1, true)).is_some());
        assert!(with("- a\n\npara\n", |l| l.realign(1, true)).is_none());
    }

    #[test]
    fn realign_finds_no_list_outside_one() {
        assert_eq!(with("para\n\n- a\n", |l| l.realign(0, true)), None);
    }

    #[test]
    fn text_col_follows_commonmark() {
        with("- a\n10. b\n-\n1.      code\n  *  c\n", |l| {
            assert_eq!(l.text_col(0), 2);
            assert_eq!(l.text_col(1), 4);
            assert_eq!(l.text_col(2), 2);
            assert_eq!(l.text_col(3), 3);
            assert_eq!(l.text_col(4), 5);
        });
    }

    /// Random lists, mixing what makes structure hard: tabs, blank and
    /// whitespace-only lines, fences, lazy text, both kinds of marker.
    fn random_lists(seed: u64, lines: usize) -> String {
        let mut x = seed;
        let mut next = move |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n) as usize
        };
        let pads = ["", " ", "  ", "   ", "    ", "\t", "  \t", "      "];
        let bodies = ["- a", "* b", "1. c", "10. d", "text", "", "\t", "```", "$$", "# h", "- [x] t"];
        (0..lines)
            .map(|_| format!("{}{}\n", pads[next(pads.len() as u64)], bodies[next(bodies.len() as u64)]))
            .collect()
    }

    /// The caches change nothing: the one-pass `shallower_above` matches a
    /// scan back, and every answer is the same whatever order a `Lists` is
    /// asked in, and from one asked nothing before.
    #[test]
    fn cached_structure_matches_a_fresh_scan() {
        for seed in 1..200 {
            let text = random_lists(seed * 7919, 40);
            let b = buf(&text);
            let cache = BlockCache::build(&b, true);
            let (fwd, rev) = (Lists::new(&b, &cache.kinds, 4), Lists::new(&b, &cache.kinds, 4));
            let n = b.last_line();
            for l in (0..=n).filter(|&l| fwd.is_item(l)) {
                let d = fwd.indent(l);
                let scan = (0..l).rev().find(|&k| fwd.is_item(k) && fwd.indent(k) < d);
                assert_eq!(fwd.shallower_above(l), scan.filter(|_| d > 0), "seed {seed} line {l}:\n{text}");
            }
            let ask = |lists: &Lists, l: usize| {
                (
                    lists.item_bounds(l),
                    lists.parent(l),
                    lists.prev_sibling_bounds(l),
                    lists.next_sibling_bounds(l),
                    lists.level_items(l),
                    lists.first_child(l),
                )
            };
            let forward: Vec<_> = (0..=n).map(|l| ask(&fwd, l)).collect();
            let mut backward: Vec<_> = (0..=n).rev().map(|l| ask(&rev, l)).collect();
            backward.reverse();
            // Each answer also from a `Lists` asked nothing before it.
            let fresh: Vec<_> = (0..=n).map(|l| ask(&Lists::new(&b, &cache.kinds, 4), l)).collect();
            assert_eq!(forward, backward, "seed {seed}:\n{text}");
            assert_eq!(forward, fresh, "seed {seed}:\n{text}");
        }
    }
}
