use super::*;

impl App {
    // ----------------------------------------------------------------- insert

    /// Enter in Insert mode. With `editor.auto_indent` the new line continues
    /// what the old one was: its leading whitespace, and — as the setting's own
    /// description promises — a list marker or quote prefix.
    ///
    /// Enter on an EMPTY item ends the list instead of laying down another
    /// marker, which is the only way out of one without reaching for Esc.
    fn newline_with_indent(&mut self) {
        let at = self.buffer.cursor;
        if !self.config.editor.auto_indent {
            if self.buffer.insert_str(at, "\n") {
                self.buffer.cursor = Cursor::new(at.line + 1, 0);
            }
            return;
        }
        // The block cache is refreshed by the RENDER path, so between two keys
        // in one event batch it can still describe the line before the last
        // edit. Ask for it here: it early-returns when the revisions match.
        self.refresh_blocks();
        let line = self.buffer.line_text(at.line);
        let ws: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let block = self.blocks.kinds.get(at.line).cloned();
        let prefix = match &block {
            Some(BlockKind::ListItem { marker, checked, .. }) => {
                Some(list_continuation(&line, *marker, *checked))
            }
            Some(BlockKind::Quote(_)) => {
                self.quote_list_continuation(at.line).or_else(|| quote_continuation(&line))
            }
            _ => None,
        };

        let Some(prefix) = prefix else {
            let text = format!("\n{ws}");
            if self.buffer.insert_str(at, &text) {
                self.buffer.cursor = Cursor::new(at.line + 1, ws.chars().count());
            }
            return;
        };

        // An item with nothing but its marker: clear the line and stop the list
        // rather than adding an empty one below it.
        if line.chars().skip(prefix.chars().count()).all(char::is_whitespace)
            && at.col >= prefix.chars().count()
        {
            // In a quote, the list ends and the quote goes on: the item's
            // line keeps its markers, and so does the new one.
            let quote = match &block {
                Some(BlockKind::Quote(_)) => quote_continuation(&line).filter(|q| *q != prefix),
                _ => None,
            };
            let keep = quote.as_deref().map_or("", str::trim_end);
            let start = self.buffer.char_index(Cursor::new(at.line, 0));
            let end = self.buffer.char_index(Cursor::new(at.line, line.chars().count()));
            self.buffer.delete_chars(start, end);
            self.buffer.insert_str(Cursor::new(at.line, 0), keep);
            let at = Cursor::new(at.line, keep.chars().count());
            let next = quote.unwrap_or_default();
            if self.buffer.insert_str(at, &format!("\n{next}")) {
                self.buffer.cursor = Cursor::new(at.line + 1, next.chars().count());
            }
            return;
        }

        let text = format!("\n{prefix}");
        if self.buffer.insert_str(at, &text) {
            self.buffer.cursor = Cursor::new(at.line + 1, prefix.chars().count());
        }
    }

    /// `editor.auto_pair` — insert the closing half of a bracket or quote.
    ///
    /// Returns true when it handled the key. OFF by default, and the rules are
    /// shaped by why: this is a PROSE editor, and prose is full of unmatched
    /// quotes. So a quote never pairs against a word character (`don't` must
    /// stay `don't`, and a closing `"` after a word is a closing quote), and
    /// nothing pairs directly in front of one.
    fn auto_pair_insert(&mut self, c: char) -> bool {
        if !self.config.editor.auto_pair {
            return false;
        }
        let at = self.buffer.cursor;
        let line = self.buffer.line_text(at.line);
        let next = line.chars().nth(at.col);
        let prev = at.col.checked_sub(1).and_then(|i| line.chars().nth(i));

        // Typing the closer that is already sitting there steps over it, so
        // finishing a pair by hand does not leave a stray second one.
        if closer_of(c).is_some() && next == Some(c) {
            self.buffer.cursor.set_col(at.col + 1);
            return true;
        }
        let Some(close) = pair_for(c) else {
            return false;
        };
        // Wrapping existing text is what Visual `gb`/`gl` are for; here it
        // would just split a word in half.
        if next.is_some_and(char::is_alphanumeric) {
            return false;
        }
        // A self-closing mark (quote, backtick) after a word is punctuation.
        if close == c && prev.is_some_and(|p| p.is_alphanumeric()) {
            return false;
        }
        let text: String = [c, close].iter().collect();
        if self.buffer.insert_str(at, &text) {
            self.buffer.cursor.set_col(at.col + 1);
        }
        true
    }

