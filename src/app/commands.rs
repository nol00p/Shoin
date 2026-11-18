use super::*;

/// Every `:` command: its name, and the other (mostly shorter) names it
/// answers to. `run_command` reads names through this table and Tab completes
/// the first column, so a command missing here can be neither run nor
/// completed.
///
/// Vim's abbreviations are here so muscle memory lands: `:u`, `:un`, `:und`,
/// and `:red`. Not `:r` or `:re` — `:r` is `:read` in vim, and taking it for
/// undo here would train the wrong reflex against every other editor.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[
    ("align", &[]),
    ("bdelete", &["bd"]),
    ("bnext", &["bn"]),
    ("bprevious", &["bp", "bprev"]),
    ("buffer", &["b"]),
    ("buffers", &["ls"]),
    ("close", &["clo"]),
    ("diff", &["diffsplit"]),
    ("edit", &["e"]),
    ("embed", &[]),
    ("export", &[]),
    ("fixlist", &[]),
    ("focus", &[]),
    ("help", &["h"]),
    ("number", &["nu"]),
    ("only", &["on"]),
    ("q", &[]),
    ("qall", &["qa", "Q"]),
    ("redo", &["red"]),
    ("reload", &[]),
    ("revert", &[]),
    ("saveas", &["sav"]),
    ("set", &[]),
    ("split", &["sp"]),
    ("typewriter", &["tw"]),
    ("undo", &["u", "un", "und"]),
    ("view", &[]),
    ("vsplit", &["vs", "vsp"]),
    ("wq", &["x"]),
    ("write", &["w"]),
    ("zen", &[]),
];

/// The commands whose argument is a path, by their name in `COMMANDS`.
pub(crate) const PATH_COMMANDS: &[&str] = &["edit", "saveas", "wq", "write"];

/// Widest `:set tab_width` will accept. A tab wider than this is not an
/// indent anyone writes.
const MAX_TAB_WIDTH: usize = 16;

/// Every `:set` option: its name, and the other names it answers to.
/// `set_option` reads keys through this table and Tab completes the first
/// column.
pub(crate) const OPTIONS: &[(&str, &[&str])] = &[
    ("align", &[]),
    ("autoreload", &[]),
    ("autosave", &[]),
    ("autosave_interval", &["autosave_min"]),
    ("clipboard", &["cb"]),
    ("code_syntax", &["syntax"]),
    ("conceal", &[]),
    ("embed", &[]),
    ("focus", &[]),
    ("hints", &[]),
    ("line_spacing", &["spacing"]),
    ("measure", &["width"]),
    ("mouse", &[]),
    ("numbers", &["number", "nu"]),
    ("tab_width", &["ts"]),
    ("typewriter", &["tw"]),
];

/// The words that turn a toggle option on; any other value turns it off.
const ON_WORDS: &[&str] = &["on", "true", "1", "yes"];

/// What `:set` offers for a toggle option.
pub(crate) const TOGGLE_VALUES: &[&str] = &["off", "on"];

/// The values `:focus` and `:set focus` take, as `FocusMode::parse` reads
/// them and its labels name them.
pub(crate) const FOCUS_VALUES: &[&str] = &["off", "paragraph", "sentence"];

/// The values `:align` and `:set align` take (`TextAlign::parse`).
pub(crate) const ALIGN_VALUES: &[&str] = &["left", "center", "right", "justified"];

/// The values `:embed` and `:set embed` take (`transclude::Mode::parse`).
pub(crate) const EMBED_VALUES: &[&str] = &["none", "short", "rec", "full"];

/// The values `:number` and `:set numbers` take (`NumberMode::parse`).
pub(crate) const NUMBER_VALUES: &[&str] = &["off", "relative", "absolute"];

/// The formats `:export` takes (`export::Format::parse`).
pub(crate) const EXPORT_VALUES: &[&str] = &["md", "txt", "html", "pdf"];

