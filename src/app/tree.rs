use super::*;

impl App {
    // ------------------------------------------------------------- file tree

    pub(super) fn cwd(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    /// A path in the form a buffer keeps it: absolute and lexically clean
    /// (`clean_path`), so it can be matched against the tree's and the
    /// finder's paths, which always are.
    pub(super) fn absolute(&self, p: &Path) -> PathBuf {
        clean_path(&self.cwd(), p)
    }

    /// A path the reader TYPED (`:e`, `:w`, `:saveas`, `:export`): `~/`
    /// expanded, then made absolute and clean like every buffer path.
    pub(super) fn typed_path(&self, typed: &str) -> PathBuf {
        self.absolute(&expand_home(typed))
    }

    /// A path as a message names it: relative to the working directory when
    /// it is beneath it, whole otherwise.
    pub(super) fn shown(&self, p: &Path) -> String {
        p.strip_prefix(self.cwd()).unwrap_or(p).display().to_string()
    }

    /// The finder's near view: the edited file's own directory, or the working
    /// directory while the buffer is still unnamed.
    pub(super) fn file_dir(&self) -> PathBuf {
        // `Path::parent` of a bare filename is the EMPTY path, not the cwd, and
        // an empty directory reads as no directory at all — hence `absolute`
        // first, and the filter after it.
        self.buffer
            .path
            .as_ref()
            .map(|p| self.absolute(p))
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| self.cwd())
    }

