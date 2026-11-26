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
    original: Option<Metadata>,
    // Preserve the identity seen before the editor can rename or replace a file.
    inputs: Vec<(PathBuf, Option<Metadata>)>,
    compiled: usize,
}

impl Destination {
    pub(super) fn new(path: PathBuf) -> Result<Self> {
        let original = inspect(&path)?;
        Ok(Self { path, original, inputs: Vec::new(), compiled: 0 })
    }

    pub(super) fn with_inputs(mut self, mut sources: Vec<PathBuf>, pictures: Vec<PathBuf>) -> Result<Self> {
        self.compiled = sources.len();
        sources.extend(pictures);
        self.inputs = sources.into_iter().map(|source| {
            let meta = fs::metadata(&source).ok();
            (source, meta)
        }).collect();
        self.check(self.original.as_ref())?;
        Ok(self)
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
        let mut file = if let Some(original) = &self.original {
            let file = match options.open(&self.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(self.changed()),
                result => result.with_context(|| short(&self.path))?,
            };
            self.revalidate()?;
            let opened = file.metadata().with_context(|| short(&self.path))?;
            if !opened.is_file() || !same_version(original, &opened) {
                return Err(self.changed());
            }
            self.check(Some(&opened))?;
            file.set_len(0).with_context(|| short(&self.path))?;
            file
        } else {
            let target = crate::fs::save::through_links(&self.path)?;
            self.revalidate()?;
            match options.create_new(true).open(&target) {
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(self.changed()),
                result => result.with_context(|| short(&self.path))?,
            }
        };
        write(&mut file).with_context(|| short(&self.path))
    }

    fn revalidate(&self) -> Result<()> {
        let current = inspect(&self.path)?;
        self.check(current.as_ref())?;
        match (&self.original, &current) {
            (None, None) => {}
            (Some(before), Some(now)) if same_version(before, now) => {}
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
    before.len() == now.len() && before.modified().ok() == now.modified().ok()
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
