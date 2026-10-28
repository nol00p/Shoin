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

use super::buffer::Buffer;
use crate::render::markdown::block::{BlockKind, Marker};

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

/// A view of one document's lists. Cheap to build; holds no state of its own.
/// `kinds` must be the block cache for `buffer`'s current text.
pub struct Lists<'a> {
    buffer: &'a Buffer,
    kinds: &'a [BlockKind],
    tab_width: usize,
}

impl<'a> Lists<'a> {
    pub fn new(buffer: &'a Buffer, kinds: &'a [BlockKind], tab_width: usize) -> Self {
        Lists {
            buffer,
            kinds,
            tab_width,
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
        let (_, end) = self.bounds(start);
        let d = self.indent(start);
        let mut e = (start + 1..=end)
            .find(|&l| self.is_item(l) && self.indent(l) > d)
            .map_or(end, |child| child - 1);
        while e > start && self.blank(e) {
            e -= 1;
        }
        Some((start, e))
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
    // Nothing outside the tests calls it until moving items (list-items step 4).
    #[allow(dead_code)]
    pub fn next_sibling_bounds(&self, line: usize) -> Option<(usize, usize)> {
        self.next_sibling(self.item_at(line)?).map(|s| self.bounds(s))
    }

    /// Every sibling of the item containing `line`, first through last, each
    /// with its children — the `aL` range.
    pub fn level_bounds(&self, line: usize) -> Option<(usize, usize)> {
        let item = self.item_at(line)?;
        let mut first = item;
        while let Some(p) = self.prev_sibling(first) {
            first = p;
        }
        let mut last = item;
        while let Some(n) = self.next_sibling(last) {
            last = n;
        }
        Some((first, self.bounds(last).1))
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
        indent_cols(&self.buffer.line_text(line), self.tab_width)
    }

    /// The extent of the item whose item line is `start`.
    fn bounds(&self, start: usize) -> (usize, usize) {
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

    /// The nearest item line above `line` with a smaller indent than it.
    fn shallower_above(&self, line: usize) -> Option<usize> {
        let d = self.indent(line);
        if d == 0 {
            return None;
        }
        (0..line)
            .rev()
            .find(|&l| self.is_item(l) && self.indent(l) < d)
    }

    /// Nearest shallower item above `item` whose bounds contain it.
    fn parent_of(&self, item: usize) -> Option<usize> {
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
        let cache = BlockCache::build(&b);
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
    fn own_bounds_stop_before_the_first_child() {
        let t = "- a\n  wrapped\n\n  para\n\n  - b\n    - c\n- d\n";
        with(t, |l| {
            assert_eq!(l.own_bounds(0), Some((0, 3)));
            assert_eq!(l.own_bounds(3), Some((0, 3)));
            assert_eq!(l.own_bounds(5), Some((5, 5)));
            assert_eq!(l.own_bounds(7), Some((7, 7)));
        });
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
}