    /// `$HOME`: the tree's permanent root, and the finder's wide net. Falls
    /// back to the working directory in an environment that has no home.
    pub(super) fn home(&self) -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| self.cwd())
    }

    /// `Root::File` is what the plain `file_tree` / `find_file` ask for, so
    /// it is the one `tree.root` can redirect — to the launch folder, for a
    /// vault whose notes live in subfolders. An unknown word keeps the file's
    /// folder (and is named by the config check).
    pub(super) fn root_dir(&self, root: Root) -> PathBuf {
        let root = match root {
            Root::File => Root::parse(&self.config.tree.root).unwrap_or(Root::File),
            other => other,
        };
        match root {
            Root::File => self.file_dir(),
            Root::Home => self.home(),
            Root::Cwd => self.cwd(),
        }
    }

    /// `<leader>fe`/`fE`: closed → open+focus; open but unfocused → focus;
    /// focused → close.
    ///
    /// `fe` roots at the edited file's own folder — the near view, and what a
    /// writer means by "here" — while `fE` roots at `$HOME`. Neither is a
    /// project root: Shoin edits prose, and a folder of notes is not a
    /// checkout. `-` and `+` move the root from wherever it landed.
    ///
    /// The edited file is selected when the listing contains it, so the tree
    /// opens pointing at where you already are rather than at its first row.
    pub(super) fn toggle_tree(&mut self, root: PathBuf) {
        // The already-open cases first, so the `FileTree` below is built with
        // `self` unborrowed and can read the buffer's path.
        match &mut self.tree {
            Some(t) if !t.focused => {
                t.focused = true;
                return;
            }
            Some(_) => {
                self.tree = None;
                return;
            }
            None => {}
        }
        let mut tree = FileTree::open(root);
        tree.set_hidden(self.config.tree.show_hidden);
        if let Some(path) = self.buffer.path.as_ref().map(|p| self.absolute(p)) {
            tree.select_path(&path);
        }
        self.tree = Some(tree);
    }

    /// `Ctrl-w` window commands, vim's own vocabulary: `v`/`s` split, `q`/`c`
    /// close, `o` keeps only this pane, `h`/`j`/`k`/`l` move by direction and
    /// `w` cycles. The file-tree sidebar joins in at the left edge: moving left
    /// from the leftmost pane focuses it, and any move out of it comes back.
    pub(super) fn window_command(&mut self, key: char) {
        // The count belongs to the command, and `run` has already spent it on
        // nothing else, so a `<C-w>` verb is free to use it.
        let tree_focused = self.tree.as_ref().is_some_and(|t| t.focused);
        if tree_focused {
            if matches!(key, 'l' | 'w' | 'j' | 'k') {
                if let Some(t) = self.tree.as_mut() {
                    t.focused = false;
                }
            }
            return;
        }
        match key {
            'v' => self.split_pane(true),
            's' => self.split_pane(false),
            'q' | 'c' => {
                if !self.close_pane() {
                    self.notify("last pane — :q to quit", FlashKind::Info);
                }
            }
            'o' => self.only_pane(),
            'w' => self.cycle_pane(),
            // Sizing. Wider/narrower move by columns, taller/shorter by rows —
            // a text measure is worth moving in more than one column at a time,
            // and a count multiplies the step (`5<C-w>>`).
            '>' | '<' | '+' | '-' => {
                let vertical = matches!(key, '>' | '<');
                let step = if vertical { WIDTH_STEP } else { HEIGHT_STEP };
                let sign = if matches!(key, '>' | '+') { 1 } else { -1 };
                let delta = sign * step * self.window_count.max(1) as i32;
                let area = self.pane_area();
                if !self.layout.resize(area, self.focus_pane, vertical, delta) {
                    self.notify("no room that way", FlashKind::Info);
                }
            }
            '=' => {
                self.layout.equalize();
                self.touch_status();
            }
            'h' | 'j' | 'k' | 'l' => {
                let dir = Dir::from_key(key).unwrap();
                let area = self.pane_area();
                match self.layout.neighbor(area, self.focus_pane, dir) {
                    Some(id) => self.focus_pane_id(id),
                    // Nothing that way: the tree is off the left edge.
                    None if dir == Dir::Left => {
                        if let Some(t) = self.tree.as_mut() {
                            t.focused = true;
                        }
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_key_tree(&mut self, key: KeyEvent) {
        // The `Ctrl-w` window prefix works from the tree too (e.g. `Ctrl-w l`
        // back to the editor). The tree runs its own key handling, so it keeps
        // its own one-key prefix rather than reaching into the grammar.
        if std::mem::take(&mut self.tree_window_prefix) {
            // Arrows fold to the vim letters, exactly as `bindings::after_prefix`
            // does for the editor — `<C-w><Right>` must work from here too.
            let c = match key.code {
                KeyCode::Left => Some('h'),
                KeyCode::Down => Some('j'),
                KeyCode::Up => Some('k'),
                KeyCode::Right => Some('l'),
                KeyCode::Char(c) => Some(c),
                _ => None,
            };
            if let Some(c) = c {
                self.window_command(c);
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('w') {
            self.tree_window_prefix = true;
            return;
        }
        // Leader sequences reach the keymap from the tree too, so the leader
        // keys mean the same everywhere: `<leader>hl` opens help (at the
        // tree's keys), `<leader>fe` closes the tree, `<leader>ff` finds. Only
        // a key that starts or continues one is passed on; the tree's own
        // single keys stay the tree's.
        if let Some(k) = Key::from_event(key) {
            let starts = matches!(self.keymap.lookup("normal", &[k]), Lookup::Prefix);
            if starts || !self.pending.partial.is_empty() {
                self.feed_pending(key, Table::Normal);
                return;
            }
        }

        let mut close = false;
        let mut to_open = None;
        let mut prompt = None;
        let mut flash = None;
        let mut remember_hidden = None;
        if let Some(tree) = self.tree.as_mut() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => close = true,
                KeyCode::Char('j') | KeyCode::Down => tree.down(),
                KeyCode::Char('k') | KeyCode::Up => tree.up(),
                KeyCode::Char('h') | KeyCode::Left => tree.collapse_or_parent(),
                KeyCode::Char('g') | KeyCode::Home => tree.select_first(),
                KeyCode::Char('G') | KeyCode::End => tree.select_last(),
                KeyCode::Char('R') => tree.refresh(),
                // A directory with no dotfiles does not change on screen, so
                // the toggle says which way it went rather than looking dead.
                KeyCode::Char('H') => {
                    let on = tree.toggle_hidden();
                    remember_hidden = Some(on);
                    flash = Some(if on { "hidden files shown" } else { "hidden files hidden" });
                }
                // The root itself moves: `-` climbs out, `=` descends into the
                // selected directory. Without them the root is a wall, since `h`
                // stops dead at depth 0. Unshifted neighbours on the keyboard,
                // because they are pressed in pairs while reading around.
                KeyCode::Char('-') => {
                    tree.root_up();
                }
                KeyCode::Char('=') => {
                    tree.root_into();
                }
                KeyCode::Char('l') | KeyCode::Char('o') | KeyCode::Enter | KeyCode::Right => {
                    if let Some(Activate::Open(path)) = tree.activate() {
                        to_open = Some(path);
                    }
                }
                // File operations. Each opens a prompt rather than acting at
                // once — even `d`, which asks before it removes anything.
                KeyCode::Char('a') => prompt = Some(PromptKind::Create),
                KeyCode::Char('r') => prompt = Some(PromptKind::Rename),
                KeyCode::Char('m') => prompt = Some(PromptKind::Move),
                KeyCode::Char('d') => prompt = Some(PromptKind::Delete { entries: 0 }),
                _ => {}
            }
        }
        // `H` writes through to the live config, the way `:set conceal` and the
        // typewriter toggle do — which is what makes it outlast the panel.
        if let Some(on) = remember_hidden {
            self.config.tree.show_hidden = on;
        }
        if let Some(msg) = flash {
            self.notify(msg, FlashKind::Info);
        }
        if let Some(kind) = prompt {
            return self.tree_prompt(kind);
        }
        if close {
            self.tree = None;
        }
        if let Some(path) = to_open {
            if self.open_file(path) {
                if self.config.tree.close_on_open {
                    self.tree = None;
                } else if let Some(t) = self.tree.as_mut() {
                    t.focused = false; // hand focus back to the editor
                }
            }
        }
    }

    /// Replace the current buffer with `path`. Refuses if there are unsaved
    /// changes. Returns whether it opened.
    /// A left-click inside the tree pane: focus it and select the clicked row.
    pub(super) fn tree_click(&mut self, row: u16) {
        // The tree's height as drawn: the window less the bottom row.
        let h = self.term_size.1.saturating_sub(crate::render::frame::bottom_rows(self)) as usize;
        if let Some(tree) = self.tree.as_mut() {
            tree.focused = true;
            let len = tree.entries.len();
            let start = if len <= h {
                0
            } else {
                tree.selected.saturating_sub(h / 2).min(len - h)
            };
            let idx = start + row as usize;
            if idx < len {
                tree.selected = idx;
            }
        }
    }

    /// Open `path` in a NEW document and switch to it — or just switch, if it
    /// is already open. Nothing is displaced, so unlike before there is no
    /// reason to refuse when the current buffer has unsaved changes.
    pub(super) fn open_file(&mut self, path: PathBuf) -> bool {
        // One file, one buffer, however it was spelled: the clean path catches
        // `n.md` against `./n.md`, `same_file` a symlink against its target.
        let path = self.absolute(&path);
        let real = std::fs::canonicalize(&path).ok();
        let open = self.docs.iter().position(|d| {
            d.buffer.path.as_deref().is_some_and(|p| {
                p == path || real.is_some() && std::fs::canonicalize(p).ok() == real
            })
        });
        if let Some(i) = open {
            self.switch_to(i);
            return true;
        }
        match Buffer::open(path.clone(), &self.config.markdown.plain_text_extensions) {
            Ok(mut buffer) => {
                buffer.read_only = self.view;
                self.docs.push(BufferState::new(buffer, self.config.markdown.math));
                self.switch_to(self.docs.len() - 1);
                self.notify(format!("opened {}", self.shown(&path)), FlashKind::Info);
                true
            }
            Err(e) => {
                self.notify(format!("{e}"), FlashKind::Error);
                false
            }
        }
    }
}