/// The values a `:set` option takes, by its name in `OPTIONS`: on and off
/// for a toggle, the named choices for the rest. A number has none to list.
pub(crate) fn option_values(option: &str) -> &'static [&'static str] {
    match option {
        "focus" => FOCUS_VALUES,
        "align" => ALIGN_VALUES,
        "embed" => EMBED_VALUES,
        "numbers" => NUMBER_VALUES,
        "autoreload" | "autosave" | "clipboard" | "code_syntax" | "conceal" | "hints" | "mouse" | "typewriter" => {
            TOGGLE_VALUES
        }
        _ => &[],
    }
}

/// The values a command's argument takes, by its name in `COMMANDS`, for
/// the commands that take a word from a fixed list.
pub(crate) fn command_values(command: &str) -> &'static [&'static str] {
    match command {
        "focus" => FOCUS_VALUES,
        "align" => ALIGN_VALUES,
        "embed" => EMBED_VALUES,
        "number" => NUMBER_VALUES,
        "export" => EXPORT_VALUES,
        _ => &[],
    }
}

/// The options that take a number, which `option_values` has no list for.
#[cfg(test)]
pub(crate) const NUMBER_OPTIONS: &[&str] = &["autosave_interval", "line_spacing", "measure", "tab_width"];

/// The window and buffer commands of `COMMANDS`. Each ends the command under
/// way, as their keys do (`App::end_recording`), whether or not it went
/// anywhere, so none is ever the change `.` repeats. A new command that
/// opens, closes or switches a buffer or pane goes here too.
pub(crate) const WINDOW_COMMANDS: &[&str] = &[
    "q", "qall", "wq", "split", "vsplit", "close", "only", "bnext", "bprevious", "buffer", "bdelete", "edit",
];