    /// Backspace between the two halves of an empty pair removes both — the
    /// counterpart to `auto_pair_insert`, or undoing a mistyped `(` would leave
    /// its `)` behind.
    fn auto_pair_backspace(&mut self) -> bool {
        if !self.config.editor.auto_pair {
            return false;
        }
        let at = self.buffer.cursor;
        if at.col == 0 {
            return false;
        }
        let line = self.buffer.line_text(at.line);
        let open = line.chars().nth(at.col - 1);
        let close = line.chars().nth(at.col);
        if open.and_then(pair_for) != close || close.is_none() {
            return false;
        }
        let idx = self.buffer.char_index(at);
        if self.buffer.delete_chars(idx - 1, idx + 1).is_some() {
            self.buffer.cursor.set_col(at.col - 1);
        }
        true
    }

    /// `input.escape_alias` (default `jk`).
    ///
    /// Deliberately NOT a keymap entry: the first character must land in the
    /// buffer immediately, or typing "jam" would stutter while the editor waited
    /// to see whether a `k` was coming. So each character is inserted as normal
    /// and this runs AFTERWARDS; completing the alias removes what it just wrote
    /// and leaves Insert mode.
    ///
    /// `input.sequence_timeout_ms` bounds the gap BETWEEN characters, not the
    /// whole sequence: `j` … long pause … `k` is a writer typing two letters,
    /// and must stay two letters.
    fn feed_escape_alias(&mut self, typed: char) {
        let alias: Vec<char> = self.config.input.escape_alias.chars().collect();
        if alias.is_empty() || self.replaying_insert {
            return;
        }
        let timeout = Duration::from_millis(self.config.input.sequence_timeout_ms);
        let in_time = self
            .escape_since
            .is_some_and(|t| t.elapsed() <= timeout);

        self.escape_run = if self.escape_run > 0 && in_time && typed == alias[self.escape_run] {
            self.escape_run + 1
        } else if typed == alias[0] {
            // A stale or broken run still restarts here, so `jjk` works.
            1
        } else {
            0
        };
        self.escape_since = (self.escape_run > 0).then(Instant::now);

        if self.escape_run < alias.len() {
            return;
        }
        self.escape_run = 0;
        self.escape_since = None;

        // Take back exactly the characters the alias just wrote. They were all
        // typed as `Char`s in a row, so they are on this line, ending at the
        // cursor — anything else means something intervened and the run would
        // have been broken already.
        let at = self.buffer.cursor;
        let n = alias.len();
        if at.col < n {
            return;
        }
        // The alias's keys were recorded as typing; they are not text to repeat.
        if let Some(r) = self.insert_repeat.as_mut() {
            let keep = r.keys.len().saturating_sub(n);
            r.keys.truncate(keep);
        }
        let end = self.buffer.char_index(at);
        if self.buffer.delete_chars(end - n, end).is_some() {
            self.buffer.cursor.set_col(at.col - n);
        }
        self.leave_insert();
    }

