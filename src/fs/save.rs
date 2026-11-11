//! Saving files. SPEC.md §10.
//!
//! Atomic: write `.<file>.<pid>-<n>.swtmp` in the SAME directory (so `rename`
//! stays on one filesystem), fsync, then rename over the target. A crash
//! mid-save leaves the original intact. Original mode bits and group are
//! preserved.
//!
//! When a rename would do harm or cannot be done, the file is written in place
//! instead: a file with other hard links, one owned by someone else, or one in
//! a folder the user cannot write to. Where the folder allows it, a synced
//! backup sits beside the file while it is written.
//!
//! No swap files, and autosave is OFF unless `[editor] autosave` asks for it —
//! a writing tool should not surprise the user with writes they did not ask
//! for. The one exception is an editor that is dying: a crash, `SIGTERM` or
//! `SIGHUP` writes each modified buffer to a recovery file (`crate::recover`),
//! and only then. When it IS on it writes through this same `write_atomic`, guard
//! included, so a timer can never clobber what `:w` would have refused to.

use std::io::Write;
use std::path::{Path, PathBuf};
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
    /// The save lands mid-insert (an autosave): the line being typed keeps
    /// its trailing spaces when the cursor is in or after them.
    pub insert_mode: bool,
}

impl SavePolicy {
    pub fn from_config(cfg: &crate::config::schema::EditorConfig) -> SavePolicy {
        SavePolicy {
            trim_trailing_whitespace: cfg.trim_on_save,
            // An unrecognised spelling falls back to the default here; startup
            // has already flagged it through `config::validate`.
            final_newline: FinalNewline::parse(&cfg.final_newline).unwrap_or_default(),
            insert_mode: false,
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
pub fn outgoing(text: String, opts: &SaveOptions) -> Outgoing {
    let mut out = if opts.trim_trailing_whitespace {
        trim_lines(&text, opts)
    } else {
        Outgoing { text, ..Outgoing::default() }
    };

    if opts.final_newline {
        if !out.text.ends_with('\n') {
            out.text.push('\n');
            out.added_newline = true;
        }
    } else if out.text.ends_with('\n') {
        // One newline, not all of them: blank lines at the end are text the
        // writer put there. And none while the cursor sits on the empty line
        // after it, where Insert mode is about to type.
        let last_line = out.text.matches('\n').count();
        if opts.keep_line != Some(last_line) {
            out.text.pop();
            // The newline dropped is the one before the INPUT's last line,
            // which may have held spaces that trimming took (`foo\n  `): it
            // sits just before that line's start, in the input's positions.
            let start = match opts.trim_trailing_whitespace {
                true => out.last_start,
                false => out.text.chars().count() + 1,
            };
            match out.cuts.last_mut() {
                Some(c) if c.start == start => c.start -= 1,
                _ => out.cuts.push(start - 1..start),
            }
        }
    }
    out.cuts.reverse();
    out
}

/// What a save writes, and the edits that turn the text it was given into
/// that, so the buffer can take exactly the same ones.
#[derive(Debug, Default, PartialEq)]
pub struct Outgoing {
    /// The text, still with `\n` endings.
    pub text: String,
    /// Char ranges of the input that were dropped, last first, so each can be
    /// cut without moving the next.
    pub cuts: Vec<std::ops::Range<usize>>,
    /// Whether a newline was added at the end of the input.
    pub added_newline: bool,
    /// Where the input's last line starts, in chars (set by trimming).
    last_start: usize,
}

impl Outgoing {
    /// `text` written as it stands, with no edits.
    pub fn unchanged(text: String) -> Outgoing {
        Outgoing { text, ..Outgoing::default() }
    }
}

/// `trim_on_save`, minus what only looks like trailing whitespace: lines of
/// fenced code (literal, and a trailing space there may be the point), the
/// two spaces of a Markdown hard break, and the line being typed.
fn trim_lines(text: &str, opts: &SaveOptions) -> Outgoing {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut fences = crate::transclude::Fences::default();
    let mut out = Outgoing { text: String::with_capacity(text.len()), ..Outgoing::default() };
    let mut at = 0; // chars into the input
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.text.push('\n');
            at += 1;
        }
        out.last_start = at;
        let chars = line.chars().count();
        // Fences are Markdown's: in a plain file a stray ``` is just text,
        // and must not stop trimming for the rest of it.
        let literal = opts.markdown && fences.literal(line);
        let kept = if literal || opts.keep_line == Some(i) {
            line.len()
        } else if opts.markdown && hard_break(line, lines.get(i + 1).copied()) {
            // Two spaces are the break; any more are whitespace.
            line.trim_end().len() + 2
        } else {
            line.trim_end().len()
        };
        out.text.push_str(&line[..kept]);
        if kept < line.len() {
            let gone = line[kept..].chars().count();
            out.cuts.push(at + chars - gone..at + chars);
        }
        at += chars;
    }
    out
}

/// Two or more trailing spaces after text, with a line of text after it:
/// CommonMark's hard line break. Spaces only, so that keeping the first two
/// keeps exactly what makes the break.
fn hard_break(line: &str, next: Option<&str>) -> bool {
    let body = line.trim_end();
    let tail = &line[body.len()..];
    !body.is_empty()
        && tail.len() >= 2
        && tail.bytes().all(|b| b == b' ')
        && next.is_some_and(|n| !n.trim().is_empty())
}

/// Returns the new mtime, for external-modification tracking.
///
/// A symlink is written through to the file it names: renaming over the link
/// itself would swap it for a regular file and leave the real one unedited.
pub fn write_atomic(path: &Path, rope: &Rope, opts: &SaveOptions) -> Result<SystemTime> {
    let text = outgoing(rope.chunks().collect(), opts).text;
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

    // One look at the file answers every question below: whether it exists,
    // its links, owner, group and mode.
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file());

