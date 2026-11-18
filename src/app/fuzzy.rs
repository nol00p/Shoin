use super::*;

impl App {
    // ---------------------------------------------------------- fuzzy finder

    /// `<leader>ff`/`fF`: open the finder. Each open rewalks, so the list is
    /// never stale against files created since.
    ///
    /// Not a toggle, unlike the tree: an open finder is a text field that
    /// swallows every key, so its own binding would arrive as query text. Esc
    /// closes it — the same deal as the `:` box.
    pub(super) fn open_finder(&mut self, root: PathBuf) {
        self.finder = Some(Finder::open(root));
    }

    /// `<C-^>` — back to the document this pane was showing before.
    ///
    /// Vim's alternate file, and the way back out of a link: `gf` opens a note
    /// in a new document, and this returns to the one that linked to it.
    /// Pressed twice it lands where it started, which is what makes it usable
    /// for reading two notes against each other.
    pub(super) fn alternate_buffer(&mut self) {
        // Never itself: closing the document you followed a link INTO leaves
        // the alternate pointing at the one you land on, and "go back to where
        // you are" is not somewhere to go.
        let alt = self
            .alternate
            .filter(|a| *a < self.docs.len() && *a != self.current());
        match alt {
            Some(a) => {
                self.switch_to(a);
                self.notify(self.doc_summary(), FlashKind::Info);
            }
            None => self.notify("no alternate buffer", FlashKind::Error),
        }
    }

    /// `<leader>fb`: the same overlay over the open buffers.
    pub(super) fn open_buffer_switcher(&mut self) {
        let entries = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let dirty = if d.buffer.modified { " ●" } else { "" };
                (i, format!("{}{dirty}", d.name()))
            })
            .collect();
        self.finder = Some(Finder::buffers(entries));
    }

    /// The finder's own tiny input mode: printable keys extend the query,
    /// Ctrl-n/p (and the arrows) move the selection, Enter opens, Esc closes.
    pub(super) fn on_key_finder(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mut close = false;
        let mut to_open = None;
        let mut to_switch = None;
        if let Some(finder) = self.finder.as_mut() {
            match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Char('c') | KeyCode::Char('[') if ctrl => close = true,
                KeyCode::Enter => {
                    match finder.kind {
                        finder::Kind::Files => to_open = finder.selected_path(),
                        finder::Kind::Buffers => to_switch = finder.selected_id(),
                    }
                    close = true;
                }
                KeyCode::Char('n') | KeyCode::Char('j') if ctrl => finder.down(),
                KeyCode::Char('p') | KeyCode::Char('k') if ctrl => finder.up(),
                KeyCode::Down | KeyCode::Tab => finder.down(),
                KeyCode::Up | KeyCode::BackTab => finder.up(),
                // Unlike the `:` line, an empty query stays open: the list
                // under it is still worth looking at.
                KeyCode::Backspace if finder.query.is_empty() => {}
                KeyCode::Char('h') if ctrl && finder.query.is_empty() => {}
                // The editing keys every field shares.
                _ => {
                    let mut outcome = FieldKey::Stay;
                    finder.edit_query(|q| outcome = field_key(key, q));
                    close = outcome == FieldKey::Cancel;
                }
            }
        }
        if close {
            self.finder = None;
        }
        if let Some(i) = to_switch {
            self.switch_to(i);
        }
        if let Some(path) = to_open {
            // Opening from the finder leaves the tree where it was, but focus
            // belongs with the file that just opened.
            if self.open_file(path) {
                if let Some(t) = self.tree.as_mut() {
                    t.focused = false;
                }
            }
        }
    }
}
