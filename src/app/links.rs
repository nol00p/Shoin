use super::*;

impl App {
    // ---------------------------------------------------------- links

    /// Where the link under the cursor points, if the cursor is on one.
    fn link_dest(&self) -> Option<Dest> {
        let line = self.buffer.line_text(self.buffer.cursor.line);
        let span = inline::span_at(&line, self.buffer.cursor.col, &self.config.markdown)?;
        let target = inline::target_of(&span, &line)?;
        Some(match span.kind {
            // A wiki target is a NAME, even when it looks like a path: §14.2
            // resolution tries it relative to this file first anyway.
            Inline::WikiLink => Dest::Note(link::Link::parse(&target)?),
            Inline::Autolink => Dest::Url(target),
            _ if is_url(&target) => Dest::Url(target),
            // `Link::parse` rather than a hand-rolled split, so `note.md#Head`
            // in a markdown link means the same thing it means in `[[…]]`.
            _ => {
                let l = link::Link::parse(&target)?;
                Dest::Path {
                    path: self.file_dir().join(&l.target),
                    section: l.section,
                }
            }
        })
    }

    /// The vault root bare-name resolution searches from, for the edited file.
    pub(super) fn link_root(&self) -> PathBuf {
        compile::search_root(&self.link_from(), &self.config.transclude)
    }

    /// The file links are resolved RELATIVE to. An unnamed buffer has no path
    /// of its own, so it borrows the working directory's — the same fallback
    /// `file_dir` makes, and for the same reason.
    pub(super) fn link_from(&self) -> PathBuf {
        match self.buffer.path.as_ref() {
            Some(p) => self.absolute(p),
            None => self.cwd().join("untitled.md"),
        }
    }

    /// `gf` — open what the cursor is on.
    pub(super) fn follow_link(&mut self) {
        match self.link_dest() {
            None => self.notify("no link under the cursor", FlashKind::Error),
            // Deliberately not "silently do the other thing". `gx` is one key
            // away, and naming it here is how it gets learned.
            Some(Dest::Url(u)) => {
                self.notify(format!("{u} is a URL — gx opens it"), FlashKind::Info)
            }
            Some(Dest::Path { path, section }) => self.open_or_create(path, &section, false),
            Some(Dest::Note(l)) => {
                let (root, from) = (self.link_root(), self.link_from());
                match link::resolve(&l, &from, &root) {
                    Ok(path) => self.open_or_create(path, &l.section, true),
                    // A name nothing answers to is a note not written YET.
                    // Resolution is unchanged; what is new is that its miss is
                    // a starting point rather than an error.
                    Err(link::Unresolved::Missing(_)) => {
                        let path = self.file_dir().join(&l.target);
                        self.open_or_create(path, &l.section, true)
                    }
                    // The refusal becomes a choice: the finder already knows
                    // how to pick one path out of several.
                    Err(link::Unresolved::Ambiguous(_, paths)) => {
                        let n = paths.len();
                        self.finder = Some(Finder::from_paths(paths, &root));
                        self.notify(
                            format!("{n} notes are called {:?} — pick one", l.target),
                            FlashKind::Info,
                        );
                    }
                }
            }
        }
    }

    /// Open `path`, writing an empty file first if it is a note that does not
    /// exist yet.
    ///
    /// The creation is the point of following a link in a vault: `[[tomorrow]]`
    /// is written before `tomorrow.md` is, and the link is how the note gets
    /// started. It is created ON DISK rather than as an unnamed buffer so that
    /// the link that made it resolves from now on — including from every other
    /// note already pointing here.
    ///
    /// `name` is true for a wiki link's target, a note's name rather than a
    /// path (see `named_ext`).
    fn open_or_create(&mut self, path: PathBuf, section: &link::Section, name: bool) {
        let path = if path.exists() { path } else { with_default_ext(path, name) };
        if !path.exists() {
            // `exists` follows links: a link to a missing note lands here too,
            // and there is nothing to create, only a link to fix.
            match ops::taken(&path) {
                Ok(false) => {}
                Ok(true) => {
                    self.notify(format!("{} is a broken link", self.shown(&path)), FlashKind::Error);
                    return;
                }
                Err(e) => {
                    self.notify(format!("{e:#}"), FlashKind::Error);
                    return;
                }
            }
            if !creatable(&path, name) {
                self.notify(format!("no {}", self.shown(&path)), FlashKind::Error);
                return;
            }
            if let Err(e) = ops::create(&path, false) {
                self.notify(format!("{e}"), FlashKind::Error);
                return;
            }
            self.disk_changed();
            if self.open_file(path.clone()) {
                self.notify(format!("created {}", self.shown(&path)), FlashKind::Info);
            }
            return;
        }
        if self.open_file(path) {
            self.jump_to_section(section);
        }
    }

    /// Put the cursor on the heading or block a `#section` asked for.
    ///
    /// A section that is not there is reported but not refused — the note is
    /// already open, and landing at its top beats not moving at all.
    fn jump_to_section(&mut self, section: &link::Section) {
        if matches!(section, link::Section::All) {
            return;
        }
        let text = self.buffer.rope.to_string();
        match link::section_line(&text, section) {
            Some(line) => {
                self.buffer.cursor = Cursor::new(line, 0);
                self.sync_after_input();
            }
            None => self.notify(
                format!("no {:?} in this note", section_label(section)),
                FlashKind::Error,
            ),
        }
    }

    /// `gx` — hand the link under the cursor to the desktop.
    ///
    /// Works on a note or a picture too, not just a URL: `gx` on an
    /// `![[photo.png]]` is how you see it at full size, and on a `[…](x.pdf)`
    /// how you read it. It never CREATES — an external opener has nothing to
    /// do with a note that does not exist.
    pub(super) fn open_external(&mut self) {
        let arg = match self.link_dest() {
            None => {
                self.notify("no link under the cursor", FlashKind::Error);
                return;
            }
            Some(Dest::Url(u)) => u,
            Some(Dest::Path { path, .. }) if path.exists() => path.display().to_string(),
            Some(Dest::Note(l)) => {
                let (root, from) = (self.link_root(), self.link_from());
                match link::resolve(&l, &from, &root) {
                    Ok(p) => p.display().to_string(),
                    Err(e) => {
                        self.notify(format!("{e}"), FlashKind::Error);
                        return;
                    }
                }
            }
            Some(Dest::Path { path, .. }) => {
                self.notify(format!("no {}", self.shown(&path)), FlashKind::Error);
                return;
            }
        };
        self.spawn_opener(&arg);
    }

    /// Run the desktop's opener on one argument.
    ///
    /// Spawned directly, never through a shell, so nothing written in a note
    /// can be read as a command. The leading-dash refusal closes the one hole
    /// an argument vector does not: `open -a` is a FLAG, whatever it was meant
    /// to be, and a document should not be able to reach one.
    pub(super) fn spawn_opener(&mut self, arg: &str) {
        if arg.starts_with('-') {
            self.notify("refusing a target that starts with '-'", FlashKind::Error);
            return;
        }
        // No test may ever open a browser or a file manager.
        if cfg!(test) {
            self.notify(format!("opened {arg}"), FlashKind::Info);
            return;
        }
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "explorer"
        } else {
            "xdg-open"
        };
        let spawned = std::process::Command::new(cmd)
            .arg(arg)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            // Reaped on a thread it outlives: these openers hand off and exit
            // at once, and a session full of un-waited children would collect
            // one zombie per `gx`.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.notify(format!("opened {arg}"), FlashKind::Info);
            }
            Err(e) => self.notify(format!("{cmd}: {e}"), FlashKind::Error),
        }
    }
}