    // A rename gives the path a new inode. When other names share the old one
    // (hard links), only this name would see the new text: write into the
    // inode itself so they all do. A file the user may not write keeps the
    // rename, which the folder's permission allows: refusing a save that used
    // to work is no fix.
    if meta.as_ref().is_some_and(shares_its_inode) {
        if let Ok(written) = write_in_place(path, dir, &text, true) {
            return written;
        }
    }

    let (tmp, created) = match create_beside(path, dir, "swtmp") {
        Ok(made) => made,
        // A folder the user cannot write to may still hold a file they can (a
        // shared folder, group write on the file alone). That does not stop an
        // ordinary write, so do that rather than refuse the save. No backup
        // either: the folder that refused the temp file refuses a backup too.
        Err(e) if meta.is_some() && e.kind() == std::io::ErrorKind::PermissionDenied => {
            return write_in_place(path, dir, &text, false)
                .unwrap_or_else(|e| Err(e).with_context(|| format!("writing {}", path.display())));
        }
        Err(e) => {
            return Err(e)
                .with_context(|| format!("creating a temp file in {}", dir.display()));
        }
    };

    // A rename hands the file to whoever created the temp file: someone
    // else's file, writable here through its group, would become ours and lose
    // its extended attributes and ACLs on the way. A group can be handed back;
    // an owner cannot, without privileges, so that file is written in place.
    if let Some(m) = &meta {
        if !keeps_owner_and_group(&created, m) {
            drop(created);
            let _ = std::fs::remove_file(&tmp);
            if let Ok(written) = write_in_place(path, dir, &text, true) {
                return written;
            }
            // Not writable in place either: the rename the folder allows is
            // still better than refusing, as it was before.
            return write_atomic_in(path, dir, &text, meta.as_ref());
        }
    }

    finish_rename(path, dir, &tmp, created, &text, meta.as_ref())
}

/// The rename route from scratch, for the one case that gave up its first
/// temp file to look at the owner.
fn write_atomic_in(
    path: &Path,
    dir: &Path,
    text: &str,
    meta: Option<&std::fs::Metadata>,
) -> Result<SystemTime> {
    let (tmp, created) = create_beside(path, dir, "swtmp")
        .with_context(|| format!("creating a temp file in {}", dir.display()))?;
    finish_rename(path, dir, &tmp, created, text, meta)
}

