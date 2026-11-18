use super::*;
use crate::input::action::FoldCmd;
use crate::render::hidden::{self, Hidden};
use crate::text::fold;

impl App {
    // ------------------------------------------------------------------ folds

    /// `za` `zo` `zc` `zR` `zM`.
    pub(super) fn fold(&mut self, cmd: FoldCmd) {
        // What can fold is read off the line kinds, which lag an edit until
        // the next frame: bring them up to date first.
        self.refresh_blocks();
        self.sync_folds();
        let line = self.buffer.cursor.line;
        match cmd {
            FoldCmd::Toggle => {
                if !self.folds.is_head(line) {
                    self.close_fold(line);
                } else {
                    self.folds.open_at(line);
                }
            }
            FoldCmd::Open => {
                if !self.folds.open_at(line) {
                    self.notify("no closed fold here", FlashKind::Info);
                }
            }
            FoldCmd::Close => self.close_fold(line),
            FoldCmd::OpenAll => self.folds.open_all(),
            FoldCmd::CloseAll => {
                let tw = self.config.editor.tab_width;
                let doc = &mut **self;
                let all = fold::all(&doc.buffer, &doc.blocks.kinds, tw);
                if all.is_empty() {
                    return self.notify("nothing to fold", FlashKind::Info);
                }
                doc.folds.close_all(all, &doc.buffer);
                // The cursor goes to the line shown for it, as vim's does.
                let shown = self.fold_hidden().shown(line);
                if shown != line {
                    self.buffer.cursor = Cursor::new(shown, 0);
                    let col = self.first_non_blank(shown);
                    self.buffer.cursor.set_col(col);
                }
            }
        }
    }

    /// Close the innermost fold around `line` that is still open, and put
    /// the cursor on its first line.
    fn close_fold(&mut self, line: usize) {
        let tw = self.config.editor.tab_width;
        let doc = &mut **self;
        let around = fold::candidates(&doc.buffer, &doc.blocks.kinds, tw, line);
        let Some(f) = around.into_iter().find(|f| !doc.folds.closed().contains(f)) else {
            return self.notify("no fold here", FlashKind::Info);
        };
        doc.folds.close(f, &doc.buffer);
        if f.start != line {
            let col = self.buffer.cursor.col;
            self.buffer.cursor = Cursor::new(f.start, col);
        }
    }

    /// The lines closed folds hide in the current document. Only folds: what
    /// `j`, `dd` and `p` treat as one line.
    pub(super) fn fold_hidden(&self) -> Hidden {
        hidden::of(&self.folds)
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
        if self.folds.is_empty() {
            return Some(count);
        }
        let from = self.buffer.cursor.line;
        self.shown_step(from, down, count).map(|to| to.abs_diff(from))
    }

    /// Bring the folds in step with the text after an edit (`Folds::sync`).
    pub(super) fn sync_folds(&mut self) {
        if self.folds.is_empty() {
            return;
        }
        self.refresh_blocks();
        let tw = self.config.editor.tab_width;
        let doc = &mut **self;
        doc.folds.sync(&doc.buffer, &doc.blocks.kinds, doc.blocks.revision, tw);
    }

    /// The cursor never rests inside a closed fold: whatever took it there
    /// (a search, `G`, `:42`, an undo) opens the folds around it. In Insert
    /// the fold it is typing on opens too.
    pub(super) fn keep_cursor_out_of_folds(&mut self) {
        if self.folds.is_empty() {
            return;
        }
        self.sync_folds();
        let line = self.buffer.cursor.line;
        let insert = self.mode == Mode::Insert;
        let folds = &mut self.folds;
        folds.reveal(line);
        if insert {
            folds.open_at(line);
        }
    }
}
