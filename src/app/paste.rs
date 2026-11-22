use super::*;

impl App {
    // ----------------------------------------------------------------- paste

    /// A bracketed paste: the terminal's own paste (Cmd+V, Ctrl+Shift+V),
    /// arriving as one piece of text instead of as keys.
    ///
    /// Into the buffer it goes in exactly as it was copied, in Normal mode as
    /// much as Insert: no auto-indent, no list continuation, no auto-pair and
    /// no escape alias, any one of which would change it on the way in. Normal
    /// mode inserts at the cursor and stays in Normal, as Neovim does — the
    /// keys-as-commands reading is the accident bracketed paste exists to
    /// prevent. A one-line field takes the first line only, since a newline
    /// there would be Enter.
    pub(super) fn on_paste(&mut self, text: String) {
        // A paste is not typing a count can repeat.
        self.insert_repeat = None;
        let text = crate::clipboard::normalize(&text);
        let first_line = || text.split('\n').next().unwrap_or("").to_string();
        if self.help.is_some() || self.diff.is_some() {
            return;
        }
        if let Some(finder) = self.finder.as_mut() {
            finder.push_str(&first_line());
            return;
        }
        match self.mode.clone() {
            Mode::Command(mut buf) => {
                buf.push_str(&first_line());
                self.mode = Mode::Command(buf);
                return;
            }
            Mode::Search { mut query, reverse } => {
                query.push_str(&first_line());
                self.mode = Mode::Search { query, reverse };
                return;
            }
            // A yes/no question is answered by one deliberate key, never by
            // whatever happened to be on the clipboard.
            Mode::Prompt(mut p) => {
                if !p.kind.is_confirm() {
                    p.input.push_str(&first_line());
                    self.mode = Mode::Prompt(p);
                }
                return;
            }
            _ => {}
        }
        if self.tree.as_ref().is_some_and(|t| t.focused) || text.is_empty() {
            return;
        }
        // A paste into the text is an edit like any other, and goes through
        // the same refusal (and the same `i` after it).
        if self.view {
            self.edit_offered = true;
            self.notify("read-only — press i to edit", FlashKind::Info);
            return;
        }
        // Over a selection, the selection ends first; a drag made while
        // inserting goes back to the insert session it came out of.
        if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            self.leave_visual();
        }
        // A half-typed command is dropped, not finished by the next key: a `d`
        // left waiting under a paste would take whatever is typed after it.
        if matches!(self.mode, Mode::Normal) {
            self.pending.reset();
        }
        // `.` repeats a paste the way it repeats anything else: from Normal
        // it is a change of its own, inside an insert session it is part of
        // the session's change.
        if !self.replaying {
            if self.is_clean() {
                self.recording.clear();
                self.mark_dot();
            }
            self.recording.push(Replay::Paste(text.clone()));
        }
        let inserting = matches!(self.mode, Mode::Insert);
        if inserting {
            // A step of its own inside the session, so `u` takes back the
            // paste and not the sentence typed before it.
            self.coalesce_insert(true);
            self.escape_run = 0;
            self.escape_since = None;
        } else {
            // Without the goal, as `handle_key`'s anchor: `u` sets it afresh.
            let anchor = Cursor::new(self.buffer.cursor.line, self.buffer.cursor.col);
            self.buffer.history.begin_group(Some(anchor));
        }
        self.insert_pasted(&text, inserting);
        if inserting {
            self.buffer.history.split();
        } else {
            self.buffer.history.end_group();
        }
        if !self.replaying && self.is_clean() && self.edited_since_mark() {
            self.dot = std::mem::take(&mut self.recording);
        }
    }

    /// Put pasted text in at the cursor. The cursor ends after it while
    /// inserting, and on its last character in Normal mode, as `P` leaves it.
    fn insert_pasted(&mut self, text: &str, inserting: bool) {
        let at = self.buffer.cursor;
        if !self.buffer.insert_str(at, text) {
            return;
        }
        let end = self.buffer.char_index(at) + text.chars().count();
        self.buffer.cursor = if inserting {
            self.idx_to_cursor(end)
        } else {
            // On the last character — and Normal mode cannot rest on a line
            // break, so a paste ending in one leaves the cursor before it.
            let c = self.idx_to_cursor(end - 1);
            let col = c.col.min(self.buffer.line_len(c.line).saturating_sub(1));
            Cursor::new(c.line, col)
        };
    }
}
