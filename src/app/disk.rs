use super::*;

impl App {
    /// Check the open documents' mtimes if the poll is due.
    pub(super) fn disk_tick(&mut self) -> bool {
        if Instant::now() < self.disk_check_at {
            return false;
        }
        self.disk_check_at = Instant::now() + DISK_POLL;
        self.check_disk()
    }

    /// Take back what changed on disk, for every document that can have it.
    ///
    /// The two cases are decided by ONE question — does this buffer hold
    /// unsaved work:
    ///
    /// - **Clean:** the file wins, and the buffer is reloaded. Nothing of the
    ///   reader's is lost by definition, and `Buffer::reload` makes it one undo
    ///   step, so even an unwanted reload is `u` away.
    /// - **Modified:** nothing is touched. The two have diverged and only the
    ///   reader can say which wins, so the buffer is flagged and the marker
    ///   goes on the status line. `:w!` keeps theirs, `:revert!` takes the
    ///   file's. This is the one thing an editor must never decide alone.
    ///
    /// A file that has been DELETED is not a change: `changed_externally`
    /// answers `false` when it cannot stat the path, so the buffer keeps its
    /// text and `:w` will recreate the file. Reloading emptiness over a
    /// document because something removed it would be the worst of the
    /// available behaviours.
    pub(super) fn check_disk(&mut self) -> bool {
        if !self.config.editor.autoreload {
            return false;
        }
        let exts = self.config.markdown.plain_text_extensions.clone();
        let now = std::time::SystemTime::now();
        let mut reloaded: Vec<String> = Vec::new();
        let mut conflicted: Vec<String> = Vec::new();
        let mut failed: Option<String> = None;

        // A document being compared is frozen: the view holds SNAPSHOTS, and
        // the comparison a reader is deciding about must not move under them
        // because a poll noticed a third write.
        let under_review = self.diff.as_ref().map(|d| d.doc);
        for (i, doc) in self.docs.iter_mut().enumerate() {
            if Some(i) == under_review {
                continue;
            }
            let Some(path) = doc.buffer.path.clone() else {
                continue;
            };
            if !crate::fs::save::changed_externally(&path, doc.buffer.disk_mtime) {
                continue;
            }
            // Let a half-written file finish. An `Err` here means the clock
            // disagrees with the filesystem (mtime in the future, which NFS
            // does) — act rather than wait forever on a comparison that will
            // never come true.
            //
            // Read BEFORE the load below, so a failed load is tied to the
            // version it failed on, not to one written a moment after.
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let settled = mtime
                .map(|m| now.duration_since(m).map(|age| age >= DISK_SETTLE).unwrap_or(true))
                .unwrap_or(true);
            if !settled {
                continue;
            }

            if doc.buffer.modified {
                // Announce a conflict ONCE. The status marker is what persists;
                // re-flashing every second would bury everything else.
                if !doc.buffer.conflict {
                    doc.buffer.conflict = true;
                    conflicted.push(doc.buffer.display_name());
                }
                continue;
            }

            match doc.buffer.reload(&exts) {
                Ok(true) => {
                    doc.buffer.reload_failed = None;
                    reloaded.push(doc.buffer.display_name());
                }
                // The mtime moved but the bytes did not. Absorbed in silence:
                // there is nothing to tell the reader about a `touch`.
                Ok(false) => doc.buffer.reload_failed = None,
                // A file that cannot be loaded (not UTF-8, unreadable) leaves
                // the text and the old mtime, so the next poll tries again and
                // a fixed file — rewritten, or `chmod`ed — still reloads. But
                // each broken version is announced ONCE: re-flashing it every
                // second would bury everything else.
                Err(e) => {
                    if doc.buffer.reload_failed != mtime {
                        doc.buffer.reload_failed = mtime;
                        failed.get_or_insert_with(|| e.to_string());
                    }
                }
            }
        }

        if !reloaded.is_empty() {
            self.disk_changed();
        }
        // A conflict outranks a reload in the report: one of them needs a
        // decision and the other is already finished.
        let conflict = match conflicted.len() {
            0 => None,
            1 => Some(format!("{} changed on disk — :revert! or :w!", conflicted[0])),
            n => Some(format!("{n} files changed on disk — see :ls")),
        };
        // Both are announced only once, so neither may hide the other.
        let failed = failed.map(|why| format!("reload: {why} — the text here is kept"));
        match (conflict, failed, reloaded.len()) {
            (Some(c), Some(f), _) => self.notify(format!("{c} · {f}"), FlashKind::Error),
            (Some(msg), None, _) | (None, Some(msg), _) => self.notify(msg, FlashKind::Error),
            (None, None, 0) => return false,
            (None, None, 1) => self.notify(format!("reloaded {}", reloaded[0]), FlashKind::Info),
            (None, None, n) => self.notify(format!("reloaded {n} files"), FlashKind::Info),
        }
        true
    }

