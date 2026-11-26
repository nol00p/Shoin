//! Protect the inputs when writing a completed export.

use std::fs::{self, File, Metadata, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock, RwLockReadGuard};

use anyhow::{bail, Context, Result};

use super::short;
use crate::transclude::same_file;

static STOPPING: AtomicBool = AtomicBool::new(false);
static WORKERS: RwLock<()> = RwLock::new(());
// This lock also orders commits against the start of shutdown.
static COMMIT: Mutex<()> = Mutex::new(());

pub(super) fn cancelled() -> bool {
    STOPPING.load(Ordering::Acquire)
}

pub(super) fn start() -> Result<RwLockReadGuard<'static, ()>> {
    let worker = WORKERS.read().unwrap();
    if cancelled() {
        bail!("export cancelled");
    }
    Ok(worker)
}

pub(super) fn stop() {
    let _commit = COMMIT.lock().unwrap();
    STOPPING.store(true, Ordering::Release);
}

pub(super) fn drain() {
    drop(WORKERS.write().unwrap());
}

enum Snapshot {
    Existing(Metadata),
    Missing(PathBuf),
    /// A character device (`/dev/null`, a tty), or a pipe with the same
    /// identity as Shoin's stdout or stderr reached by another name (such as
    /// `/dev/fd/3` duplicated from it): written as it is, never truncated.
    Device(Metadata),
    /// A name for Shoin's own stdout (1) or stderr (2), such as
    /// `/dev/stdout`: written through that descriptor, never reopened.
    Stream(i32, Metadata),
}

impl Snapshot {
    fn inspect(path: &Path, shown: &str) -> Result<Self> {
        if let Some(fd) = own_stream(path) {
            let meta = stream(fd).and_then(|file| file.metadata()).with_context(|| shown.to_owned())?;
            return Ok(Self::Stream(fd, meta));
        }
        match fs::metadata(path) {
            Ok(meta) if meta.is_file() => Ok(Self::Existing(meta)),
            Ok(meta) if is_device(&meta) => Ok(Self::Device(meta)),
            Ok(meta) => bail!("{shown} is {}", kind(&meta)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::Missing(missing_name(path)?)),
            Err(e) => Err(e).with_context(|| shown.to_owned()),
        }
    }
}

/// Whether an export may write to `meta` without truncating it. A named
/// pipe with no reader would block the export, so a FIFO only qualifies when
/// it is the pipe Shoin's own stdout or stderr already writes to.
fn is_device(meta: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let kind = meta.file_type();
        kind.is_char_device() || (kind.is_fifo() && own_output(meta))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// The descriptor (1 or 2) that `path` names, if it names Shoin's own stdout
/// or stderr: `/dev/stdout`, `/dev/stderr`, `/dev/fd/1`, `/dev/fd/2`,
/// `/proc/self/fd/1`, `/proc/self/fd/2`, or a chain of symlinks that reaches
/// one of those names. The match is by name, so it does not depend on what
/// stat reports through these special files (on macOS, fdesc reports its own
/// device for `/dev/fd/1`, not the pipe's).
fn own_stream(path: &Path) -> Option<i32> {
    if !cfg!(unix) {
        return None;
    }
    let mut path = std::path::absolute(path).ok()?;
    // Bounded like the kernel's own symlink limit, so a loop ends.
    for _ in 0..40 {
        let name = path.to_str().unwrap_or("");
        let fd = match name {
            "/dev/stdout" | "/dev/fd/1" | "/proc/self/fd/1" => Some(1),
            "/dev/stderr" | "/dev/fd/2" | "/proc/self/fd/2" => Some(2),
            _ => None,
        };
        if fd.is_some() {
            return fd;
        }
        let link = fs::read_link(&path).ok()?;
        path = std::path::absolute(path.parent().unwrap_or(Path::new("/")).join(link)).ok()?;
    }
    None
}

/// A duplicate of descriptor `fd` (1 or 2) to write to.
fn stream(fd: i32) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        let owned = if fd == 1 { std::io::stdout().as_fd().try_clone_to_owned() } else { std::io::stderr().as_fd().try_clone_to_owned() };
        owned.map(File::from)
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

#[cfg(unix)]
fn own_output(meta: &Metadata) -> bool {
    use std::os::fd::AsFd;
    let same = |fd: std::os::fd::BorrowedFd| {
        fd.try_clone_to_owned()
            .ok()
            .and_then(|fd| File::from(fd).metadata().ok())
            .is_some_and(|own| same_inode(&own, meta))
    };
    same(std::io::stdout().as_fd()) || same(std::io::stderr().as_fd())
}

/// What a refused destination is, for the message.
fn kind(meta: &Metadata) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let kind = meta.file_type();
        if kind.is_fifo() {
            return "a named pipe";
        }
        if kind.is_socket() {
            return "a socket";
        }
        if kind.is_block_device() {
            return "a block device";
        }
    }
    if meta.is_dir() { "a directory" } else { "not a regular file" }
}

