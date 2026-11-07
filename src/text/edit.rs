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
        self.rope.insert(idx, text);
        let inserted = breaks(text);
        let cursor_after = self.cursor_at(idx + text.chars().count());
        self.history.record(Change {
            start: idx,
            before: String::new(),
            after: text.to_string(),
            cursor_before,
            cursor_after,
        });
        self.touch(idx, 0, inserted);
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
        self.rope.remove(a..b);
        self.history.record(Change {
            start: a,
            before: removed.clone(),
            after: String::new(),
            cursor_before,
            cursor_after: self.cursor_at(a),
        });
        self.touch(a, breaks(&removed), 0);
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
            self.rope.remove(change.start..change.start + after_len);
            self.rope.insert(change.start, &change.before);
            self.mark_dirty(change.start, breaks(&change.after), breaks(&change.before));
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
            self.rope.remove(change.start..change.start + before_len);
            self.rope.insert(change.start, &change.after);
            self.mark_dirty(change.start, breaks(&change.before), breaks(&change.after));
        }
        if let Some(last) = step.changes.last() {
            self.cursor = last.cursor_after;
        }
        self.revision += 1;
        self.sync_modified();
        true
    }

    /// Bump revision, set modified, and remember the lowest line touched since
    /// a cache last cleared `dirty_line`. Called by every primitive above with
    /// the absolute char index the edit started at.
    fn touch(&mut self, at: usize, removed: usize, inserted: usize) {
        self.revision += 1;
        self.sync_modified();
        self.mark_dirty(at, removed, inserted);
    }

    /// Record an edit at `at` that took out `removed` line breaks and put in
    /// `inserted`. `dirty_line` drops to its line: taking the minimum is what
    /// makes one rescan cover a command that made several edits. `dirty_last`
    /// rises to the last line it wrote, after moving an earlier record below
    /// it by the lines this edit added or took away.
    fn mark_dirty(&mut self, at: usize, removed: usize, inserted: usize) {
        // The line holding `at` is the same before and after the edit: only
        // what follows it moved.
        let line = self.rope.char_to_line(at.min(self.rope.len_chars()));
        self.dirty_line = Some(self.dirty_line.map_or(line, |prev| prev.min(line)));
        let earlier = self.dirty_last.map(|e| match e > line {
            true => e.saturating_sub(removed).max(line) + inserted,
            false => e,
        });
        self.dirty_last = Some(earlier.map_or(line + inserted, |e| e.max(line + inserted)));
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

/// How many line breaks `text` holds.
fn breaks(text: &str) -> usize {
    text.chars().filter(|c| *c == '\n').count()
}
