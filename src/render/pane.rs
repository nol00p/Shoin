//! The window layout: a tree of splits whose leaves are editor panes.
//!
//! The file tree is NOT a leaf here — it is chrome, and takes its width from
//! the frame before these panes divide what is left.
//!
//! `frame::render` used to assume ONE text area. It now asks this tree for a
//! rect per leaf and draws each pane into its own, which is the whole of split
//! view: a pane is a view onto a document, and the document (`BufferState`) is
//! shared, so splitting costs a cursor's worth of state, not a copy.
//!
//! Each pane keeps its OWN cursor (and scroll), so two views of one file can
//! sit at different places in it — the document's `Buffer::cursor` is the live
//! one, belonging to whichever pane has focus, and every other pane holds the
//! position it will return to.
//!
//! Concealment is the one thing that stays shared: the row index lives in the
//! document's render cache, so the active (raw) line is the FOCUSED pane's, in
//! every pane showing that document. A pane showing a document that is not the
//! current one has no cursor in it at all and conceals throughout.
//!
//! The file tree is deliberately NOT in here. It is a fixed-width sidebar of
//! chrome rather than a view onto a document, and folding it in would mean
//! every leaf carrying a "which kind am I" tag for one special case.

use ratatui::layout::Rect;

use crate::text::cursor::Cursor;

/// Identifies a pane for as long as it exists. Not an index — panes come and go
/// and an index would shift under the focus.
pub type PaneId = usize;

/// One view onto a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub id: PaneId,
    /// Index into `App::docs`.
    pub doc: usize,
    /// Top visual row. A HINT — the renderer recomputes it from the cursor
    /// every frame — but per pane, so two views of one file scroll apart.
    pub scroll: usize,
    /// This pane's share of its parent split. See `EVEN`.
    pub weight: u16,
    /// Where this pane is looking. The FOCUSED pane's copy is written back
    /// every keystroke and the document's own `Buffer::cursor` is the live one;
    /// this is what the pane returns to when focus comes back, and what anchors
    /// its scroll while another pane has the cursor.
    ///
    /// It can go stale — an edit in another pane onto the same document moves
    /// text under it — so it is CLAMPED wherever it is read rather than
    /// tracked through every edit.
    pub cursor: Cursor,
}

/// A split, or a pane at the bottom of one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Leaf(Pane),
    /// `vertical` means the DIVIDER is vertical, so the children sit side by
    /// side — vim's `:vsplit`.
    Split { vertical: bool, weight: u16, children: Vec<Node> },
}

/// A node's share of its parent. Relative, so the layout scales with the
/// terminal; `resize` rewrites them as cell counts, which reproduces the sizes
/// you dragged to and still scales afterwards.
pub const EVEN: u16 = 100;

/// A pane narrower or shorter than this is not worth drawing, so a resize will
/// not push one below it.
const MIN_COLS: u16 = 8;
const MIN_ROWS: u16 = 3;

/// A pane's rect, and the divider drawn to separate it from its neighbour.
pub struct Geometry {
    pub panes: Vec<(PaneId, Rect)>,
    /// One cell wide (or tall) each; purely decorative.
    pub dividers: Vec<Rect>,
}

