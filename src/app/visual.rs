use super::*;

impl App {
    // ----------------------------------------------------------------- visual

    pub(super) fn enter_visual(&mut self, mode: Mode) {
        // A selection dragged out of a counted insert ends the count: what
        // follows is no longer the text the count was for.
        self.insert_repeat = None;
        // Without the goal: `o` sets one afresh where it lands, as Neovim's,
        // rather than bring back `$`'s line end.
        self.anchor = Some(Cursor::new(self.buffer.cursor.line, self.buffer.cursor.col));
        self.mode = mode;
        self.visual_owner = Some((self.focus_pane, self.current()));
        // A keyboard `v` always returns to Normal. Only the mouse sets this,
        // and it sets it straight after this call.
        self.visual_resumes_insert = false;
        self.touch_status();
    }

    /// The selection's other end, in Visual mode only. `sync_after_input`
    /// drops the anchor outside Visual after each input; this covers the rest
    /// of the input that left Visual, before that runs.
    pub(super) fn visual_anchor(&self) -> Option<Cursor> {
        match self.mode {
            Mode::Visual | Mode::VisualLine => self.anchor,
            _ => None,
        }
    }

    /// Leave Visual for whatever comes next: back into the insert session when
    /// the selection was dragged out of one, Normal otherwise.
    ///
    /// Every Visual exit funnels through here — `Esc`, the operators, and the
    /// writer verbs alike — so the resume is honoured once rather than
    /// remembered at each of them.
    pub(super) fn end_visual(&mut self) {
        self.mode = if self.visual_resumes_insert {
            Mode::Insert
        } else {
            Mode::Normal
        };
        self.visual_resumes_insert = false;
        self.anchor = None;
    }

    pub(super) fn leave_visual(&mut self) {
        self.end_visual();
        self.pending.reset();
    }

    pub(super) fn visual_operate(&mut self, op: Operator) {
        let anchor = self.anchor.unwrap_or(self.buffer.cursor);
        let cursor = self.buffer.cursor;
        let linewise = matches!(self.mode, Mode::VisualLine);
        if linewise {
            let start = self.visual_line_start();
            self.end_visual();
            self.operate_linewise(op, anchor.line, cursor.line, start);
            return;
        }
        // Visual selections are inclusive of the cursor character, and stop
        // short of the file's final newline.
        let (lo, start, end) = self.charwise_selection();
        self.end_visual();
        if start == end {
            // Nothing to act on; `c` still starts typing there.
            return self.operate_charwise(op, lo, lo, false);
        }
        let hi = self.idx_to_cursor(end - 1);
        self.operate_charwise(op, lo, hi, true);
    }

