//! Saving files. SPEC.md §10.
//!
//! Atomic: write `.<file>.swtmp` in the SAME directory (so `rename` stays on
//! one filesystem), fsync, then rename over the target. A crash mid-save leaves
//! the original intact. Original mode bits are preserved.
//!
//! When a rename would do harm or cannot be done, the file is written in place
//! instead: a folder the user cannot write to, a name with no room for the temp
//! name, a file with other hard links, or one owned by someone else.
//!
//! No swap files, and autosave is OFF unless `[editor] autosave` asks for it —
//! a writing tool should not surprise the user with writes they did not ask
//! for. The one exception is an editor that is dying: a crash, `SIGTERM` or
//! `SIGHUP` writes each modified buffer to a recovery file (`crate::recover`),
//! and only then. When it IS on it writes through this same `write_atomic`, guard
//! included, so a timer can never clobber what `:w` would have refused to.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use ropey::Rope;

use crate::text::buffer::LineEnding;

pub struct SaveOptions {
    pub line_ending: LineEnding,
    pub final_newline: bool,
    pub trim_trailing_whitespace: bool,
    /// Markdown's trimming rule: the two trailing spaces of a hard line break
    /// are markup, not whitespace.
    pub markdown: bool,
    /// The cursor's line, which trimming and the final-newline rule leave
    /// alone: the buffer takes the same changes after a save, and an autosave
    /// must not eat the space just typed before the next word.
    pub keep_line: Option<usize>,
}

/// What `[editor] final_newline` asks of a save.
///
/// `Preserve` is the default and the one that surprises nobody: a file that
/// arrived without a trailing newline leaves without one. The other two exist
/// because a repository with a lint on it wants the same answer every time,
/// whatever each file happened to have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FinalNewline {
    #[default]
    Preserve,
    Always,
    Never,
}

impl FinalNewline {
    /// `None` for a spelling this does not know — the caller reports it rather
    /// than silently picking one, which is how this setting sat unread for a
    /// release.
    pub fn parse(s: &str) -> Option<FinalNewline> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "preserve" | "keep" | "as-is" => FinalNewline::Preserve,
            "always" | "true" | "yes" | "on" => FinalNewline::Always,
            "never" | "false" | "no" | "off" => FinalNewline::Never,
            _ => return None,
        })
    }

    /// Resolve against what the file had when it was opened.
    pub fn resolve(self, had_one: bool) -> bool {
        match self {
            FinalNewline::Preserve => had_one,
            FinalNewline::Always => true,
            FinalNewline::Never => false,
        }
    }
}

/// The `[editor]` settings a save consults, gathered once so `save`/`save_as`
/// take one argument that says what it is instead of a row of bare bools.
#[derive(Clone, Copy, Debug, Default)]
pub struct SavePolicy {
    pub trim_trailing_whitespace: bool,
    pub final_newline: FinalNewline,
}

impl SavePolicy {
    pub fn from_config(cfg: &crate::config::schema::EditorConfig) -> SavePolicy {
        SavePolicy {
            trim_trailing_whitespace: cfg.trim_on_save,
            // An unrecognised spelling falls back to the default here; startup
            // has already flagged it through `config::validate`.
            final_newline: FinalNewline::parse(&cfg.final_newline).unwrap_or_default(),
        }
    }
}

/// How often `[editor] autosave` writes, in whole minutes.
///
/// A ladder of five rather than a free number of seconds. The point of a bound
/// is that both ends are wrong for the feature: a 10-second autosave is a swap
/// file with the document's own name on it, and an hourly one has not saved
/// anything you would miss. Minutes also keep the setting readable — `3` is a
/// number a reader can hold in their head, `180000` is one they copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutosaveInterval(u8);

impl AutosaveInterval {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 5;
    pub const DEFAULT: u8 = 3;

    /// `None` outside 1–5, so the caller reports the number instead of quietly
    /// rounding it — the same rule `FinalNewline::parse` follows, and for the
    /// same reason: a setting that silently ignores what you wrote reads as a
    /// setting that does not work.
    pub fn parse(minutes: u8) -> Option<AutosaveInterval> {
        (Self::MIN..=Self::MAX)
            .contains(&minutes)
            .then_some(AutosaveInterval(minutes))
    }

