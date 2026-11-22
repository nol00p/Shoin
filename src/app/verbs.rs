use super::*;

impl App {
    pub(super) fn repeat_dot(&mut self) {
        self.repeat_dot_with(None);
    }

    /// `.`, or `3.`: with a count, the change is replayed with that count in
    /// place of the one it was made with, and is remembered that way, so a
    /// later `.` repeats it with the new count too.
    pub(super) fn repeat_dot_with(&mut self, count: Option<usize>) {
        // `.` is kept out of every recording, but a `.` reached from inside a
        // replay must still not replay itself forever.
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
                Replay::Select { lines, cols, backwards } => self.replay_select(lines, cols, backwards),
                Replay::Insert => self.begin_insert_session(),
            }
        }
        self.replaying = false;
        // Reached through `run` (`3.`, `1.`, `".`, a bound key), the keys
        // that asked for the repeat are not the change to repeat next: the
        // replayed one is. They are dropped, and the revision marked as seen.
        self.drop_command_keys();
        self.mark_dot();
    }

    /// Take the keys of the command under way back out of the recording: a
    /// `.` is never part of a change, in Normal or in Visual.
    pub(super) fn drop_command_keys(&mut self) {
        if !self.replaying {
            self.recording.truncate(self.command_start);
        }
    }

    /// The document on screen and its revision now.
    pub(super) fn dot_mark(&self) -> DotMark {
        DotMark { doc: self.id, rev: self.buffer.revision }
    }

    /// Start counting edits from here, in the current document.
    pub(super) fn mark_dot(&mut self) {
        self.dot_rev = self.dot_mark();
    }

    /// Whether the command under way edited. Only an edit to the document it
    /// began in counts: a command that ends in another one (`:bn`, `gf`)
    /// moved, whatever that document's revision happens to be.
    pub(super) fn edited_since_mark(&self) -> bool {
        self.dot_rev.doc == self.id && self.dot_rev.rev != self.buffer.revision
    }

    /// A window or buffer key ends the command under way, whether or not it
    /// moves focus: a selection ends, as in vim, and nothing recorded so far
    /// becomes the change `.` repeats. `.` keeps the change before it, and
    /// repeats that in whatever document is current then.
    ///
    /// An insert session is left alone: it ends in its own document first,
    /// sealed and filed for `.` (`end_insert_session`), where focus leaves
    /// it.
    pub(super) fn end_recording(&mut self) {
        if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            // To Normal, even out of `-- (insert) VISUAL --`: the session it
            // came from is over too.
            self.visual_resumes_insert = false;
            self.leave_visual();
        }
        if self.mode == Mode::Insert {
            return;
        }
        self.pending.reset();
        self.drag_extent = false;
        if !self.replaying {
            self.recording.clear();
            self.mark_dot();
        }
    }

    /// A drag that starts a selection from Normal starts a command, as `v`
    /// would.
    pub(super) fn open_drag_recording(&mut self) {
        if self.replaying {
            return;
        }
        self.recording.clear();
        self.mark_dot();
        self.drag_extent = true;
    }

    /// The first key into a dragged selection writes the selection into the
    /// recording first, as a `Replay::Select`. That is how vim repeats a
    /// Visual operator: over as much text again, from the cursor.
    pub(super) fn describe_drag(&mut self) {
        if !std::mem::take(&mut self.drag_extent) {
            return;
        }
        let (Some(anchor), Mode::Visual) = (self.anchor, &self.mode) else {
            return;
        };
        let cur = self.buffer.cursor;
        let backwards = (cur.line, cur.col) < (anchor.line, anchor.col);
        let (lo, hi) = if backwards { (cur, anchor) } else { (anchor, cur) };
        let (lines, cols) = if lo.line == hi.line {
            (0, hi.col - lo.col)
        } else {
            (hi.line - lo.line, hi.col)
        };
        self.recording.push(Replay::Select { lines, cols, backwards });
    }

    /// `Replay::Select`: the selection again, from the cursor. Its end stops
    /// at the end of the text; `sync_after_input` clamps it as it does a
    /// dragged one.
    fn replay_select(&mut self, lines: usize, cols: usize, backwards: bool) {
        let from = self.buffer.cursor;
        let last = self.buffer.last_line();
        let to = if lines == 0 {
            Cursor::new(from.line, (from.col + cols).min(self.buffer.line_len(from.line)))
        } else {
            let line = (from.line + lines).min(last);
            Cursor::new(line, cols.min(self.buffer.line_len(line)))
        };
        self.enter_visual(Mode::Visual);
        let (anchor, cursor) = if backwards { (to, from) } else { (from, to) };
        self.anchor = Some(anchor);
        self.buffer.cursor = cursor;
        self.sync_after_input();
    }

    // ----------------------------------------------------------------- normal

    /// Normal and Visual mode are one line of code each now: hand the key to
    /// the grammar and run whatever command comes back.
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
        self.reading_scroll(|row, last_top, _| (row > last_top).then_some(last_top));
    }

    /// Move read-only mode's view: `to` answers with the row to start it at,
    /// from the row it starts at now, the last one it can and the pane's
    /// height (see [`frame::reading_view`]). The focused pane keeps that row
    /// as its scroll, and the cursor moves to the line it belongs to. False
    /// when there is no view to move: no frame drawn yet, or no text.
    pub(super) fn reading_scroll(&mut self, to: impl FnOnce(usize, usize, usize) -> Option<usize>) -> bool {
        let area = Rect { x: 0, y: 0, width: self.term_size.0, height: self.term_size.1 };
        let Some((row, top)) = frame::reading_view(self, area, to) else {
            return false;
        };
        if let Some(pane) = self.focused_pane() {
            pane.scroll.set(row);
        }
        self.buffer.cursor = top.cursor;
        self.reading.insert(self.focus_pane, top);
        true
    }

    /// The motions that scroll in read-only mode, made by rows rather than
    /// lines, as `less` does: `j` `k` a row, `Ctrl-d` `Ctrl-u` half a page,
    /// `Ctrl-f` `Ctrl-b` a page, `gg` `G` to either end. Every row is
    /// reached, a display block's laid out over more rows than the pane has
    /// included, which a cursor stepping by lines cannot do: the block has
    /// one position for all its rows. True when `m` was one of them and was
    /// made; with no view to move it is left to the line motions.
    pub(super) fn reading_move(&mut self, m: Motion, count: usize) -> bool {
        let down = match m {
            Motion::Down | Motion::HalfPageDown | Motion::PageDown | Motion::BufferEnd => true,
            Motion::Up | Motion::HalfPageUp | Motion::PageUp | Motion::BufferStart => false,
            _ => return false,
        };
        let moved = self.reading_scroll(|row, last_top, height| {
            let by = match m {
                Motion::Down | Motion::Up => count,
                Motion::HalfPageDown | Motion::HalfPageUp => (height / 2).max(1),
                Motion::PageDown | Motion::PageUp => height,
                _ => usize::MAX,
            };
            let row = row.min(last_top);
            Some(if down { row.saturating_add(by) } else { row.saturating_sub(by) })
        });
        // Clamped to the text already: the clamp after the key need not
        // sync the cache a second time to find that out.
        self.reading_clamped |= moved;
        moved
    }

    /// The lines shown raw: the cursor's (or the selection's), or none at all
    /// in read-only mode, where there is nothing to edit under a cursor.
    ///
    /// A closed display math block the set touches is laid out over several
    /// rows when the cursor is away (#77), so the set widens to all of it:
    /// the whole source shows while any line of it is being edited. Read off
    /// the block kinds, so they must be in step with the text: when they are
    /// not, nothing widens.
    pub(super) fn active_set(&self, anchor: Option<Cursor>) -> ActiveSet {
        if self.view {
            return ActiveSet::none();
        }
        let mut set = ActiveSet::compute(&self.mode, &self.buffer.cursor, anchor);
        let fresh = self.blocks.revision == self.buffer.revision;
        let laid_out = self.config.layout.conceal && self.buffer.syntax == crate::text::buffer::Syntax::Markdown;
        if fresh && laid_out {
            // Binary searches of the blocks kept with the kinds: a cursor move
            // does not walk the block it is in.
            let block = |line| crate::render::hidden::math_block_at(&self.blocks, line);
            let first = block(set.start);
            // A cursor alone (or a selection within one block) needs one.
            let last = match &first {
                Some(b) if b.contains(&set.end) => first.clone(),
                _ => block(set.end),
            };
            if let Some(b) = first {
                set.start = b.start;
            }
            if let Some(b) = last {
                set.end = b.end - 1;
            }
        }
        set
    }

    /// Enter or leave read-only mode, for every open buffer: a page to read,
    /// not to edit. Every line conceals, the caret is hidden, an edit is
    /// refused with a message, and `q` quits. `:view` leaves read-only mode
    /// at any time. `i` starts editing if pressed first or right after a
    /// refusal, and otherwise takes two presses.
    pub fn set_view(&mut self, on: bool) {
        self.view = on;
        // Coming back to read-only mode starts at the cursor's line.
        self.reading.clear();
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
        self.blocks = BlockCache::build(&self.buffer, self.config.markdown.math, self.config.editor.tab_width);
        // Widened over a display block the cursor starts on, as at startup.
        self.active = self.active_set(None);
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

    /// Decide where one undo step ends inside an insert session. A whole
    /// `i…Esc` as one step is too coarse, so the step is split at a
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
            // `iL` shifts its items as `al` shifts one: they nest under the
            // item before them, or leave their parent.
            return self.shift_list(op == Operator::Outdent, obj == 'L' && around, first.line, last.line);
        }
        if linewise {
            self.operate_linewise(op, first.line, last.line, first);
        } else {
            self.operate_charwise(op, first, last, true);
        }
    }

    /// The cursors bounding a text object, and whether it is linewise.
    fn object_bounds(&mut self, obj: char, around: bool) -> Option<(Cursor, Cursor, bool)> {
        // The list objects read line kinds, which must describe this text.
        self.refresh_blocks();
        if matches!(obj, 'l' | 'L') {
            if let Some(found) = self.quote_list_object(obj, around) {
                return found;
            }
        }
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
            _ => motion::resolve(&self.buffer, m, count, page, true, self.config.editor.tab_width),
        };
        let Some(res) = res else {
            return;
        };
        if res.kind == Linewise {
            let start = self.linewise_start(from, res.target);
            return self.operate_linewise(op, from.line, res.target.line, start);
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
                let at = self.linewise_start(start, start);
                return self.operate_linewise(op, start.line, line, at);
            }
            end = Cursor::new(line, self.buffer.line_len(line));
        }
        if op == Operator::Delete && self.delete_goes_linewise(start, end, inclusive) {
            return self.operate_linewise(op, start.line, end.line, start);
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