    /// End the insert session in the current document as Esc would, but
    /// with the cursor left where it is: focus is moving to another pane or
    /// document (vim's rule for a click). The session's undo step is sealed
    /// here, in its own document, and it becomes the change `.` repeats,
    /// complete with its Esc.
    pub(super) fn end_insert_session(&mut self) {
        if self.mode != Mode::Insert {
            return;
        }
        // A count is for typing that goes on; a click away ends it, as an
        // arrow key does.
        self.insert_repeat = None;
        self.finish_insert(false);
        if !self.replaying {
            if self.edited_since_mark() {
                self.recording.push(Replay::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
                self.dot = std::mem::take(&mut self.recording);
            } else {
                self.recording.clear();
            }
        }
        self.buffer.history.end_group();
    }

    /// A new insert session at the cursor in the pane focus has moved to,
    /// with an undo step of its own, recorded for `.` as `Replay::Insert`.
    pub(super) fn begin_insert_session(&mut self) {
        if !self.replaying {
            self.recording = vec![Replay::Insert];
            self.mark_dot();
        }
        let at = self.buffer.cursor;
        self.buffer.history.begin_group(Some(at));
        self.enter_insert();
    }

    /// Esc, and everything that behaves like it: Normal mode with the cursor
    /// stepped back onto the last character it was sitting after.
    fn leave_insert(&mut self) {
        self.finish_insert(true);
    }

    /// What every end of an insert session does, Esc or a click away:
    /// Normal mode, a count's copies typed, an unused `cc` indent taken
    /// back, the table aligned. Only Esc steps the cursor back.
    fn finish_insert(&mut self, step_back: bool) {
        self.mode = Mode::Normal;
        self.escape_run = 0;
        self.escape_since = None;
        self.repeat_insert();
        // `cc` then `Esc` with nothing typed: the indent it laid down was
        // never used, so it goes, as vim's 'autoindent' takes it back.
        if let Some(line) = self.cc_indent_line.take() {
            let text = self.buffer.line_text(line);
            if line == self.buffer.cursor.line && !text.is_empty() && text.trim().is_empty() {
                let start = self.buffer.rope.line_to_char(line);
                let n = text.chars().count();
                self.buffer.delete_chars(start, start + n);
                self.buffer.cursor = Cursor::new(line, 0);
            }
        }
        if step_back && self.buffer.cursor.col > 0 {
            let col = self.buffer.cursor.col - 1;
            self.buffer.cursor.set_col(col);
        }
        self.auto_align_table();
        self.touch_status();
    }

    /// `3ix<Esc>` is `xxx`, `2otext<Esc>` two lines: the keys this insert
    /// session typed are replayed `count - 1` more times (each after a new
    /// line, for `o`/`O`), so auto-pair, Backspace and list continuation
    /// come out as they did the first time. A session that did anything but
    /// type (an arrow, a paste, a drag) dropped its count on the way.
    fn repeat_insert(&mut self) {
        let Some(r) = self.insert_repeat.take() else { return };
        let copies = r.count - 1;
        if r.keys.is_empty() || copies.saturating_mul(r.keys.len()) > INSERT_REPEAT_LIMIT {
            return;
        }
        // Typing that went back past where it began (Backspace over older
        // text) has no first copy to follow: nothing is repeated.
        let grown = self.buffer.rope.len_chars().checked_sub(r.len_before);
        let end = self.buffer.char_index(self.buffer.cursor);
        let Some(grown) = grown.filter(|_| r.line || end >= r.start) else { return };
        self.replaying_insert = true;
        for k in 1..=copies {
            if r.line {
                self.open_line(false);
            } else {
                self.buffer.cursor = self.idx_to_cursor(r.start + k * grown);
            }
            for &key in &r.keys {
                self.on_key_insert(key);
            }
        }
        self.replaying_insert = false;
        self.mode = Mode::Normal;
    }

    /// Keep a counted insert's typing for `repeat_insert`; anything that is
    /// not typing ends the count.
    fn record_insert_key(&mut self, key: KeyEvent) {
        if self.replaying_insert {
            return;
        }
        let Some(r) = self.insert_repeat.as_mut() else { return };
        let chord = key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab if !chord => {
                r.keys.push(key)
            }
            _ => self.insert_repeat = None,
        }
    }

    /// A window or buffer key bound in `[keys.insert]`, taken out of the
    /// session before it runs: the key is no part of the change `.` or a
    /// count replays. Where it switches documents it ends the session
    /// (`end_insert_session`); where it does not (a refused `gf`), the
    /// session goes on without it.
    fn window_key_in_insert(&mut self, key: KeyEvent) {
        if !self.replaying && matches!(self.recording.last(), Some(Replay::Key(k)) if *k == key) {
            self.recording.pop();
        }
        if let Some(r) = self.insert_repeat.as_mut() {
            if r.keys.last() == Some(&key) {
                r.keys.pop();
            }
        }
    }