/// Write the temp file, give it the original's mode, rename it over `path`.
fn finish_rename(
    path: &Path,
    dir: &Path,
    tmp: &Path,
    created: std::fs::File,
    text: &str,
    meta: Option<&std::fs::Metadata>,
) -> Result<SystemTime> {
    // Everything from here can fail, and a failure must not leave a temp file
    // sitting beside the file — a dotfile the editor hides and the reader
    // never asked for. One closure so there is a single place to clean up from.
    let attempt = || -> Result<SystemTime> {
        {
            let mut f = created;
            f.write_all(text.as_bytes())?;
            // Mode after any group change: a chown may clear set-id bits.
            if let Some(m) = meta {
                let _ = f.set_permissions(m.permissions());
            }
            f.sync_all()?;
        }

        std::fs::rename(tmp, path)
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
        let _ = std::fs::remove_file(tmp);
    }
    result
}

/// A new file beside `path`, named `.<name>.<pid>-<n>.<ext>`: unique, so a
/// leftover from another run or another user is never truncated or mistaken
/// for ours, and cut to fit the 255-byte name limit, so a long name can still
/// be saved by rename (and a NEW file with a long name created at all).
fn create_beside(path: &Path, dir: &Path, ext: &str) -> std::io::Result<(PathBuf, std::fs::File)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string());
    let pid = std::process::id();
    let mut tries = 0;
    loop {
        tries += 1;
        let n = N.fetch_add(1, Ordering::Relaxed);
        let suffix = format!(".{pid}-{n}.{ext}");
        let mut base = name.as_str();
        while 1 + base.len() + suffix.len() > NAME_MAX {
            let mut cut = base.len() - 1;
            while !base.is_char_boundary(cut) {
                cut -= 1;
            }
            base = &base[..cut];
        }
        let candidate = dir.join(format!(".{base}{suffix}"));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(f) => return Ok((candidate, f)),
            // Someone else's file under the same name: take the next number.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 100 => continue,
            Err(e) => return Err(e),
        }
    }
}

/// The longest file name most filesystems take, in bytes.
const NAME_MAX: usize = 255;

/// The fallback for a file a rename would damage or cannot reach: write over
/// the file itself. `Err` (outer) when the file cannot be opened for writing,
/// so the caller can choose the rename instead.
///
/// Not atomic, so where the folder allows it (`backup`) a copy of the original
/// is written and synced beside it first, and removed once the new text is
/// synced; a failed write leaves it, and the error names it. A folder the user
/// cannot write to allows no backup: there a crash mid-write can leave the
/// file half old, half new.
///
/// The old bytes are overwritten first and the length cut after, rather than
/// truncating up front: a full disk then fails while the original is still
/// mostly there, not after it has been emptied.
fn write_in_place(
    path: &Path,
    dir: &Path,
    text: &str,
    backup: bool,
) -> std::io::Result<Result<SystemTime>> {
    let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
    let kept = match backup {
        true => match back_up(path, dir) {
            Ok(b) => Some(b),
            // The folder refused it after all: the same case as above.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => None,
            Err(e) => {
                return Ok(Err(e).with_context(|| {
                    format!("backing up {} before writing it in place", path.display())
                }));
            }
        },
        false => None,
    };

    let written = fail_in_place_for_tests()
        .and_then(|_| f.write_all(text.as_bytes()))
        .and_then(|_| f.set_len(text.len() as u64))
        .and_then(|_| f.sync_all());
    Ok(match (written, kept) {
        (Ok(()), kept) => {
            if let Some(b) = kept {
                let _ = std::fs::remove_file(b);
            }
            Ok(mtime_of(path))
        }
        (Err(e), Some(b)) => Err(e).with_context(|| {
            format!("writing {} (the original is kept in {})", path.display(), b.display())
        }),
        (Err(e), None) => Err(e).with_context(|| format!("writing {}", path.display())),
    })
}