pub(super) struct Destination {
    path: PathBuf,
    /// The destination as the user typed it, for messages.
    shown: String,
    original: Snapshot,
    // Preserve the identity seen before the editor can rename or replace a file.
    inputs: Vec<(PathBuf, Option<Metadata>)>,
    compiled: usize,
}

impl Destination {
    pub(super) fn new(path: PathBuf, shown: String) -> Result<Self> {
        let original = Snapshot::inspect(&path, &shown)?;
        Ok(Self { path, shown, original, inputs: Vec::new(), compiled: 0 })
    }

    pub(super) fn with_inputs(mut self, mut sources: Vec<PathBuf>, pictures: Vec<PathBuf>) -> Result<Self> {
        self.compiled = sources.len();
        sources.extend(pictures);
        self.inputs = sources.into_iter().map(|source| {
            let meta = fs::metadata(&source).ok();
            (source, meta)
        }).collect();
        self.check(&self.original)?;
        Ok(self)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn check(&self, snapshot: &Snapshot) -> Result<()> {
        let (current, missing) = match snapshot {
            Snapshot::Existing(meta) => (Some(meta), None),
            Snapshot::Missing(name) => (None, Some(name)),
            Snapshot::Stream(_, meta) if meta.is_file() => (Some(meta), None),
            Snapshot::Device(_) | Snapshot::Stream(..) => return Ok(()),
        };
        for (index, (source, original)) in self.inputs.iter().enumerate() {
            if current.zip(original.as_ref()).is_some_and(|(a, b)| same_inode(a, b))
                || current.zip(fs::metadata(source).ok().as_ref()).is_some_and(|(a, b)| same_inode(a, b))
                || same_file(&self.path, source)
                || missing.is_some_and(|name| missing_name(source).as_ref().ok() == Some(name))
            {
                match index {
                    0 => bail!("that would overwrite the document you are exporting"),
                    i if i < self.compiled => bail!("that would overwrite {}, which the document embeds", short(source)),
                    _ => bail!("that would overwrite {}, which the document shows", short(source)),
                }
            }
        }
        Ok(())
    }

    pub(super) fn commit(&self, write: impl FnOnce(&mut File) -> std::io::Result<()>) -> Result<()> {
        let _commit = COMMIT.lock().unwrap();
        if cancelled() || crate::recover::stopping() {
            bail!("export cancelled");
        }
        self.revalidate()?;
        if let Snapshot::Stream(fd, original) = &self.original {
            // Write where the shell pointed the stream (a pipe, a tty, or a
            // file opened with `>` or `>>`), without truncating it.
            let mut file = stream(*fd).with_context(|| self.shown.clone())?;
            let opened = file.metadata().with_context(|| self.shown.clone())?;
            if !same_inode(original, &opened) {
                return Err(self.changed());
            }
            self.check(&Snapshot::Stream(*fd, opened))?;
            return write(&mut file).with_context(|| self.shown.clone());
        }
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", short(parent)))?;
        }
        // Open an existing file without creating or truncating it. For an
        // absent file, resolve dangling links explicitly so exclusive creation
        // still works through them.
        let mut options = OpenOptions::new();
        options.write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Avoid blocking on a FIFO swapped in after the path check.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let shown = || self.shown.clone();
        let mut file = match &self.original {
            Snapshot::Existing(original) => {
                let file = match options.open(&self.path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(self.changed()),
                    result => result.with_context(shown)?,
                };
                self.revalidate()?;
                let opened = file.metadata().with_context(shown)?;
                if !opened.is_file() || !same_version(original, &opened) {
                    return Err(self.changed());
                }
                self.check(&Snapshot::Existing(opened))?;
                file.set_len(0).with_context(shown)?;
                file
            }
            Snapshot::Missing(target) => {
                self.revalidate()?;
                match options.create_new(true).open(target) {
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(self.changed()),
                    result => result.with_context(shown)?,
                }
            }
            Snapshot::Stream(..) => unreachable!("written above"),
            Snapshot::Device(original) => {
                let file = options.open(&self.path).with_context(shown)?;
                let opened = file.metadata().with_context(shown)?;
                if !same_inode(original, &opened) || !is_device(&opened) {
                    return Err(self.changed());
                }
                blocking(&file).with_context(shown)?;
                file
            }
        };
        write(&mut file).with_context(shown)
    }