    pub(super) fn on_key_insert(&mut self, key: KeyEvent) {
        self.record_insert_key(key);
        // `[keys.insert]` first — the same "config overrides, built-ins remain"
        // rule Normal and Visual follow. Single keys only (`Keymap::insert_key`
        // says why), and only `Act` verbs: a motion or an operator has no
        // meaning while typing.
        if let Some(k) = Key::from_event(key) {
            if let Some(Verb::Act(action)) = self.keymap.insert_key(k).cloned() {
                self.escape_run = 0;
                self.escape_since = None;
                let window = ends_command(&action);
                if window {
                    self.window_key_in_insert(key);
                }
                self.apply_action(action);
                if window && self.mode != Mode::Insert {
                    self.end_recording();
                }
                return;
            }
        }
        // Anything that is not a plain character breaks a half-typed alias:
        // `j`, Backspace, `k` is not `jk`.
        if !matches!(key.code, KeyCode::Char(_)) {
            self.escape_run = 0;
            self.escape_since = None;
        }
        match key.code {
            KeyCode::Esc => self.leave_insert(),
            // Ctrl-C leaves without the count's copies, as in vim.
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_repeat = None;
                self.mode = Mode::Normal;
                self.escape_run = 0;
                self.escape_since = None;
                self.touch_status();
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.write(None, false);
            }
            // A control chord arrives as its plain letter plus CONTROL, so
            // without this `<C-a>` would type an `a`. Unbound chords do nothing.
            KeyCode::Char(_) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.escape_run = 0;
                self.escape_since = None;
            }
            KeyCode::Char(c) => {
                self.coalesce_insert(c.is_whitespace());
                if !self.auto_pair_insert(c) {
                    let at = self.buffer.cursor;
                    let mut s = [0u8; 4];
                    if self.buffer.insert_str(at, c.encode_utf8(&mut s)) {
                        let col = at.col + 1;
                        self.buffer.cursor.set_col(col);
                    }
                }
                self.feed_escape_alias(c);
            }
            KeyCode::Enter => {
                self.coalesce_insert(true);
                self.newline_with_indent();
            }
            KeyCode::Backspace => {
                if self.auto_pair_backspace() {
                    return;
                }
                let c = self.buffer.cursor;
                if c.col > 0 {
                    let idx = self.buffer.char_index(c);
                    if self.buffer.delete_chars(idx - 1, idx).is_some() {
                        self.buffer.cursor.set_col(c.col - 1);
                    }
                } else if c.line > 0 {
                    let prev_len = self.buffer.line_len(c.line - 1);
                    let idx = self.buffer.char_index(c);
                    if self.buffer.delete_chars(idx - 1, idx).is_some() {
                        self.buffer.cursor = Cursor::new(c.line - 1, prev_len);
                    }
                }
            }
            // On a list item, Tab and Shift-Tab move the whole item in and
            // out — what every writing app does — rather than type a tab.
            KeyCode::Tab | KeyCode::BackTab if self.list_tab_applies() => {
                let outdent = key.code == KeyCode::BackTab
                    || key.modifiers.contains(KeyModifiers::SHIFT);
                self.shift_list_item(outdent);
            }
            KeyCode::Tab => {
                let at = self.buffer.cursor;
                let text = if self.config.editor.expand_tab {
                    " ".repeat(self.config.editor.tab_width)
                } else {
                    "\t".to_string()
                };
                let n = text.chars().count();
                if self.buffer.insert_str(at, &text) {
                    self.buffer.cursor.set_col(at.col + n);
                }
            }

            // Cursor movement while inserting. The cursor may sit one past the
            // last character here (`Mode::allows_past_end`), so movement uses the
            // full line length, not length-1.
            KeyCode::Left => {
                let c = self.buffer.cursor;
                if c.col > 0 {
                    self.buffer.cursor.set_col(c.col - 1);
                } else if c.line > 0 {
                    let prev = c.line - 1;
                    self.buffer.cursor = Cursor::new(prev, self.buffer.line_len(prev));
                }
            }
            KeyCode::Right => {
                let c = self.buffer.cursor;
                let len = self.buffer.line_len(c.line);
                if c.col < len {
                    self.buffer.cursor.set_col(c.col + 1);
                } else if c.line + 1 < self.buffer.line_count() {
                    self.buffer.cursor = Cursor::new(c.line + 1, 0);
                }
            }
            KeyCode::Up => self.move_by(Motion::Up, 1),
            KeyCode::Down => self.move_by(Motion::Down, 1),
            KeyCode::Home => self.buffer.cursor.set_col(0),
            KeyCode::End => {
                let len = self.buffer.line_len(self.buffer.cursor.line);
                self.buffer.cursor.set_col(len);
            }
            KeyCode::PageUp => self.move_by(Motion::HalfPageUp, 1),
            KeyCode::PageDown => self.move_by(Motion::HalfPageDown, 1),
            KeyCode::Delete => {
                let idx = self.buffer.char_index(self.buffer.cursor);
                if idx < self.buffer.rope.len_chars() {
                    self.buffer.delete_chars(idx, idx + 1);
                }
            }
            _ => {}
        }
    }
}
