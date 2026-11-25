use super::*;
use crate::render::layout::{cell_width_at, tab_stop};

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
            // Enter on a line that holds only the indent the last Enter laid
            // down takes it back, as Neovim's 'autoindent': `A<CR><CR>`
            // leaves an empty line between, and the indent goes on.
            let mut at = at;
            if self.cc_indent_line == Some(at.line) && !ws.is_empty() && ws.chars().count() == line.chars().count() {
                let start = self.buffer.rope.line_to_char(at.line);
                if self.buffer.delete_chars(start, start + ws.chars().count()).is_some() {
                    at = Cursor::new(at.line, 0);
                }
            }
            let text = format!("\n{ws}");
            if self.buffer.insert_str(at, &text) {
                self.buffer.cursor = Cursor::new(at.line + 1, ws.chars().count());
                // Typing nothing on it before Esc takes it back too.
                self.cc_indent_line = (!ws.is_empty()).then_some(at.line + 1);
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

    /// Up, Down and the page keys in Insert. The cursor stands before the
    /// character it is on, a tab included, so a goal not yet measured is
    /// that character's first cell, as Neovim's Insert has it, rather than
    /// the tab's last cell a Normal-mode goal takes.
    fn insert_move_by(&mut self, m: Motion) {
        let c = self.buffer.cursor;
        if c.goal_cells.is_none() {
            let cells = motion::goal_start_cells(&self.buffer, c, self.config.editor.tab_width);
            self.buffer.cursor.goal_cells = Some(cells);
        }
        self.move_by(m, 1);
        // After `End`, past the last character, where Insert may stand.
        if self.buffer.cursor.goal_col == Cursor::LINE_END {
            self.buffer.cursor.col = self.buffer.line_len(self.buffer.cursor.line);
        }
    }

    /// The cells the first `col` chars of `at`'s line take, each tab to its
    /// stop: the screen cell of column `col`.
    fn cells_before(&self, at: Cursor, col: usize) -> usize {
        let tw = self.config.editor.tab_width;
        let line = self.buffer.rope.line(at.line.min(self.buffer.line_count().saturating_sub(1)));
        line.chars().take(col).fold(0, |cells, c| cells + cell_width_at(c, cells, tw))
    }

    /// Backspace with only blanks before the cursor goes back to the tab
    /// stop before it, as Neovim's default 'smarttab': the blanks past that
    /// stop go, and when a tab took the cursor further back than the stop,
    /// spaces make up the difference. False, and nothing done, past the
    /// indent, where Backspace takes one character.
    fn backspace_in_indent(&mut self) -> bool {
        let c = self.buffer.cursor;
        let blanks = self.buffer.line_text(c.line).chars().take(c.col).all(|ch| ch == ' ' || ch == '\t');
        if c.col == 0 || !blanks {
            return false;
        }
        let tw = self.config.editor.tab_width.max(1);
        let want = (self.cells_before(c, c.col) - 1) / tw * tw;
        let mut col = c.col;
        while col > 0 && self.cells_before(c, col) > want {
            col -= 1;
        }
        let pad = want - self.cells_before(c, col);
        let line = self.buffer.rope.line_to_char(c.line);
        if self.buffer.delete_chars(line + col, line + c.col).is_none() {
            return true;
        }
        if pad > 0 {
            self.buffer.insert_str(Cursor::new(c.line, col), &" ".repeat(pad));
        }
        self.buffer.cursor.set_col(col + pad);
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
        self.insert_unterminated = self.text_unterminated();
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
        // The goal stays the indent's width, as Neovim's: `k` goes on from
        // there.
        let mut goal = None;
        if let Some(line) = self.cc_indent_line.take() {
            let text = self.buffer.line_text(line);
            if line == self.buffer.cursor.line && !text.is_empty() && text.trim().is_empty() {
                let start = self.buffer.rope.line_to_char(line);
                let n = text.chars().count();
                let width = self.cells_before(Cursor::new(line, 0), n);
                if self.buffer.delete_chars(start, start + n).is_some() {
                    self.buffer.cursor = Cursor::new(line, 0);
                    goal = Some(width);
                }
            }
        }
        self.keep_new_last_line();
        if step_back && self.buffer.cursor.col > 0 {
            let col = self.buffer.cursor.col - 1;
            self.buffer.cursor.set_col(col);
        } else if self.buffer.cursor.goal_col == Cursor::LINE_END {
            // `End` on an empty line: Esc takes the goal afresh, as Neovim's.
            let col = self.buffer.cursor.col;
            self.buffer.cursor.set_col(col);
        }
        if goal.is_some() {
            self.buffer.cursor.goal_cells = goal;
        }
        self.auto_align_table();
        self.touch_status();
    }

    /// Whether the text has no final newline: the last line is not one the
    /// text ends with a line break after. An empty text counts, as its one
    /// line has none.
    pub(super) fn text_unterminated(&self) -> bool {
        let len = self.buffer.rope.len_chars();
        len == 0 || self.buffer.rope.char(len - 1) != '\n'
    }

    /// The last line an insert session may move to: the empty line after a
    /// final newline when this session made it, by an Enter or `o` at the
    /// end of a text that had none, as Neovim has a line there; otherwise
    /// the last line.
    fn insert_last_line(&self) -> usize {
        match self.insert_unterminated {
            true => self.buffer.line_count() - 1,
            false => self.buffer.last_line(),
        }
    }

    /// At the end of an insert session that began on a text with no final
    /// newline and ends with one: that newline opened a new empty last
    /// line (`o` `Esc`, `A` `Enter` `Esc`), which Neovim keeps, with the
    /// cursor on it. It gets one more newline, so it is a line of its own
    /// rather than the final newline of the one before, as `r<CR>` gives
    /// one ([`App::replace_with_break`]). A save that keeps the missing
    /// final newline then writes what Neovim's `nofixeol` does.
    fn keep_new_last_line(&mut self) {
        if !self.insert_unterminated || self.text_unterminated() {
            return;
        }
        let end = self.idx_to_cursor(self.buffer.rope.len_chars());
        let cursor = self.buffer.cursor;
        if self.buffer.insert_str(end, "\n") {
            self.buffer.cursor = cursor;
        }
        // Now terminated: a session the count or `.` goes on with is one
        // on this text.
        self.insert_unterminated = false;
    }

    /// `3ix<Esc>` is `xxx`, `2otext<Esc>` two lines: the keys this insert
    /// session typed are replayed `count - 1` more times (each after a new
    /// line, for `o`/`O`), so auto-pair, Backspace and list continuation
    /// come out as they did the first time. A session that did anything but
    /// type (an arrow, a paste, a drag) dropped its count on the way.
    fn repeat_insert(&mut self) {
        let Some(r) = self.insert_repeat.take() else { return };
        let copies = r.count - 1;
        // `3o<Esc>` opens three lines with nothing typed, as Neovim's.
        if (r.keys.is_empty() && !r.line) || copies.saturating_mul(r.keys.len().max(1)) > INSERT_REPEAT_LIMIT {
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
                self.keep_new_last_line();
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
                if self.backspace_in_indent() {
                    return;
                }
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
                // Spaces to the next tab stop, as Neovim's 'expandtab' with
                // 'softtabstop' 0: `a` then Tab is three spaces at width 4.
                let text = if self.config.editor.expand_tab {
                    let tw = self.config.editor.tab_width;
                    " ".repeat(tab_stop(self.cells_before(at, at.col), tw))
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
                } else if c.line < self.insert_last_line() {
                    self.buffer.cursor = Cursor::new(c.line + 1, 0);
                }
            }
            KeyCode::Up => self.insert_move_by(Motion::Up),
            KeyCode::Down => self.insert_move_by(Motion::Down),
            KeyCode::Home => self.buffer.cursor.set_col(0),
            // The line-end goal, as `$`'s and Neovim's Insert `<End>`: `Up`
            // and `Down` go on to the end of each line.
            KeyCode::End => {
                let len = self.buffer.line_len(self.buffer.cursor.line);
                self.buffer.cursor.set_col(len);
                self.buffer.cursor.goal_col = Cursor::LINE_END;
                self.buffer.cursor.goal_cells = Some(Cursor::LINE_END);
            }
            KeyCode::PageUp => self.insert_move_by(Motion::HalfPageUp),
            KeyCode::PageDown => self.insert_move_by(Motion::HalfPageDown),
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

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod cursor_tests;