    fn revalidate(&self) -> Result<()> {
        let current = Snapshot::inspect(&self.path, &self.shown)?;
        self.check(&current)?;
        match (&self.original, &current) {
            (Snapshot::Missing(before), Snapshot::Missing(now)) if before == now => {}
            (Snapshot::Existing(before), Snapshot::Existing(now)) if same_version(before, now) => {}
            (Snapshot::Device(before), Snapshot::Device(now)) if same_inode(before, now) => {}
            (Snapshot::Stream(a, before), Snapshot::Stream(b, now)) if a == b && same_inode(before, now) => {}
            _ => return Err(self.changed()),
        }
        Ok(())
    }

    fn changed(&self) -> anyhow::Error {
        anyhow::anyhow!("{} changed while exporting", self.shown)
    }
}

/// Clear the `O_NONBLOCK` the destination was opened with, so a write to a
/// tty or a pipe waits for room instead of failing part-way.
fn blocking(file: &File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = file.as_raw_fd();
        // SAFETY: plain fcntl calls on a descriptor this function borrows.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn same_version(before: &Metadata, now: &Metadata) -> bool {
    #[cfg(unix)]
    if !same_inode(before, now) {
        return false;
    }
    // Like Neovim's changed-file check, this accepts same-size edits with an unchanged mtime.
    before.len() == now.len() && before.modified().ok() == now.modified().ok()
}

/// Resolve the links and canonicalize the existing portion of a missing path.
fn missing_name(path: &Path) -> Result<PathBuf> {
    let target = std::path::absolute(crate::fs::save::through_links(path)?)?;
    for parent in target.ancestors().skip(1) {
        if let Ok(dir) = fs::canonicalize(parent) {
            return Ok(dir.join(target.strip_prefix(parent).unwrap()));
        }
    }
    Ok(target)
}

fn same_inode(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn names_of_our_own_streams_are_recognised_without_stat() {
        for (name, fd) in [
            ("/dev/stdout", Some(1)), ("/dev/fd/1", Some(1)), ("/proc/self/fd/1", Some(1)),
            ("/dev/stderr", Some(2)), ("/dev/fd/2", Some(2)), ("/proc/self/fd/2", Some(2)),
            ("/dev/fd/0", None), ("/dev/fd/3", None), ("/dev/stdoutx", None), ("/dev/null", None),
        ] {
            assert_eq!(own_stream(Path::new(name)), fd, "{name}");
        }
        let dir = std::env::temp_dir().join(format!("shoin-stream-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        std::os::unix::fs::symlink("/dev/stdout", dir.join("out")).unwrap();
        std::os::unix::fs::symlink("out", dir.join("again")).unwrap();
        std::os::unix::fs::symlink("/dev/fd/2", dir.join("err")).unwrap();
        std::os::unix::fs::symlink("loop", dir.join("loop")).unwrap();
        fs::write(dir.join("plain"), "").unwrap();
        assert_eq!(own_stream(&dir.join("out")), Some(1));
        assert_eq!(own_stream(&dir.join("again")), Some(1));
        assert_eq!(own_stream(&dir.join("err")), Some(2));
        assert_eq!(own_stream(&dir.join("loop")), None);
        assert_eq!(own_stream(&dir.join("plain")), None);
        assert_eq!(own_stream(&dir.join("absent")), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