    /// The Visual meaning of a Normal-mode edit key, which the Visual table
    /// passes through unchanged (`bindings::visual`). Returns true when the
    /// key is handled here, and `run` goes no further.
    ///
    /// Without this, `p`, `~`, `r`, `J` and `D` ran their Normal action at
    /// the cursor and left Visual on, ignoring the selection.
    pub(super) fn visual_key(&mut self, action: &Action, count: usize) -> bool {
        match action {
            Action::PasteAfter => self.visual_paste(true, count),
            Action::PasteBefore => self.visual_paste(false, count),
            // `~` is `g~` on the selection, as Neovim's: it lands as `U`
            // does, and undo and redo with it.
            Action::ToggleCase => self.visual_operate(Operator::ToggleCase),
            // `r` then Enter puts a carriage return in every selected place
            // in Neovim, which Shoin's text would read as line breaks: it
            // does nothing here, and the selection stays.
            Action::ReplaceChar('\n') => {}
            Action::ReplaceChar(r) => {
                let r = *r;
                self.visual_map_chars(move |_, out| out.push(r));
            }
            Action::JoinLines => {
                // A closed fold in the selection joins whole, as in vim.
                let (a, b) = self.visual_lines();
                let (a, b) = self.fold_hidden().widen(a, b);
                let b = b.min(self.buffer.last_line());
                self.end_visual();
                self.buffer.cursor = Cursor::new(a, 0);
                // At least two lines, as vim: `vJ` on one line joins the next.
                self.join_lines((b - a + 1).max(2));
            }
            // The capital letters work on whole LINES in Visual, whatever
            // kind of selection it is.
            Action::DeleteToEol | Action::DeleteCharBack => self.visual_lines_operate(Operator::Delete),
            Action::YankLine => self.visual_lines_operate(Operator::Yank),
            Action::ChangeToEol => self.visual_lines_operate(Operator::Change),
            // Insert before the selection's first character, or after its
            // last (in `V`, at the start of its first line or the end of its
            // last), as vim's `I`/`A` do outside block mode.
            Action::InsertLineStart | Action::AppendLineEnd => {
                let (_, end, lo, linewise) = self.visual_span();
                let (_, last) = self.visual_lines();
                self.end_visual();
                self.buffer.cursor = match (action, linewise) {
                    (Action::InsertLineStart, _) => lo,
                    (_, true) => Cursor::new(last, self.buffer.line_len(last)),
                    // After the last selected character; a selection ending on
                    // a line break appends at that line's end.
                    _ => {
                        let at = self.idx_to_cursor(end);
                        if end > 0 && self.buffer.rope.char(end - 1) == '\n' {
                            let l = at.line.saturating_sub(1);
                            Cursor::new(l, self.buffer.line_len(l))
                        } else {
                            at
                        }
                    }
                };
                self.enter_insert();
            }
            // Edits with no Visual meaning do nothing, rather than edit at
            // the cursor with a selection still showing.
            Action::OpenAbove
            | Action::OpenBelow
            | Action::Insert
            | Action::Append
            | Action::DeleteChar
            | Action::Redo => {}
            // `.` in Visual does nothing, as in vim, and leaves no keys in
            // the change the selection goes on to make.
            Action::Repeat => self.drop_command_keys(),
            _ => return false,
        }
        true
    }

    /// The selection's first and last line.
    fn visual_lines(&self) -> (usize, usize) {
        let a = self.anchor.unwrap_or(self.buffer.cursor).line;
        let b = self.buffer.cursor.line;
        (a.min(b), a.max(b))
    }

    /// The selection as a char range `start..end`, the first character's
    /// position, and whether it is linewise. A linewise range takes each line
    /// whole, its last newline included when there is one.
    fn visual_span(&self) -> (usize, usize, Cursor, bool) {
        if matches!(self.mode, Mode::VisualLine) {
            let (a, b) = self.visual_lines();
            let start = self.buffer.rope.line_to_char(a);
            let end = if b < self.buffer.last_line() {
                self.buffer.rope.line_to_char(b + 1)
            } else {
                self.buffer.rope.len_chars()
            };
            return (start, end, Cursor::new(a, 0), true);
        }
        let (lo, start, end) = self.charwise_selection();
        (start, end, lo, false)
    }

    /// A charwise selection: its first character's position, and the chars
    /// `start..end` it covers, both ends included. The file's final newline
    /// is never one of them, so an operator or a writer verb acts on the
    /// text only; a selection of nothing else is empty (`start == end`).
    pub(super) fn charwise_selection(&self) -> (Cursor, usize, usize) {
        let anchor = self.anchor.unwrap_or(self.buffer.cursor);
        let cur = self.buffer.cursor;
        let lo = if (anchor.line, anchor.col) <= (cur.line, cur.col) { anchor } else { cur };
        let hi = if lo == anchor { cur } else { anchor };
        let rope = &self.buffer.rope;
        let len = rope.len_chars();
        let text_end = if len > 0 && rope.char(len - 1) == '\n' { len - 1 } else { len };
        let start = self.buffer.char_index(lo).min(text_end);
        let end = (self.buffer.char_index(hi) + 1).min(text_end).max(start);
        (lo, start, end)
    }

