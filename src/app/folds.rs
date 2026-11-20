use super::*;
use crate::input::action::FoldCmd;
use crate::render::hidden::{self, Hidden};
use crate::text::fold::{self, Fold, Folds};

impl App {
    // ------------------------------------------------------------------ folds

    /// What the focused pane has folded in the current document. Folds are
    /// per pane, as vim's are per window: two panes on one file fold apart.
    pub(crate) fn folds(&self) -> &Folds {
        self.docs[self.current()].folds_in(self.focus_pane)
    }

    /// The focused pane's folds, to change, and the text they fold.
    fn folds_mut(&mut self) -> (&mut Folds, &Buffer) {
        let (pane, i) = (self.focus_pane, self.current());
        let doc = &mut self.docs[i];
        (doc.folds.entry(pane).or_default(), &doc.buffer)
    }

    /// `za` `zo` `zc` `zR` `zM`. In Visual, `zo` and `zc` work on every line
    /// of the selection and end it; the others act at the cursor and keep it,
    /// as vim's do.
    pub(super) fn fold(&mut self, cmd: FoldCmd) {
        // What can fold is read off the line kinds, which lag an edit until
        // the next frame: bring them up to date first.
        self.refresh_blocks();
        self.sync_folds();
        let visual = matches!(self.mode, Mode::Visual | Mode::VisualLine);
        if visual && matches!(cmd, FoldCmd::Open | FoldCmd::Close) {
            return self.fold_selection(cmd == FoldCmd::Open);
        }
        let line = self.buffer.cursor.line;
        match cmd {
            FoldCmd::Toggle => {
                if !self.folds().is_head(line) {
                    self.close_fold(line);
                } else {
                    self.folds_mut().0.open_at(line);
                }
            }
            FoldCmd::Open => {
                if !self.folds_mut().0.open_at(line) {
                    self.notify("no closed fold here", FlashKind::Info);
                }
            }
            FoldCmd::Close => self.close_fold(line),
            FoldCmd::OpenAll => self.folds_mut().0.open_all(),
            FoldCmd::CloseAll => {
                let tw = self.config.editor.tab_width;
                let all = fold::all(&self.buffer, &self.blocks.kinds, tw);
                if all.is_empty() {
                    return self.notify("nothing to fold", FlashKind::Info);
                }
                let (folds, buffer) = self.folds_mut();
                folds.close_all(all, buffer);
                // The cursor goes to the line shown for it, as vim's does.
                self.cursor_to_shown();
            }
        }
    }

    /// Visual `zc` / `zo`, as vim's: walk the selected lines top to bottom.
    /// `zc` closes the innermost open fold around each and goes on after it;
    /// `zo` opens the closed fold each shown line stands for, one level, and
    /// goes on after it. The cursor goes to the start of the selection.
    fn fold_selection(&mut self, open: bool) {
        let anchor = self.anchor.unwrap_or(self.buffer.cursor);
        let cursor = self.buffer.cursor;
        let (lo, hi) = (anchor.line.min(cursor.line), anchor.line.max(cursor.line));
        let start = if matches!(self.mode, Mode::VisualLine) {
            Cursor::new(lo, 0)
        } else if (anchor.line, anchor.col) <= (cursor.line, cursor.col) {
            anchor
        } else {
            cursor
        };
        self.end_visual();
        let mut done = false;
        let mut l = self.fold_hidden().shown(lo);
        while l <= hi {
            if open {
                let next = self.fold_hidden().last_of(l) + 1;
                done |= self.folds_mut().0.open_at(l);
                l = next;
            } else {
                done |= self.close_innermost(l).is_some();
                let h = self.fold_hidden();
                l = h.last_of(h.shown(l)) + 1;
            }
        }
        self.buffer.cursor = start;
        self.cursor_to_shown();
        if !done {
            self.notify(if open { "no closed fold here" } else { "no fold here" }, FlashKind::Info);
        }
    }