/// A typed name's entry in `table`, by its name or one of its other names.
pub(crate) fn canonical(table: &[(&'static str, &[&str])], typed: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(name, other)| *name == typed || other.contains(&typed))
        .map(|(name, _)| *name)
}

impl App {
    pub(crate) fn run_command(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        let (name, arg) = match cmd.split_once(char::is_whitespace) {
            Some((n, a)) => (n, a.trim()),
            None => (cmd, ""),
        };
        if name.is_empty() {
            return;
        }
        // NOT a forced `:e`: this editor spends `:e!` on the config. A command
        // that re-reads the file needs its own name (`:revert`) rather than a
        // second meaning for one that is taken.
        if name == "e!" {
            return self.reload_config();
        }
        // `!` is "I know what I'm doing": past the external-modification
        // guard for a write, past unsaved changes for a quit. Only the
        // commands below that take it accept it.
        let (base, bang) = match name.strip_suffix('!') {
            Some(b) => (b, true),
            None => (name, false),
        };
        let Some(command) = canonical(COMMANDS, base) else {
            return self.notify(format!("not a command: {name}"), FlashKind::Error);
        };

        match (command, bang) {
            ("write", force) => {
                self.write(if arg.is_empty() { None } else { Some(arg) }, force);
            }
            // `:saveas` switches the buffer to the new file; `:w <path>` only
            // writes a copy there.
            ("saveas", force) => {
                if arg.is_empty() {
                    self.notify("usage: :saveas <path>", FlashKind::Error);
                } else {
                    self.save_as(arg, force);
                }
            }
            ("q", force) => self.try_quit(force),
            // Capital Q leaves outright — the whole session, every buffer.
            // `qa`/`qall` are vim's spelling of the same thing.
            ("qall", force) => self.quit_all(force),
            ("split", false) => self.split_pane(false),
            ("vsplit", false) => self.split_pane(true),
            ("close", false) => {
                if !self.close_pane() {
                    self.notify("last pane — :q to quit", FlashKind::Info);
                }
            }
            ("only", false) => self.only_pane(),
            ("view", false) => self.set_view(!self.view),
            ("bnext", false) => self.cycle_buffer(true),
            ("bprevious", false) => self.cycle_buffer(false),
            ("buffer", false) => {
                if arg.is_empty() {
                    self.notify(self.buffer_list(), FlashKind::Info);
                } else {
                    self.switch_by_name(arg);
                }
            }
            ("buffers", false) => self.notify(self.buffer_list(), FlashKind::Info),
            ("bdelete", force) => self.close_buffer(force),
            ("edit", false) => {
                if arg.is_empty() {
                    self.notify("usage: :e <path>", FlashKind::Error);
                } else {
                    self.open_file(self.typed_path(arg));
                }
            }
            // With a path, `:wq` saves the buffer AS that file and then
            // quits: a copy (what `:w <path>` writes) would leave this file's
            // edits unsaved, and `:wq!` would then throw them away.
            ("wq", false) => {
                if arg.is_empty() {
                    self.write(None, false);
                } else {
                    self.save_as(arg, false);
                }
                self.try_quit(false);
            }
            // The `!` forces the WRITE, not the quit: a write that fails
            // still keeps the editor open, or the unsaved text would be lost.
            ("wq", true) => {
                let written = if arg.is_empty() {
                    self.write(None, true)
                } else {
                    self.save_as(arg, true)
                };
                if written {
                    self.try_quit(true);
                }
            }
            ("undo", false) => self.undo_or_warn(),
            ("redo", false) => self.redo_or_warn(),
            ("reload", false) => self.reload_config(),
            ("diff", false) => self.open_diff(),
            ("revert", force) => self.revert(force),
            ("help", false) => self.open_help(arg),
            ("focus", false) => {
                let mode = if arg.is_empty() {
                    Some(self.focus.next())
                } else {
                    FocusMode::parse(arg)
                };
                match mode {
                    Some(m) => self.set_focus(m),
                    None => self.notify(
                        format!("focus: {}, not {arg:?}", FOCUS_VALUES.join(" · ")),
                        FlashKind::Error,
                    ),
                }
            }
            ("typewriter", false) => {
                self.config.layout.typewriter = !self.config.layout.typewriter;
                let on = if self.config.layout.typewriter { "on" } else { "off" };
                self.notify(format!("typewriter {on}"), FlashKind::Info);
            }
            ("number", false) => self.set_numbers(arg),
            ("align", false) => self.set_align(arg),
            ("export", false) => self.open_export(arg),
            // §14.3. Also re-reads every target, so it doubles as the way to
            // refresh an expansion after editing the file it came from.
            ("embed", false) => self.set_embed_mode(arg),
            ("set", false) => self.set_option(arg),
            ("zen", false) => self.apply_action(Action::ToggleZen),
            // Through `run`, so read-only mode refuses it like any edit.
            ("fixlist", false) => self.run(Command::bare(Action::FixList)),
            _ => self.notify(format!("not a command: {name}"), FlashKind::Error),
        }
        if WINDOW_COMMANDS.contains(&command) {
            self.end_recording();
        }
    }

    /// `:embed [off|short|long|full]` — how much of an embed to expand
    /// (SPEC.md §14.3).
    ///
    /// With no argument it TOGGLES, and turning it back on returns to whichever
    /// mode was last chosen: someone reading in `full` who glances at the raw
    /// source expects `:embed` twice to put them back where they were.
    fn set_embed_mode(&mut self, arg: &str) {
        use crate::transclude::Mode;
        let next = if arg.trim().is_empty() {
            if self.embed_mode.is_on() {
                Mode::Off
            } else {
                self.last_embed_mode
            }
        } else {
            match Mode::parse(arg) {
                Some(m) => m,
                None => {
                    return self.notify(
                        format!("embed: {}, not {arg:?}", EMBED_VALUES.join(" · ")),
                        FlashKind::Error,
                    )
                }
            }
        };
        if next.is_on() {
            self.last_embed_mode = next;
        }
        self.embed_mode = next;
        self.notify(format!("embed {}", next.name()), FlashKind::Info);
    }

    /// `:number [relative|absolute]` — the line-number gutter.
    ///
    /// With no argument it TOGGLES, and turning it back on returns to whichever
    /// mode was last chosen: someone who switched to `relative` for a `5dj`
    /// expects `:number` twice to put them back in `relative`, not to default
    /// to `absolute`.
    pub(super) fn set_numbers(&mut self, arg: &str) {
        let next = if arg.trim().is_empty() {
            if self.numbers.is_on() {
                NumberMode::Off
            } else {
                self.last_numbers
            }
        } else {
            match NumberMode::parse(arg) {
                Some(m) => m,
                None => {
                    return self.notify(
                        format!("number: {}, not {arg:?}", NUMBER_VALUES.join(" · ")),
                        FlashKind::Error,
                    )
                }
            }
        };
        if next.is_on() {
            self.last_numbers = next;
        }
        self.numbers = next;
        self.notify(format!("number: {}", next.label()), FlashKind::Info);
    }

    /// `:align [left|center|right|justified]` — how each row of text sits
    /// within the writing column (SPEC.md §6). Column placement itself
    /// (`layout.align`, `center | left`) is untouched by this.
    ///
    /// With no argument it CYCLES left → center → right → justified → left,
    /// same shape as `:focus`: there is no single "off" state to toggle back
    /// to, so cycling is the only motion that reaches every value from a key
    /// press alone.
    pub(super) fn set_align(&mut self, arg: &str) {
        let next = if arg.trim().is_empty() {
            self.text_align.next()
        } else {
            match TextAlign::parse(arg) {
                Some(a) => a,
                None => {
                    return self.notify(
                        format!("align: {}, not {arg:?}", ALIGN_VALUES.join(" · ")),
                        FlashKind::Error,
                    )
                }
            }
        };
        self.text_align = next;
        self.notify(format!("align: {}", next.label()), FlashKind::Info);
    }

    /// `:set <key> [on|off]` or `:set <key>=<value>` — toggle when no value.
    ///
    /// A NUMERIC option reports its current value instead of toggling when no
    /// value is given, since there is nothing to flip. The render cache keys its
    /// entries on the measure, so changing it re-wraps on the next frame with no
    /// help from here.
    pub(super) fn set_option(&mut self, arg: &str) {
        let (key, val) = match arg.split_once(['=', ' ', '\t']) {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (arg.trim(), ""),
        };
        let Some(option) = canonical(OPTIONS, key) else {
            return self.notify(format!("unknown option: {key}"), FlashKind::Error);
        };
        let resolve = |v: &str, cur: bool| match v {
            "" => !cur,
            v => ON_WORDS.contains(&v),
        };
        match option {
            // The text measure — `layout.measure` in the config file, and the
            // one setting a writer wants to try on rather than commit to.
            "measure" => {
                let cur = self.config.layout.measure;
                if val.is_empty() {
                    return self.notify(format!("measure={cur}"), FlashKind::Info);
                }
                let Ok(n) = val.parse::<u16>() else {
                    return self.notify(format!("measure: {val:?} is not a number"), FlashKind::Error);
                };
                if n < MIN_MEASURE {
                    return self.notify(
                        format!("measure: {MIN_MEASURE} columns is the minimum"),
                        FlashKind::Error,
                    );
                }
                self.config.layout.measure = n;
                return self.notify(format!("measure={n}"), FlashKind::Info);
            }
            // Line height on a grid that has none: blank rows after each line.
            // Numeric, so like `measure` it reports rather than toggles.
            "line_spacing" => {
                let cur = self.config.layout.line_spacing;
                if val.is_empty() {
                    return self.notify(format!("line_spacing={cur}"), FlashKind::Info);
                }
                let Ok(n) = val.parse::<u16>() else {
                    return self.notify(
                        format!("line_spacing: {val:?} is not a number"),
                        FlashKind::Error,
                    );
                };
                self.config.layout.line_spacing = n.min(crate::render::layout::MAX_LINE_SPACING);
                let set = self.config.layout.line_spacing;
                return self.notify(format!("line_spacing={set}"), FlashKind::Info);
            }
            // How many columns a tab counts: the indent unit of `>`, and how
            // deep a tab-indented list item is. `refresh_blocks` sees the
            // block caches no longer match and reclassifies every document.
            "tab_width" => {
                let cur = self.config.editor.tab_width;
                if val.is_empty() {
                    return self.notify(format!("tab_width={cur}"), FlashKind::Info);
                }
                let Ok(n) = val.parse::<usize>() else {
                    return self.notify(format!("tab_width: {val:?} is not a number"), FlashKind::Error);
                };
                if !(1..=MAX_TAB_WIDTH).contains(&n) {
                    return self.notify(format!("tab_width: 1 to {MAX_TAB_WIDTH}"), FlashKind::Error);
                }
                self.config.editor.tab_width = n;
                return self.notify(format!("tab_width={n}"), FlashKind::Info);
            }
            // Reports on/off rather than falling through to the generic
            // "set autosave", which for a toggle says everything except which
            // way it went — and this is a toggle that writes files.
            "autosave" => {
                let on = resolve(val, self.config.editor.autosave);
                self.config.editor.autosave = on;
                self.arm_autosave();
                let every = crate::fs::save::Autosave::from_config(&self.config.editor)
                    .every
                    .minutes();
                return match on {
                    true => self.notify(format!("autosave on · every {every} min"), FlashKind::Info),
                    false => self.notify("autosave off", FlashKind::Info),
                };
            }
            "autoreload" => {
                let on = resolve(val, self.config.editor.autoreload);
                self.config.editor.autoreload = on;
                return match on {
                    true => self.notify("autoreload on", FlashKind::Info),
                    // Leave any standing conflict flag alone: the files really
                    // did diverge, and turning the watcher off does not undo it.
                    false => self.notify("autoreload off", FlashKind::Info),
                };
            }
            "clipboard" => {
                let on = resolve(val, self.config.editor.clipboard);
                self.config.editor.clipboard = on;
                return match on {
                    true => self.notify("clipboard on · y d p use the system clipboard", FlashKind::Info),
                    false => self.notify("clipboard off · \"+y \"+p still reach it", FlashKind::Info),
                };
            }
            // Minutes, so it reports rather than toggles — like `measure`.
            // Setting it does NOT turn autosave on: the interval is a
            // preference, switching it on is a decision.
            "autosave_interval" => {
                use crate::fs::save::AutosaveInterval;
                let cur = self.config.editor.autosave_interval;
                if val.is_empty() {
                    return self.notify(format!("autosave_interval={cur}"), FlashKind::Info);
                }
                let n = val.parse::<u8>().ok().and_then(AutosaveInterval::parse);
                let Some(n) = n else {
                    return self.notify(
                        format!(
                            "autosave_interval: {} to {} minutes, not {val:?}",
                            AutosaveInterval::MIN,
                            AutosaveInterval::MAX
                        ),
                        FlashKind::Error,
                    );
                };
                self.config.editor.autosave_interval = n.minutes();
                self.arm_autosave();
                return self.notify(
                    format!("autosave_interval={}", n.minutes()),
                    FlashKind::Info,
                );
            }
            "code_syntax" => {
                self.config.markdown.code_syntax = resolve(val, self.config.markdown.code_syntax);
            }
            // The hint bar. Says which way it went: turned off, the row just
            // goes quiet, which would look like nothing happened.
            "hints" => {
                let on = resolve(val, self.config.hints.enabled);
                self.config.hints.enabled = on;
                let msg = if on { "hints on" } else { "hints off · [hints] enabled = false keeps it off" };
                return self.notify(msg, FlashKind::Info);
            }
            "typewriter" => {
                self.config.layout.typewriter = resolve(val, self.config.layout.typewriter);
            }
            "conceal" => {
                self.config.layout.conceal = resolve(val, self.config.layout.conceal);
            }
            "embed" => return self.set_embed_mode(val),
            "numbers" => return self.set_numbers(val),
            "align" => return self.set_align(val),
            "mouse" => {
                self.config.input.mouse = resolve(val, self.config.input.mouse);
                self.set_mouse_capture(self.config.input.mouse);
            }
            "focus" => {
                let mode = if val.is_empty() {
                    Some(self.focus.next())
                } else {
                    FocusMode::parse(val)
                };
                match mode {
                    Some(m) => self.set_focus(m),
                    None => self.notify(
                        format!("focus: {}, not {val:?}", FOCUS_VALUES.join(" · ")),
                        FlashKind::Error,
                    ),
                }
                return;
            }
            // Every name in `OPTIONS` has an arm above.
            _ => return,
        }
        self.notify(format!("set {key}"), FlashKind::Info);
    }

    /// `:w`. True when the file was written.
    ///
    /// `:w <path>` on a buffer that already has a file writes a COPY and keeps
    /// editing the original, as vim does: `:w backup.md` is a checkpoint, not
    /// a move. An unnamed buffer takes the name instead, which is how it gets
    /// one (`:saveas` switches a named buffer).
    pub(super) fn write(&mut self, path: Option<&str>, force: bool) -> bool {
        if let Some(typed) = path {
            if self.buffer.path.is_none() {
                return self.save_as(typed, force);
            }
            let p = self.typed_path(typed);
            if !self.buffer.is_own_file(&p) {
                return self.write_copy(&p, force);
            }
        }
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        let result = self.buffer.save(policy, force);
        self.after_write(result)
    }

    /// `:saveas <path>`, and `:w <path>` on an unnamed buffer: write there and
    /// make it this buffer's file, styled by its new extension.
    fn save_as(&mut self, typed: &str, force: bool) -> bool {
        let p = self.typed_path(typed);
        // A file is edited in ONE buffer (`open_file` holds to that); taking
        // over another buffer's file would leave two copies racing to save.
        let current = self.current();
        if self.docs.iter().enumerate().any(|(i, d)| i != current && d.buffer.is_own_file(&p)) {
            self.notify(format!("{} is open in another buffer", self.shown(&p)), FlashKind::Error);
            return false;
        }
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        let result = self.buffer.save_as(p.clone(), policy, force);
        if result.is_ok() {
            let exts = &self.config.markdown.plain_text_extensions;
            self.buffer.syntax = crate::fs::open::syntax_for(&p, exts);
        }
        self.after_write(result)
    }

    /// `:w <path>` from a buffer with its own file: the text goes to `path`,
    /// and the buffer stays what it was.
    fn write_copy(&mut self, path: &Path, force: bool) -> bool {
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        match self.buffer.write_copy(path, policy, force) {
            Ok(()) => {
                self.disk_changed();
                let mine = self.buffer.display_name();
                self.notify(
                    format!("{} written (still editing {mine})", self.shown(path)),
                    FlashKind::Info,
                );
                true
            }
            Err(e) => {
                self.notify(format!("{e}"), FlashKind::Error);
                false
            }
        }
    }

    /// Report a write of this buffer's own file.
    fn after_write(&mut self, result: anyhow::Result<()>) -> bool {
        match result {
            Ok(()) => {
                self.claim_save_revisions();
                self.disk_changed();
                let name = self.buffer.display_name();
                let words = self.buffer.word_count();
                self.notify(format!("{name} written · {words} words"), FlashKind::Info);
                // The clock starts from the last write, not from startup.
                self.arm_autosave();
                true
            }
            Err(e) => {
                self.notify(format!("{e}"), FlashKind::Error);
                false
            }
        }
    }
}