    /// `~` and `r{c}` in Visual: rewrite every selected character but the
    /// line breaks, end Visual, and leave the cursor at the selection's start.
    fn visual_map_chars(&mut self, f: impl Fn(char, &mut String)) {
        let (start, end, lo, _) = self.visual_span();
        self.end_visual();
        let text = self.buffer.rope.slice(start..end).to_string();
        let mut mapped = String::with_capacity(text.len());
        for c in text.chars() {
            if c == '\n' {
                mapped.push(c);
            } else {
                f(c, &mut mapped);
            }
        }
        if mapped != text {
            self.buffer.delete_chars(start, end);
            self.buffer.insert_str(lo, &mapped);
        }
        self.buffer.cursor = lo;
    }

    /// `D`/`X`, `Y` and `C`/`S`/`R` in Visual: the operator on the selected
    /// lines.
    fn visual_lines_operate(&mut self, op: Operator) {
        let (a, b) = self.visual_lines();
        let start = self.visual_line_start();
        self.end_visual();
        self.operate_linewise(op, a, b, start);
    }

    /// Where a Visual selection begins for a linewise operator, as Neovim's:
    /// the cursor when it is above the anchor's line, else the anchor's line
    /// at column 0. `Vky` and `VkU` stay where `k` put them; `Vy` and `VjU`
    /// go to the line's start.
    pub(super) fn visual_line_start(&self) -> Cursor {
        let anchor = self.anchor.unwrap_or(self.buffer.cursor);
        let cursor = self.buffer.cursor;
        if cursor.line < anchor.line {
            Cursor::new(cursor.line, cursor.col)
        } else {
            Cursor::new(anchor.line, 0)
        }
    }

    /// `p` / `P` in Visual: replace the selection with the register, `count`
    /// copies of it. `p` puts what it replaced in the unnamed register, so
    /// the next `p` swaps it back; `P` leaves the registers as they were
    /// (vim 9), so one yank can replace several selections.
    fn visual_paste(&mut self, keep_replaced: bool, count: usize) {
        let Some(reg) = self.take_register() else {
            self.leave_visual();
            return;
        };
        if count > 1 && count.saturating_mul(reg.text.len()) > PASTE_LIMIT {
            self.leave_visual();
            return self.notify("paste: too large", FlashKind::Error);
        }
        // A charwise selection that takes a line break (`vp` on an empty
        // line, `v$p`) replaces the break too, joining the next line on, and
        // `p` keeps it in the register, as Neovim's.
        let (start, end, lo, linewise) = self.visual_span();
        self.end_visual();
        let ends_at_eof = end == self.buffer.rope.len_chars();
        let replaced = self.buffer.rope.slice(start..end).to_string();

        let reg_linewise = reg.linewise;
        let mut one = reg.text;
        if reg_linewise && !one.ends_with('\n') {
            one.push('\n');
        }
        let mut text = one.repeat(count.max(1));
        if reg_linewise && !linewise {
            // Lines into the middle of a line split it around them.
            text.insert(0, '\n');
        } else if !reg_linewise && linewise {
            // Text in place of lines is a line of its own.
            text.push('\n');
        }
        // Lines replacing the last line of the file must not leave a blank
        // one after them.
        if linewise && ends_at_eof && !replaced.ends_with('\n') && text.ends_with('\n') {
            text.pop();
        }

        self.buffer.delete_chars(start, end);
        self.buffer.insert_str(lo, &text);
        if keep_replaced {
            let mut replaced = replaced;
            if linewise && !replaced.ends_with('\n') {
                replaced.push('\n');
            }
            self.store_register(replaced, linewise, true);
        }
        self.buffer.cursor = if reg_linewise {
            let line = if linewise { lo.line } else { lo.line + 1 };
            let line = line.min(self.buffer.last_line());
            Cursor::new(line, self.first_non_blank(line))
        } else if text.trim_end_matches('\n').contains('\n') {
            // At the start of text that spans lines, as Normal `p` leaves it.
            lo
        } else {
            // On the last character of one line of text.
            let n = text.trim_end_matches('\n').chars().count();
            self.idx_to_cursor(start + n.saturating_sub(1))
        };
    }
}