/// A synced copy of `path` beside it, for an in-place write to fall back on.
fn back_up(path: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let (bak, mut out) = create_beside(path, dir, "swbak")?;
    let copied = std::fs::File::open(path)
        .and_then(|mut src| std::io::copy(&mut src, &mut out))
        .and_then(|_| out.sync_all());
    match copied {
        Ok(()) => Ok(bak),
        Err(e) => {
            let _ = std::fs::remove_file(&bak);
            Err(e)
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Makes the next in-place write fail after its backup, for the test
    /// that the backup is left behind. No real error is easy to provoke there.
    static FAIL_IN_PLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn fail_in_place_for_tests() -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_IN_PLACE.with(|f| f.replace(false)) {
        return Err(std::io::Error::other("injected failure"));
    }
    Ok(())
}

fn mtime_of(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or_else(|_| SystemTime::now())
}

#[cfg(unix)]
fn shares_its_inode(m: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    m.nlink() > 1
}

#[cfg(not(unix))]
fn shares_its_inode(_: &std::fs::Metadata) -> bool {
    false
}

/// Whether the temp file, once renamed, would carry the original's owner and
/// group. A different group is handed over first; only a different owner (or
/// a group the user cannot give) means no.
#[cfg(unix)]
fn keeps_owner_and_group(tmp: &std::fs::File, original: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(ours) = tmp.metadata() else { return true };
    if ours.uid() != original.uid() {
        return false;
    }
    ours.gid() == original.gid()
        || std::os::unix::fs::fchown(tmp, None, Some(original.gid())).is_ok()
}

#[cfg(not(unix))]
fn keeps_owner_and_group(_: &std::fs::File, _: &std::fs::Metadata) -> bool {
    true
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

    /// `outgoing`'s text, after checking that its edits, replayed on the
    /// input as the buffer replays them, give that same text.
    fn out(text: &str, o: &SaveOptions) -> String {
        let got = outgoing(text.to_string(), o);
        let mut replayed: Vec<char> = text.chars().collect();
        if got.added_newline {
            replayed.push('\n');
        }
        let mut last = usize::MAX;
        for c in &got.cuts {
            assert!(c.end <= last, "cuts run last first: {:?}", got.cuts);
            last = c.start;
            replayed.drain(c.clone());
        }
        assert_eq!(replayed.into_iter().collect::<String>(), got.text, "edits for {text:?}");
        got.text
    }

    fn trimmed(text: &str, markdown: bool, keep_line: Option<usize>) -> String {
        out(text, &SaveOptions { markdown, keep_line, ..opts(true, true) })
    }

    /// The final newline dropped after trimming is the one before a last line
    /// that held only spaces, not the input's last character. The buffer once
    /// kept `foo\n ` while the file got `foo`.
    #[test]
    fn the_edits_follow_a_trimmed_last_line() {
        let never = SaveOptions { markdown: true, ..opts(false, true) };
        assert_eq!(out("foo\n  ", &never), "foo");
        assert_eq!(out("a\n\n ", &never), "a\n");
        assert_eq!(out("a \n\n\n", &never), "a\n\n");
        assert_eq!(out("a\n", &opts(false, false)), "a");
        assert_eq!(out("é\n", &opts(false, false)), "é");
        assert_eq!(out("a ", &opts(true, true)), "a\n");
        assert_eq!(out("", &opts(true, true)), "\n");
        assert_eq!(out("", &opts(false, true)), "");
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

    /// Two or more trailing spaces before a line of text are a hard line
    /// break in Markdown, kept as two. Anything else is whitespace.
    #[test]
    fn trimming_keeps_a_markdown_hard_break() {
        assert_eq!(trimmed("one  \ntwo\n", true, None), "one  \ntwo\n");
        assert_eq!(trimmed("one  \ntwo\n", false, None), "one\ntwo\n", "not in plain text");
        assert_eq!(trimmed("one     \ntwo\n", true, None), "one  \ntwo\n", "more is two");
        assert_eq!(trimmed("one\t  \ntwo\n", true, None), "one\ntwo\n", "spaces only");
        assert_eq!(trimmed("one \ntwo\n", true, None), "one\ntwo\n", "one is not a break");
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
        assert_eq!(out("a\n", &never), "a\n");
        let never = SaveOptions { keep_line: Some(0), ..opts(false, false) };
        assert_eq!(out("a\n", &never), "a");
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

    #[cfg(unix)]
    fn inode(p: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).unwrap().ino()
    }

    /// Root opens any file for writing and writes into any folder, so tests
    /// about what a user may not write prove nothing as root.
    #[cfg(unix)]
    fn running_as_root(d: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let probe = d.join("probe-owner");
        std::fs::write(&probe, "").unwrap();
        let root = std::fs::metadata(&probe).unwrap().uid() == 0;
        std::fs::remove_file(&probe).unwrap();
        root
    }

    fn leftovers(d: &Path, ext: &str) -> Vec<String> {
        std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(ext))
            .collect()
    }

    /// A hard-linked file the user may not write, in a folder they may: the
    /// rename still works there, so the save must not fail where it used to.
    #[cfg(unix)]
    #[test]
    fn a_read_only_hard_linked_file_still_saves_by_rename() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("rolink");
        if running_as_root(&d) {
            return;
        }
        let p = d.join("a.md");
        std::fs::write(&p, "old\n").unwrap();
        std::fs::hard_link(&p, d.join("b.md")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();
        let before = inode(&p);

        write_atomic(&p, &Rope::from_str("new\n"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        assert_ne!(inode(&p), before, "renamed, not written in place");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A name with no room left for `.` and `.swtmp` around it still saves by
    /// rename, and a NEW file with such a name can be created at all: the temp
    /// name is cut to fit.
    #[cfg(unix)]
    #[test]
    fn a_name_near_the_length_limit_saves_by_rename() {
        let d = dir("longname");
        let p = d.join(format!("{}.md", "n".repeat(250)));
        write_atomic(&p, &Rope::from_str("first\n"), &opts(true, false)).expect("a new file");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "first\n");

        let before = inode(&p);
        write_atomic(&p, &Rope::from_str("new\n"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        assert_ne!(inode(&p), before, "renamed, not written in place");

        // A name cut inside a multi-byte character must not split it.
        let p = d.join(format!("{}.md", "é".repeat(125)));
        write_atomic(&p, &Rope::from_str("x\n"), &opts(true, false)).expect("cut on a boundary");
        assert!(leftovers(&d, ".swtmp").is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    /// A temp file left by another run (or user) under the old fixed name is
    /// neither truncated nor in the way: the save is still a rename.
    #[cfg(unix)]
    #[test]
    fn a_stale_temp_file_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("stale");
        let p = d.join("a.md");
        std::fs::write(&p, "old\n").unwrap();
        let stale = d.join(".a.md.swtmp");
        std::fs::write(&stale, "someone else's\n").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o444)).unwrap();
        let before = inode(&p);

        write_atomic(&p, &Rope::from_str("new\n"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        assert_ne!(inode(&p), before, "renamed, not written in place");
        assert_eq!(std::fs::read_to_string(&stale).unwrap(), "someone else's\n");
        std::fs::remove_dir_all(&d).ok();
    }

    /// An in-place write that fails leaves the backup of the original, and
    /// the error says where it is. One that succeeds leaves nothing.
    #[cfg(unix)]
    #[test]
    fn a_failed_in_place_write_leaves_its_backup() {
        let d = dir("backup");
        let p = d.join("a.md");
        std::fs::write(&p, "original\n").unwrap();
        std::fs::hard_link(&p, d.join("b.md")).unwrap();

        FAIL_IN_PLACE.with(|f| f.set(true));
        let err = write_atomic(&p, &Rope::from_str("new\n"), &opts(true, false))
            .expect_err("the injected failure");
        let left = leftovers(&d, ".swbak");
        assert_eq!(left.len(), 1, "one backup: {left:?}");
        assert_eq!(std::fs::read_to_string(d.join(&left[0])).unwrap(), "original\n");
        assert!(format!("{err:#}").contains(&left[0]), "named in the error: {err:#}");

        std::fs::remove_file(d.join(&left[0])).unwrap();
        write_atomic(&p, &Rope::from_str("new\n"), &opts(true, false)).unwrap();
        assert!(leftovers(&d, ".swbak").is_empty(), "removed after a good write");
        assert!(leftovers(&d, ".swtmp").is_empty());
        std::fs::remove_dir_all(&d).ok();
    }
}
