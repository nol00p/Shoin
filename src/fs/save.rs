//! Saving files. SPEC.md §10.
//!
//! Atomic: write `.<file>.swtmp` in the SAME directory (so `rename` stays on
//! one filesystem), fsync, then rename over the target. A crash mid-save leaves
//! the original intact. Original mode bits are preserved.
//!
//! No swap files, no autosave in v1. Explicit `:w` only — a writing tool should
//! not surprise the user with writes they did not ask for.

use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};
use ropey::Rope;

use crate::text::buffer::LineEnding;

pub struct SaveOptions {
    pub line_ending: LineEnding,
    pub final_newline: bool,
    pub trim_trailing_whitespace: bool,
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

/// Returns the new mtime, for external-modification tracking.
pub fn write_atomic(path: &Path, rope: &Rope, opts: &SaveOptions) -> Result<SystemTime> {
    let mut text: String = rope.chunks().collect();

    if opts.trim_trailing_whitespace {
        let mut out = String::with_capacity(text.len());
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(line.trim_end());
        }
        text = out;
    }

    if opts.final_newline {
        if !text.ends_with('\n') {
            text.push('\n');
        }
    } else {
        while text.ends_with('\n') {
            text.pop();
        }
    }

    if opts.line_ending == LineEnding::Crlf {
        text = text.replace('\n', "\r\n");
    }

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string());
    let tmp = dir.join(format!(".{stem}.swtmp"));

    // Everything from here can fail, and a failure must not leave `.a.md.swtmp`
    // sitting beside the file — a dotfile the editor hides and the reader never
    // asked for. One closure so there is a single place to clean up from.
    let attempt = || -> Result<SystemTime> {
        {
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("creating temp file {}", tmp.display()))?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }

        // Preserve the original's mode bits if it already existed.
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }

        std::fs::rename(&tmp, path)
            .with_context(|| format!("replacing {} with {}", path.display(), tmp.display()))?;

        Ok(std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or_else(|_| SystemTime::now()))
    };

    let result = attempt();
    if result.is_err() {
        // Best effort: if this fails too there is nothing further to try, and
        // the error worth reporting is the one from the save itself.
        let _ = std::fs::remove_file(&tmp);
    }
    result
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

        write_atomic(&p, &Rope::from_str("one\ntwo\n\n\n"), &opts(false, false)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "one\ntwo",
            "every trailing newline goes, not just the last"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn trailing_whitespace_goes_only_when_asked() {
        let d = dir("trim");
        let p = d.join("a.md");

        write_atomic(&p, &Rope::from_str("a  \nb\t\n"), &opts(true, false)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a  \nb\t\n");

        write_atomic(&p, &Rope::from_str("a  \nb\t\n"), &opts(true, true)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\nb\n");
        std::fs::remove_dir_all(&d).ok();
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
}