    /// `:diff` — open the conflict view: this buffer beside the file.
    ///
    /// Useful without a conflict too (it reports when the two match), because a
    /// command that only works in the one state you are already alarmed about
    /// is a command nobody has practised.
    pub(super) fn open_diff(&mut self) {
        let Some(path) = self.buffer.path.clone() else {
            return self.notify("no file to compare against", FlashKind::Error);
        };
        if !path.exists() {
            let name = self.buffer.display_name();
            return self.notify(format!("{name} is not on disk yet"), FlashKind::Error);
        }
        let exts = self.config.markdown.plain_text_extensions.clone();
        let loaded = match crate::fs::open::load(&path, &exts) {
            Ok(l) => l,
            Err(e) => return self.notify(format!("{e}"), FlashKind::Error),
        };

        let name = self.buffer.display_name();
        let mine = crate::diff::lines_of(&self.buffer.rope.to_string());
        let theirs = crate::diff::lines_of(&loaded.rope.to_string());
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        let mut view =
            crate::diff::DiffView::new(self.current(), name.clone(), mine, theirs, mtime);
        if view.align.is_identical() {
            return self.notify(format!("{name} matches the file on disk"), FlashKind::Info);
        }
        // Open ON the first difference rather than at the top: the reader asked
        // to see what changed, and in a long note that is rarely line 1.
        if let Some(first) = view.align.hunks.first().map(|h| h.start) {
            view.reveal(first, crate::diff::DiffView::body_height(self.term_size.1));
        }
        // The view draws the flash inside it, so one left from before it
        // opened (a failed save, say) would read as being about the merge.
        self.flash = None;
        if view.align.coarse {
            self.notify("the two versions share little — showing one block", FlashKind::Info);
        }
        self.diff = Some(view);
        self.needs_redraw = true;
    }

    /// `w` in the diff view — write the merge the selections add up to.
    ///
    /// Commits the WHOLE document, not the difference under the cursor: every
    /// hunk always has a chosen side, so the view is a complete text at all
    /// times and this is the one place it becomes real. That is what makes `q`
    /// a true abort and the result a single undo step.
    ///
    /// It re-checks the file first. `check_disk` freezes a document while its
    /// diff is open, so nothing WE do can move it — but nothing stops a third
    /// party writing the file while the reader deliberates, and forcing past
    /// that would clobber an edit this view never showed them.
    fn commit_diff(&mut self) {
        let Some(view) = self.diff.as_ref() else { return };
        let doc = view.doc;
        let name = view.name.clone();
        let merged = view.merged();
        let snapshot = view.snapshot_mtime;

        let Some(path) = self.docs.get(doc).and_then(|d| d.buffer.path.clone()) else {
            self.diff = None;
            return self.notify("the file is gone — nothing to write", FlashKind::Error);
        };
        if crate::fs::save::changed_externally(&path, snapshot) {
            self.diff = None;
            return self.notify(
                format!("{name} changed again — :diff to see it"),
                FlashKind::Error,
            );
        }

        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        let doc = &mut self.docs[doc];
        // The merge has to land in the buffer before it is written. A buffer
        // that refuses it (read-only mode, a read-only range) must not be
        // written either: that would put the UNMERGED text over the file.
        //
        // Read-only mode keeps the view OPEN, every choice in it, and says how
        // to go on from there: `W` leaves read-only and writes those choices.
        // Closing would throw the reader's per-difference work away over a
        // mode one key can change. A read-only RANGE is not the reader's to
        // change, so that refusal closes the view as before: nothing in it
        // could ever be written.
        if !doc.buffer.replace_all(&merged) {
            self.needs_redraw = true;
            if doc.buffer.read_only {
                let msg = "read-only — nothing written; W leaves read-only and writes";
                return self.notify(msg, FlashKind::Error);
            }
            self.diff = None;
            let msg = format!("part of {name} refuses edits — nothing written");
            return self.notify(msg, FlashKind::Error);
        }
        self.diff = None;
        // Forced, and safe to force: the guard exists to catch a change nobody
        // has looked at, and the check above just proved there is none.
        match doc.buffer.save(policy, true) {
            Ok(()) => {
                self.claim_save_revisions();
                self.disk_changed();
                self.notify(format!("{name} merged and written"), FlashKind::Info)
            }
            Err(e) => self.notify(format!("{e}"), FlashKind::Error),
        }
        self.sync_after_input();
    }

