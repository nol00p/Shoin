//! PNG files used by one PDF conversion.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::schema::MarkdownConfig;

use crate::render::mermaid::{Failure, MermaidRenderer};

pub(super) struct Prepared<'a> {
    pub markdown: Cow<'a, str>,
    pub note: Option<String>,
    _assets: Option<Assets>,
}

/// Replace successful fences and retain failed ones verbatim. Pandoc receives
/// absolute paths just as it does for the document's existing pictures.
pub(super) fn prepare<'a>(
    markdown: &'a str, md: &MarkdownConfig, renderer: MermaidRenderer,
) -> Prepared<'a> {
    let mut fences = super::mermaid::render(markdown, md, renderer);
    let original = |note| Prepared { markdown: Cow::Borrowed(markdown), note, _assets: None };
    if !fences.iter().any(|fence| fence.png.is_ok()) {
        return original(super::mermaid::diagnostic(&fences));
    }
    let Ok(assets) = Assets::create() else {
        for fence in &mut fences {
            if fence.png.is_ok() {
                fence.png = Err(Failure::AssetWrite);
            }
        }
        return original(super::mermaid::diagnostic(&fences));
    };
    let mut written = HashMap::<Arc<[u8]>, Option<String>>::new();
    let mut out = String::with_capacity(markdown.len());
    let mut previous_end = 0;
    for (index, fence) in fences.iter_mut().enumerate() {
        let Ok(bytes) = &fence.png else { continue };
        let target = written.entry(Arc::clone(bytes)).or_insert_with(|| assets.write(index, bytes).ok());
        let Some(target) = target else {
            fence.png = Err(Failure::AssetWrite);
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
    let note = super::mermaid::diagnostic(&fences);
    if previous_end == 0 {
        return original(note);
    }
    out.push_str(&markdown[previous_end..]);
    Prepared { markdown: Cow::Owned(out), note, _assets: Some(assets) }
}

// A signal can exit the process without running the export worker's Drop.
static LIVE: Mutex<Option<Vec<PathBuf>>> = Mutex::new(Some(Vec::new()));

pub(crate) fn cleanup() {
    if let Some(paths) = LIVE.lock().unwrap().take() {
        for path in paths {
            let _ = fs::remove_dir_all(path);
        }
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
        let mut live = LIVE.lock().unwrap();
        let paths = live.as_mut().ok_or_else(|| io::Error::from(io::ErrorKind::Interrupted))?;
        for _ in 0..8 {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("shoin-pdf-{}-{timestamp:x}-{serial}", std::process::id()));
            match builder.create(&path) {
                Ok(()) => {
                    paths.push(path.clone());
                    return Ok(Self(path));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not reserve a Mermaid asset folder"))
    }

    fn write(&self, index: usize, bytes: &[u8]) -> io::Result<String> {
        let path = self.0.join(format!("mermaid-{index}.png"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&path)?.write_all(bytes)?;
        Ok(markdown_target(&path))
    }
}

impl Drop for Assets {
    fn drop(&mut self) {
        if let Some(paths) = LIVE.lock().unwrap().as_mut() {
            let _ = fs::remove_dir_all(&self.0);
            paths.retain(|path| path != &self.0);
        }
    }
}

fn markdown_target(path: &Path) -> String {
    #[cfg(windows)]
    let path = PathBuf::from(path.to_string_lossy().replace('\\', "/"));
    let mut target = String::new();
    // Pandoc decodes percent escapes in local image destinations. Keep '/'
    // (and an optional drive colon) so it still recognizes an absolute path.
    for &byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"/:._-~".contains(&byte) {
            target.push(char::from(byte));
        } else {
            let _ = write!(target, "%{byte:02X}");
        }
    }
    target
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn markdown_targets_escape_reserved_path_bytes() {
        assert_eq!(
            markdown_target(Path::new("/tmp/é #1? &%<>'/mermaid-0.png")),
            "/tmp/%C3%A9%20%231%3F%20%26%25%3C%3E%27/mermaid-0.png",
        );
    }
}
