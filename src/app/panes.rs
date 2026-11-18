use super::*;

impl App {
    // ---------------------------------------------------------------- panes

    /// The document the focused pane is showing. Every `self.buffer` in this
    /// file resolves through here (see the `Deref` impl).
    pub fn current(&self) -> usize {
        self.layout
            .pane(self.focus_pane)
            .map(|p| p.doc)
            .unwrap_or(0)
            .min(self.docs.len().saturating_sub(1))
    }

    pub(super) fn focused_pane(&self) -> Option<&Pane> {
        self.layout.pane(self.focus_pane)
    }

    /// The focused pane's scroll position — a hint the renderer refines.
    pub fn scroll_hint(&self) -> usize {
        self.focused_pane().map(|p| p.scroll.get()).unwrap_or(0)
    }

    /// The whole area panes are laid out in — the frame minus the tree sidebar
    /// and the status line, matching what the renderer does.
    pub(super) fn pane_area(&self) -> Rect {
        crate::render::frame::pane_area(self, Rect {
            x: 0,
            y: 0,
            width: self.term_size.0,
            height: self.term_size.1,
        })
    }

    /// Split, for tests in other modules (the renderer's).
    #[cfg(test)]
    pub fn split_pane_for_test(&mut self, vertical: bool) {
        self.split_pane(vertical);
    }

    /// `<leader>sv` / `<C-w>v` / `:vsplit` — a second view onto the same
    /// document, side by side (or stacked, for a horizontal split).
    pub(super) fn split_pane(&mut self, vertical: bool) {
        let doc = self.current();
        let scroll = self.focused_pane().map(|p| p.scroll.get()).unwrap_or(0);
        let cursor = self.buffer.cursor;
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        if self.layout.split(self.focus_pane, vertical, Pane { id, doc, scroll: std::cell::Cell::new(scroll), weight: crate::render::pane::EVEN, cursor }) {
            self.focus_pane = id;
            self.touch_status();
        }
    }

    /// Close the focused pane. The last one stays: with no pane there is
    /// nothing to draw, and the document is not what is being closed here.
    pub(super) fn close_pane(&mut self) -> bool {
        if self.layout.count() < 2 {
            return false;
        }
        let gone = self.focus_pane;
        // Pick the neighbour BEFORE the tree forgets where this pane was.
        let area = self.pane_area();
        let next = self
            .layout
            .neighbor(area, gone, Dir::Left)
            .or_else(|| self.layout.neighbor(area, gone, Dir::Up))
            .or_else(|| self.layout.neighbor(area, gone, Dir::Right))
            .or_else(|| self.layout.neighbor(area, gone, Dir::Down));
        self.layout.close(gone);
        self.focus_pane = next.unwrap_or_else(|| self.layout.ids()[0]);
        self.sync_after_input();
        self.touch_status();
        true
    }

    /// `<C-w>o` — close every pane but this one.
    pub(super) fn only_pane(&mut self) {
        let Some(pane) = self.focused_pane().cloned() else {
            return;
        };
        self.layout = Node::Leaf(pane);
        self.touch_status();
    }

    pub(super) fn focus_pane_id(&mut self, id: PaneId) {
        let Some(pane) = self.layout.pane(id) else { return };
        if id == self.focus_pane {
            return;
        }
        let cursor = pane.cursor;
        // Typing goes on in the pane focus moves to, as a session of its own:
        // the one here ends, as Esc would, with its own undo step.
        let inserting = self.mode == Mode::Insert;
        self.end_insert_session();
        self.focus_pane = id;
        // Restore where this pane was looking. `sync_after_input` clamps it,
        // which is what catches a saved position that edits elsewhere have run
        // off the end of.
        self.buffer.cursor = cursor;
        if inserting {
            self.begin_insert_session();
        }
        // A selection belongs to the pane it was made in: `sync_after_input`
        // ends it here.
        self.sync_after_input();
        self.touch_status();
    }

    /// `<C-w>w` — cycle through the panes in layout order.
    pub(super) fn cycle_pane(&mut self) {
        let ids = self.layout.ids();
        if ids.len() < 2 {
            return;
        }
        let i = ids.iter().position(|id| *id == self.focus_pane).unwrap_or(0);
        self.focus_pane_id(ids[(i + 1) % ids.len()]);
    }
}
