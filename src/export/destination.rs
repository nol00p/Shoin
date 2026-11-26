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
}

impl Snapshot {
    fn inspect(path: &Path) -> Result<Self> {
        match fs::metadata(path) {
            Ok(meta) if meta.is_file() => Ok(Self::Existing(meta)),
            Ok(_) => bail!("{} is not a regular file", short(path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::Missing(missing_name(path)?)),
            Err(e) => Err(e).with_context(|| short(path)),
        }
    }
}

pub(super) struct Destination {
    path: PathBuf,
    original: Snapshot,
    // Preserve the identity seen before the editor can rename or replace a file.
    inputs: Vec<(PathBuf, Option<Metadata>)>,
    compiled: usize,
}

impl Destination {
    pub(super) fn new(path: PathBuf) -> Result<Self> {
        let original = Snapshot::inspect(&path)?;
        Ok(Self { path, original, inputs: Vec::new(), compiled: 0 })
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
        let mut file = match &self.original {
            Snapshot::Existing(original) => {
                let file = match options.open(&self.path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(self.changed()),
                    result => result.with_context(|| short(&self.path))?,
                };
                self.revalidate()?;
                let opened = file.metadata().with_context(|| short(&self.path))?;
                if !opened.is_file() || !same_version(original, &opened) {
                    return Err(self.changed());
                }
                self.check(&Snapshot::Existing(opened))?;
                file.set_len(0).with_context(|| short(&self.path))?;
                file
            }
            Snapshot::Missing(target) => {
                self.revalidate()?;
                match options.create_new(true).open(target) {
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(self.changed()),
                    result => result.with_context(|| short(&self.path))?,
                }
            }
        };
        write(&mut file).with_context(|| short(&self.path))
    }

    fn revalidate(&self) -> Result<()> {
        let current = Snapshot::inspect(&self.path)?;
        self.check(&current)?;
        match (&self.original, &current) {
            (Snapshot::Missing(before), Snapshot::Missing(now)) if before == now => {}
            (Snapshot::Existing(before), Snapshot::Existing(now)) if same_version(before, now) => {}
            _ => return Err(self.changed()),
        }
        Ok(())
    }

    fn changed(&self) -> anyhow::Error {
        anyhow::anyhow!("{} changed while exporting", short(&self.path))
    }
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
