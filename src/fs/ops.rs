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
        // On a filesystem that ignores case (macOS by default, Windows)
        // `Notes.md` "exists" as soon as `notes.md` does. That is not a
        // collision: it is the same file asking for a new spelling.
        if is_respelling(from, to) {
            return respell(from, to);
        }
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

/// Whether `to` is `from` under another spelling — `Notes.md` for `notes.md`
/// where case is ignored — rather than a second entry.
///
/// Two checks, because each alone is fooled. A hard link (or a path through a
/// symlinked folder) shows the same file identity too, and renaming onto a
/// hard link of yourself is a silent no-op that keeps both names. So the exact
/// spelling of `to` must also be missing from its folder's listing: a
/// case-folding filesystem lists the name as stored, while a real second entry
/// is listed under its own name.
fn is_respelling(from: &Path, to: &Path) -> bool {
    same_entry(from, to) && !listed(to)
}

/// Whether both paths reach the same file.
#[cfg(unix)]
fn same_entry(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // Not following links: renaming a link moves the link, so a link and the
    // file it points at are two entries.
    match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Without inode numbers the resolved path stands in: Windows resolves a case
/// variant to the spelling on disk.
#[cfg(not(unix))]
fn same_entry(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether `path`'s folder lists an entry spelled exactly like `path`'s name.
fn listed(path: &Path) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return true;
    };
    let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
    // An unreadable folder answers "listed", which keeps the refusal.
    let Ok(read) = std::fs::read_dir(parent) else {
        return true;
    };
    read.flatten().any(|e| e.file_name() == name)
}

/// Give an entry a new spelling of its own name, by way of a third name.
///
/// A direct `rename` does it on APFS and NTFS, but some case-insensitive
/// filesystems (FAT and exFAT drives, some network shares) treat it as a
/// rename onto itself and change nothing. Two renames through a name that
/// cannot clash work on all of them. If the second step fails the first is
/// undone, so the entry is never left under the temporary name.
fn respell(from: &Path, to: &Path) -> Result<()> {
    let dir = from.parent().unwrap_or(Path::new(""));
    let base = name_of(from);
    let tmp = (0u32..)
        .map(|n| dir.join(format!(".{base}.shoin-rename-{}-{n}", std::process::id())))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("an unused name");
    let moving = || format!("moving {} to {}", name_of(from), to.display());
    std::fs::rename(from, &tmp).with_context(moving)?;
    if let Err(e) = std::fs::rename(&tmp, to) {
        let _ = std::fs::rename(&tmp, from);
        return Err(e).with_context(moving);
    }
    Ok(())
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

    fn names_in(d: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The parts of the "same file, other spelling" decision any filesystem can
    /// show: a hard link has the same identity but is listed under its own
    /// name, so it is a second entry, not a respelling — and renaming onto it
    /// would be a silent no-op. On a case-sensitive filesystem the link could
    /// be `Notes.md` itself; `alias.md` makes the same point on both kinds.
    #[test]
    fn a_hard_link_is_not_a_respelling() {
        let d = tmp("hardlink");
        std::fs::write(d.join("notes.md"), "body\n").unwrap();
        std::fs::hard_link(d.join("notes.md"), d.join("alias.md")).unwrap();
        std::fs::write(d.join("other.md"), "other\n").unwrap();

        assert!(same_entry(&d.join("notes.md"), &d.join("alias.md")));
        assert!(!same_entry(&d.join("notes.md"), &d.join("other.md")));
        assert!(!same_entry(&d.join("notes.md"), &d.join("missing.md")));
        assert!(listed(&d.join("alias.md")));
        assert!(!listed(&d.join("NOTES.md")));
        assert!(!is_respelling(&d.join("notes.md"), &d.join("alias.md")));

        let err = rename(&d.join("notes.md"), &d.join("alias.md")).unwrap_err().to_string();
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(names_in(&d), ["alias.md", "notes.md", "other.md"]);

        std::fs::remove_dir_all(&d).ok();
    }

    /// On a case-sensitive filesystem `Notes.md` next to `notes.md` is another
    /// file, and stays a collision.
    #[test]
    fn a_case_twin_that_is_another_file_is_refused() {
        let d = tmp("twin");
        std::fs::write(d.join("notes.md"), "lower\n").unwrap();
        if d.join("Notes.md").exists() {
            // Case-insensitive temp dir (macOS): the twin cannot exist.
            std::fs::remove_dir_all(&d).ok();
            return;
        }
        std::fs::write(d.join("Notes.md"), "upper\n").unwrap();
        assert!(!is_respelling(&d.join("notes.md"), &d.join("Notes.md")));
        let err = rename(&d.join("notes.md"), &d.join("Notes.md")).unwrap_err().to_string();
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(std::fs::read_to_string(d.join("Notes.md")).unwrap(), "upper\n");
        std::fs::remove_dir_all(&d).ok();
    }

    /// The two-step move runs the same on any filesystem: the entry ends under
    /// the new spelling with its content, and no temporary name is left.
    #[test]
    fn respell_goes_through_a_third_name_and_leaves_nothing() {
        let d = tmp("respell");
        std::fs::write(d.join("notes.md"), "body\n").unwrap();
        respell(&d.join("notes.md"), &d.join("Notes.md")).unwrap();
        assert_eq!(names_in(&d), ["Notes.md"]);
        assert_eq!(std::fs::read_to_string(d.join("Notes.md")).unwrap(), "body\n");

        // A folder too, with what is inside it.
        std::fs::create_dir(d.join("docs")).unwrap();
        std::fs::write(d.join("docs/a.md"), "a\n").unwrap();
        respell(&d.join("docs"), &d.join("Docs")).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("Docs/a.md")).unwrap(), "a\n");

        // A failed second step puts the entry back under its old name.
        std::fs::write(d.join("keep.md"), "keep\n").unwrap();
        assert!(respell(&d.join("keep.md"), &d.join("missing/Keep.md")).is_err());
        assert_eq!(names_in(&d), ["Docs", "Notes.md", "keep.md"]);

        std::fs::remove_dir_all(&d).ok();
    }

    /// The bug where it lives: on macOS's default filesystem `Notes.md`
    /// "exists" as soon as `notes.md` does, and the rename was refused.
    #[cfg(target_os = "macos")]
    #[test]
    fn case_only_rename_works_on_a_case_insensitive_filesystem() {
        let d = tmp("caseonly");
        std::fs::write(d.join("notes.md"), "body\n").unwrap();
        assert!(d.join("Notes.md").exists(), "expected a case-insensitive temp dir");
        assert!(is_respelling(&d.join("notes.md"), &d.join("Notes.md")));

        rename(&d.join("notes.md"), &d.join("Notes.md")).unwrap();
        assert_eq!(names_in(&d), ["Notes.md"]);
        assert_eq!(std::fs::read_to_string(d.join("Notes.md")).unwrap(), "body\n");

        // A folder, through the public entry point.
        std::fs::create_dir(d.join("docs")).unwrap();
        std::fs::write(d.join("docs/a.md"), "a\n").unwrap();
        rename(&d.join("docs"), &d.join("Docs")).unwrap();
        assert_eq!(names_in(&d), ["Docs", "Notes.md"]);

        // Another file under another case is still a collision.
        std::fs::write(d.join("other.md"), "other\n").unwrap();
        let err = rename(&d.join("Notes.md"), &d.join("OTHER.md")).unwrap_err().to_string();
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(std::fs::read_to_string(d.join("other.md")).unwrap(), "other\n");

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
