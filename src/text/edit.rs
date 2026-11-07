//! Primitive edits. Every mutation of the rope goes through this module so
//! that history recording and revision bumping happen in exactly one place.
//! SPEC.md §4.

use super::buffer::Buffer;
use super::cursor::Cursor;
use super::history::Change;

impl Buffer {
    /// Returns false if the target range is read-only.
    pub fn insert_str(&mut self, at: Cursor, text: &str) -> bool {
        let idx = self.char_index(at);
        if !self.is_editable(&(idx..idx)) {
            return false;
        }
        let cursor_before = self.cursor;
        let line = self.rope.char_to_line(idx);
        self.rope.insert(idx, text);
        let end = idx + text.chars().count();
        let cursor_after = self.cursor_at(end);
        self.history.record(Change {
            start: idx,
            before: String::new(),
            after: text.to_string(),
            cursor_before,
            cursor_after,
        });
        self.touch(line, idx, end);
        true
    }

    /// Deletes an absolute char range, returning what was removed.
    pub fn delete_chars(&mut self, from: usize, to: usize) -> Option<String> {
        let len = self.rope.len_chars();
        let a = from.min(to).min(len);
        let b = from.max(to).min(len);
        if a == b {
            return None;
        }
        if !self.is_editable(&(a..b)) {
            return None;
        }
        let removed = self.rope.slice(a..b).to_string();
        let cursor_before = self.cursor;
        let line = self.rope.char_to_line(a);
        self.rope.remove(a..b);
        self.history.record(Change {
            start: a,
            before: removed.clone(),
            after: String::new(),
            cursor_before,
            cursor_after: self.cursor_at(a),
        });
        self.touch(line, a, a);
        Some(removed)
    }

    /// The (line, col) cursor for an absolute char index.
    fn cursor_at(&self, idx: usize) -> Cursor {
        let idx = idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(idx);
        let col = idx - self.rope.line_to_char(line);
        Cursor::new(line, col)
    }

    /// Undo the last step: invert its changes (in reverse) directly on the rope,
    /// bypassing recording, and restore the pre-command cursor.
    pub fn undo(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        let step = match self.history.undo() {
            Some(s) => s,
            None => return false,
        };
        for change in step.changes.iter().rev() {
            let after_len = change.after.chars().count();
            let line = self.rope.char_to_line(change.start);
            self.rope.remove(change.start..change.start + after_len);
            self.rope.insert(change.start, &change.before);
            self.mark_dirty(line, change.start, change.start + change.before.chars().count());
        }
        // Prefer the anchor taken when the command began. `cursor_before` is
        // sampled inside the primitive, by which point a Visual selection has
        // already moved the cursor to the far end of the range.
        if let Some(c) = step
            .cursor_start
            .or_else(|| step.changes.first().map(|c| c.cursor_before))
        {
            self.cursor = c;
        }
        self.revision += 1;
        self.sync_modified();
        true
    }

    /// Redo the last undone step: re-apply its changes forward.
    pub fn redo(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        let step = match self.history.redo() {
            Some(s) => s,
            None => return false,
        };
        for change in step.changes.iter() {
            let before_len = change.before.chars().count();
            let line = self.rope.char_to_line(change.start);
            self.rope.remove(change.start..change.start + before_len);
            self.rope.insert(change.start, &change.after);
            self.mark_dirty(line, change.start, change.start + change.after.chars().count());
        }
        if let Some(last) = step.changes.last() {
            self.cursor = last.cursor_after;
        }
        self.revision += 1;
        self.sync_modified();
        true
    }

    /// Bump revision, set modified, and record the lines an edit touched:
    /// `line` is where it started (read before the edit), `at` the same
    /// spot as a char index after it, and `end` where its new text ends.
    fn touch(&mut self, line: usize, at: usize, end: usize) {
        self.revision += 1;
        self.sync_modified();
        self.mark_dirty(line, at, end);
    }

    /// Lower `dirty_line` to the first line the edit touched — taking the
    /// minimum is what makes one rescan cover a command that made several
    /// edits — and `dirty_tail` to the distance from its last line to the
    /// end of the file. Both are read off the rope, so they use its line
    /// metric. `line` is taken before the edit, and the line of `at` after
    /// it, since a deleted `\n` after a `\r` can move it.
    fn mark_dirty(&mut self, line: usize, at: usize, end: usize) {
        let len = self.rope.len_chars();
        let first = line.min(self.rope.char_to_line(at.min(len)));
        let last = self.rope.char_to_line(end.min(len));
        let tail = (self.rope.len_lines() - 1).saturating_sub(last);
        self.dirty_line = Some(self.dirty_line.map_or(first, |prev| prev.min(first)));
        self.dirty_tail = Some(self.dirty_tail.map_or(tail, |prev| prev.min(tail)));
    }

    /// The lines touched since the last take, first to last, and forget
    /// them. Lines past the last are known to be as they were.
    pub fn take_dirty(&mut self) -> Option<(usize, usize)> {
        let first = self.dirty_line.take();
        let tail = self.dirty_tail.take();
        let last_line = self.rope.len_lines() - 1;
        first.map(|f| (f, last_line.saturating_sub(tail.unwrap_or(0)).max(f)))
    }

    /// Forget the lines touched, when a cache has been rebuilt whole.
    pub fn clear_dirty(&mut self) {
        self.dirty_line = None;
        self.dirty_tail = None;
    }

    /// Every mutation checks this first. Always passes today — the vec is
    /// empty and transclusion turned out not to need it — and that one branch
    /// is what keeps the guarantee cheap to reintroduce. See
    /// `Buffer::readonly_ranges` and SPEC.md §14.5.
    fn is_editable(&self, range: &std::ops::Range<usize>) -> bool {
        !self.read_only
            && !self
            .readonly_ranges
            .iter()
            .any(|r| range.start < r.end && r.start < range.end)
    }
}

