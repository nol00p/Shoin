use super::*;

impl App {
    /// `>al` `<al` `>il` `<il` `>aL` `<aL`: shift lines `l1..=l2` of the item
    /// (or level) under the cursor, unless that would break the list's
    /// structure — then refuse, change nothing, and say why. The cursor stays
    /// on the same character of text.
    pub(super) fn shift_list(&mut self, outdent: bool, level: bool, l1: usize, l2: usize) {
        let width = self.config.editor.tab_width.max(1);
        let line = self.buffer.cursor.line;
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, width);
        let top_level = lists.parent(line).is_none();
        let at_col = lists.item_indent(line).unwrap_or(0);
        // How far to shift, in the document's own steps rather than always
        // `tab_width`. Out, the item lands on its parent's column: it becomes
        // the parent's sibling, and a deeper item cannot skip a level. In, it
        // lines up with the children it joins, or else nests by the step the
        // nearest nesting in the file uses.
        //
        // A whole level moves by one step either way, and out no further than
        // its parent's column. Its own distance from that parent is what
        // changes, so it is not a measure of the step.
        let skip = if level { lists.level_items(line) } else { Vec::new() };
        let step = lists.step(line, &skip).unwrap_or(width);
        let gap = lists.parent(line).map(|p| at_col.saturating_sub(lists.item_indent(p).unwrap_or(0)));
        let cols = if outdent && level {
            gap.map_or(step, |g| step.min(g))
        } else if outdent {
            gap.unwrap_or(width)
        } else if level {
            step
        } else {
            lists
                .prev_sibling_bounds(line)
                .and_then(|(p, _)| lists.first_child(p))
                .and_then(|c| lists.item_indent(c))
                .filter(|&c| c > at_col)
                .map_or(step, |c| c - at_col)
        };
        let refusal = if outdent {
            top_level.then_some("already at the top level")
        } else if level {
            // A top-level list indented 4 columns is an indented code block
            // to every other Markdown tool. Shoin would still call it a list,
            // which only hides the problem. A tab is `width` columns too.
            let to = list::indent_cols(&self.buffer.line_text(l1), width) + cols;
            (top_level && to >= 4).then_some("a top-level list 4 columns in reads as code")
        } else {
            // The first child has nothing to nest under: it would only look
            // deeper while staying the same child of the same parent.
            lists.prev_sibling_bounds(line).is_none().then_some("nothing to indent under")
        };
        if let Some(why) = refusal {
            return self.notify(why, FlashKind::Error);
        }
        // A shift changes columns, never line numbers, so these still name
        // the same text afterwards: the item, the neighbours it may have left
        // behind, its first child (which `>il` and `<il` re-parent), and the
        // list it is about to join. A whole level keeps every relationship,
        // so there is nothing to redo.
        let mut touched = Vec::new();
        if !level {
            let prev = lists.prev_sibling_bounds(line).map(|b| b.0);
            touched.push(line);
            touched.extend(lists.first_child(line));
            touched.extend(prev);
            touched.extend(lists.next_sibling_bounds(line).map(|b| b.0));
            if outdent {
                touched.extend(lists.parent(line));
            } else {
                touched.extend(prev.and_then(|p| lists.first_child(p)));
            }
        }
        let starts = list_starts(&lists, &touched);

