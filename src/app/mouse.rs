use super::*;

impl App {
    /// Enable or disable terminal mouse capture at runtime. Not under
    /// `cargo test`, where stdout is the test runner's terminal, not ours.
    pub(super) fn set_mouse_capture(&self, on: bool) {
        use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        if cfg!(test) {
            return;
        }
        let Some(_frame) = crate::recover::frame() else { return };
        let _ = if on {
            execute!(std::io::stdout(), EnableMouseCapture)
        } else {
            execute!(std::io::stdout(), DisableMouseCapture)
        };
    }

    /// The whole terminal, for mapping a mouse position through the layout.
    fn mouse_area(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.term_size.0,
            height: self.term_size.1,
        }
    }

    /// Left-click positions the cursor and drag-selects from it; the wheel
    /// scrolls the page, as `Ctrl-e` / `Ctrl-y` do.
    pub(super) fn on_mouse(&mut self, m: MouseEvent) {
        if !self.config.input.mouse {
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A press ends whatever the previous one started, wherever it
                // lands — including in the tree, which arms no drag.
                self.drag_from = None;
                let tw = if self.tree.is_some() {
                    crate::tree::WIDTH.min(self.term_size.0 / 2)
                } else {
                    0
                };
                if self.tree.is_some() && m.column < tw {
                    self.tree_click(m.row);
                    return;
                }
                let area = self.mouse_area();
                // A click focuses the pane it landed in first — otherwise it
                // would be mapped through the geometry of a different one.
                if let Some(id) = frame::pane_at(self, area, m.column, m.row) {
                    self.focus_pane_id(id);
                    if let Some(t) = self.tree.as_mut() {
                        t.focused = false;
                    }
                }
                if let Some(cursor) = frame::locate_click(self, area, m.column, m.row) {
                    self.buffer.cursor = cursor;
                    // A click abandons any half-typed command.
                    self.pending.reset();
                    // ...and drops any selection, rather than extending it to
                    // where you clicked. Vim does the same: the click puts the
                    // cursor down, and the drag that may follow starts there.
                    if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
                        self.leave_visual();
                    }
                    // Normal and Insert both arm a drag, with the document in
                    // front. Under an overlay the text being dragged over is
                    // not the text on screen, so nothing is armed there.
                    let overlaid =
                        self.help.is_some() || self.diff.is_some() || self.finder.is_some();
                    if matches!(self.mode, Mode::Normal | Mode::Insert) && !overlaid {
                        self.drag_from = Some(cursor);
                    }
                }
            }
            // Dragging out of that press is Visual mode, charwise, anchored
            // where the button went down.
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(from) = self.drag_from else {
                    return;
                };
                // The press armed this in Normal or Insert, but a key since
                // may have moved on — into `:`, or a tree prompt. Those own
                // the input.
                if !matches!(self.mode, Mode::Normal | Mode::Insert | Mode::Visual) {
                    return;
                }
                let area = self.mouse_area();
                let Some(cursor) = frame::locate_drag(self, area, m.column, m.row) else {
                    return;
                };
                // Entering only once the drag has actually reached another
                // cell keeps a shaky hand — a Drag event on the pressed cell —
                // from turning every click into a one-character selection.
                if cursor == from && self.mode != Mode::Visual {
                    return;
                }
                if self.mode != Mode::Visual {
                    // Dragged out of an insert session, the selection remembers
                    // to go back into it — vim's `-- (insert) VISUAL --`. The
                    // insert group is left open deliberately: the operator and
                    // the typing that resumes after it undo as one step, the
                    // same shape `cw` already has.
                    let resumes_insert = self.mode == Mode::Insert;
                    self.enter_visual(Mode::Visual);
                    self.visual_resumes_insert = resumes_insert;
                    // Naming the mode once, on the way in. The caret carries it
                    // from there; nothing stays on screen.
                    if resumes_insert {
                        self.notify("-- (insert) visual --", FlashKind::Info);
                    }
                }
                self.anchor = Some(from);
                self.buffer.cursor = cursor;
            }
            // The selection outlives the button: releasing leaves Visual mode
            // up, so `y`, `d` and `<leader>b` all have something to act on.
            MouseEventKind::Up(MouseButton::Left) => self.drag_from = None,
            MouseEventKind::ScrollDown => self.scroll_view(3),
            MouseEventKind::ScrollUp => self.scroll_view(-3),
            _ => {}
        }
    }
}