    pub fn minutes(self) -> u8 {
        self.0
    }

    pub fn duration(self) -> Duration {
        Duration::from_secs(self.0 as u64 * 60)
    }
}

impl Default for AutosaveInterval {
    fn default() -> Self {
        AutosaveInterval(AutosaveInterval::DEFAULT)
    }
}

/// `[editor] autosave` and `autosave_interval` reduced to the one question the
/// event loop asks: how long until the next write, or nothing at all.
///
/// Two fields rather than one `Option<u8>` because they answer to two different
/// people. Turning autosave on and off is the decision; the interval is a
/// preference that should still be there, unchanged, the next time it goes on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Autosave {
    pub enabled: bool,
    pub every: AutosaveInterval,
}

impl Autosave {
    pub fn from_config(cfg: &crate::config::schema::EditorConfig) -> Autosave {
        Autosave {
            enabled: cfg.autosave,
            // Out of range falls back here; `config::validate` is what makes
            // the number visible rather than this silently taking 3.
            every: AutosaveInterval::parse(cfg.autosave_interval).unwrap_or_default(),
        }
    }

    /// The gap between writes, or `None` when autosave is off — which is also
    /// the disarmed timer, so the loop has one thing to ask and not two.
    pub fn interval(self) -> Option<Duration> {
        self.enabled.then(|| self.every.duration())
    }
}

/// The text as a save writes it, before the line ending is applied: trimmed
/// and with its final newline settled as `opts` asks.
///
/// Only ever shortens lines and adds or removes the one final newline, which
/// is what lets `Buffer` replay the result as a few small edits.
pub fn outgoing(text: &str, opts: &SaveOptions) -> String {
    let mut text = if opts.trim_trailing_whitespace {
        trim_lines(text, opts)
    } else {
        text.to_string()
    };

    if opts.final_newline {
        if !text.ends_with('\n') {
            text.push('\n');
        }
    } else if text.ends_with('\n') {
        // One newline, not all of them: blank lines at the end are text the
        // writer put there. And none while the cursor sits on the empty line
        // after it, where Insert mode is about to type.
        let last = text.matches('\n').count();
        if opts.keep_line != Some(last) {
            text.pop();
        }
    }
    text
}

/// `trim_on_save`, minus what only looks like trailing whitespace: lines of
/// fenced code (literal, and a trailing space there may be the point), the
/// two spaces of a Markdown hard break, and the cursor's line.
fn trim_lines(text: &str, opts: &SaveOptions) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut fences = crate::transclude::Fences::default();
    let mut out = String::with_capacity(text.len());
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // Fences are Markdown's: in a plain file a stray ``` is just text,
        // and must not stop trimming for the rest of it.
        let literal = opts.markdown && fences.literal(line);
        let keep = literal
            || opts.keep_line == Some(i)
            || (opts.markdown && hard_break(line, lines.get(i + 1).copied()));
        out.push_str(if keep { line } else { line.trim_end() });
    }
    out
}

/// Exactly two trailing spaces after text, with a line of text after it.
fn hard_break(line: &str, next: Option<&str>) -> bool {
    let body = line.trim_end();
    !body.is_empty()
        && &line[body.len()..] == "  "
        && next.is_some_and(|n| !n.trim().is_empty())
}

/// Returns the new mtime, for external-modification tracking.
///
/// A symlink is written through to the file it names: renaming over the link
/// itself would swap it for a regular file and leave the real one unedited.
pub fn write_atomic(path: &Path, rope: &Rope, opts: &SaveOptions) -> Result<SystemTime> {
    let text = outgoing(&rope.chunks().collect::<String>(), opts);
    write_text(path, &text, opts.line_ending)
}