    /// Close the innermost fold around `line` that is still open, and put
    /// the cursor on its first line.
    fn close_fold(&mut self, line: usize) {
        let Some(f) = self.close_innermost(line) else {
            return self.notify("no fold here", FlashKind::Info);
        };
        if f.start != line {
            let col = self.buffer.cursor.col;
            self.buffer.cursor = Cursor::new(f.start, col);
        }
    }

    /// Close the innermost fold around `line` that is still open, if there
    /// is one, and say which.
    fn close_innermost(&mut self, line: usize) -> Option<Fold> {
        let tw = self.config.editor.tab_width;
        let around = fold::candidates(&self.buffer, &self.blocks.kinds, tw, line);
        let (folds, buffer) = self.folds_mut();
        let f = around.into_iter().find(|f| !folds.closed().contains(f))?;
        folds.close(f, buffer);
        Some(f)
    }

    /// A cursor a closed fold now hides goes to the fold's line, at its start:
    /// where vim shows a cursor inside a closed fold.
    fn cursor_to_shown(&mut self) {
        let line = self.buffer.cursor.line;
        let shown = self.fold_hidden().shown(line);
        if shown != line {
            self.buffer.cursor = Cursor::new(shown, 0);
        }
    }

    /// The lines the focused pane's closed folds hide in the current
    /// document. Only folds: what `j`, `dd` and `p` treat as one line.
    pub(super) fn fold_hidden(&self) -> Hidden {
        hidden::of(self.folds(), &[])
    }

    /// The line `count` shown lines down (or up) from `from`, a closed fold
    /// counting as one line; as far as there are lines. `None` when there is
    /// no shown line that way at all.
    pub(super) fn shown_step(&self, from: usize, down: bool, count: usize) -> Option<usize> {
        let h = self.fold_hidden();
        let last = self.buffer.last_line();
        let mut l = from;
        for _ in 0..count {
            if down {
                let next = h.last_of(l) + 1;
                if next > last {
                    break;
                }
                l = next;
            } else {
                if l == 0 {
                    break;
                }
                l = h.shown(l - 1);
            }
        }
        (l != from).then_some(l)
    }

    /// `j`/`k` with a count, as the line count `motion::resolve` takes:
    /// closed folds are stepped over as one line. `None` when there is
    /// nowhere to go.
    pub(super) fn fold_count(&self, m: Motion, count: usize) -> Option<usize> {
        let down = match m {
            Motion::Down => true,
            Motion::Up => false,
            _ => return Some(count),
        };
        if self.folds().is_empty() {
            return Some(count);
        }
        let from = self.buffer.cursor.line;
        self.shown_step(from, down, count).map(|to| to.abs_diff(from))
    }

    /// The motions that go by more than a line, around closed folds, as
    /// vim's. True when `m` was one of them and has been made.
    ///
    /// `Ctrl-d` `Ctrl-u` `Ctrl-f` `Ctrl-b` count a closed fold as one line,
    /// as `j` and `k` do, so they never land inside one.
    ///
    /// `}` from a folded line starts after the fold, and `{` before it, so
    /// the blank lines a fold hides do not stop it. Each step of a count
    /// starts that way. A `}` or `{` that lands on a blank line inside a
    /// closed fold leaves the cursor on the folded line, which stays closed.
    pub(super) fn fold_move(&mut self, m: Motion, count: usize) -> bool {
        if self.folds().is_empty() {
            return false;
        }
        let page = self.viewport_height();
        let (lines, down) = match m {
            Motion::HalfPageDown => (page / 2, true),
            Motion::HalfPageUp => (page / 2, false),
            Motion::PageDown => (page, true),
            Motion::PageUp => (page, false),
            Motion::ParagraphForward | Motion::ParagraphBack => {
                self.paragraph_over_folds(m, count, page);
                return true;
            }
            _ => return false,
        };
        // As `j` and `k` that many shown lines, which keeps the column the
        // way they do.
        let from = self.buffer.cursor.line;
        let Some(to) = self.shown_step(from, down, lines.max(1)) else { return true };
        let step = if down { Motion::Down } else { Motion::Up };
        let tw = self.config.editor.tab_width;
        if let Some(res) = motion::resolve(&self.buffer, step, to.abs_diff(from), page, false, tw) {
            self.buffer.cursor = res.target;
        }
        true
    }

