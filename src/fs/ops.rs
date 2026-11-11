//! Creating, deleting and moving paths — what the file tree's `a`/`d`/`r`/`m`
//! do once the user has answered the prompt.
//!
//! THE RULE HERE: never destroy something the user did not name. `std::fs`
//! will happily `rename` one file over another and take the target with it, so
//! every entry point below refuses a destination that already exists and says
//! so — except where that destination is the very entry being renamed, spelled
//! in another case (see `rename`). Deletion is the one operation that removes,
//! and it only ever removes the path it was handed.

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
/// not a file to overwrite. The one exception is a respelling — `notes.md` to
/// `Notes.md` on a filesystem that ignores case, where the new name "exists"
/// only because it IS the old one. Moving a directory into itself is caught
/// too — the OS reports it, but not in words worth showing.
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    if from == to {
        return Ok(());
    }
    if to.starts_with(from) {
        bail!("cannot move {} inside itself", name_of(from));
    }
    if to.exists() {
        if is_respelling(from, to) {
            return respell(from, to);
        }
        // `m docs/a.md` to `Docs/a.md` where case is ignored: nothing to move,
        // and "already exists" would read as if another file were in the way.
        if same_entry(from, to) && same_folder(from, to) && from.file_name() == to.file_name() {
            bail!("{} is already there", name_of(to));
        }
        bail!("{} already exists", name_of(to));
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
/// Every check guards against something the others let through:
/// - the same folder and a name that differs only in case: a hard link
///   `alias.md` to the same file must never count, or renaming `alias.md` to
///   `NOTES.md` would land on `notes.md` — a silent no-op that loses `alias.md`;
/// - the same file, without following links: a symlink and its target are two
///   entries, and renaming one over the other replaces the target;
/// - the exact new spelling missing from the folder's listing: a case-folding
///   filesystem lists the name as stored, a real second entry (a hard link on
///   a case-sensitive one) is listed under its own name.
///
/// Names that differ only in Unicode normalization (NFC vs NFD) do not count;
/// they stay refused, which is the safe side.
fn is_respelling(from: &Path, to: &Path) -> bool {
    same_folder(from, to)
        && same_name_ignoring_case(from, to)
        && same_entry(from, to)
        && !listed(to)
}

/// Whether two names differ, but only in case.
fn same_name_ignoring_case(a: &Path, b: &Path) -> bool {
    match (a.file_name().and_then(|s| s.to_str()), b.file_name().and_then(|s| s.to_str())) {
        (Some(a), Some(b)) => a != b && a.to_lowercase() == b.to_lowercase(),
        _ => false,
    }
}

/// Whether both paths sit in the same folder — compared as folders on disk,
/// since `docs` and `Docs` are one folder where case is ignored.
fn same_folder(a: &Path, b: &Path) -> bool {
    let (Some(a), Some(b)) = (a.parent(), b.parent()) else {
        return false;
    };
    same_dir(dot_if_empty(a), dot_if_empty(b))
}

/// The folder is where the entry lives, so following links here is right.
#[cfg(unix)]
fn same_dir(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether both paths are the same entry, never following a link: renaming a
/// link moves the link, so a link and the file it points at are two entries.
#[cfg(unix)]
fn same_entry(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Without inode numbers the resolved path stands in (Windows resolves a case
/// variant to the spelling on disk), but resolving follows links, so a link is
/// never the same entry as anything here.
#[cfg(not(unix))]
fn same_entry(a: &Path, b: &Path) -> bool {
    let is_link = |p: &Path| std::fs::symlink_metadata(p).map_or(true, |m| m.file_type().is_symlink());
    if is_link(a) || is_link(b) {
        return false;
    }
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
    // An unreadable folder answers "listed", which keeps the refusal.
    let Ok(read) = std::fs::read_dir(dot_if_empty(parent)) else {
        return true;
    };
    read.flatten().any(|e| e.file_name() == name)
}

fn dot_if_empty(dir: &Path) -> &Path {
    if dir.as_os_str().is_empty() { Path::new(".") } else { dir }
}

/// Give an entry a new spelling of its own name.
///
/// A direct `rename` does it on APFS and NTFS, in one atomic step — no moment
/// where the editor, which polls open files every second, finds neither name.
/// Some case-insensitive filesystems (FAT and exFAT drives, some network
/// shares) take it as a rename onto itself and change nothing; the listing
/// shows that, and then the rename goes through a third name instead.
fn respell(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to)
        .with_context(|| format!("moving {} to {}", name_of(from), to.display()))?;
    if listed(to) && !listed(from) {
        return Ok(());
    }
    respell_via_temp(from, to)
}

/// The two-step respelling: `from` to an unused name, then to `to`. If the
/// second step fails or changes nothing, the first is undone; if even that
/// fails, the error says where the entry now is.
fn respell_via_temp(from: &Path, to: &Path) -> Result<()> {
    let moving = || format!("moving {} to {}", name_of(from), to.display());
    // Short, and not built from the file's name: a long name plus a suffix
    // could pass the 255-byte limit on a name.
    let dir = from.parent().unwrap_or(Path::new(""));
    let tmp = (0u32..)
        .map(|n| dir.join(format!(".shoin-rename-{}-{n}", std::process::id())))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("an unused name");
    std::fs::rename(from, &tmp).with_context(moving)?;
    let second = std::fs::rename(&tmp, to);
    if second.is_ok() && listed(to) && !listed(&tmp) {
        return Ok(());
    }
    if let Err(undo) = std::fs::rename(&tmp, from) {
        bail!(
            "{}: stopped half-way, the entry is now {} ({undo})",
            moving(),
            tmp.display()
        );
    }
    match second {
        Err(e) => Err(e).with_context(moving),
        Ok(()) => bail!("{}: the filesystem kept the old name", moving()),
    }
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

    /// Only a change of case to the name, in the same folder, is a respelling:
    /// a hard link under another name (`alias.md` to `NOTES.md`) must never
    /// pass, or the rename would land on the other link and lose `alias.md`.
    #[test]
    fn a_respelling_is_the_same_name_in_the_same_folder_ignoring_case() {
        let same = |a: &str, b: &str| same_name_ignoring_case(Path::new(a), Path::new(b));
        assert!(same("notes.md", "Notes.md"));
        assert!(same("d/notes.md", "d/NOTES.MD"));
        assert!(same("Ärger.md", "ärger.md"));
        assert!(!same("notes.md", "notes.md"), "not a change at all");
        assert!(!same("alias.md", "NOTES.md"));
        assert!(!same("notes.md", "notes.markdown"));

        let d = tmp("folder");
        std::fs::create_dir(d.join("a")).unwrap();
        std::fs::create_dir(d.join("b")).unwrap();
        assert!(same_folder(&d.join("a/x.md"), &d.join("a/y.md")));
        assert!(!same_folder(&d.join("a/x.md"), &d.join("b/x.md")));
        // Compared as folders on disk, not as bytes.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.join("a"), d.join("link")).unwrap();
            assert!(same_folder(&d.join("a/x.md"), &d.join("link/x.md")));
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// The direct rename, tried first: the entry ends under the new spelling
    /// with its content, and nothing else is left in the folder.
    #[test]
    fn respell_renames_and_leaves_nothing() {
        let d = tmp("respell");
        std::fs::write(d.join("notes.md"), "body\n").unwrap();
        respell(&d.join("notes.md"), &d.join("Notes.md")).unwrap();
        assert_eq!(names_in(&d), ["Notes.md"]);
        assert_eq!(std::fs::read_to_string(d.join("Notes.md")).unwrap(), "body\n");

        std::fs::create_dir(d.join("docs")).unwrap();
        std::fs::write(d.join("docs/a.md"), "a\n").unwrap();
        respell(&d.join("docs"), &d.join("Docs")).unwrap();
        assert_eq!(names_in(&d), ["Docs", "Notes.md"]);
        assert_eq!(std::fs::read_to_string(d.join("Docs/a.md")).unwrap(), "a\n");

        std::fs::remove_dir_all(&d).ok();
    }

    /// The fallback through a third name, for filesystems where the direct
    /// rename changes nothing. It runs the same on any filesystem.
    #[test]
    fn respell_via_temp_goes_through_a_third_name_and_leaves_nothing() {
        let d = tmp("viatemp");
        std::fs::write(d.join("notes.md"), "body\n").unwrap();
        respell_via_temp(&d.join("notes.md"), &d.join("Notes.md")).unwrap();
        assert_eq!(names_in(&d), ["Notes.md"]);
        assert_eq!(std::fs::read_to_string(d.join("Notes.md")).unwrap(), "body\n");

        std::fs::create_dir(d.join("docs")).unwrap();
        std::fs::write(d.join("docs/a.md"), "a\n").unwrap();
        respell_via_temp(&d.join("docs"), &d.join("Docs")).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("Docs/a.md")).unwrap(), "a\n");

        // A failed second step puts the entry back under its old name.
        std::fs::write(d.join("keep.md"), "keep\n").unwrap();
        assert!(respell_via_temp(&d.join("keep.md"), &d.join("missing/Keep.md")).is_err());
        assert_eq!(names_in(&d), ["Docs", "Notes.md", "keep.md"]);

        std::fs::remove_dir_all(&d).ok();
    }

    /// A name near the 255-byte limit still respells: the temporary name is
    /// not built from it.
    #[test]
    fn respell_handles_a_name_at_the_length_limit() {
        let d = tmp("long");
        let lower = format!("{}.md", "a".repeat(250));
        let upper = format!("{}.md", "A".repeat(250));
        std::fs::write(d.join(&lower), "body\n").unwrap();
        respell_via_temp(&d.join(&lower), &d.join(&upper)).unwrap();
        assert_eq!(names_in(&d), [upper.as_str()]);
        respell(&d.join(&upper), &d.join(&lower)).unwrap();
        assert_eq!(names_in(&d), [lower]);
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

        // A hard link to Notes.md, renamed to a case variant of Notes.md: the
        // same file, but not the same name, so refused — and both names stay.
        std::fs::hard_link(d.join("Notes.md"), d.join("alias.md")).unwrap();
        let err = rename(&d.join("alias.md"), &d.join("NOTES.md")).unwrap_err().to_string();
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(names_in(&d), ["Docs", "Notes.md", "alias.md", "other.md"]);

        // Only the folder's case differs: nothing to move, and said so.
        let err = rename(&d.join("Docs/a.md"), &d.join("docs/a.md")).unwrap_err().to_string();
        assert!(err.contains("already there"), "got: {err}");
        assert_eq!(names_in(&d.join("Docs")), ["a.md"]);

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
