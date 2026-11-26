//! Protect the inputs and publish a completed export by rename.

use std::fs::{self, File, Metadata};
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

pub(super) struct Destination {
    path: PathBuf,
    // Preserve the identity seen before the editor can rename or replace a file.
    original: Option<Metadata>,
    inputs: Vec<(PathBuf, Option<Metadata>)>,
    compiled: usize,
}

impl Destination {
    pub(super) fn new(path: PathBuf, mut sources: Vec<PathBuf>, pictures: Vec<PathBuf>) -> Result<Self> {
        let compiled = sources.len();
        sources.extend(pictures);
        let inputs = sources.into_iter().map(|source| {
            let meta = fs::metadata(&source).ok();
            (source, meta)
        }).collect();
        let original = inspect(&path)?;
        let dest = Self { path, original, inputs, compiled };
        dest.check(dest.original.as_ref())?;
        Ok(dest)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn check(&self, current: Option<&Metadata>) -> Result<()> {
        for (index, (source, original)) in self.inputs.iter().enumerate() {
            if current.zip(original.as_ref()).is_some_and(|(a, b)| same_inode(a, b))
                || same_file(&self.path, source)
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
        let current = self.revalidate()?;
        // Retain an existing symlink and atomically replace its target.
        let target = if current.is_some() { fs::canonicalize(&self.path)? } else { self.path.clone() };
        let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(parent).with_context(|| format!("creating {}", short(parent)))?;
        let (path, mut file) = crate::fs::save::create_beside(&target, parent, "tmp")
            .with_context(|| short(&self.path))?;
        let staged = StagedFile(path);
        if let Some(meta) = current {
            file.set_permissions(meta.permissions()).with_context(|| short(&self.path))?;
        }
        write(&mut file).with_context(|| short(&self.path))?;
        drop(file);
        self.revalidate()?;
        if crate::recover::stopping() {
            bail!("export cancelled");
        }
        fs::rename(&staged.0, target).with_context(|| short(&self.path))
    }

    fn revalidate(&self) -> Result<Option<Metadata>> {
        let current = inspect(&self.path)?;
        self.check(current.as_ref())?;
        if match (&self.original, &current) {
            (None, None) => false,
            (Some(before), Some(now)) => changed(before, now),
            _ => true,
        } {
            bail!("{} changed while exporting", short(&self.path));
        }
        Ok(current)
    }
}

fn inspect(path: &Path) -> Result<Option<Metadata>> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(meta)),
        Ok(_) => bail!("{} is not a regular file", short(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| short(path)),
    }
}

fn changed(before: &Metadata, now: &Metadata) -> bool {
    #[cfg(unix)]
    if !same_inode(before, now) {
        return true;
    }
    before.len() != now.len() || before.modified().ok() != now.modified().ok()
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

struct StagedFile(PathBuf);

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