/// `write_atomic` for text `outgoing` has already prepared, so a caller that
/// needs that text too computes it once.
pub fn write_text(path: &Path, text: &str, line_ending: LineEnding) -> Result<SystemTime> {
    let resolved = path
        .is_symlink()
        .then(|| std::fs::canonicalize(path).ok())
        .flatten();
    let path = resolved.as_deref().unwrap_or(path);

    let text: std::borrow::Cow<str> = match line_ending {
        LineEnding::Crlf => text.replace('\n', "\r\n").into(),
        LineEnding::Lf => text.into(),
    };

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));

    // A rename gives the path a new inode. When other names share the old one
    // (hard links), only this name would see the new text: write into the
    // inode itself so they all do.
    // A file the user may not write keeps the rename, which the folder's
    // permission allows: refusing a save that used to work is no fix.
    if shares_its_inode(path) {
        if let Some(f) = open_in_place(path) {
            return write_in_place(f, path, &text);
        }
    }

    let stem = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string());
    let tmp = dir.join(format!(".{stem}.swtmp"));

    let created = match std::fs::File::create(&tmp) {
        Ok(f) => f,
        // A folder the user cannot write to may still hold a file they can (a
        // shared folder, group write on the file alone), and a name near the
        // 255-byte limit has no room for the temp name's extra bytes. Neither
        // stops an ordinary write, so do that rather than refuse the save.
        // `InvalidFilename` is how std reports ENAMETOOLONG.
        Err(e)
            if path.is_file()
                && matches!(
                    e.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidFilename
                ) =>
        {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .with_context(|| format!("writing {}", path.display()))?;
            return write_in_place(f, path, &text);
        }
        Err(e) => {
            return Err(e).with_context(|| format!("creating temp file {}", tmp.display()));
        }
    };

    // A rename hands the file to whoever created the temp file: someone else's
    // file, writable here through its group, would become ours (or change
    // group), and lose its extended attributes and ACLs on the way. The temp
    // file's owner says who "ours" is without a libc call.
    if owned_by_someone_else(path, &created) {
        if let Some(f) = open_in_place(path) {
            drop(created);
            let _ = std::fs::remove_file(&tmp);
            return write_in_place(f, path, &text);
        }
    }

    // Everything from here can fail, and a failure must not leave `.a.md.swtmp`
    // sitting beside the file — a dotfile the editor hides and the reader never
    // asked for. One closure so there is a single place to clean up from.
    let attempt = || -> Result<SystemTime> {
        {
            let mut f = created;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }

        // Preserve the original's mode bits if it already existed.
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }

        std::fs::rename(&tmp, path)
            .with_context(|| format!("replacing {} with {}", path.display(), tmp.display()))?;

        // The rename lives in the folder's entry list, which has buffers of its
        // own: without this a power cut can bring back the old file although
        // the new one was synced. Best effort, since not every system lets a
        // folder be opened and synced, and the save itself has succeeded.
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }

        Ok(mtime_of(path))
    };

    let result = attempt();
    if result.is_err() {
        // Best effort: if this fails too there is nothing further to try, and
        // the error worth reporting is the one from the save itself.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The file opened for an in-place write, or `None` when the user may not
/// write it (or it is gone), so the caller can fall back to the rename.
fn open_in_place(path: &Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new().write(true).open(path).ok()
}

/// The fallback for a file a rename would damage or cannot reach: write over
/// the file itself. Not atomic (a crash mid-write leaves it half old, half
/// new), which is why it is the fallback and not the rule.
///
/// The old bytes are overwritten first and the length cut after, rather than
/// truncating up front: a full disk then fails while the original is still
/// mostly there, not after it has been emptied.
fn write_in_place(mut f: std::fs::File, path: &Path, text: &str) -> Result<SystemTime> {
    f.write_all(text.as_bytes())
        .and_then(|_| f.set_len(text.len() as u64))
        .and_then(|_| f.sync_all())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(mtime_of(path))
}

fn mtime_of(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or_else(|_| SystemTime::now())
}

#[cfg(unix)]
fn shares_its_inode(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.nlink() > 1)
}

#[cfg(not(unix))]
fn shares_its_inode(_: &Path) -> bool {
    false
}

#[cfg(unix)]
fn owned_by_someone_else(path: &Path, ours: &std::fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(path), ours.metadata()) {
        (Ok(theirs), Ok(ours)) => {
            theirs.is_file() && (theirs.uid() != ours.uid() || theirs.gid() != ours.gid())
        }
        _ => false,
    }
}

#[cfg(not(unix))]
fn owned_by_someone_else(_: &Path, _: &std::fs::File) -> bool {
    false
}

