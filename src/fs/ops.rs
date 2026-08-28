//! Creating, deleting and moving paths — what the file tree's `a`/`d`/`r`/`m`
//! do once the user has answered the prompt.
//!
//! THE RULE HERE: never destroy something the user did not name. `std::fs`
//! will happily `rename` one file over another and take the target with it, so
//! every entry point below refuses a destination that already exists and says
//! so. Deletion is the one operation that removes, and it only ever removes the
//! path it was handed.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// Create an empty file or a directory, plus any missing parents.
///
/// Refuses an existing path rather than truncating it — `a` is for making
/// something new, and a name collision is a question, not a decision.
pub fn create(path: &Path, dir: bool) -> Result<()> {
    if path.exists() {
        bail!("{} already exists", name_of(path));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if dir {
        std::fs::create_dir(path).with_context(|| format!("creating {}", path.display()))?;
    } else {
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    }
    Ok(())
}

/// Delete a file, or a directory and everything under it.
///
/// The recursion is why the caller must confirm first, and why the confirmation
/// says how many entries are going: `remove_dir_all` on the wrong row is not
/// something an editor can undo.
pub fn remove(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("{} is gone", name_of(path)))?;
    // A symlink to a directory is removed as a LINK, never followed — deleting
    // through one would reach outside the tree entirely.
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .with_context(|| format!("deleting {}", path.display()))
}

/// Rename or move, creating the destination's parents.
///
/// Same refusal as `create`: an existing destination is a collision to report,
/// not a file to overwrite. Moving a directory into itself is caught too — the
/// OS reports it, but not in words worth showing.
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    if from == to {
        return Ok(());
    }
    if to.exists() {
        bail!("{} already exists", name_of(to));
    }
    if to.starts_with(from) {
        bail!("cannot move {} inside itself", name_of(from));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::rename(from, to)
        .with_context(|| format!("moving {} to {}", name_of(from), to.display()))
}

/// How many entries `remove` would take, for the confirmation. Capped, because
/// the number stops meaning anything past a point and walking a huge tree to
/// find it out is worse than saying "many".
pub fn count_entries(path: &Path, cap: usize) -> usize {
    fn walk(path: &Path, cap: usize, n: &mut usize) {
        if *n >= cap {
            return;
        }
        *n += 1;
        let Ok(read) = std::fs::read_dir(path) else {
            return;
        };
        for e in read.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&e.path(), cap, n);
            } else {
                *n += 1;
            }
            if *n >= cap {
                return;
            }
        }
    }
    let mut n = 0;
    walk(path, cap, &mut n);
    n.min(cap)
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("shoin-ops-{tag}-{t}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn create_makes_files_dirs_and_missing_parents() {
        let d = tmp("create");
        create(&d.join("a.md"), false).unwrap();
        assert!(d.join("a.md").is_file());

        create(&d.join("sub"), true).unwrap();
        assert!(d.join("sub").is_dir());

        // Parents are made on the way.
        create(&d.join("deep/deeper/note.md"), false).unwrap();
        assert!(d.join("deep/deeper/note.md").is_file());

        std::fs::remove_dir_all(&d).ok();
    }

    /// A collision is reported, never written through — `a` makes new things.
    #[test]
    fn create_refuses_to_clobber() {
        let d = tmp("clobber");
        std::fs::write(d.join("a.md"), "keep me\n").unwrap();
        let err = create(&d.join("a.md"), false).unwrap_err().to_string();
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(std::fs::read_to_string(d.join("a.md")).unwrap(), "keep me\n");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rename_moves_and_refuses_a_taken_name() {
        let d = tmp("rename");
        std::fs::write(d.join("a.md"), "body\n").unwrap();
        std::fs::write(d.join("b.md"), "other\n").unwrap();

        let err = rename(&d.join("a.md"), &d.join("b.md")).unwrap_err().to_string();
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(std::fs::read_to_string(d.join("b.md")).unwrap(), "other\n");

        // Into a directory that does not exist yet.
        rename(&d.join("a.md"), &d.join("sub/moved.md")).unwrap();
        assert!(!d.join("a.md").exists());
        assert_eq!(std::fs::read_to_string(d.join("sub/moved.md")).unwrap(), "body\n");

        // A directory cannot swallow itself.
        let err = rename(&d.join("sub"), &d.join("sub/inner")).unwrap_err().to_string();
        assert!(err.contains("inside itself"), "got: {err}");

        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn remove_takes_files_and_whole_directories() {
        let d = tmp("remove");
        std::fs::create_dir_all(d.join("sub/inner")).unwrap();
        std::fs::write(d.join("sub/one.md"), "").unwrap();
        std::fs::write(d.join("sub/inner/two.md"), "").unwrap();

        assert_eq!(count_entries(&d.join("sub"), 100), 4, "dir + 2 files + inner");
        remove(&d.join("sub")).unwrap();
        assert!(!d.join("sub").exists());

        std::fs::write(d.join("solo.md"), "").unwrap();
        remove(&d.join("solo.md")).unwrap();
        assert!(!d.join("solo.md").exists());

        std::fs::remove_dir_all(&d).ok();
    }
}
