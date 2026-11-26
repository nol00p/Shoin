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

pub(super) struct Destination {
    path: PathBuf,
    // Preserve the identity seen before the editor can rename or replace a file.
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
        let dest = Self { path, inputs, compiled };
        dest.check(inspect(&dest.path)?.as_ref())?;
        Ok(dest)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn check(&self, current: Option<&Metadata>) -> Result<()> {
        let missing = if current.is_none() { missing_name(&self.path)? } else { None };
        for (index, (source, original)) in self.inputs.iter().enumerate() {
            if current.zip(original.as_ref()).is_some_and(|(a, b)| same_inode(a, b))
                || current.zip(fs::metadata(source).ok().as_ref()).is_some_and(|(a, b)| same_inode(a, b))
                || same_file(&self.path, source)
                || missing.as_ref().is_some_and(|name| missing_name(source).ok().flatten().as_ref() == Some(name))
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
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", short(parent)))?;
        }
        self.check(inspect(&self.path)?.as_ref())?;

        // Open through the destination path as usual, including dangling links.
        // Check the opened inode before truncating so a path swap cannot redirect
        // the write into a captured input between the check and the open.
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Avoid blocking on a FIFO swapped in after the path check.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let mut file = options.open(&self.path).with_context(|| short(&self.path))?;
        let opened = file.metadata().with_context(|| short(&self.path))?;
        if !opened.is_file() {
            bail!("{} is not a regular file", short(&self.path));
        }
        self.check(Some(&opened))?;
        file.set_len(0).with_context(|| short(&self.path))?;
        write(&mut file).with_context(|| short(&self.path))
    }
}

/// The name a dangling link or a path in an existing directory would create.
fn missing_name(path: &Path) -> Result<Option<PathBuf>> {
    let target = crate::fs::save::through_links(path)?;
    let Some(name) = target.file_name() else { return Ok(None) };
    let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Ok(fs::canonicalize(parent).ok().map(|dir| dir.join(name)))
}

fn inspect(path: &Path) -> Result<Option<Metadata>> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(meta)),
        Ok(_) => bail!("{} is not a regular file", short(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| short(path)),
    }
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
