//! PNG files used by one PDF conversion.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::schema::MarkdownConfig;

const WRITE_FAILED: &str = "Mermaid left as source: could not write a temporary PNG";

pub(super) struct Prepared<'a> {
    pub markdown: Cow<'a, str>,
    pub note: Option<String>,
    _assets: Option<Assets>,
}

/// Replace successful fences and retain failed ones verbatim. Pandoc receives
/// absolute paths just as it does for the document's existing pictures.
pub(super) fn prepare<'a>(markdown: &'a str, md: &MarkdownConfig) -> Prepared<'a> {
    let fences = super::mermaid::render(markdown, md);
    let diagnostic = super::mermaid::diagnostic(&fences);
    let original = |note| Prepared { markdown: Cow::Borrowed(markdown), note, _assets: None };
    if !fences.iter().any(|fence| fence.png.is_ok()) {
        return original(diagnostic.map(str::to_owned));
    }
    let Ok(assets) = Assets::create() else {
        return original(note(diagnostic, true));
    };
    let mut written = HashMap::<&[u8], Option<String>>::new();
    let mut out = String::with_capacity(markdown.len());
    let mut previous_end = 0;
    let mut write_failed = false;
    for (index, fence) in fences.iter().enumerate() {
        let Ok(bytes) = &fence.png else { continue };
        let target = written.entry(bytes).or_insert_with(|| assets.write(index, bytes).ok());
        let Some(target) = target else {
            write_failed = true;
            continue;
        };
        let raw = &markdown[fence.bytes.clone()];
        let ending = if raw.contains("\r\n") { "\r\n" } else { "\n" };
        let previous = &markdown[previous_end..fence.bytes.start];
        out.push_str(previous);
        // A fenced block can interrupt a paragraph, while a Markdown image
        // cannot. Keep the replacement in its own paragraph and preserve the
        // opener's indentation for list continuations.
        if !previous.is_empty() && previous.lines().last().is_some_and(|line| !line.trim().is_empty()) {
            out.push_str(ending);
        }
        let _ = write!(out, "{}![{}](<{target}>)", fence.indent, fence.alt);
        if raw.ends_with('\n') {
            out.push_str(ending);
        }
        if markdown[fence.bytes.end..].lines().next().is_some_and(|line| !line.trim().is_empty()) {
            out.push_str(ending);
        }
        previous_end = fence.bytes.end;
    }
    if previous_end == 0 {
        return original(note(diagnostic, write_failed));
    }
    out.push_str(&markdown[previous_end..]);
    Prepared { markdown: Cow::Owned(out), note: note(diagnostic, write_failed), _assets: Some(assets) }
}

fn note(render: Option<&str>, write_failed: bool) -> Option<String> {
    match (render, write_failed) {
        (Some(render), true) => Some(format!("{render} · {WRITE_FAILED}")),
        (Some(render), false) => Some(render.to_owned()),
        (None, true) => Some(WRITE_FAILED.to_owned()),
        (None, false) => None,
    }
}

struct Assets(PathBuf);

impl Assets {
    fn create() -> io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::path::absolute(std::env::temp_dir())?;
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let builder = DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        for _ in 0..8 {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("shoin-pdf-{}-{timestamp:x}-{serial}", std::process::id()));
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not reserve a Mermaid asset folder"))
    }

    fn write(&self, index: usize, bytes: &[u8]) -> io::Result<String> {
        let path = self.0.join(format!("mermaid-{index}.png"));
        let target = markdown_target(&path).ok_or_else(|| io::Error::new(
            io::ErrorKind::InvalidInput,
            "temporary path cannot be written in Markdown",
        ))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&path)?.write_all(bytes)?;
        Ok(target)
    }
}

impl Drop for Assets {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn markdown_target(path: &Path) -> Option<String> {
    let target = path.to_str()?;
    #[cfg(windows)]
    let target = target.replace('\\', "/");
    if target.contains(['<', '>', '\\', '\n', '\r', '%', '#', '?']) {
        return None;
    }
    // Ampersands can form a Markdown entity inside a link destination.
    Some(target.replace('&', "&amp;"))
}
