use super::*;

impl App {
    /// Record text into the register a `"x` prefix named, or — with no prefix —
    /// into the unnamed register plus the one vim would also fill: `"0` for a
    /// yank, the `"1`-`"9` ring for a delete.
    ///
    /// An UPPERCASE name appends to that register instead of replacing it, so
    /// `"Ayy` on several lines collects them.
    ///
    /// `"+` and `"*` are the system clipboard — one clipboard, not X11's two:
    /// PRIMARY is the selection a terminal already owns. With
    /// `[editor] clipboard` on, an unnamed store goes there as well, the way
    /// vim's `clipboard=unnamedplus` does; a NAMED one still does not, so
    /// `"ayy` remains a way to keep something out of it.
    pub(super) fn store_register(&mut self, text: String, linewise: bool, deleted: bool) {
        let reg = Register { text, linewise };
        if let Some(name) = self.pending_register.take() {
            if is_clipboard_register(name) {
                self.copy_to_clipboard(reg);
                return;
            }
            let key = name.to_ascii_lowercase();
            let merged = if name.is_ascii_uppercase() {
                match self.registers.get(&key) {
                    Some(prev) => {
                        // Lines on either side make the result lines, each
                        // ending in a newline: `"ayy"Ayl` holds `ab\na\n`, and
                        // `"ayl"Ayy` holds `a\nab\n`, as in vim.
                        let linewise = prev.linewise || reg.linewise;
                        let mut text = prev.text.clone();
                        if linewise && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(&reg.text);
                        if linewise && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        Register { text, linewise }
                    }
                    None => reg.clone(),
                }
            } else {
                reg.clone()
            };
            self.registers.insert(key, merged.clone());
            self.registers.insert('"', merged);
            return;
        }
        if deleted {
            for n in (1..9u8).rev() {
                let from = (b'0' + n) as char;
                let to = (b'0' + n + 1) as char;
                if let Some(r) = self.registers.get(&from).cloned() {
                    self.registers.insert(to, r);
                }
            }
            self.registers.insert('1', reg.clone());
        } else {
            self.registers.insert('0', reg.clone());
        }
        if self.config.editor.clipboard {
            self.copy_to_clipboard(reg);
        } else {
            self.registers.insert('"', reg);
        }
    }

    /// Copy to the system clipboard, keeping a record of what went: `"+` for
    /// `read_clipboard_register` to recognise, and `"` as every store fills.
    fn copy_to_clipboard(&mut self, reg: Register) {
        self.clipboard.copy(&reg.text);
        self.registers.insert('+', reg.clone());
        self.registers.insert('"', reg);
    }

    /// The register a command should read: the one a `"x` prefix named (consumed
    /// here), else the unnamed one — or the system clipboard, for `"+`/`"*` or
    /// an unnamed paste with `[editor] clipboard` on.
    pub(super) fn take_register(&mut self) -> Option<Register> {
        let name = self
            .pending_register
            .take()
            .unwrap_or('"')
            .to_ascii_lowercase();
        let system =
            is_clipboard_register(name) || (name == '"' && self.config.editor.clipboard);
        if !system {
            return self.registers.get(&name).cloned();
        }
        match self.clipboard.paste() {
            // A clipboard that holds no text — emptied, or an image copied
            // since — pastes nothing, not a copy it has since replaced.
            Some(text) if text.is_empty() => None,
            Some(text) => Some(self.read_clipboard_register(text)),
            // No clipboard to read — no program, or over SSH, where copying
            // went out as OSC 52 and cannot come back. Shoin's own record of
            // its last copy is the best answer left.
            None => {
                let key = if name == '"' { '"' } else { '+' };
                self.registers.get(&key).cloned()
            }
        }
    }

    /// A register for text read off the system clipboard, which says nothing
    /// about lines. If it still holds what Shoin last put there, that copy's
    /// record knows whether it was `yy` or `yw`; otherwise text ending in a
    /// newline is taken for whole lines, which is vim's rule too.
    fn read_clipboard_register(&self, text: String) -> Register {
        if let Some(ours) = self.registers.get(&'+').filter(|r| r.text == text) {
            return ours.clone();
        }
        let linewise = text.ends_with('\n');
        Register { text, linewise }
    }

    /// `p` / `P`: paste the register after / before the cursor, `count` times
    /// over as one piece of text (`3p`).
    pub(super) fn paste(&mut self, after: bool, count: usize) {
        let mut reg = match self.take_register() {
            Some(r) => r,
            None => return,
        };
        if reg.linewise && !reg.text.ends_with('\n') {
            reg.text.push('\n');
        }
        // Checked BEFORE the repeat: `99999p` would otherwise ask for
        // gigabytes. Only a count is refused — one copy of a register is
        // already in memory, however large it is.
        if count > 1 && count.saturating_mul(reg.text.len()) > PASTE_LIMIT {
            return self.notify("paste: too large", FlashKind::Error);
        }
        reg.text = reg.text.repeat(count.max(1));
        if reg.linewise {
            let cur = self.buffer.cursor;
            // After a closed fold is after all of it, as in vim.
            let line = if after { self.fold_hidden().last_of(cur.line) + 1 } else { cur.line };
            if line >= self.buffer.line_count() {
                // Appending past the last line: add a separator newline first.
                let end_line = self.buffer.line_count().saturating_sub(1);
                let tail = self.buffer.line_len(end_line);
                self.buffer.insert_str(Cursor::new(end_line, tail), "\n");
                self.buffer
                    .insert_str(Cursor::new(end_line + 1, 0), reg.text.trim_end_matches('\n'));
                self.buffer.cursor = Cursor::new(end_line + 1, self.first_non_blank(end_line + 1));
            } else {
                self.buffer.insert_str(Cursor::new(line, 0), &reg.text);
                self.buffer.cursor = Cursor::new(line, self.first_non_blank(line));
            }
        } else {
            let cur = self.buffer.cursor;
            let len = self.buffer.line_len(cur.line);
            let col = if after && len > 0 {
                (cur.col + 1).min(len)
            } else {
                cur.col
            };
            let at = Cursor::new(cur.line, col);
            if self.buffer.insert_str(at, &reg.text) {
                // On the last character of one line of text; at the start of
                // several, as vim leaves it.
                self.buffer.cursor = if reg.text.contains('\n') {
                    at
                } else {
                    let n = reg.text.chars().count();
                    Cursor::new(cur.line, col + n.saturating_sub(1))
                };
            }
        }
    }
}
