use super::*;

impl App {
    // -------------------------------------------------------- writer verbs (g)

    pub(super) fn idx_to_cursor(&self, idx: usize) -> Cursor {
        let idx = idx.min(self.buffer.rope.len_chars());
        let line = self.buffer.rope.char_to_line(idx);
        Cursor::new(line, idx - self.buffer.rope.line_to_char(line))
    }

    /// Toggle a symmetric inline marker (`**`, `*`, `==`, `` ` ``) around the
    /// word under the cursor, or the selection in Visual mode.
    pub(super) fn writer_toggle(&mut self, marker: &str) {
        let (start, end) = if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            let (_, s, e) = self.charwise_selection();
            self.end_visual();
            (s, e)
        } else {
            let cur = self.buffer.cursor;
            match self.word_range(cur.line, cur.col) {
                Some((sc, ec)) => (
                    self.buffer.char_index(Cursor::new(cur.line, sc)),
                    self.buffer.char_index(Cursor::new(cur.line, ec)),
                ),
                None => return,
            }
        };
        // A selection of the final newline alone has no text to wrap.
        if start >= end {
            return;
        }
        // The bold and italic toggles act on what the RENDERER styles, read
        // with its own scanner, and nothing else. That makes `_x_` and `__x__`
        // emphasis like the star forms, and `snake_case` or `a_b_c` plain text.
        // `==` and `` ` `` have one spelling and keep the plain check.
        let italic = match marker {
            "*" => true,
            "**" => false,
            _ => return self.toggle_inline(marker, start, end),
        };
        // A line the renderer styles nothing on (a fence, a table row), or a
        // range over several lines, keeps the plain check too: it only ever
        // strips the marker it writes, so an underscore the reader sees
        // literally is never taken for emphasis.
        let Some(spans) = self.emphasis_spans(start, end) else {
            return self.toggle_inline(marker, start, end);
        };
        // The span the range lies in. Only one that holds ALL of it: a
        // selection running out of a span (`_a_ b`) is not about that span.
        // The innermost, the last listed (`**[_word_](u)**` styles `word`
        // italic in the link in the bold).
        let Some(span) = spans.iter().rfind(|s| s.outer.start <= start && end <= s.outer.end) else {
            return self.wrap_inline(marker, start, end);
        };
        let has = match span.kind {
            Inline::Italic => italic,
            Inline::Bold => !italic,
            _ => true,
        };
        // Whichever part of the span the range covers, the toggle is about
        // the span: it changes the span's own delimiter runs, star or
        // underscore, by the marker's length.
        let n = marker.len();
        let (s, e) = (span.inner.start, span.inner.end);
        let at = start.clamp(s, e);
        if has {
            // One from `***x***` leaves `**x**`; all of `_x_` leaves `x`.
            self.buffer.delete_chars(e, e + n);
            self.buffer.delete_chars(s - n, s);
            self.buffer.cursor = self.idx_to_cursor(at - n);
        } else {
            // Lengthen the runs: `**x**` becomes `***x***`. A marker written
            // INSIDE the span (`**_x_**`) would show literally, since the
            // renderer styles no span within another; a run of three is the
            // one bold-and-italic it draws.
            let delim = self.buffer.rope.char(span.outer.start).to_string().repeat(n);
            let (at_e, at_s) = (self.idx_to_cursor(e), self.idx_to_cursor(s));
            self.buffer.insert_str(at_e, &delim);
            self.buffer.insert_str(at_s, &delim);
            self.buffer.cursor = self.idx_to_cursor(at + n);
        }
    }

    /// The emphasis spans the renderer styles on the line `[start, end)` sits
    /// on, as char indexes into the rope.
    ///
    /// The renderer styles the top-level spans of a line, a link's text, and
    /// the links in emphasis (`inline::scan_styled` then `to_styled`):
    /// `_word_` inside `**…**` or `==…==` shows its underscores, so it is not
    /// emphasis here either, while `[_word_](u)` and `**[_word_](u)**` are. `None` where
    /// it styles nothing (`allows_inline`, with the same `[markdown] tables`
    /// override as `render::cache::build`) or the range crosses a line.
    fn emphasis_spans(&mut self, start: usize, end: usize) -> Option<Vec<EmphasisSpan>> {
        self.refresh_blocks();
        let rope = &self.buffer.rope;
        let line = rope.char_to_line(start);
        if end > start && rope.char_to_line(end - 1) != line {
            return None;
        }
        let block = match self.blocks.kinds.get(line) {
            Some(BlockKind::Table) if !self.config.markdown.tables => BlockKind::Paragraph,
            Some(kind) => kind.clone(),
            None => BlockKind::Paragraph,
        };
        if !block.allows_inline() {
            return None;
        }
        let base = rope.line_to_char(line);
        let spans = inline::scan_styled(&self.buffer.line_text(line), &self.config.markdown)
            .into_iter()
            .filter(|s| matches!(s.kind, Inline::Italic | Inline::Bold | Inline::BoldItalic))
            .map(|s| EmphasisSpan {
                kind: s.kind,
                outer: s.outer.start + base..s.outer.end + base,
                inner: s.inner.start + base..s.inner.end + base,
            })
            .collect();
        Some(spans)
    }

    /// The word (alphanumeric/`_` run) under `col`, as a `[start, end)` column
    /// range, or `None` if the cursor is not on a word character.
    fn word_range(&self, line: usize, col: usize) -> Option<(usize, usize)> {
        let chars: Vec<char> = self.buffer.line_text(line).chars().collect();
        if chars.is_empty() {
            return None;
        }
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let col = col.min(chars.len() - 1);
        if !is_word(chars[col]) {
            return None;
        }
        let mut s = col;
        while s > 0 && is_word(chars[s - 1]) {
            s -= 1;
        }
        let mut e = col;
        while e < chars.len() && is_word(chars[e]) {
            e += 1;
        }
        Some((s, e))
    }

    /// Wrap `[start, end)` in `marker`, or strip it if already wrapped.
    fn toggle_inline(&mut self, marker: &str, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let mlen = marker.chars().count();
        let len = self.buffer.rope.len_chars();
        let before = if start >= mlen {
            self.buffer.rope.slice(start - mlen..start).to_string()
        } else {
            String::new()
        };
        let after = if end + mlen <= len {
            self.buffer.rope.slice(end..end + mlen).to_string()
        } else {
            String::new()
        };

        if before == marker && after == marker {
            // Strip: delete the trailing marker first so `start` stays valid.
            self.buffer.delete_chars(end, end + mlen);
            self.buffer.delete_chars(start - mlen, start);
            self.buffer.cursor = self.idx_to_cursor(start - mlen);
        } else {
            self.wrap_inline(marker, start, end);
        }
    }

    /// Wrap `[start, end)` in `marker`, whatever is around it already.
    fn wrap_inline(&mut self, marker: &str, start: usize, end: usize) {
        if start >= end {
            return;
        }
        // Insert at the end first, then the start, so `start` stays valid.
        let (at_end, at_start) = (self.idx_to_cursor(end), self.idx_to_cursor(start));
        self.buffer.insert_str(at_end, marker);
        self.buffer.insert_str(at_start, marker);
        self.buffer.cursor = self.idx_to_cursor(start + marker.chars().count());
    }

    /// `gt`: flip a task checkbox, or add one to a plain list item.
    pub(super) fn toggle_task(&mut self) {
        // The block cache is refreshed by the RENDER path, so between two keys
        // in one event batch it can still describe the line before the last
        // edit — and `gt` on a line that only just became a list item would
        // read the kind it had before. Same guard, and the same reason, as
        // `open_line_break`; it early-returns when the revisions match.
        self.refresh_blocks();
        let line = self.buffer.cursor.line;
        let chars: Vec<char> = self.buffer.line_text(line).chars().collect();
        let base = self.buffer.rope.line_to_char(line);

        // A task's box sits right after the list marker — or the indent, on a
        // line that is not an item — and only there: a `[x]` further on is
        // link text or prose (`See [x](notes.md)`), never a box to tick.
        let indent = chars.iter().take_while(|c| **c == ' ' || **c == '\t').count();
        let item = match self.blocks.kinds.get(line) {
            Some(BlockKind::ListItem { marker, .. }) => Some(*marker),
            _ => None,
        };
        let mut p = indent;
        if let Some(marker) = item {
            match marker {
                Marker::Ordered => {
                    while p < chars.len() && chars[p].is_ascii_digit() {
                        p += 1;
                    }
                    p += 1;
                }
                _ => p += 1,
            }
            if p < chars.len() && (chars[p] == ' ' || chars[p] == '\t') {
                p += 1;
            }
        }
        let p = p.min(chars.len());

        // Flip an existing `[ ]` / `[x]`.
        if let ['[', inner @ (' ' | 'x' | 'X'), ']', ..] = chars[p..] {
            let new = if inner == ' ' { "x" } else { " " };
            self.buffer.delete_chars(base + p + 1, base + p + 2);
            self.buffer.insert_str(Cursor::new(line, p + 1), new);
            return;
        }

        // Otherwise, if it is a list item, insert a checkbox after the marker.
        if item.is_some() {
            self.buffer.insert_str(Cursor::new(line, p), "[ ] ");
        }
    }

    /// `ga`: pad every cell of the table under the cursor to its column's
    /// width, in the file. The cursor stays on the same character of the
    /// same cell; an already aligned table is left untouched, so `ga` on one
    /// adds nothing to undo.
    pub(super) fn align_table(&mut self) {
        if !self.align_table_at_cursor() {
            self.notify("not in a table", FlashKind::Error);
        }
    }

    /// `editor.table_align = "auto"`: leaving Insert after an edit in a table
    /// aligns it, as `ga` would. It joins the Insert session's undo step, so
    /// one `u` takes back the typing and the alignment together. Anywhere
    /// else it does nothing, and says nothing.
    pub(super) fn auto_align_table(&mut self) {
        let auto = TableAlign::parse(&self.config.editor.table_align) == Some(TableAlign::Auto);
        if auto && self.edited_since_mark() {
            self.align_table_at_cursor();
        }
    }

    /// Align the table on the cursor's line. False when it is not in one.
    fn align_table_at_cursor(&mut self) -> bool {
        self.refresh_blocks();
        let at = self.buffer.cursor;
        let Some((s, e)) = table::bounds(&self.blocks.kinds, at.line) else {
            return false;
        };
        let rows: Vec<String> = (s..=e).map(|l| self.buffer.line_text(l)).collect();
        let aligned = table::align(&rows);
        if aligned == rows {
            return true;
        }
        let from = self.buffer.rope.line_to_char(s);
        let to = self.buffer.char_index(Cursor::new(e, self.buffer.line_len(e)));
        self.buffer.delete_chars(from, to);
        self.buffer.insert_str(Cursor::new(s, 0), &aligned.join("\n"));
        let col = table::cursor_col(&rows[at.line - s], &aligned[at.line - s], at.col);
        self.buffer.cursor = Cursor::new(at.line, col);
        true
    }

    /// `g1`..`g6` set the heading level of the current line; `g0` strips it.
    pub(super) fn set_heading(&mut self, level: u8) {
        let line = self.buffer.cursor.line;
        let chars: Vec<char> = self.buffer.line_text(line).chars().collect();
        let hashes = chars.iter().take_while(|c| **c == '#').count();
        let mut content = hashes;
        while content < chars.len() && (chars[content] == ' ' || chars[content] == '\t') {
            content += 1;
        }
        let base = self.buffer.rope.line_to_char(line);
        if content > 0 {
            self.buffer.delete_chars(base, base + content);
        }
        if level >= 1 {
            let prefix = format!("{} ", "#".repeat(level as usize));
            self.buffer.insert_str(Cursor::new(line, 0), &prefix);
            self.buffer.cursor = Cursor::new(line, prefix.chars().count());
        } else {
            self.buffer.cursor = Cursor::new(line, 0);
        }
    }

    pub(super) fn enter_insert(&mut self) {
        self.insert_repeat = None;
        self.cc_indent_line = None;
        self.mode = Mode::Insert;
        self.touch_status();
    }

    /// `o` / `O`. On a closed fold, `o` opens the line below the whole fold
    /// and `O` the one above it, and the fold stays closed, as in vim.
    pub(super) fn open_line(&mut self, above: bool) {
        let line = self.buffer.cursor.line;
        let line = if above { line } else { self.fold_hidden().last_of(line) };
        let at = if above {
            Cursor::new(line, 0)
        } else {
            Cursor::new(line, self.buffer.line_len(line))
        };
        if above {
            self.buffer.insert_str(at, "\n");
            self.buffer.cursor = Cursor::new(line, 0);
        } else {
            self.buffer.insert_str(at, "\n");
            self.buffer.cursor = Cursor::new(line + 1, 0);
        }
        self.enter_insert();
    }

    /// `x`: delete `count` characters from under the cursor, on this line only.
    pub(super) fn delete_chars_under(&mut self, count: usize) {
        let c = self.buffer.cursor;
        let len = self.buffer.line_len(c.line);
        if len == 0 || c.col >= len {
            return;
        }
        let end_col = c.col.saturating_add(count).min(len);
        let start = self.buffer.char_index(c);
        let end = self.buffer.char_index(Cursor::new(c.line, end_col));
        if end > start {
            let text = self.buffer.rope.slice(start..end).to_string();
            self.buffer.delete_chars(start, end);
            self.store_register(text, false, true);
        }
    }

    /// `X`: delete `count` characters before the cursor, on this line.
    pub(super) fn delete_chars_before(&mut self, count: usize) {
        let c = self.buffer.cursor;
        if c.col == 0 {
            return;
        }
        let start_col = c.col.saturating_sub(count);
        let a = self.buffer.char_index(Cursor::new(c.line, start_col));
        let b = self.buffer.char_index(c);
        if b > a {
            let text = self.buffer.rope.slice(a..b).to_string();
            self.buffer.delete_chars(a, b);
            self.buffer.cursor = Cursor::new(c.line, start_col);
            self.store_register(text, false, true);
        }
    }

    /// `J`: join the next line onto this one, separated by a single space (unless
    /// this line is empty / already ends in space, or the next line is empty).
    /// `NJ` joins N lines.
    pub(super) fn join_lines(&mut self, count: usize) {
        for _ in 0..count.saturating_sub(1).max(1) {
            let line = self.buffer.cursor.line;
            if line >= self.buffer.last_line() {
                break;
            }
            let cur_len = self.buffer.line_len(line);
            let cur_text = self.buffer.line_text(line);
            let next_text = self.buffer.line_text(line + 1);
            let lead = next_text.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            let eol = self.buffer.char_index(Cursor::new(line, cur_len));
            let next_start = self.buffer.rope.line_to_char(line + 1);
            self.buffer.delete_chars(eol, next_start + lead);

            let ends_ws = cur_text.chars().last().is_some_and(|c| c == ' ' || c == '\t');
            let next_empty = next_text.chars().nth(lead).is_none();
            if cur_len > 0 && !ends_ws && !next_empty {
                self.buffer.insert_str(Cursor::new(line, cur_len), " ");
            }
            self.buffer.cursor = Cursor::new(line, cur_len);
        }
    }

    /// `~`: flip the case of `count` characters and advance.
    pub(super) fn toggle_case(&mut self, count: usize) {
        let cur = self.buffer.cursor;
        let chars: Vec<char> = self.buffer.line_text(cur.line).chars().collect();
        let n = count.min(chars.len().saturating_sub(cur.col));
        if n == 0 {
            return;
        }
        let mut out = String::new();
        for &c in &chars[cur.col..cur.col + n] {
            if c.is_uppercase() {
                out.extend(c.to_lowercase());
            } else if c.is_lowercase() {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
        }
        let a = self.buffer.char_index(cur);
        let b = self.buffer.char_index(Cursor::new(cur.line, cur.col + n));
        self.buffer.delete_chars(a, b);
        self.buffer.insert_str(cur, &out);
        let len = self.buffer.line_len(cur.line);
        self.buffer.cursor = Cursor::new(cur.line, (cur.col + n).min(len.saturating_sub(1)));
    }

    /// `;` / `,`: repeat the last `f`/`t` (`,` reverses direction).
    ///
    /// A repeated till that would stay put goes on to the match after, as
    /// vim's does: `t` stops just short of its match, so searching again from
    /// there finds the same match and would never move.
    /// The find `;` (`,` with `reverse`) stands for, and the count to give
    /// it: one more when a till would otherwise stay where it is.
    pub(super) fn find_again(&self, reverse: bool, count: usize) -> Option<(Motion, usize)> {
        let (target, forward, till) = self.last_find?;
        let m = Motion::FindChar { target, forward: forward ^ reverse, till };
        let stays = till && motion::resolve(&self.buffer, m, count, 1, false, self.config.editor.tab_width).is_none();
        Some((m, if stays { count + 1 } else { count }))
    }

    /// `*`: search forward for the word under the cursor.
    pub(super) fn search_word_under_cursor(&mut self) {
        let cur = self.buffer.cursor;
        if let Some((s, e)) = self.word_range(cur.line, cur.col) {
            let word: String = self.buffer.line_text(cur.line).chars().skip(s).take(e - s).collect();
            self.search = Some(Search { pattern: word, reverse: false });
            self.search_move(false);
        }
    }

    /// `gl`: wrap the word (or selection) as `[text](url)` and drop the cursor
    /// into the empty URL slot in Insert mode.
    pub(super) fn wrap_link(&mut self) {
        let (start, end) = if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            let (_, s, e) = self.charwise_selection();
            self.end_visual();
            (s, e)
        } else {
            let cur = self.buffer.cursor;
            match self.word_range(cur.line, cur.col) {
                Some((sc, ec)) => (
                    self.buffer.char_index(Cursor::new(cur.line, sc)),
                    self.buffer.char_index(Cursor::new(cur.line, ec)),
                ),
                None => return,
            }
        };
        if start >= end {
            return;
        }
        let (at_end, at_start) = (self.idx_to_cursor(end), self.idx_to_cursor(start));
        self.buffer.insert_str(at_end, "]()");
        self.buffer.insert_str(at_start, "[");
        self.buffer.cursor = self.idx_to_cursor(end + 3);
        self.enter_insert();
    }

    /// `D` / `C`: operate from the cursor to the end of the line.
    pub(super) fn change_or_delete_to_eol(&mut self, op: Operator) {
        let c = self.buffer.cursor;
        let len = self.buffer.line_len(c.line);
        if len > c.col {
            self.operate_charwise(op, c, Cursor::new(c.line, len - 1), true);
        } else if op == Operator::Change {
            self.enter_insert();
        }
    }

    /// `r`: overwrite `count` characters with `ch`.
    pub(super) fn replace_chars(&mut self, ch: char, count: usize) {
        let cur = self.buffer.cursor;
        let len = self.buffer.line_len(cur.line);
        let n = count.min(len.saturating_sub(cur.col));
        if n == 0 {
            return;
        }
        let start = self.buffer.char_index(cur);
        let end = self.buffer.char_index(Cursor::new(cur.line, cur.col + n));
        self.buffer.delete_chars(start, end);
        let repl: String = std::iter::repeat_n(ch, n).collect();
        self.buffer.insert_str(cur, &repl);
        self.buffer.cursor = Cursor::new(cur.line, cur.col + n - 1);
    }

    /// Delete/change/yank/case a charwise range between two cursors.
    pub(super) fn operate_charwise(&mut self, op: Operator, a: Cursor, b: Cursor, inclusive: bool) {
        // Indent is a LINE operation whatever motion named it: `>w` shifts the
        // whole line, exactly as in vim.
        if matches!(op, Operator::Indent | Operator::Outdent) {
            return self.operate_linewise(op, a.line, b.line);
        }
        let (lo, hi) = if (a.line, a.col) <= (b.line, b.col) {
            (a, b)
        } else {
            (b, a)
        };
        let start = self.buffer.char_index(lo);
        let mut end = self.buffer.char_index(hi);
        if inclusive {
            end += 1;
        }
        end = end.min(self.buffer.rope.len_chars());
        if start >= end {
            // An empty range changes nothing, but `c` still starts typing
            // there, as vim's `cT(` next to the `(` does.
            if op == Operator::Change {
                self.buffer.cursor = lo;
                self.enter_insert();
            }
            return;
        }
        let text = self.buffer.rope.slice(start..end).to_string();
        match op {
            Operator::Yank => {
                self.store_register(text, false, false);
                self.buffer.cursor = lo;
            }
            Operator::Delete => {
                self.buffer.delete_chars(start, end);
                self.store_register(text, false, true);
                self.buffer.cursor = lo;
            }
            Operator::Change => {
                self.buffer.delete_chars(start, end);
                self.store_register(text, false, true);
                self.buffer.cursor = lo;
                self.enter_insert();
            }
            Operator::Lowercase | Operator::Uppercase => {
                self.recase(start, end, op == Operator::Uppercase);
                self.buffer.cursor = lo;
            }
            Operator::Indent | Operator::Outdent => unreachable!("handled above"),
        }
    }

    /// Delete/change/yank whole lines `l1..=l2`.
    pub(super) fn operate_linewise(&mut self, op: Operator, l1: usize, l2: usize) {
        let last = self.buffer.last_line();
        // The cursor's goal as a screen cell, read before the edit: a delete
        // or shift puts the cursor back on it, as Neovim's 'nostartofline'.
        let tw = self.config.editor.tab_width;
        let cells = motion::goal_cells(&self.buffer, self.buffer.cursor, tw);
        let revision = self.buffer.revision;
        // A closed fold at either end is taken whole: `dd` on one deletes
        // every line it stands for, as in vim.
        let (a, b) = self.fold_hidden().widen(l1.min(l2), l1.max(l2));
        let b = b.min(last);
        let start = self.buffer.rope.line_to_char(a);
        let end = if b < last {
            self.buffer.rope.line_to_char(b + 1)
        } else {
            self.buffer.rope.len_chars()
        };
        let mut text = self.buffer.rope.slice(start..end).to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        match op {
            Operator::Yank => {
                self.store_register(text, true, false);
                // `yy` and `yj` leave the cursor where it was; a range that
                // starts above it (`yk`, `yip`) takes it to its first line.
                let cur = self.buffer.cursor;
                if a < cur.line {
                    let col = cur.col.min(self.buffer.line_len(a).saturating_sub(1));
                    self.buffer.cursor = Cursor::new(a, col);
                }
            }
            Operator::Delete => {
                self.buffer.delete_chars(start, end);
                self.store_register(text, true, true);
                // On the goal's column of the line now there, which becomes
                // the goal: Neovim's `dd` from a long line onto `ab` and then
                // `j` stays on `ab`'s column.
                let line = a.min(self.buffer.last_line());
                self.buffer.cursor = Cursor::new(line, motion::col_at_cells(&self.buffer, line, cells, tw));
                self.land_for_redo(revision);
            }
            Operator::Change => {
                // With `auto_indent`, the line to type into keeps the first
                // line's indent, as vim's `cc` does with 'autoindent'.
                let indent: String = if self.config.editor.auto_indent {
                    text.chars().take_while(|c| *c == ' ' || *c == '\t').collect()
                } else {
                    String::new()
                };
                self.buffer.delete_chars(start, end);
                self.store_register(text, true, true);
                // Leave one line to type into.
                let at = Cursor::new(a.min(self.buffer.line_count().saturating_sub(1)), 0);
                self.buffer.insert_str(at, &format!("{indent}\n"));
                let line = a.min(self.buffer.line_count().saturating_sub(1));
                self.buffer.cursor = Cursor::new(line, indent.chars().count());
                self.enter_insert();
                if !indent.is_empty() {
                    self.cc_indent_line = Some(line);
                }
            }
            Operator::Lowercase | Operator::Uppercase => {
                self.recase(start, end, op == Operator::Uppercase);
                self.buffer.cursor = Cursor::new(a, 0);
            }
            Operator::Indent | Operator::Outdent => {
                // The same screen column, not the same character, as
                // Neovim's `>>`: the text moves under the cursor.
                self.shift_lines(a, b, op == Operator::Outdent);
                self.buffer.cursor = Cursor::new(a, motion::col_at_cells(&self.buffer, a, cells, tw));
                self.land_for_redo(revision);
            }
        }
    }

    /// Redo of the edit just made puts the cursor where it is now, as
    /// Neovim's does, rather than where the edit's last change ended (the
    /// start of a deleted range, the end of an added indent). Only when the
    /// text changed since `revision`: otherwise the latest change belongs to
    /// an earlier command.
    fn land_for_redo(&mut self, revision: u64) {
        if self.buffer.revision != revision {
            let cursor = self.buffer.cursor;
            self.buffer.history.set_redo_cursor(cursor);
        }
    }

    /// Rewrite an absolute char range in one case. Delete + insert so it is a
    /// single history step, and so a mapping that changes length (`ß` -> `SS`)
    /// still lands correctly.
    fn recase(&mut self, start: usize, end: usize, upper: bool) {
        let text = self.buffer.rope.slice(start..end).to_string();
        let mapped = if upper {
            text.to_uppercase()
        } else {
            text.to_lowercase()
        };
        if mapped == text {
            return;
        }
        let at = self.idx_to_cursor(start);
        self.buffer.delete_chars(start, end);
        self.buffer.insert_str(at, &mapped);
    }
}
