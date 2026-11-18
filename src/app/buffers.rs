use super::*;

impl App {
    // -------------------------------------------------------------- buffers

    /// Note that a file may have changed on disk; see `disk_epoch`.
    pub(super) fn disk_changed(&mut self) {
        self.disk_epoch = self.disk_epoch.wrapping_add(1);
    }

    /// Make document `i` current. The one being left keeps its cursor, scroll
    /// and parse cache, so coming back costs nothing.
    pub(super) fn switch_to(&mut self, i: usize) {
        if i >= self.docs.len() {
            return;
        }
        // Recorded HERE rather than at each caller: every way of reaching
        // another document — `:b`, the switcher, following a link, closing a
        // buffer — goes through this one function, and each of them is a move
        // `<C-^>` should be able to undo.
        let from = self.current();
        if from != i {
            self.alternate = Some(from);
        }
        let cursor = self.docs[i].buffer.cursor;
        if let Some(pane) = self.layout.pane_mut(self.focus_pane) {
            pane.doc = i;
            pane.scroll.set(0);
            // A pane arriving at a document picks up where that document was
            // left, not where this pane was in the last one.
            pane.cursor = cursor;
        }
        // However it was reached, a switch is not a change `.` repeats, and
        // it lands in Normal mode: out of a selection, out of an insert.
        self.end_recording();
        self.mode = Mode::Normal;
        // Coming back to a document re-reads its embeds: the notes they name
        // may have been written, or created, while it was out of sight.
        self.disk_changed();
        // The block cache of a document edited while it was in the background
        // (there is no such path today, but reload could add one) resyncs on
        // the next frame through the usual revision check.
        self.active = self.active_set(None);
        self.touch_status();
    }

    /// `:bn` / `:bp` — the buffer list is a ring.
    pub(super) fn cycle_buffer(&mut self, forward: bool) {
        let n = self.docs.len();
        if n < 2 {
            self.notify("only one buffer", FlashKind::Info);
            return;
        }
        let cur = self.current();
        let next = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
        self.switch_to(next);
        self.notify(self.doc_summary(), FlashKind::Info);
    }

    /// `:b <name>` — switch to the open buffer whose name contains `name`.
    pub(super) fn switch_by_name(&mut self, name: &str) {
        if let Ok(n) = name.parse::<usize>() {
            if n >= 1 && n <= self.docs.len() {
                self.switch_to(n - 1);
                return;
            }
        }
        let lower = name.to_lowercase();
        let hit = self
            .docs
            .iter()
            .position(|d| d.name().to_lowercase().contains(&lower));
        match hit {
            Some(i) => self.switch_to(i),
            None => self.notify(format!("no buffer matching {name:?}"), FlashKind::Error),
        }
    }

    /// `:bd` — close the current document. The last one never closes: an editor
    /// with no buffer has nothing to draw.
    pub(super) fn close_buffer(&mut self, force: bool) {
        if self.buffer.modified && !force {
            self.notify("unsaved changes — :bd! to discard", FlashKind::Error);
            return;
        }
        if self.docs.len() == 1 {
            self.notify("last buffer — :q to quit", FlashKind::Error);
            return;
        }
        let gone = self.current();
        self.docs.remove(gone);
        // Every pane showing a document after the removed one shifts down; the
        // ones that were showing the removed document fall back to its
        // neighbour, which is what `min` does here.
        for id in self.layout.ids() {
            if let Some(pane) = self.layout.pane_mut(id) {
                if pane.doc > gone {
                    pane.doc -= 1;
                }
                pane.doc = pane.doc.min(self.docs.len() - 1);
            }
        }
        // The alternate shifts with the panes, for the same reason — and the
        // document that was closed stops being somewhere to go back to.
        self.alternate = match self.alternate {
            Some(a) if a == gone => None,
            Some(a) if a > gone => Some(a - 1),
            other => other,
        };
        let to = gone.min(self.docs.len() - 1);
        // `switch_to` would record the neighbour we are landing on, which is
        // not where `<C-^>` should go back to after a close.
        let keep = self.alternate;
        self.switch_to(to);
        self.alternate = keep;
        self.notify(self.doc_summary(), FlashKind::Info);
    }

    /// `3/5  notes.md` — where you are in the buffer list.
    pub(super) fn doc_summary(&self) -> String {
        format!(
            "{}/{}  {}",
            self.current() + 1,
            self.docs.len(),
            self.buffer.display_name()
        )
    }

    /// `:ls` — the open buffers on one line, the current one marked.
    pub(super) fn buffer_list(&self) -> String {
        self.docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let mark = if i == self.current() { "▸" } else { " " };
                let dirty = if d.buffer.modified { self.config.glyphs.modified.as_str() } else { "" };
                format!("{mark}{} {}{dirty}", i + 1, d.name())
            })
            .collect::<Vec<_>>()
            .join("   ")
    }

    /// Every open document that still has unsaved changes.
    /// The ONE place "may we quit?" is answered, and it answers for EVERY open
    /// document — not just the one on screen.
    ///
    /// `:q` and `<leader>q` used to disagree: the command counted every buffer
    /// and the action looked only at `self.buffer`, so quitting from a binding
    /// discarded a modified background buffer without a word.
    /// `:q` — with more than one buffer open this closes the CURRENT one and
    /// stays in the editor, so a session unwinds the way it was built up: one
    /// file at a time. The last buffer takes the editor with it.
    ///
    /// `:Q` leaves in one step instead, and `!` discards either way.
    pub(super) fn try_quit(&mut self, force: bool) {
        if !force && self.buffer.modified {
            self.notify("unsaved changes — :q! to discard", FlashKind::Error);
            return;
        }
        if self.docs.len() > 1 {
            return self.close_buffer(force);
        }
        self.quit = true;
    }

    /// `:Q` (also `:qa`) — leave with every buffer at once, but only when
    /// nothing would be lost. The COUNT is the useful part of the refusal: it
    /// says how much is still unsaved without making the reader walk the list
    /// to find out.
    pub(super) fn quit_all(&mut self, force: bool) {
        let unsaved = self.unsaved_count();
        if force || unsaved == 0 {
            self.quit = true;
            return;
        }
        let what = if unsaved == 1 {
            "1 buffer has unsaved changes".to_string()
        } else {
            format!("{unsaved} buffers have unsaved changes")
        };
        self.notify(format!("{what} — :Q! to discard"), FlashKind::Error);
    }

    fn unsaved_count(&self) -> usize {
        self.docs.iter().filter(|d| d.buffer.modified).count()
    }
}