/// Compare the recorded mtime against the file on disk. `true` means someone
/// else changed it and we must prompt rather than clobber.
pub fn changed_externally(path: &Path, known: Option<SystemTime>) -> bool {
    let known = match known {
        Some(k) => k,
        None => return false,
    };
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(actual) => actual != known,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shoin-save-{tag}-{t}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn opts(final_newline: bool, trim: bool) -> SaveOptions {
        SaveOptions {
            line_ending: LineEnding::Lf,
            final_newline,
            trim_trailing_whitespace: trim,
            markdown: true,
            keep_line: None,
        }
    }

    /// The three spellings, and the aliases people reach for.
    #[test]
    fn the_final_newline_ladder_parses() {
        assert_eq!(FinalNewline::parse("preserve"), Some(FinalNewline::Preserve));
        assert_eq!(FinalNewline::parse("  ALWAYS "), Some(FinalNewline::Always));
        assert_eq!(FinalNewline::parse("never"), Some(FinalNewline::Never));
        assert_eq!(FinalNewline::parse("true"), Some(FinalNewline::Always));
        // A typo is reported, not silently taken as the default — that is what
        // let this setting sit unread.
        assert_eq!(FinalNewline::parse("alway"), None);
    }

    /// `preserve` answers with the file's own habit; the other two overrule it.
    #[test]
    fn the_policy_resolves_against_what_the_file_had() {
        assert!(FinalNewline::Preserve.resolve(true));
        assert!(!FinalNewline::Preserve.resolve(false));
        assert!(FinalNewline::Always.resolve(false), "always overrules");
        assert!(!FinalNewline::Never.resolve(true), "never overrules");
    }

    /// The bound is the feature: five values, and a number outside it is
    /// reported rather than rounded into range.
    #[test]
    fn the_autosave_interval_is_one_to_five_minutes() {
        for m in 1..=5u8 {
            let i = AutosaveInterval::parse(m).expect("in range");
            assert_eq!(i.minutes(), m);
            assert_eq!(i.duration(), Duration::from_secs(m as u64 * 60));
        }
        assert_eq!(AutosaveInterval::parse(0), None, "0 is not an interval");
        assert_eq!(AutosaveInterval::parse(6), None);
        assert_eq!(AutosaveInterval::parse(255), None);
        assert_eq!(AutosaveInterval::default().minutes(), 3);
    }

    /// The `[editor]` section reaches the timer, and the default is OFF — the
    /// one assertion that has to hold whatever else changes here.
    #[test]
    fn the_editor_section_reaches_the_autosave_timer() {
        use crate::config::schema::EditorConfig;
        let mut cfg = EditorConfig::default();
        assert!(!cfg.autosave, "autosave is off unless asked for");
        assert_eq!(
            Autosave::from_config(&cfg).interval(),
            None,
            "off means no deadline at all, not a deadline nobody acts on"
        );

        cfg.autosave = true;
        assert_eq!(
            Autosave::from_config(&cfg).interval(),
            Some(Duration::from_secs(180)),
            "the default interval is three minutes"
        );

        cfg.autosave_interval = 5;
        assert_eq!(
            Autosave::from_config(&cfg).interval(),
            Some(Duration::from_secs(300))
        );

        // Out of range falls back rather than refusing the config;
        // `config::validate` is what makes the number visible.
        cfg.autosave_interval = 9;
        assert_eq!(
            Autosave::from_config(&cfg).every.minutes(),
            AutosaveInterval::DEFAULT
        );

        // Turning it off keeps the interval, so turning it back on remembers.
        cfg.autosave = false;
        cfg.autosave_interval = 2;
        assert_eq!(Autosave::from_config(&cfg).interval(), None);
        assert_eq!(Autosave::from_config(&cfg).every.minutes(), 2);
    }

    /// The `[editor]` section actually reaches a save. This is the assertion
    /// that was missing: `final_newline` was parsed, defaulted, documented in
    /// `shoin/editor.conf`, and read by nothing at all.
    #[test]
    fn the_editor_section_reaches_the_save() {
        use crate::config::schema::EditorConfig;
        let mut cfg = EditorConfig::default();
        assert_eq!(
            SavePolicy::from_config(&cfg).final_newline,
            FinalNewline::Preserve
        );

        cfg.final_newline = "never".into();
        cfg.trim_on_save = true;
        let p = SavePolicy::from_config(&cfg);
        assert_eq!(p.final_newline, FinalNewline::Never);
        assert!(p.trim_trailing_whitespace);

        // A value nothing understands falls back rather than refusing the
        // config; `config::validate` is what makes that visible.
        cfg.final_newline = "alway".into();
        assert_eq!(
            SavePolicy::from_config(&cfg).final_newline,
            FinalNewline::Preserve
        );
    }

    #[test]
    fn a_final_newline_is_added_or_stripped_as_asked() {
        let d = dir("nl");
        let p = d.join("a.md");

        write_atomic(&p, &Rope::from_str("one\ntwo"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo\n");

        write_atomic(&p, &Rope::from_str("one\ntwo\n"), &opts(false, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo");

        // Blank lines the writer put at the end are text, not newlines to
        // strip: only the final one goes.
        write_atomic(&p, &Rope::from_str("a\n\n\n"), &opts(false, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\n\n");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn trailing_whitespace_goes_only_when_asked() {
        let d = dir("trim");
        let p = d.join("a.md");

        write_atomic(&p, &Rope::from_str("a  \nb\t\n"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a  \nb\t\n");

        write_atomic(&p, &Rope::from_str("a \nb\t\n"), &opts(true, true)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\nb\n");
        std::fs::remove_dir_all(&d).ok();
    }

    fn trimmed(text: &str, markdown: bool, keep_line: Option<usize>) -> String {
        let o = SaveOptions { markdown, keep_line, ..opts(true, true) };
        outgoing(text, &o)
    }

    /// Code is literal: a trailing space inside a fence may be the point.
    #[test]
    fn trimming_leaves_fenced_code_alone() {
        assert_eq!(
            trimmed("x \n```\nx  \n```\ny \n", true, None),
            "x\n```\nx  \n```\ny\n"
        );
        assert_eq!(trimmed("~~~ \n\tx\t\n~~~\n", true, None), "~~~ \n\tx\t\n~~~\n");
        // Outside Markdown a stray ``` is text, and trimming carries on.
        assert_eq!(trimmed("```\nx \n", false, None), "```\nx\n");
    }

    /// Two trailing spaces before a line of text are a hard line break in
    /// Markdown. Anything else at the end of a line is whitespace.
    #[test]
    fn trimming_keeps_a_markdown_hard_break() {
        assert_eq!(trimmed("one  \ntwo\n", true, None), "one  \ntwo\n");
        assert_eq!(trimmed("one  \ntwo\n", false, None), "one\ntwo\n", "not in plain text");
        assert_eq!(trimmed("one   \ntwo\n", true, None), "one\ntwo\n", "three is not two");
        assert_eq!(trimmed("one  \n\ntwo\n", true, None), "one\n\ntwo\n", "nothing to break to");
        assert_eq!(trimmed("one  \n", true, None), "one\n", "last line");
        assert_eq!(trimmed("  \ntwo\n", true, None), "\ntwo\n", "a blank line");
    }

    /// The cursor's line keeps its spaces, and a cursor on the empty line
    /// after the final newline keeps that newline: the buffer takes the same
    /// changes, and the reader is typing there.
    #[test]
    fn the_cursor_line_is_left_alone() {
        assert_eq!(trimmed("a \nb \n", true, Some(1)), "a\nb \n");
        let never = SaveOptions { keep_line: Some(1), ..opts(false, false) };
        assert_eq!(outgoing("a\n", &never), "a\n");
        let never = SaveOptions { keep_line: Some(0), ..opts(false, false) };
        assert_eq!(outgoing("a\n", &never), "a");
    }

    /// The rope holds `\n` only; the file gets whatever the original used.
    #[test]
    fn crlf_is_reapplied_on_the_way_out() {
        let d = dir("crlf");
        let p = d.join("a.md");
        let o = SaveOptions {
            line_ending: LineEnding::Crlf,
            final_newline: true,
            trim_trailing_whitespace: false,
            markdown: true,
            keep_line: None,
        };
        write_atomic(&p, &Rope::from_str("one\ntwo\n"), &o).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\r\ntwo\r\n");
        std::fs::remove_dir_all(&d).ok();
    }

    /// The temp file is an implementation detail and must not outlive the save
    /// — including the save that fails.
    #[test]
    fn no_temp_file_is_left_behind() {
        let d = dir("tmp");
        let p = d.join("a.md");
        write_atomic(&p, &Rope::from_str("body\n"), &opts(true, false)).unwrap();

        let left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".swtmp"))
            .collect();
        assert!(left.is_empty(), "temp files left: {left:?}");

        // A destination that cannot be renamed over: the rename fails, and the
        // temp file must still be gone.
        let blocked = d.join("sub");
        std::fs::create_dir_all(&blocked).unwrap();
        assert!(
            write_atomic(&blocked, &Rope::from_str("x\n"), &opts(true, false)).is_err(),
            "a directory is not a file to overwrite"
        );
        let left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".swtmp"))
            .collect();
        assert!(left.is_empty(), "temp file survived a failed save: {left:?}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// The external-change guard is what `:w` consults before clobbering.
    #[test]
    fn an_external_write_is_noticed() {
        let d = dir("mtime");
        let p = d.join("a.md");
        let t = write_atomic(&p, &Rope::from_str("mine\n"), &opts(true, false)).unwrap();
        assert!(!changed_externally(&p, Some(t)));
        assert!(!changed_externally(&p, None), "nothing known, nothing to guard");

        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&p, "theirs\n").unwrap();
        assert!(changed_externally(&p, Some(t)));
        std::fs::remove_dir_all(&d).ok();
    }

    /// A symlink is written through: the file it names gets the text, and the
    /// link itself survives as a link.
    #[cfg(unix)]
    #[test]
    fn a_symlink_is_written_through_not_replaced() {
        let d = dir("link");
        let real = d.join("real.md");
        let link = d.join("link.md");
        std::fs::write(&real, "old\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomic(&link, &Rope::from_str("new\n"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new\n");
        assert!(link.is_symlink(), "the link is still a link");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A writable file in a folder the user cannot write to: no temp file can
    /// go beside it, but the file itself can still be written.
    #[cfg(unix)]
    #[test]
    fn a_file_in_a_read_only_folder_still_saves() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("rofolder");
        let p = d.join("a.md");
        std::fs::write(&p, "old\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Root writes anywhere, so the folder would not stop it and the test
        // would prove nothing.
        let root = std::fs::File::create(d.join("probe")).is_ok();
        let result = (!root).then(|| write_atomic(&p, &Rope::from_str("new\n"), &opts(true, false)));

        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        if let Some(result) = result {
            result.expect("saves in place");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// A rename would give one name a new inode and leave the other names on
    /// the old text.
    #[cfg(unix)]
    #[test]
    fn every_hard_link_sees_the_saved_text() {
        let d = dir("hardlink");
        let p = d.join("a.md");
        let other = d.join("b.md");
        // Longer than the new text, so the old tail has to be cut off.
        std::fs::write(&p, "old text, longer than the new\n").unwrap();
        std::fs::hard_link(&p, &other).unwrap();

        write_atomic(&p, &Rope::from_str("new\n"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "new\n", "the other name");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A hard-linked file the user may not write, in a folder they may: the
    /// rename still works there, so the save must not fail where it used to.
    #[cfg(unix)]
    #[test]
    fn a_read_only_hard_linked_file_still_saves_by_rename() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("rolink");
        let p = d.join("a.md");
        std::fs::write(&p, "old\n").unwrap();
        std::fs::hard_link(&p, d.join("b.md")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();

        write_atomic(&p, &Rope::from_str("new\n"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A name with no room left for `.` and `.swtmp` around it.
    #[cfg(unix)]
    #[test]
    fn a_name_near_the_length_limit_saves() {
        let d = dir("longname");
        let p = d.join(format!("{}.md", "n".repeat(250)));
        std::fs::write(&p, "old\n").unwrap();
        write_atomic(&p, &Rope::from_str("new\n"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        std::fs::remove_dir_all(&d).ok();
    }
}
