use super::*;

impl App {
    pub(super) fn repeat_dot(&mut self) {
        self.repeat_dot_with(None);
    }

    /// `.`, or `3.`: with a count, the change is replayed with that count in
    /// place of the one it was made with, and is remembered that way, so a
    /// later `.` repeats it with the new count too.
    pub(super) fn repeat_dot_with(&mut self, count: Option<usize>) {
        // A `.` that ended up inside `dot` (in Visual) must not replay itself
        // forever.
        if self.dot.is_empty() || self.replaying {
            return;
        }
        if let Some(n) = count {
            self.dot = with_count(&self.dot, n);
        }
        self.replaying = true;
        for input in self.dot.clone() {
            match input {
                Replay::Key(key) => self.on_key(key),
                Replay::Paste(text) => self.on_paste(text),
            }
        }
        self.replaying = false;
        // Reached through `run` (`3.`, `1.`, `".`, a bound key), the keys
        // that asked for the repeat are not the change to repeat next: the
        // replayed one is. Marking the revision as seen keeps them out.
        self.dot_rev = self.buffer.revision;
    }

    // ----------------------------------------------------------------- normal

    /// Normal and Visual mode are one line of code each now: hand the key to
    /// the grammar (SPEC.md §7.2) and run whatever command comes back.
    pub(super) fn on_key_normal(&mut self, key: KeyEvent) {
        // `q` leaves a page you are only reading, as it leaves `less`. Only as
        // a whole command: `"q` and `fq` still get their q.
        if self.view
            && self.pending.is_clean()
            && key.code == KeyCode::Char('q')
            && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            self.try_quit(false);
            return;
        }
        self.feed_pending(key, Table::Normal);
    }

    pub(super) fn on_key_visual(&mut self, key: KeyEvent) {
        self.feed_pending(key, Table::Visual);
    }

    pub(super) fn feed_pending(&mut self, key: KeyEvent, table: Table) {
        let Some(k) = Key::from_event(key) else {
            return;
        };
        // The machine is borrowed out of `self` for the call: it holds no
        // editor state, so it cannot need `&mut self` while resolving.
        let mut pending = std::mem::take(&mut self.pending);
        let resolution = pending.feed(k, &self.keymap, table);
        self.pending = pending;
        match resolution {
            Resolution::Pending => {}
            Resolution::Invalid => {}
            Resolution::Command(cmd) => self.run(cmd),
        }
    }

    // ------------------------------------------------------ pending-state verbs

    /// `u` / `:undo` / a bound `Action::Undo` — all three flash the same hint
    /// when there is nothing left to take back.
    pub(super) fn undo_or_warn(&mut self) {
        if !self.buffer.undo() {
            self.notify("already at oldest change", FlashKind::Info);
        }
    }

    /// `Ctrl-r` and friends. Mirror of `undo_or_warn`.
    pub(super) fn redo_or_warn(&mut self) {
        if !self.buffer.redo() {
            self.notify("already at newest change", FlashKind::Info);
        }
    }

    /// In read-only mode the cursor is the top of the screen (see
    /// `layout::scroll_offset`). Past the row where the last screenful starts
    /// it would move with nothing on screen moving, and `k` would then seem
    /// dead until it came back — so it stops there, as `less` does at the end.
    pub(super) fn clamp_reading_cursor(&mut self) {
        let height = self.viewport_height();
        let line = {
            let cache = self.cache.borrow();
            let last_top = cache.total_rows().saturating_sub(height);
            let (row, _) = cache.locate(self.buffer.cursor.line, 0);
            if row <= last_top {
                return;
            }
            match cache.row(last_top) {
                Some(r) => r.line(),
                None => return,
            }
        };
        self.buffer.cursor = Cursor::new(line, 0);
    }

    /// The lines shown raw: the cursor's (or the selection's), or none at all
    /// in read-only mode, where there is nothing to edit under a cursor.
    pub(super) fn active_set(&self, anchor: Option<Cursor>) -> ActiveSet {
        if self.view {
            ActiveSet::none()
        } else {
            ActiveSet::compute(&self.mode, &self.buffer.cursor, anchor)
        }
    }

    /// Enter or leave read-only mode, for every open buffer: a page to read,
    /// not to edit. Every line conceals, the caret is hidden, an edit is
    /// refused with a message, and `q` quits. Leaving it is `i` after that
    /// message, or `:view` again.
    pub fn set_view(&mut self, on: bool) {
        self.view = on;
        // Entering says "i to edit", so the very next `i` does exactly that.
        self.edit_offered = on;
        for doc in &mut self.docs {
            doc.buffer.read_only = on;
        }
        if on {
            self.leave_visual();
            self.notify("read-only — q to quit, i to edit", FlashKind::Info);
        } else {
            self.notify("editing", FlashKind::Info);
        }
        self.active = self.active_set(self.anchor);
        self.needs_redraw = true;
    }

    /// Start on `buffer` instead of the blank page: text piped in on stdin.
    pub fn open_unnamed(&mut self, buffer: Buffer) {
        self.buffer = buffer;
        // Built here, not by `refresh_blocks`: both buffers are at revision 0,
        // which it would take to mean nothing changed.
        self.blocks = BlockCache::build(&self.buffer, self.config.markdown.math);
    }

    /// Turn zen mode on or off, remembering what it hid so leaving it restores
    /// the user's own settings rather than a hardcoded default.
    /// Whether zen mode is on: no chrome, the hint bar included.
    pub fn zen_on(&self) -> bool {
        self.zen.is_some()
    }

    /// The command being typed, for the hint bar to say what can follow it.
    pub fn pending(&self) -> &Pending {
        &self.pending
    }

    pub fn set_zen(&mut self, on: bool) {
        match (on, self.zen.take()) {
            (true, None) => {
                self.zen = Some(ZenState {
                    status: self.config.status.enabled,
                    scroll_hint: self.config.layout.scroll_hint,
                });
                self.config.status.enabled = false;
                self.config.layout.scroll_hint = false;
            }
            (true, Some(saved)) => self.zen = Some(saved),
            (false, Some(saved)) => {
                self.config.status.enabled = saved.status;
                self.config.layout.scroll_hint = saved.scroll_hint;
            }
            (false, None) => {}
        }
    }

    /// Re-apply zen over a freshly loaded config, capturing the new file's
    /// values as what leaving zen should restore.
    pub(super) fn reapply_zen(&mut self) {
        if self.zen.is_some() {
            self.zen = None;
            self.set_zen(true);
        }
    }

    /// Decide where one undo step ends inside an insert session. SPEC.md §4:
    /// a whole `i…Esc` as one step is too coarse, so the step is split at a
    /// word boundary, at a newline, and after `editor.undo_coalesce_ms` of
    /// thinking time — the three places a writer would expect `u` to stop.
    pub(super) fn coalesce_insert(&mut self, boundary: bool) {
        let now = Instant::now();
        let idle = self
            .last_insert
            .is_some_and(|t| now.duration_since(t) >= self.coalesce_gap());
        if boundary || idle {
            self.buffer.history.split();
        }
        self.last_insert = Some(now);
    }

    fn coalesce_gap(&self) -> Duration {
        Duration::from_millis(self.config.editor.undo_coalesce_ms.max(1))
    }

    /// Whether the cursor sits on whitespace (or past the end of its line).
    fn cursor_on_blank(&self) -> bool {
        let c = self.buffer.cursor;
        self.buffer
            .line_text(c.line)
            .chars()
            .nth(c.col)
            .is_none_or(|ch| ch.is_whitespace())
    }

    /// `vi(` / `vap`: make the object the selection, ready for any verb.
    pub(super) fn select_object(&mut self, obj: char, around: bool) {
        let Some((first, last, linewise)) = self.object_bounds(obj, around) else {
            return;
        };
        if !matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            self.enter_visual(if linewise { Mode::VisualLine } else { Mode::Visual });
        } else if linewise {
            self.mode = Mode::VisualLine;
        }
        self.anchor = Some(first);
        self.buffer.cursor = last;
    }

    /// Apply an operator over a text object. Objects reach the same `operate_*`
    /// helpers as motions do — an object is just another way of naming a range,
    /// so `diw` and `dw` share every line of their edit, register and undo
    /// behavior.
    pub(super) fn operate_object(&mut self, op: Operator, obj: char, around: bool, count: usize) {
        let Some((first, mut last, linewise)) = self.object_bounds(obj, around) else {
            return;
        };
        // `d2aw`: a count takes that many words, each one resolved from just
        // past the last. Only the word objects count; the rest are one thing.
        if matches!(obj, 'w' | 'W') && count > 1 {
            // `object_bounds` is cheap to call again: the block refresh is a
            // no-op at the same revision, and `Lists` is lazy.
            let home = self.buffer.cursor;
            let mut end = self.buffer.char_index(last) + 1;
            for _ in 1..count {
                if end >= self.buffer.rope.len_chars() {
                    break;
                }
                self.buffer.cursor = self.idx_to_cursor(end);
                match self.object_bounds(obj, around) {
                    Some((_, l, _)) if self.buffer.char_index(l) + 1 > end => {
                        end = self.buffer.char_index(l) + 1
                    }
                    _ => break,
                }
            }
            self.buffer.cursor = home;
            last = self.idx_to_cursor(end - 1);
        }
        if matches!(op, Operator::Indent | Operator::Outdent) && matches!(obj, 'l' | 'L') {
            return self.shift_list(op == Operator::Outdent, obj == 'L', first.line, last.line);
        }
        if linewise {
            self.operate_linewise(op, first.line, last.line);
        } else {
            self.operate_charwise(op, first, last, true);
        }
    }

    /// The cursors bounding a text object, and whether it is linewise.
    fn object_bounds(&mut self, obj: char, around: bool) -> Option<(Cursor, Cursor, bool)> {
        // The list objects read line kinds, which must describe this text.
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        let range = object::resolve(&self.buffer, &lists, obj, around)?;
        if range.end <= range.start {
            return None;
        }
        Some((
            self.idx_to_cursor(range.start),
            self.idx_to_cursor(range.end - 1),
            range.linewise,
        ))
    }

    pub(super) fn apply_operator(&mut self, op: Operator, m: Motion, count: usize) {
        use motion::MotionKind::{Exclusive, Inclusive, Linewise};
        let from = self.buffer.cursor;
        let page = self.viewport_height();
        let res = match (op, m) {
            // vim's one irregular verb: on a non-blank, `cw` changes to the
            // END of the word rather than up to the next one, so it does not
            // swallow the space it is about to type over.
            (Operator::Change, Motion::WordForward { big }) if !self.cursor_on_blank() => {
                Some(motion::MotionResult {
                    target: motion::change_word_end(&self.buffer, count, big),
                    kind: Inclusive,
                })
            }
            _ => motion::resolve(&self.buffer, m, count, page, true),
        };
        let Some(res) = res else {
            return;
        };
        if res.kind == Linewise {
            return self.operate_linewise(op, from.line, res.target.line);
        }
        let inclusive = res.kind == Inclusive;
        let (start, mut end) = if (res.target.line, res.target.col) < (from.line, from.col) {
            (res.target, from)
        } else {
            (from, res.target)
        };
        // vim's rule for every exclusive motion, either way it moves
        // (`:help exclusive`): a range ending in column 0 of a later line ends
        // at the end of the line before instead — and is whole lines if it
        // started in the indent. So `dw` on a line's last word and `db` from
        // a line start both leave the line break alone.
        if res.kind == Exclusive && end.col == 0 && end.line > start.line {
            let line = end.line - 1;
            if start.col <= self.first_non_blank(start.line) {
                return self.operate_linewise(op, start.line, line);
            }
            end = Cursor::new(line, self.buffer.line_len(line));
        }
        if op == Operator::Delete && self.delete_goes_linewise(start, end, inclusive) {
            return self.operate_linewise(op, start.line, end.line);
        }
        self.operate_charwise(op, start, end, inclusive)
    }

    /// vim's `op_delete` rule: a charwise delete over more than one line that
    /// starts in the first line's indent and ends with nothing but blanks
    /// after it removes whole lines instead, so `d3w` from the start of
    /// `a / b c / d` leaves `d` rather than an empty line above it.
    fn delete_goes_linewise(&self, start: Cursor, end: Cursor, inclusive: bool) -> bool {
        if end.line <= start.line {
            return false;
        }
        let in_indent = start.col <= self.first_non_blank(start.line);
        let rest_blank = self
            .buffer
            .line_text(end.line)
            .chars()
            .skip(end.col + usize::from(inclusive))
            .all(char::is_whitespace);
        in_indent && rest_blank
    }
}