impl Node {
    pub fn leaf(id: PaneId, doc: usize) -> Node {
        Node::Leaf(Pane { id, doc, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) })
    }

    /// Divide `area` among the leaves. Children share their parent's space
    /// evenly, minus one cell per divider — even splits are what vim gives you
    /// too, and per-pane sizing has no keys to drive it yet.
    pub fn geometry(&self, area: Rect) -> Geometry {
        let mut geo = Geometry { panes: Vec::new(), dividers: Vec::new() };
        self.layout_into(area, &mut geo);
        geo
    }

    fn layout_into(&self, area: Rect, geo: &mut Geometry) {
        match self {
            Node::Leaf(p) => geo.panes.push((p.id, area)),
            Node::Split { vertical, children, .. } => {
                let n = children.len().max(1) as u16;
                let total = if *vertical { area.width } else { area.height };
                // n-1 dividers, then the rest shared out BY WEIGHT; the last
                // child takes the rounding slack so the far edge is flush.
                let gaps = n.saturating_sub(1);
                let usable = total.saturating_sub(gaps) as u32;
                let sum: u32 = children.iter().map(|c| c.weight().max(1) as u32).sum();
                let mut offset = 0u16;
                for (i, child) in children.iter().enumerate() {
                    let last = i + 1 == children.len();
                    let share = (usable * child.weight().max(1) as u32 / sum.max(1)) as u16;
                    let size = if last { total.saturating_sub(offset) } else { share };
                    let rect = if *vertical {
                        Rect { x: area.x + offset, y: area.y, width: size, height: area.height }
                    } else {
                        Rect { x: area.x, y: area.y + offset, width: area.width, height: size }
                    };
                    child.layout_into(rect, geo);
                    offset += size;
                    if !last {
                        geo.dividers.push(if *vertical {
                            Rect { x: area.x + offset, y: area.y, width: 1, height: area.height }
                        } else {
                            Rect { x: area.x, y: area.y + offset, width: area.width, height: 1 }
                        });
                        offset += 1;
                    }
                }
            }
        }
    }

    /// Every pane, left to right and top to bottom — the order `<C-w>w` cycles.
    pub fn ids(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.walk(&mut |p| out.push(p.id));
        out
    }

    pub fn count(&self) -> usize {
        let mut n = 0;
        self.walk(&mut |_| n += 1);
        n
    }

    pub fn weight(&self) -> u16 {
        match self {
            Node::Leaf(p) => p.weight,
            Node::Split { weight, .. } => *weight,
        }
    }

    fn set_weight(&mut self, w: u16) {
        match self {
            Node::Leaf(p) => p.weight = w,
            Node::Split { weight, .. } => *weight = w,
        }
    }

    /// `<C-w>=` — back to equal shares, all the way down.
    pub fn equalize(&mut self) {
        self.set_weight(EVEN);
        if let Node::Split { children, .. } = self {
            children.iter_mut().for_each(|c| c.equalize());
        }
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        match self {
            Node::Leaf(p) if p.id == id => Some(p),
            Node::Leaf(_) => None,
            Node::Split { children, .. } => children.iter().find_map(|c| c.pane(id)),
        }
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        match self {
            Node::Leaf(p) if p.id == id => Some(p),
            Node::Leaf(_) => None,
            Node::Split { children, .. } => children.iter_mut().find_map(|c| c.pane_mut(id)),
        }
    }

    fn walk(&self, f: &mut impl FnMut(&Pane)) {
        match self {
            Node::Leaf(p) => f(p),
            Node::Split { children, .. } => children.iter().for_each(|c| c.walk(f)),
        }
    }

    /// Split the pane holding `id`, putting `new` beside (or below) it.
    ///
    /// Splitting inside a split of the same direction EXTENDS it rather than
    /// nesting: three `<C-w>v`s give three columns, not a column containing a
    /// column, which is what makes the sizes come out even.
    pub fn split(&mut self, id: PaneId, vertical: bool, new: Pane) -> bool {
        match self {
            Node::Leaf(p) if p.id == id => {
                let old = std::mem::replace(p, new.clone());
                // The new split inherits the space the pane had, and its two
                // children share it evenly.
                let weight = old.weight;
                let mut old = old;
                old.weight = EVEN;
                *self = Node::Split {
                    vertical,
                    weight,
                    children: vec![Node::Leaf(old), Node::Leaf(new)],
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { vertical: dir, children, .. } => {
                if *dir == vertical {
                    if let Some(i) = children.iter().position(|c| matches!(c, Node::Leaf(p) if p.id == id)) {
                        children.insert(i + 1, Node::Leaf(new));
                        return true;
                    }
                }
                children.iter_mut().any(|c| c.split(id, vertical, new.clone()))
            }
        }
    }

    /// Remove a pane. A split left with one child collapses into it, so the
    /// tree never keeps a node that no longer divides anything.
    pub fn close(&mut self, id: PaneId) -> bool {
        let Node::Split { children, .. } = self else {
            return false; // a lone leaf is the whole window
        };
        let mut removed = children
            .iter()
            .position(|c| matches!(c, Node::Leaf(p) if p.id == id))
            .map(|i| {
                children.remove(i);
                true
            })
            .unwrap_or(false);
        if !removed {
            removed = children.iter_mut().any(|c| c.close(id));
        }
        if children.len() == 1 {
            *self = children.remove(0);
        }
        removed
    }

    /// Where the resize should land: the path from the root to the nearest
    /// ancestor split that divides the RIGHT way, plus which of its children
    /// leads to `id`. `None` when nothing above this pane splits that way —
    /// there is nothing to take the space from.
    fn resize_target(&self, id: PaneId, vertical: bool) -> Option<(Vec<usize>, usize)> {
        let Node::Split { vertical: dir, children, .. } = self else {
            return None;
        };
        for (i, child) in children.iter().enumerate() {
            let holds = child.pane(id).is_some();
            // Deeper matches win: resizing acts on the split closest to the
            // pane, so `<C-w>>` inside a column widens that column's panes
            // rather than the whole column.
            if let Some((mut path, idx)) = child.resize_target(id, vertical) {
                path.insert(0, i);
                return Some((path, idx));
            }
            if holds && *dir == vertical {
                return Some((Vec::new(), i));
            }
        }
        None
    }

    fn at_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        match path.split_first() {
            None => Some(self),
            Some((i, rest)) => match self {
                Node::Split { children, .. } => children.get_mut(*i)?.at_mut(rest),
                Node::Leaf(_) => None,
            },
        }
    }

    /// Every pane in this subtree, for measuring the space a split occupies.
    fn subtree_ids(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.walk(&mut |p| out.push(p.id));
        out
    }

    /// Grow (or shrink) the pane holding `id` by `delta` cells, taking the
    /// difference from a neighbour in the same split.
    ///
    /// Weights are rewritten as the CURRENT cell sizes first, so the change is
    /// exactly the number of cells asked for rather than some proportion of a
    /// proportion — and they stay relative afterwards, so the layout still
    /// scales when the terminal resizes.
    pub fn resize(&mut self, area: Rect, id: PaneId, vertical: bool, delta: i32) -> bool {
        let Some((path, child)) = self.resize_target(id, vertical) else {
            return false;
        };
        // The split's own rect: the union of the leaves under it.
        let geo = self.geometry(area);
        let ids = match self.at_mut(&path) {
            Some(node) => node.subtree_ids(),
            None => return false,
        };
        let rects: Vec<Rect> = geo
            .panes
            .iter()
            .filter(|(pid, _)| ids.contains(pid))
            .map(|(_, r)| *r)
            .collect();
        if rects.is_empty() {
            return false;
        }
        let x0 = rects.iter().map(|r| r.x).min().unwrap();
        let y0 = rects.iter().map(|r| r.y).min().unwrap();
        let x1 = rects.iter().map(|r| r.right()).max().unwrap();
        let y1 = rects.iter().map(|r| r.bottom()).max().unwrap();
        let span = if vertical { x1 - x0 } else { y1 - y0 };

        let Some(Node::Split { children, .. }) = self.at_mut(&path) else {
            return false;
        };
        let n = children.len();
        if n < 2 {
            return false;
        }
        // Current sizes in cells, dividers excluded.
        let usable = span.saturating_sub(n as u16 - 1) as i32;
        let sum: i32 = children.iter().map(|c| c.weight().max(1) as i32).sum();
        let mut sizes: Vec<i32> = children
            .iter()
            .map(|c| usable * c.weight().max(1) as i32 / sum.max(1))
            .collect();

        // Take from the next neighbour, or the previous one at the far edge.
        let victim = if child + 1 < n { child + 1 } else { child.wrapping_sub(1) };
        if victim >= n {
            return false;
        }
        let min = if vertical { MIN_COLS } else { MIN_ROWS } as i32;
        let delta = delta
            .min(sizes[victim] - min)
            .max(min - sizes[child]);
        if delta == 0 {
            return false;
        }
        sizes[child] += delta;
        sizes[victim] -= delta;
        for (c, size) in children.iter_mut().zip(sizes) {
            c.set_weight(size.max(1) as u16);
        }
        true
    }

    /// The pane nearest `from` in a direction, by rect geometry: the closest
    /// candidate whose centre lies that way, preferring the smallest gap.
    pub fn neighbor(&self, area: Rect, from: PaneId, dir: Dir) -> Option<PaneId> {
        let geo = self.geometry(area);
        let (_, here) = geo.panes.iter().find(|(id, _)| *id == from)?;

        geo.panes
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(id, r)| {
                let ok = match dir {
                    Dir::Left => r.right() <= here.x,
                    Dir::Right => r.x >= here.right(),
                    Dir::Up => r.bottom() <= here.y,
                    Dir::Down => r.y >= here.bottom(),
                };
                if !ok {
                    return None;
                }
                // The gap along the move first, then how far the candidate's
                // near edge sits from ours ACROSS it: stepping right into a
                // split column lands in its top pane, as vim does, rather than
                // in whichever one happens to be centred nearest.
                let along = match dir {
                    Dir::Left => here.x.saturating_sub(r.right()),
                    Dir::Right => r.x.saturating_sub(here.right()),
                    Dir::Up => here.y.saturating_sub(r.bottom()),
                    Dir::Down => r.y.saturating_sub(here.bottom()),
                };
                let across = match dir {
                    Dir::Left | Dir::Right => r.y.abs_diff(here.y),
                    Dir::Up | Dir::Down => r.x.abs_diff(here.x),
                };
                Some((along, across, *id))
            })
            .min()
            .map(|(_, _, id)| id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    pub fn from_key(c: char) -> Option<Dir> {
        Some(match c {
            'h' => Dir::Left,
            'l' => Dir::Right,
            'k' => Dir::Up,
            'j' => Dir::Down,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect { x: 0, y: 0, width: 80, height: 24 }
    }

    fn tree() -> Node {
        Node::leaf(1, 0)
    }

    #[test]
    fn one_pane_takes_the_whole_area() {
        let geo = tree().geometry(area());
        assert_eq!(geo.panes, vec![(1, area())]);
        assert!(geo.dividers.is_empty());
    }

    #[test]
    fn a_vertical_split_puts_panes_side_by_side_with_a_divider() {
        let mut t = tree();
        assert!(t.split(1, true, Pane { id: 2, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) }));
        let geo = t.geometry(area());
        assert_eq!(geo.panes.len(), 2);
        let (_, left) = geo.panes[0];
        let (_, right) = geo.panes[1];
        assert_eq!(left.width + right.width + 1, 80, "one column goes to the divider");
        assert_eq!(left.height, 24, "a vertical divider does not shorten them");
        assert_eq!(geo.dividers.len(), 1);
        assert_eq!(geo.dividers[0].x, left.right());
        assert_eq!(right.x, left.right() + 1, "no overlap");
    }

    #[test]
    fn a_horizontal_split_stacks_them() {
        let mut t = tree();
        t.split(1, false, Pane { id: 2, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) });
        let geo = t.geometry(area());
        let (_, top) = geo.panes[0];
        let (_, bottom) = geo.panes[1];
        assert_eq!(top.width, 80);
        assert_eq!(top.height + bottom.height + 1, 24);
        assert_eq!(bottom.y, top.bottom() + 1);
    }

    /// Splitting the same way again extends the existing split, so three panes
    /// are three equal columns rather than a column inside a column.
    #[test]
    fn repeated_splits_stay_flat_and_even() {
        let mut t = tree();
        t.split(1, true, Pane { id: 2, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) });
        t.split(2, true, Pane { id: 3, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) });
        assert_eq!(t.ids(), vec![1, 2, 3]);
        let geo = t.geometry(area());
        let widths: Vec<u16> = geo.panes.iter().map(|(_, r)| r.width).collect();
        assert_eq!(widths[0], widths[1], "even, bar the rounding slack at the end");
        assert_eq!(geo.dividers.len(), 2);
        // Everything adds up to the area, with no gaps or overlaps.
        let covered: u16 = widths.iter().sum::<u16>() + geo.dividers.len() as u16;
        assert_eq!(covered, 80);
    }

    #[test]
    fn closing_collapses_a_split_with_one_child_left() {
        let mut t = tree();
        t.split(1, true, Pane { id: 2, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) });
        t.split(2, false, Pane { id: 3, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) });
        assert_eq!(t.count(), 3);

        assert!(t.close(3));
        assert_eq!(t.count(), 2);
        assert!(matches!(t, Node::Split { vertical: true, .. }), "the inner split is gone");

        assert!(t.close(2));
        assert_eq!(t, Node::leaf(1, 0), "back to a bare leaf");

        assert!(!t.close(1), "the last pane never closes");
    }

    fn widths(t: &Node, a: Rect) -> Vec<u16> {
        t.geometry(a).panes.iter().map(|(_, r)| r.width).collect()
    }

    #[test]
    fn resizing_moves_the_boundary_by_the_cells_asked_for() {
        let mut t = tree();
        t.split(1, true, Pane { id: 2, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) });
        let a = area();
        let before = widths(&t, a);

        assert!(t.resize(a, 2, true, 6), "grow the right pane by six columns");
        let after = widths(&t, a);
        assert_eq!(after[1], before[1] + 6);
        assert_eq!(after[0], before[0] - 6, "taken from its neighbour");
        assert_eq!(after[0] + after[1] + 1, 80, "still exactly fills the area");

        // …and back the other way.
        assert!(t.resize(a, 2, true, -6));
        assert_eq!(widths(&t, a), before);
    }

    #[test]
    fn a_resize_stops_at_the_minimum_rather_than_squeezing_a_pane_out() {
        let mut t = tree();
        t.split(1, true, Pane { id: 2, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) });
        let a = area();
        assert!(t.resize(a, 2, true, 500), "clamped, not refused");
        let w = widths(&t, a);
        assert!(w[0] >= MIN_COLS, "the squeezed pane keeps {MIN_COLS} columns, got {}", w[0]);
        assert_eq!(w[0] + w[1] + 1, 80);
        // Nothing left to take: the next attempt reports that it did nothing.
        assert!(!t.resize(a, 2, true, 500));
    }

    #[test]
    fn resizing_needs_a_split_that_divides_that_way() {
        let mut t = tree();
        t.split(1, true, Pane { id: 2, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) });
        // Side by side, so there is no height to redistribute.
        assert!(!t.resize(area(), 2, false, 4));
        assert!(!tree().resize(area(), 1, true, 4), "a lone pane has no neighbour");
    }

    #[test]
    fn weights_are_relative_so_the_layout_scales_and_equalize_resets() {
        let mut t = tree();
        t.split(1, true, Pane { id: 2, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) });
        let a = area();
        t.resize(a, 2, true, 20);
        let wide = widths(&t, a);
        assert!(wide[1] > wide[0]);

        // Twice the terminal, same proportions (within a cell of rounding).
        let big = Rect { x: 0, y: 0, width: 160, height: 24 };
        let scaled = widths(&t, big);
        let ratio_before = wide[1] as f32 / (wide[0] + wide[1]) as f32;
        let ratio_after = scaled[1] as f32 / (scaled[0] + scaled[1]) as f32;
        assert!((ratio_before - ratio_after).abs() < 0.02, "{ratio_before} vs {ratio_after}");

        t.equalize();
        let even = widths(&t, a);
        // 79 usable columns cannot halve exactly; the last child takes the odd
        // one, as it takes all the rounding slack.
        assert!(even[0].abs_diff(even[1]) <= 1, "equal shares again: {even:?}");
    }

    #[test]
    fn neighbors_are_found_by_geometry() {
        let mut t = tree();
        t.split(1, true, Pane { id: 2, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) }); // 1 | 2
        t.split(2, false, Pane { id: 3, doc: 0, scroll: 0, weight: EVEN, cursor: Cursor::new(0, 0) }); // right column: 2 over 3
        let a = area();

        assert_eq!(t.neighbor(a, 1, Dir::Right), Some(2), "the top of the right column");
        assert_eq!(t.neighbor(a, 2, Dir::Left), Some(1));
        assert_eq!(t.neighbor(a, 2, Dir::Down), Some(3));
        assert_eq!(t.neighbor(a, 3, Dir::Up), Some(2));
        assert_eq!(t.neighbor(a, 1, Dir::Left), None, "nothing further left");
        assert_eq!(t.neighbor(a, 1, Dir::Up), None);
    }
}