    fn paragraph_over_folds(&mut self, m: Motion, count: usize, page: usize) {
        let h = self.fold_hidden();
        let forward = m == Motion::ParagraphForward;
        let tw = self.config.editor.tab_width;
        let start = self.buffer.cursor;
        let mut at = start;
        for _ in 0..count.max(1) {
            let shown = h.shown(at.line);
            let from = if forward { h.last_of(shown) } else { shown };
            self.buffer.cursor = Cursor::new(from, 0);
            match motion::resolve(&self.buffer, m, 1, page, false, tw) {
                Some(res) => at = res.target,
                None => break,
            }
        }
        self.buffer.cursor = start;
        if at.line != start.line || at.col != start.col {
            self.buffer.cursor = at;
            self.cursor_to_shown();
        }
    }

    /// Run `edit`, which changes line `line` in place (`J` joining onto it),
    /// and keep closed every fold that started there, in every pane, if it
    /// still measures as one: vim's `J` on a closed fold leaves it closed.
    pub(super) fn keeping_folds_at(&mut self, line: usize, edit: impl FnOnce(&mut Self)) {
        let i = self.current();
        let kept: Vec<(PaneId, Fold)> = self.docs[i]
            .folds
            .iter()
            .flat_map(|(p, fs)| fs.closed().iter().filter(|f| f.start == line).map(move |f| (*p, *f)))
            .collect();
        edit(self);
        if kept.is_empty() || self.current() != i {
            return;
        }
        self.refresh_blocks();
        self.sync_folds();
        let tw = self.config.editor.tab_width;
        let doc = &mut self.docs[i];
        for (pane, f) in kept {
            if let Some(g) = fold::remeasure(&doc.buffer, &doc.blocks.kinds, tw, f.kind, f.start) {
                doc.folds.entry(pane).or_default().close(g, &doc.buffer);
            }
        }
    }

    /// Bring every pane's folds in the current document in step with the
    /// text after an edit (`Folds::sync`). An edit in one pane moves the
    /// folds of another pane on the same file too.
    pub(super) fn sync_folds(&mut self) {
        let i = self.current();
        if self.docs[i].folds.values().all(Folds::is_empty) {
            return;
        }
        self.refresh_blocks();
        let tw = self.config.editor.tab_width;
        let doc = &mut self.docs[i];
        for folds in doc.folds.values_mut() {
            folds.sync(&doc.buffer, &doc.blocks.kinds, doc.blocks.revision, tw);
        }
    }

    /// The cursor never rests inside a closed fold: whatever took it there
    /// (a search, `G`, `:42`, an undo) opens the folds around it. In Insert
    /// the fold it is typing on opens too.
    pub(super) fn keep_cursor_out_of_folds(&mut self) {
        self.sync_folds();
        if self.folds().is_empty() {
            return;
        }
        let line = self.buffer.cursor.line;
        let insert = self.mode == Mode::Insert;
        let (folds, _) = self.folds_mut();
        folds.reveal(line);
        if insert {
            folds.open_at(line);
        }
    }

    /// A new pane made from the focused one starts with its folds, in every
    /// document, as vim's `:split` copies the window's.
    pub(super) fn copy_folds(&mut self, from: PaneId, to: PaneId) {
        for doc in &mut self.docs {
            if let Some(f) = doc.folds.get(&from).cloned() {
                doc.folds.insert(to, f);
            }
        }
    }

    /// Forget the folds of panes that are gone.
    pub(super) fn drop_folds_of_closed_panes(&mut self) {
        let ids = self.layout.ids();
        for doc in &mut self.docs {
            doc.folds.retain(|p, _| ids.contains(p));
        }
    }
}