    /// Keys inside the diff view. It has drawn over everything, so this is the
    /// whole grammar while it is up — deliberately small, and none of it edits.
    pub(super) fn on_key_diff(&mut self, key: KeyEvent) {
        let height = crate::diff::DiffView::body_height(self.term_size.1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(view) = self.diff.as_mut() else { return };

        let max = view.rows().saturating_sub(height);
        match key.code {
            // Single keys, not vim's `]c` / `[c`: this view has its own grammar
            // and nothing to be compatible with, so nothing needs holding.
            KeyCode::Char('n') | KeyCode::Char('p') => {
                let forward = matches!(key.code, KeyCode::Char('n'));
                if let Some(row) = view.step_hunk(forward) {
                    view.reveal(row, height);
                }
            }
            KeyCode::Char('j') | KeyCode::Down => view.scroll = (view.scroll + 1).min(max),
            KeyCode::Char('k') | KeyCode::Up => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => view.scroll = (view.scroll + height / 2).min(max),
            KeyCode::Char('u') if ctrl => view.scroll = view.scroll.saturating_sub(height / 2),
            KeyCode::Char('g') | KeyCode::Home => view.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => view.scroll = max,
            KeyCode::Char('q') | KeyCode::Esc => {
                self.diff = None;
            }
            // `<Tab>` rather than `<Space>`: space is the LEADER key everywhere
            // else in the editor, and a key that starts a command sequence in
            // every other context should not silently decide which version of
            // your work survives in this one. Tab is a two-way switch by nature
            // and has no Normal-mode meaning to unlearn.
            KeyCode::Tab | KeyCode::BackTab => {
                if let Some(side) = view.cycle_current() {
                    let n = view.hunk + 1;
                    let total = view.align.hunks.len();
                    self.notify(
                        format!("difference {n}/{total}: keep {}", side.label()),
                        FlashKind::Info,
                    );
                }
            }
            // The bulk answers. They SET every difference rather than resolving
            // outright: with per-difference choices in play, a key that both
            // selects and commits would silently discard whatever the reader
            // had already toggled.
            KeyCode::Char('l') => {
                view.set_all(crate::diff::Side::Live);
                self.notify("every difference: keep live · w to commit", FlashKind::Info);
            }
            KeyCode::Char('f') => {
                view.set_all(crate::diff::Side::File);
                self.notify("every difference: keep file · w to commit", FlashKind::Info);
            }
            KeyCode::Char('b') => {
                view.set_all(crate::diff::Side::Both);
                self.notify("every difference: keep both · w to commit", FlashKind::Info);
            }
            KeyCode::Char('w') => return self.commit_diff(),
            // The way on from a `w` refused in read-only mode, named in that
            // refusal: what `:view` would do, without leaving the view and
            // the choices in it. Elsewhere it is `w`.
            KeyCode::Char('W') => {
                if self.view {
                    self.set_view(false);
                }
                return self.commit_diff();
            }
            _ => {}
        }
        self.needs_redraw = true;
    }

    /// `:revert` — take what is on disk, discarding whatever is unsaved.
    ///
    /// The manual form of the same reload, and the "take theirs" answer to a
    /// conflict. It needs the `!` for the same reason `:q` does: on a modified
    /// buffer it throws work away, and that has to be typed deliberately.
    pub(super) fn revert(&mut self, force: bool) {
        if self.buffer.path.is_none() {
            return self.notify("no file to revert to", FlashKind::Error);
        }
        if self.buffer.modified && !force {
            return self.notify(
                "unsaved changes — :revert! to discard them",
                FlashKind::Error,
            );
        }
        let exts = self.config.markdown.plain_text_extensions.clone();
        // Bound, not deref: a deref borrows all of `App`.
        let i = self.current();
        let doc = &mut self.docs[i];
        let result = doc.buffer.reload(&exts);
        let name = doc.buffer.display_name();
        match result {
            Ok(true) => {
                self.disk_changed();
                self.notify(format!("reverted {name}"), FlashKind::Info)
            }
            Ok(false) => self.notify(format!("{name} already matches disk"), FlashKind::Info),
            Err(e) => self.notify(format!("{e}"), FlashKind::Error),
        }
    }

    /// Arm — or disarm — the autosave deadline from the config as it stands.
    ///
    /// Called at every point the answer can have changed: a write, a `:set`, a
    /// hot reload. `Autosave::interval` returning `None` for "off" is what lets
    /// this be one line rather than a branch.
    pub(super) fn arm_autosave(&mut self) {
        self.autosave_at = crate::fs::save::Autosave::from_config(&self.config.editor)
            .interval()
            .map(|every| Instant::now() + every);
    }

    /// Write if the deadline has passed. Reports whether anything needs drawing.
    ///
    /// The deadline is re-armed BEFORE the write and whatever the write does, so
    /// a file that cannot be saved — one something else has touched — is
    /// reported once every interval and not once every tick. A file made at
    /// the path after the buffer opened it missing is reported once in all
    /// (see `autosave_now`).
    pub(super) fn autosave_tick(&mut self) -> bool {
        match self.autosave_at {
            Some(due) if Instant::now() >= due => {}
            _ => return false,
        }
        self.arm_autosave();
        self.autosave_now()
    }

    /// Save every modified buffer that has a filename, and say so.
    ///
    /// Three deliberate limits, because an unattended write has none of the
    /// escape hatches `:w` does:
    ///
    /// - **Never `force`.** The external-modification guard is the whole reason
    ///   `:w` can refuse, and a background write is exactly when nobody is
    ///   watching for the refusal. A file someone else has written since we read
    ///   it is LEFT ALONE and named on the status line; `:w!` remains the only
    ///   way past that, because overruling it is a decision.
    /// - **A buffer with no path is skipped**, silently. `save` would fail with
    ///   "no filename", and a timer that flashes an error every three minutes at
    ///   a scratch buffer is worse than one that says nothing.
    /// - **Every document, not just the visible one.** A modified buffer behind
    ///   a split or in another pane is precisely what an autosave is for; `:w`
    ///   only ever means the one in front of you.
    ///
    /// Like `:w`, this seals the undo step (`Buffer::write_to` calls
    /// `history.split()`), so an undo after an autosave stops at the point it
    /// fired. That is the same boundary a manual save leaves and the same one
    /// `undo_coalesce_ms` draws anyway.
    fn autosave_now(&mut self) -> bool {
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        let mut written: Vec<String> = Vec::new();
        let mut refused: Option<String> = None;
        let (current, typing) = (self.current(), self.mode.allows_past_end());
        for (i, doc) in self.docs.iter_mut().enumerate() {
            // A file made at the path after this buffer opened it missing is
            // refused until the reader decides (`:w!`, `:revert!`). That is said
            // once per buffer, not every interval; once it is no longer so
            // (the file gone, saved over, reloaded) the next one is news.
            let created = doc.buffer.created_since_open();
            if !created {
                doc.buffer.created_reported = false;
            }
            if !doc.buffer.modified || doc.buffer.path.is_none() {
                continue;
            }
            let policy =
                crate::fs::save::SavePolicy { insert_mode: typing && i == current, ..policy };
            // A conflicted buffer would be refused by the guard every single
            // interval. The status marker already says the file diverged, and
            // saying it again on a timer buries whatever else the line had.
            // (Only reachable with `autoreload` on — nothing else sets this.)
            if doc.buffer.conflict || (created && doc.buffer.created_reported) {
                continue;
            }
            match doc.buffer.save(policy, false) {
                Ok(()) => written.push(doc.buffer.display_name()),
                // The FIRST refusal: one line of status, and the rest of the
                // documents still get their turn. A created-after refusal
                // counts as said only when it is the one on the line.
                Err(e) if refused.is_none() => {
                    refused = Some(e.to_string());
                    doc.buffer.created_reported = created;
                }
                Err(_) => {}
            }
        }
        if !written.is_empty() {
            self.claim_save_revisions();
            self.disk_changed();
            // A trim may have shortened the Visual anchor's line (the cursor
            // is clamped by the buffer; other panes clamp theirs on focus).
            if let Some(a) = self.anchor.as_mut() {
                a.line = a.line.min(self.docs[current].buffer.last_line());
                a.col = a.col.min(self.docs[current].buffer.line_len(a.line));
            }
        }
        match (refused, written.len()) {
            (Some(why), _) => self.notify(format!("autosave: {why}"), FlashKind::Error),
            (None, 0) => return false,
            (None, 1) => self.notify(format!("autosaved {}", written[0]), FlashKind::Info),
            (None, n) => self.notify(format!("autosaved {n} files"), FlashKind::Info),
        }
        true
    }

    /// A save that trimmed the text edited the buffer, but that edit is not
    /// the reader's command: move the "did this command edit" mark past it, so
    /// `.` and the table alignment on leaving Insert see only the reader's.
    pub(super) fn claim_save_revisions(&mut self) {
        let current = self.current();
        for (i, doc) in self.docs.iter_mut().enumerate() {
            let spent = std::mem::take(&mut doc.buffer.save_revisions);
            if i == current {
                self.dot_rev += spent;
            }
        }
    }
}