        let at = self.buffer.cursor;
        let before = self.buffer.line_len(at.line) as isize;
        self.shift_lines_by(l1, l2, outdent, cols);
        if self.config.editor.list_renumber && !touched.is_empty() {
            self.renumber_lists(&touched, &starts, l1);
        }
        let moved = self.buffer.line_len(at.line) as isize - before;
        self.buffer.cursor = Cursor::new(at.line, (at.col as isize + moved).max(0) as usize);
    }

    /// `Alt-j` / `Alt-k`: move the item under the cursor, children and all,
    /// past `count` of its siblings — stopping at the end of the list rather
    /// than failing when the count is larger. It never leaves its list: that
    /// is what `<al` is for.
    ///
    /// In Visual mode it moves the run of siblings the selection touches
    /// (`Lists::sibling_run`) as one block, and the selection stays on it.
    ///
    /// The block and the siblings it passes swap places as two blocks of
    /// text, and the blank lines between them stay where they were. The
    /// cursor, and in Visual the anchor, follow the block on the same
    /// character.
    pub(super) fn move_list_item(&mut self, down: bool, count: usize) {
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        let at = self.buffer.cursor;
        let anchor = self.visual_anchor();
        // `run`: the item lines of the first and last item that move;
        // `item`: the lines they span, children included.
        let found = match anchor {
            None => lists.item_bounds(at.line).map(|b| ((b.0, b.0), b)).ok_or(RunError::NotInList),
            Some(a) => lists
                .sibling_run(a.line.min(at.line), a.line.max(at.line))
                .and_then(|run| {
                    let end = lists.item_bounds(run.1).ok_or(RunError::NotInList)?.1;
                    Ok((run, (run.0, end)))
                }),
        };
        let (run, item) = match found {
            Ok(found) => found,
            Err(RunError::NotInList) => return self.notify("not in a list", FlashKind::Error),
            Err(RunError::NoItem) => {
                return self.notify("selection touches no item", FlashKind::Error);
            }
            Err(RunError::TwoLists) => {
                return self.notify("selection spans two lists", FlashKind::Error);
            }
        };
        // The far end of the siblings passed over.
        let step = |line: usize| match down {
            true => lists.next_sibling_bounds(line),
            false => lists.prev_sibling_bounds(line),
        };
        let Some(near) = step(if down { run.1 } else { run.0 }) else {
            let why = if down { "last item" } else { "first item" };
            return self.notify(why, FlashKind::Error);
        };
        let mut far = near;
        for _ in 1..count {
            match step(far.0) {
                Some(b) => far = b,
                None => break,
            }
        }
        let past = if down { (near.0, far.1) } else { (far.0, near.1) };
        let start = list_starts(&lists, &[item.0]).first().map(|(_, n)| *n);
        // Each end's line length, to follow a renumber that widens it.
        let width = |c: Cursor| self.buffer.line_len(c.line) as isize;
        let (cursor_was, anchor_was) = (width(at), anchor.map(width));

        // `first`, the gap, then `second` become `second`, the gap, `first`.
        let (first, second) = if down { (item, past) } else { (past, item) };
        let lines = |(a, b): (usize, usize)| (a..=b).map(|l| self.buffer.line_text(l)).collect::<Vec<_>>();
        let gap: Vec<String> = (first.1 + 1..second.0).map(|l| self.buffer.line_text(l)).collect();
        let swapped = [lines(second), gap.clone(), lines(first)].concat().join("\n");

        let from = self.buffer.rope.line_to_char(first.0);
        let to = self.buffer.char_index(Cursor::new(second.1, self.buffer.line_len(second.1)));
        self.buffer.delete_chars(from, to);
        self.buffer.insert_str(Cursor::new(first.0, 0), &swapped);

        let new_start = if down { first.0 + (second.1 - second.0 + 1) + gap.len() } else { first.0 };
        let new_end = new_start + (item.1 - item.0);
        // Its own list, counting from where it started — the other lists the
        // two blocks hold moved whole, so their numbers are still in order.
        if self.config.editor.list_renumber && start.is_some() {
            self.refresh_blocks();
            let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
            let edits = lists.renumber(new_start, start);
            self.apply_renumber(edits);
        }
        // An end inside the block moves with its line, and its column with the
        // line's text, which only changed if its number did. An end on a
        // blank line outside it lands on the block's edge.
        let follow = |buffer: &Buffer, c: Cursor, was: isize| {
            if c.line < item.0 {
                Cursor::new(new_start, 0)
            } else if c.line > item.1 {
                Cursor::new(new_end, buffer.line_len(new_end).saturating_sub(1))
            } else {
                let line = new_start + (c.line - item.0);
                let moved = buffer.line_len(line) as isize - was;
                Cursor::new(line, (c.col as isize + moved).max(0) as usize)
            }
        };
        self.buffer.cursor = follow(&self.buffer, at, cursor_was);
        if let (Some(a), Some(was)) = (anchor, anchor_was) {
            self.anchor = Some(follow(&self.buffer, a, was));
        }
    }

    /// `move_lines_down` / `move_lines_up`: the selected lines (the cursor's
    /// line outside Visual) swap with the `count` lines past them, like Vim's
    /// `:'<,'>m '>+1 | gv`. Both ends of the selection move with the text, so
    /// it stays selected for the next press, and each press is its own undo
    /// step because every Visual key opens one. A count reaching past the
    /// buffer's end goes as far as it can; none to go at all is refused.
    pub(super) fn move_lines(&mut self, down: bool, count: usize) {
        let cur = self.buffer.cursor;
        let anchor = self.visual_anchor();
        let other = anchor.unwrap_or(cur);
        // `sync_after_input` keeps the anchor inside the text between keys,
        // but not between the keys of a `.` replay, where an edit can shorten
        // the text under it.
        let last = self.buffer.last_line();
        let (top, bot) = (cur.line.min(other.line).min(last), cur.line.max(other.line).min(last));
        let room = if down { self.buffer.last_line() - bot } else { top };
        let n = count.min(room);
        if n == 0 {
            let why = if down { "last line" } else { "first line" };
            return self.notify(why, FlashKind::Error);
        }
        // `first` then `second` become `second` then `first`.
        let (first, second) = if down { ((top, bot), (bot + 1, bot + n)) } else { ((top - n, top - 1), (top, bot)) };
        let lines = |(a, b): (usize, usize)| (a..=b).map(|l| self.buffer.line_text(l)).collect::<Vec<_>>();
        let swapped = [lines(second), lines(first)].concat().join("\n");

        let from = self.buffer.rope.line_to_char(first.0);
        let to = self.buffer.char_index(Cursor::new(second.1, self.buffer.line_len(second.1)));
        self.buffer.delete_chars(from, to);
        self.buffer.insert_str(Cursor::new(first.0, 0), &swapped);

        let line = |l: usize| if down { l + n } else { l - n };
        self.buffer.cursor = Cursor::new(line(cur.line), cur.col);
        self.anchor = anchor.map(|a| Cursor::new(line(a.line.min(last)), a.col));
    }

    /// Rewrite the ordinals `edits` name. Each is on a line of its own and
    /// leaves the line count alone, so the order they go in does not matter.
    fn apply_renumber(&mut self, edits: impl IntoIterator<Item = list::Renumber>) {
        for r in edits {
            let start = self.buffer.char_index(Cursor::new(r.line, r.cols.start));
            self.buffer.delete_chars(start, start + r.cols.len());
            self.buffer.insert_str(Cursor::new(r.line, r.cols.start), &r.number);
        }
    }

    /// `>al` / `<al` on the item under the cursor, from any mode — the
    /// `list_indent` / `list_outdent` actions and Insert-mode Tab.
    pub(super) fn shift_list_item(&mut self, outdent: bool) {
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        match lists.item_bounds(self.buffer.cursor.line) {
            Some((s, e)) => self.shift_list(outdent, false, s, e),
            None => self.notify("not in a list", FlashKind::Error),
        }
    }

    /// `:fixlist` / `fix_list`: re-indent the list under the cursor so that
    /// CommonMark tools read it the way Shoin shows it (`Lists::realign`).
    /// One undo step; the cursor stays on the same character of text.
    pub(super) fn fix_list(&mut self) {
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        let Some(plan) = lists.realign(self.buffer.cursor.line, self.blocks.math) else {
            return self.notify("not in a list", FlashKind::Error);
        };
        let skipped = match plan.skipped.as_slice() {
            [] => String::new(),
            lines => {
                let at: Vec<String> = lines.iter().map(|l| (l + 1).to_string()).collect();
                let (items, them) = if lines.len() == 1 { ("item", "it") } else { ("items", "them") };
                format!(
                    "left {} {items} as written (line{} {}): moving {them} would change how Shoin shows the list",
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" },
                    at.join(", ")
                )
            }
        };
        if plan.lines.is_empty() {
            return match skipped.is_empty() {
                true => self.notify("list already lined up", FlashKind::Info),
                false => self.notify(skipped, FlashKind::Info),
            };
        }
        let at = self.buffer.cursor;
        let before = self.buffer.line_len(at.line) as isize;
        // One undo step. Bound in Insert, it is a step of its own inside the
        // session, as a paste is, and leaves the session's grouping alone.
        let inserting = matches!(self.mode, Mode::Insert);
        if inserting {
            self.coalesce_insert(true);
        } else {
            self.buffer.history.begin_group(Some(at));
        }
        for &(line, cols) in &plan.lines {
            let start = self.buffer.rope.line_to_char(line);
            let ws = leading_ws_cols(&self.buffer.line_text(line));
            self.buffer.delete_chars(start, start + ws);
            self.buffer.insert_str(Cursor::new(line, 0), &" ".repeat(cols));
        }
        if inserting {
            self.buffer.history.split();
        } else {
            self.buffer.history.end_group();
        }
        let moved = self.buffer.line_len(at.line) as isize - before;
        self.buffer.cursor = Cursor::new(at.line, (at.col as isize + moved).max(0) as usize);
        let n = plan.lines.len();
        let mut msg = format!("re-indented {n} line{}", if n == 1 { "" } else { "s" });
        if !skipped.is_empty() {
            msg = format!("{msg}; {skipped}");
        }
        self.notify(msg, FlashKind::Info);
    }

    /// Whether Insert-mode Tab acts on a list item: `editor.list_tab` is on
    /// and the cursor is on an item's own line. A continuation line, or any
    /// other, keeps the plain Tab.
    pub(super) fn list_tab_applies(&mut self) -> bool {
        if !self.config.editor.list_tab {
            return false;
        }
        self.refresh_blocks();
        matches!(
            self.blocks.kinds.get(self.buffer.cursor.line),
            Some(BlockKind::ListItem { .. })
        )
    }

    /// Put every ordered level containing one of `lines` back in sequence
    /// after `moved` (an item line) changed level. Lines outside a list, and
    /// bullet lists, are skipped.
    ///
    /// A list keeps the start number it had: `starts` are the levels as they
    /// were, and a level now counts from the old one it shares an item with —
    /// the moved item aside, since it just arrived. Sharing none, it is a
    /// sub-list the edit created, and counts from 1.
    fn renumber_lists(&mut self, lines: &[usize], starts: &[(Vec<usize>, u64)], moved: usize) {
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        let start = |items: &[usize]| {
            starts
                .iter()
                .find(|(old, _)| old.iter().any(|i| *i != moved && items.contains(i)))
                .map_or(1, |(_, n)| *n)
        };
        // Keyed by line: two of `lines` in one level ask for the same edits.
        let edits: std::collections::BTreeMap<usize, list::Renumber> = lines
            .iter()
            .flat_map(|&l| lists.renumber(l, Some(start(&lists.level_items(l)))))
            .map(|r| (r.line, r))
            .collect();
        self.apply_renumber(edits.into_values());
    }

    /// `>`/`<` over whole lines. One indent unit is `tab_width` spaces, or a
    /// literal tab when `expand_tab` is off. Blank lines are left alone —
    /// indenting whitespace-only lines just leaves trailing whitespace behind.
    pub(super) fn shift_lines(&mut self, l1: usize, l2: usize, outdent: bool) {
        let width = self.config.editor.tab_width.max(1);
        self.shift_lines_by(l1, l2, outdent, width);
    }

    /// `shift_lines` by `cols` columns instead of one indent unit. Indenting
    /// inserts tabs only when `expand_tab` is off and `cols` is whole tabs,
    /// spaces otherwise; outdenting removes leading whitespace up to `cols`
    /// columns, a tab counting as `tab_width`.
    fn shift_lines_by(&mut self, l1: usize, l2: usize, outdent: bool, cols: usize) {
        let width = self.config.editor.tab_width.max(1);
        let cols = cols.max(1);
        let unit: String = if !self.config.editor.expand_tab && cols.is_multiple_of(width) {
            "\t".repeat(cols / width)
        } else {
            " ".repeat(cols)
        };
        for line in l1..=l2.min(self.buffer.line_count().saturating_sub(1)) {
            let text = self.buffer.line_text(line);
            if text.trim().is_empty() {
                continue;
            }
            if !outdent {
                self.buffer.insert_str(Cursor::new(line, 0), &unit);
                continue;
            }
            let mut drop = 0usize;
            let mut gone = 0usize;
            for c in text.chars() {
                if gone >= cols {
                    break;
                }
                match c {
                    '\t' => gone += width,
                    ' ' => gone += 1,
                    _ => break,
                }
                drop += 1;
            }
            if drop > 0 {
                let start = self.buffer.char_index(Cursor::new(line, 0));
                self.buffer.delete_chars(start, start + drop);
            }
        }
    }

    /// Column of the first non-blank character on `line`.
    pub(super) fn first_non_blank(&self, line: usize) -> usize {
        self.buffer
            .line_text(line)
            .chars()
            .take_while(|c| c.is_whitespace())
            .count()
    }
}
