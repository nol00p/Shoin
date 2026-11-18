use super::*;

impl App {
    // ---------------------------------------------------------------- command

    pub(super) fn on_key_command(&mut self, key: KeyEvent, mut buf: String) {
        match field_key(key, &mut buf) {
            FieldKey::Cancel => {
                self.mode = Mode::Normal;
                self.flash = None;
            }
            FieldKey::Submit => {
                self.mode = Mode::Normal;
                self.run_command(&buf);
            }
            FieldKey::Stay => self.mode = Mode::Command(buf),
        }
    }

    // ------------------------------------------------------------ tree prompts

    /// A panel's one-line question. Swallows every key, exactly as `:` does:
    /// an open prompt is a text field, so `a` would otherwise re-open it.
    pub(super) fn on_key_prompt(&mut self, key: KeyEvent, mut p: Prompt) {
        if p.kind.is_confirm() {
            // A plain `y` only: `Ctrl-y` is not a yes.
            let chord = key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') if !chord => {
                    self.mode = Mode::Normal;
                    self.finish_prompt(p);
                }
                // Anything that is not a yes is a no. A destructive answer must
                // be typed on purpose, never fallen into.
                _ => {
                    self.mode = Mode::Normal;
                    self.flash = None;
                }
            }
            return;
        }
        // `Ctrl-U` matters most here: `r` and `m` open PRE-FILLED, so
        // replacing the name outright needs to be one key.
        match field_key(key, &mut p.input) {
            FieldKey::Cancel => {
                self.mode = Mode::Normal;
                self.flash = None;
            }
            FieldKey::Submit => {
                self.mode = Mode::Normal;
                self.finish_prompt(p);
            }
            FieldKey::Stay => self.mode = Mode::Prompt(p),
        }
    }

    /// Carry out an answered prompt, report what happened, and put the tree's
    /// cursor where the user will look for it.
    fn finish_prompt(&mut self, p: Prompt) {
        // Export is not a tree operation and must work with the tree closed,
        // so it is answered before the tree is required.
        if let PromptKind::Export { format, justify } = p.kind {
            return self.finish_export(format, justify, &p.target, p.input.trim());
        }
        let Some(tree) = self.tree.as_ref() else { return };
        let root = tree.root.clone();
        let was = tree.selected;

        let outcome = match &p.kind {
            PromptKind::Create => {
                let raw = p.input.trim();
                if raw.is_empty() {
                    return;
                }
                // Neo-tree's convention: a trailing `/` means a directory. It
                // reads as one, which is the whole reason to use it.
                let is_dir = raw.ends_with('/');
                let path = p.target.join(expand_home(raw.trim_end_matches('/')));
                ops::create(&path, is_dir).map(|()| Some(path))
            }
            PromptKind::Rename => {
                let raw = p.input.trim();
                if raw.is_empty() {
                    return;
                }
                // A rename stays put; only the name changes. Anything with a
                // separator in it is a move, and `m` is where that lives.
                if raw.contains('/') {
                    self.notify("a rename is a name, not a path — use m to move", FlashKind::Error);
                    return;
                }
                let dest = p.target.with_file_name(raw);
                self.move_path(&p.target, &dest)
            }
            PromptKind::Move => {
                let raw = p.input.trim();
                if raw.is_empty() {
                    return;
                }
                let typed = expand_home(raw);
                let mut dest = if typed.is_absolute() {
                    typed
                } else {
                    self.tree.as_ref().map(|t| t.resolve(raw)).unwrap_or_else(|| root.join(raw))
                };
                // Moving ONTO a directory means "into it", the way `mv` does.
                if dest.is_dir() {
                    if let Some(name) = p.target.file_name() {
                        dest = dest.join(name);
                    }
                }
                self.move_path(&p.target, &dest)
            }
            PromptKind::Delete { .. } => ops::remove(&p.target).map(|()| None),
            // Answered at the top of this function, with no tree required.
            PromptKind::Export { .. } => return,
        };

        match outcome {
            Ok(landed) => {
                self.disk_changed();
                let verb = match p.kind {
                    PromptKind::Create => "created",
                    PromptKind::Rename => "renamed",
                    PromptKind::Move => "moved",
                    PromptKind::Delete { .. } => "deleted",
                    // Answered above, before the tree was required.
                    PromptKind::Export { .. } => unreachable!(),
                };
                let name = landed.as_ref().unwrap_or(&p.target);
                let shown = self
                    .tree
                    .as_ref()
                    .map(|t| t.relative(name))
                    .unwrap_or_else(|| name.display().to_string());
                if let Some(tree) = self.tree.as_mut() {
                    match &landed {
                        Some(path) => tree.reveal(path),
                        None => {
                            tree.refresh();
                            tree.select_nearest(was);
                        }
                    }
                }
                self.notify(format!("{shown} {verb}"), FlashKind::Info);
            }
            Err(e) => self.notify(format!("{e}"), FlashKind::Error),
        }
    }

    /// `:export [md|txt|html|pdf] [justify]` — open the save dialog for the
    /// finished doc. `justify` stretches every wrapped line but a
    /// paragraph's last to fill `layout.measure` — only `html` and `txt` have
    /// a layout of their own to stretch, so it is refused on `md` (raw
    /// markup, no visual layout) and `pdf` (pandoc's own template decides
    /// that).
    ///
    /// The document is flattened from what is ON DISK, so an unsaved buffer is
    /// refused rather than exported from a stale file. Silently exporting
    /// yesterday's version is the one outcome nobody could detect.
    pub(super) fn open_export(&mut self, arg: &str) {
        let mut format_tok = None;
        let mut justify = false;
        for tok in arg.split_whitespace() {
            match tok.to_ascii_lowercase().as_str() {
                "justify" | "justified" => justify = true,
                _ if format_tok.is_none() => format_tok = Some(tok),
                _ => return self.notify(format!("export: unexpected {tok:?}"), FlashKind::Error),
            }
        }
        let format = match format_tok {
            None => Format::Markdown,
            Some(f) => match Format::parse(f) {
                Some(f) => f,
                None => {
                    return self
                        .notify(format!("export: {f:?} is not md, txt, html or pdf"), FlashKind::Error)
                }
            },
        };
        if justify && !matches!(format, Format::Html | Format::Text) {
            return self.notify("export: justify only applies to html or txt", FlashKind::Error);
        }
        let Some(src) = self.buffer.path.clone() else {
            return self.notify("export: save this file first", FlashKind::Error);
        };
        if self.buffer.modified {
            return self.notify("export: unsaved changes — :w first", FlashKind::Error);
        }
        // Suggested, not decided: the reader edits this before pressing Enter.
        // Shown relative to the working directory, because an absolute path in
        // a 60-column box is mostly directories they already know they are in.
        let suggestion = crate::export::default_path(&src, format);
        let shown = self.shown(&suggestion);
        self.mode = Mode::Prompt(Prompt {
            kind: PromptKind::Export { format, justify },
            target: src,
            input: shown,
        });
    }

    /// Carry out an answered export prompt.
    fn finish_export(&mut self, format: Format, justify: bool, source: &Path, dest: &str) {
        if dest.is_empty() {
            return;
        }
        let dest = self.typed_path(dest);
        // HTML is drawn from the theme AS AUTHORED, not from the running one:
        // `self.theme` has been adapted to this terminal's color depth, and a
        // page has no such limit to be adapted to.
        let page = crate::export::html::Page {
            theme: crate::render::theme::Theme::authored(&self.config.theme)
                .unwrap_or_else(|_| self.theme.clone()),
            measure: self.config.layout.measure,
            base: source
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .to_path_buf(),
            justify,
            md: self.config.markdown.for_export(),
        };
        match crate::export::write(source, &dest, format, &self.config.transclude, &page) {
            Err(e) => self.notify(format!("export: {e}"), FlashKind::Error),
            Ok(()) => {
                let name = dest
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dest.display().to_string());
                self.notify(format!("{name} exported"), FlashKind::Info);
            }
        }
    }

    /// Rename or move a path, carrying any OPEN buffer along with it.
    ///
    /// Without the second half, renaming a file you are editing would leave the
    /// buffer pointing at a name that no longer exists, and the next `:w` would
    /// silently recreate the old one.
    pub(super) fn move_path(&mut self, from: &Path, to: &Path) -> anyhow::Result<Option<PathBuf>> {
        let (from, to) = (self.absolute(from), self.absolute(to));
        // Which buffers go along is decided BEFORE the rename, while `from`
        // still resolves: a buffer spelled through a symlinked folder (a
        // `/home` that is really `/var/home`) is matched by where it really
        // is. Not when `from` IS a symlink: moving a link leaves the file it
        // points at, and that file's buffer, where they are. A moved
        // DIRECTORY takes every buffer beneath it, not just an exact match.
        let is_link = std::fs::symlink_metadata(&from).is_ok_and(|m| m.file_type().is_symlink());
        let real_from = if is_link { None } else { std::fs::canonicalize(&from).ok() };
        let carried: Vec<(usize, PathBuf)> = self
            .docs
            .iter()
            .enumerate()
            .filter_map(|(i, doc)| {
                let path = doc.buffer.path.as_deref()?;
                let rest = match path.strip_prefix(&from) {
                    Ok(rest) => rest.to_path_buf(),
                    Err(_) => {
                        let real = std::fs::canonicalize(path).ok()?;
                        real.strip_prefix(real_from.as_deref()?).ok()?.to_path_buf()
                    }
                };
                Some((i, rest))
            })
            .collect();
        ops::rename(&from, &to)?;
        for (i, rest) in carried {
            self.docs[i].buffer.path = Some(if rest.as_os_str().is_empty() {
                to.clone()
            } else {
                to.join(rest)
            });
        }
        Ok(Some(to))
    }

    /// Open a tree prompt on the selected row, if the operation makes sense for
    /// it. The root row is deliberately untouchable: every other path in the
    /// pane is expressed relative to it.
    pub(super) fn tree_prompt(&mut self, kind: PromptKind) {
        let Some(tree) = self.tree.as_ref() else { return };
        let Some(entry) = tree.selected_entry() else { return };

        let (target, input) = match kind {
            PromptKind::Create => (tree.target_dir(), String::new()),
            PromptKind::Rename => (entry.path.clone(), entry.name.clone()),
            PromptKind::Move => (entry.path.clone(), tree.relative(&entry.path)),
            PromptKind::Delete { .. } => (entry.path.clone(), String::new()),
            // `:export` opens its own prompt; the tree never raises one.
            PromptKind::Export { .. } => return,
        };
        if !matches!(kind, PromptKind::Create) && tree.selected_is_root() {
            self.notify("the tree root stays put", FlashKind::Error);
            return;
        }
        // Counted here rather than in the label, so a huge directory is walked
        // once and only when a deletion is actually being asked about.
        let kind = match kind {
            PromptKind::Delete { .. } => PromptKind::Delete {
                entries: ops::count_entries(&target, DELETE_COUNT_CAP),
            },
            other => other,
        };
        self.mode = Mode::Prompt(Prompt { kind, target, input });
    }
}
