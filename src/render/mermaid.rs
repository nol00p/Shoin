//! Asynchronous Unicode and PNG previews from a configured local Merman CLI.

use std::collections::VecDeque;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(crate) const MAX_SOURCE_BYTES: usize = 32 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_PNG_BYTES: usize = 1024 * 1024;
const MAX_PNG_SIDE: u32 = 2048;
const MAX_PNG_PIXELS: u32 = 2 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 4 * 1024;
const MAX_ROWS: usize = 48;
const MAX_ROW_BYTES: usize = 4096;
const MAX_EXECUTABLE_BYTES: usize = 4096;
pub(crate) const MAX_CACHE_ENTRIES: usize = 32;
const MAX_JOBS: usize = 2;
const TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RenderState<T> {
    Pending,
    Ready(T),
    Failed(Failure),
}

pub(crate) type Preview = RenderState<Arc<[String]>>;
pub(crate) type PicturePreview = RenderState<Arc<[u8]>>;

impl<T> RenderState<T> {
    fn map<U>(self, apply: impl FnOnce(T) -> U) -> RenderState<U> {
        match self {
            Self::Pending => RenderState::Pending,
            Self::Ready(value) => RenderState::Ready(apply(value)),
            Self::Failed(failure) => RenderState::Failed(failure),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Unicode,
    Png,
}

#[derive(Clone, Debug)]
enum Output {
    Rows(Arc<[String]>),
    Png(Arc<[u8]>),
}

impl Output {
    fn rows(self) -> Arc<[String]> {
        let Self::Rows(rows) = self else { unreachable!("a text job yields text") };
        rows
    }

    fn png(self) -> Arc<[u8]> {
        let Self::Png(bytes) = self else { unreachable!("a PNG job yields PNG") };
        bytes
    }
}

type Rendered = RenderState<Output>;
type Cache = VecDeque<(Arc<Key>, Rendered)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    MissingExecutable,
    Unavailable,
    Timeout,
    Invalid,
    Unsupported,
    TooLarge,
    UnsafeOutput,
    InvalidPng,
}

impl Failure {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::MissingExecutable => "Mermaid CLI not found. Install merman-cli or check mermaid_renderer.",
            Self::Unavailable => "Could not start the Mermaid renderer",
            Self::Timeout => "Mermaid render timed out",
            Self::Invalid => "Mermaid render failed",
            Self::Unsupported => "Unsupported Mermaid text diagram",
            Self::TooLarge => "Mermaid diagram too large",
            Self::UnsafeOutput => "Mermaid renderer returned invalid terminal text",
            Self::InvalidPng => "Mermaid renderer returned an invalid PNG",
        }
    }
}

#[derive(Debug)]
struct Key {
    executable: String,
    source: String,
    format: Format,
}

impl Key {
    fn matches(&self, executable: &str, source: &str, format: Format) -> bool {
        self.executable == executable && self.source == source && self.format == format
    }
}

#[derive(Debug)]
struct Job {
    key: Arc<Key>,
    result: Option<mpsc::Receiver<Rendered>>,
    thread: JoinHandle<()>,
}

/// Share one instance across panes. Admission and cache access happen on the
/// caller thread. Process startup, I/O and reaping happen on worker threads.
#[derive(Debug, Default)]
pub(crate) struct MermaidRenderer {
    caches: [Cache; 2],
    jobs: Vec<Job>,
}

impl MermaidRenderer {
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        self.jobs.retain_mut(|job| {
            // Check thread completion before receiving, so a worker finishing
            // during this poll cannot have its final message discarded.
            let finished = job.thread.is_finished();
            if let Some(result) = &job.result {
                let preview = match result.try_recv() {
                    Ok(preview) => Some(preview),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Rendered::Failed(Failure::Unavailable))
                    }
                    Err(mpsc::TryRecvError::Empty) => None,
                };
                if let Some(preview) = preview {
                    remember(&mut self.caches[job.key.format as usize], Arc::clone(&job.key), preview);
                    job.result = None;
                    changed = true;
                }
            }
            changed |= finished;
            !finished
        });
        changed
    }

    pub(crate) fn cached(&self, executable: &str, source: &str) -> Option<Preview> {
        self.cached_output(executable, source, Format::Unicode).map(|state| state.map(Output::rows))
    }

    pub(crate) fn cached_png(&self, executable: &str, source: &str) -> Option<PicturePreview> {
        self.cached_output(executable, source, Format::Png).map(|state| state.map(Output::png))
    }

    fn cached_output(&self, executable: &str, source: &str, format: Format) -> Option<Rendered> {
        self.caches[format as usize]
            .iter()
            .find(|(key, _)| key.matches(executable, source, format))
            .map(|(_, output)| output.clone())
    }

    pub(crate) fn request(&mut self, executable: &str, source: &str) -> Preview {
        self.request_output(executable, source, Format::Unicode).map(Output::rows)
    }

    pub(crate) fn request_png(&mut self, executable: &str, source: &str) -> PicturePreview {
        self.request_output(executable, source, Format::Png).map(Output::png)
    }

    fn request_output(&mut self, executable: &str, source: &str, format: Format) -> Rendered {
        if source.len() > MAX_SOURCE_BYTES {
            return Rendered::Failed(Failure::TooLarge);
        }
        if executable.is_empty() || executable.len() > MAX_EXECUTABLE_BYTES {
            return Rendered::Failed(Failure::Unavailable);
        }
        let cache = &mut self.caches[format as usize];
        if let Some(index) = cache.iter().position(|(key, _)| key.matches(executable, source, format)) {
            let entry = cache.remove(index).expect("cache index exists");
            let output = entry.1.clone();
            cache.push_back(entry);
            return output;
        }
        if self.jobs.len() >= MAX_JOBS
            || self.jobs.iter().any(|job| job.key.matches(executable, source, format))
        {
            return Rendered::Pending;
        }

        let key = Arc::new(Key { executable: executable.into(), source: source.into(), format });
        let worker_key = Arc::clone(&key);
        let (send, receive) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("mermaid-render".into())
            .spawn(move || render(&worker_key, TIMEOUT, send))
        {
            Ok(thread) => {
                self.jobs.push(Job {
                    key,
                    result: Some(receive),
                    thread,
                });
                Rendered::Pending
            }
            Err(_) => {
                let preview = Rendered::Failed(Failure::Unavailable);
                remember(cache, key, preview.clone());
                preview
            }
        }
    }
}

fn remember(cache: &mut Cache, key: Arc<Key>, preview: Rendered) {
    if cache.len() == MAX_CACHE_ENTRIES {
        cache.pop_front();
    }
    cache.push_back((key, preview));
}

fn render(key: &Key, timeout: Duration, send: mpsc::SyncSender<Rendered>) {
    let deadline = Instant::now() + timeout;
    let mut command = Command::new(&key.executable);
    command.args(["render", "-", "--format"]);
    let stdout_limit = match key.format {
        Format::Unicode => {
            command.args(["unicode", "--ascii-color", "plain"]);
            MAX_TEXT_BYTES
        }
        Format::Png => {
            // Merman's native raster pipeline drops external resource references.
            // Do not enable network or icon packs. Set explicit raster and inline
            // image budgets in addition to the constrained general policy.
            command
                .args(["png", "--input-kind", "mermaid", "--quiet", "--resource-profile", "constrained"])
                .arg("--raster-max-width").arg(MAX_PNG_SIDE.to_string())
                .arg("--raster-max-height").arg(MAX_PNG_SIDE.to_string())
                .arg("--raster-max-pixels").arg(MAX_PNG_PIXELS.to_string())
                .args([
                    "--embedded-image-max-bytes", "1",
                    "--embedded-image-max-total-bytes", "1",
                    "--embedded-image-max-pixels", "1",
                    "--embedded-image-max-total-pixels", "1",
                    "--background", "#ffffff",
                ]);
            MAX_PNG_BYTES
        }
    };
    command
        .args(["--output", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    // A renderer being replaced can briefly be open for writing on Unix.
    let child = command.spawn().or_else(|error| {
        if error.kind() == std::io::ErrorKind::ExecutableFileBusy {
            thread::sleep(POLL_INTERVAL * 2);
            command.spawn()
        } else {
            Err(error)
        }
    });
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            let failure = if error.kind() == std::io::ErrorKind::NotFound {
                Failure::MissingExecutable
            } else {
                Failure::Unavailable
            };
            let _ = send.send(Rendered::Failed(failure));
            return;
        }
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    thread::scope(|scope| {
        let (out_send, out_receive) = mpsc::sync_channel(1);
        let (err_send, err_receive) = mpsc::sync_channel(1);
        let input = thread::Builder::new()
            .spawn_scoped(scope, move || stdin.write_all(key.source.as_bytes()));
        let output = thread::Builder::new().spawn_scoped(scope, move || {
            let _ = out_send.send(read_capped(stdout, stdout_limit));
        });
        let diagnostics = thread::Builder::new().spawn_scoped(scope, move || {
            let _ = err_send.send(read_capped(stderr, MAX_DIAGNOSTIC_BYTES));
        });

        let mut exited = false;
        let result = if input.is_ok() && output.is_ok() && diagnostics.is_ok() {
            let mut out = None;
            let mut err = None;
            loop {
                if Instant::now() >= deadline {
                    break Err(Failure::Timeout);
                }
                if let Err(failure) = collect(&out_receive, &mut out)
                    .and_then(|()| collect(&err_receive, &mut err))
                {
                    break Err(failure);
                }
                if let (Some(out), Some(err)) = (&out, &err) {
                    let status = match child.try_wait() {
                        Ok(Some(status)) => status,
                        Ok(None) => {
                            thread::sleep(POLL_INTERVAL);
                            continue;
                        }
                        Err(_) => break Err(Failure::Invalid),
                    };
                    exited = true;
                    break if status.success() {
                        match key.format {
                            Format::Unicode => literal_rows(out).map(Output::Rows),
                            Format::Png => validate_png(out).map(|()| Output::Png(Arc::from(out.as_slice()))),
                        }
                    } else {
                        let diagnostic = String::from_utf8_lossy(err).to_ascii_lowercase();
                        Err(if diagnostic.contains("unsupported")
                            || diagnostic.contains("not supported")
                        {
                            Failure::Unsupported
                        } else {
                            Failure::Invalid
                        })
                    };
                }
                thread::sleep(POLL_INTERVAL);
            }
        } else {
            Err(Failure::Unavailable)
        };
        // Publish promptly even if an unrelated process inherited a pipe.
        // Scoped I/O threads retain this job's slot until all pipes close.
        if !exited {
            terminate(&mut child);
        }
        let _ = send.send(match result {
            Ok(output) => Rendered::Ready(output),
            Err(failure) => Rendered::Failed(failure),
        });
    });
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: std::ffi::c_int, signal: std::ffi::c_int) -> std::ffi::c_int;
        }
        const SIGKILL: std::ffi::c_int = 9;
        // SAFETY: process_group(0) makes the unreaped child's ID its group ID.
        // A negative ID confines the signal to that group, including wrappers'
        // children that still hold the renderer's pipes.
        if let Ok(group) = std::ffi::c_int::try_from(child.id()) {
            unsafe { kill(-group, SIGKILL) };
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn collect(
    receive: &mpsc::Receiver<Result<Vec<u8>, Failure>>,
    result: &mut Option<Vec<u8>>,
) -> Result<(), Failure> {
    if result.is_none() {
        match receive.try_recv() {
            Ok(bytes) => *result = Some(bytes?),
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => return Err(Failure::Invalid),
        }
    }
    Ok(())
}

fn read_capped(input: impl Read, limit: usize) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    input
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::Invalid)?;
    if bytes.len() > limit {
        return Err(Failure::TooLarge);
    }
    Ok(bytes)
}

/// Accept the static 8-bit RGBA PNG emitted by Merman. Dimensions are
/// bounded before the terminal decodes the pixels, and the chunk whitelist
/// excludes metadata, animation, and any trailing data.
fn validate_png(bytes: &[u8]) -> Result<(), Failure> {
    let Some(header) = bytes.get(..33) else { return Err(Failure::InvalidPng) };
    if !header.starts_with(b"\x89PNG\r\n\x1a\n")
        || &header[8..16] != b"\0\0\0\x0dIHDR"
        || header[24..29] != [8, 6, 0, 0, 0]
    {
        return Err(Failure::InvalidPng);
    }
    let info = crate::image::probe(header).ok_or(Failure::InvalidPng)?;
    if info.width == 0 || info.height == 0 {
        return Err(Failure::InvalidPng);
    }
    if info.width > MAX_PNG_SIDE || info.height > MAX_PNG_SIDE || info.width * info.height > MAX_PNG_PIXELS {
        return Err(Failure::TooLarge);
    }

    let mut chunks = &bytes[33..];
    let mut image_bytes = 0;
    while let Some(header) = chunks.get(..8) {
        let size = u32::from_be_bytes(header[..4].try_into().expect("chunk size")) as usize;
        let Some(tail) = size.checked_add(12).and_then(|end| chunks.get(end..)) else {
            return Err(Failure::InvalidPng);
        };
        match &header[4..8] {
            b"IDAT" => image_bytes += size,
            b"IEND" if size == 0 && tail.is_empty() && image_bytes != 0 => return Ok(()),
            _ => break,
        }
        chunks = tail;
    }
    Err(Failure::InvalidPng)
}

fn literal_rows(bytes: &[u8]) -> Result<Arc<[String]>, Failure> {
    let output = std::str::from_utf8(bytes).map_err(|_| Failure::UnsafeOutput)?;
    let mut rows = Vec::new();
    for line in output.lines() {
        if rows.len() == MAX_ROWS || line.len() > MAX_ROW_BYTES {
            return Err(Failure::TooLarge);
        }
        // CRLF is handled by lines(). Reject terminal controls, alternate line
        // separators and bidirectional overrides/isolates in literal rows.
        if line.chars().any(|ch| {
            ch.is_control()
                || matches!(
                    ch,
                    '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        }) {
            return Err(Failure::UnsafeOutput);
        }
        rows.push(line.to_owned());
    }
    if rows.iter().all(|row| row.trim().is_empty()) {
        return Err(Failure::Invalid);
    }
    Ok(rows.into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs::{self, DirBuilder};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TEST_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
        0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
        0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0x0f, 0x04, 0x00,
        0x09, 0xfb, 0x03, 0xfd, 0xfb, 0x5e, 0x6b, 0x2b, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    struct FakeRenderer(PathBuf);

    impl FakeRenderer {
        fn new(script: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "shoin-mermaid-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            DirBuilder::new().mode(0o700).create(&dir).unwrap();
            let fake = Self(dir);
            let executable = fake.0.join("renderer");
            fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
            fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
            fake
        }

        fn executable(&self) -> String {
            self.0.join("renderer").into_os_string().into_string().unwrap()
        }
    }

    impl Drop for FakeRenderer {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn settle(renderer: &mut MermaidRenderer) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !renderer.jobs.is_empty() {
            assert!(Instant::now() < deadline, "renderer job did not finish");
            renderer.poll();
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn run(executable: String, timeout: Duration) -> Preview {
        let (send, receive) = mpsc::sync_channel(1);
        let key = Key { executable, source: "graph LR".into(), format: Format::Unicode };
        render(&key, timeout, send);
        receive.recv().unwrap().map(Output::rows)
    }

    #[test]
    fn passes_literal_stdin_and_reuses_exact_source_and_executable() {
        let fake = FakeRenderer::new(
            r#"[ "$*" = 'render - --format unicode --ascii-color plain --output -' ] || exit 2
cat"#,
        );
        let executable = fake.executable();
        let source = "  ┌─$(echo no) **bold**\r\n\n  → ┘  \n";
        let mut renderer = MermaidRenderer::default();
        assert_eq!(renderer.request(&executable, source), Preview::Pending);
        assert_eq!(renderer.request(&executable, source), Preview::Pending);
        assert_eq!(renderer.jobs.len(), 1);
        assert_eq!(renderer.cached(&executable, source), None);
        settle(&mut renderer);
        let expected = Preview::Ready(
            ["  ┌─$(echo no) **bold**", "", "  → ┘  "].map(str::to_owned).to_vec().into(),
        );
        assert_eq!(renderer.cached(&executable, source), Some(expected.clone()));
        assert_eq!(renderer.request(&executable, source), expected);
        assert_eq!(renderer.request(&executable, "edited"), Preview::Pending);
        let other = FakeRenderer::new("printf 'other renderer\n'");
        assert_eq!(renderer.request(&other.executable(), source), Preview::Pending);
        assert_eq!(renderer.jobs.len(), MAX_JOBS);
        assert_eq!(renderer.request(&executable, "more edits"), Preview::Pending);
        assert_eq!(renderer.jobs.len(), MAX_JOBS);
        settle(&mut renderer);
        assert_eq!(
            renderer.request(&executable, "edited"),
            Preview::Ready(vec!["edited".into()].into())
        );
        assert_eq!(
            renderer.request(&other.executable(), source),
            Preview::Ready(vec!["other renderer".into()].into())
        );
        assert_eq!(renderer.request(&executable, source), expected);
    }

    #[test]
    fn caches_text_and_png_independently_and_rejects_non_png() {
        let fake = FakeRenderer::new(
            r#"case "$4" in
unicode) cat ;;
png) cat >/dev/null; cat "$(dirname "$0")/sample.png" ;;
*) exit 2 ;;
esac"#,
        );
        let source = "graph LR";
        let executable = fake.executable();
        fs::write(fake.0.join("sample.png"), TEST_PNG).unwrap();

        let mut renderer = MermaidRenderer::default();
        assert_eq!(renderer.request(&executable, source), Preview::Pending);
        assert_eq!(renderer.request_png(&executable, source), PicturePreview::Pending);
        assert_eq!(renderer.request_png(&executable, source), PicturePreview::Pending);
        assert_eq!(renderer.jobs.len(), 2);
        settle(&mut renderer);
        assert_eq!(renderer.cached(&executable, source), Some(Preview::Ready(vec![source.into()].into())));
        assert_eq!(renderer.cached_png(&executable, source), Some(PicturePreview::Ready(TEST_PNG.into())));

        fs::write(fake.0.join("sample.png"), b"<svg/>").unwrap();
        assert_eq!(renderer.request_png(&executable, "new source"), PicturePreview::Pending);
        settle(&mut renderer);
        assert_eq!(
            renderer.cached_png(&executable, "new source"),
            Some(PicturePreview::Failed(Failure::InvalidPng)),
        );
    }

    #[test]
    fn only_accepts_bounded_static_png_chunks() {
        assert_eq!(validate_png(TEST_PNG), Ok(()));
        assert_eq!(validate_png(&TEST_PNG[..32]), Err(Failure::InvalidPng));

        let mut oversized = TEST_PNG.to_vec();
        oversized[16..20].copy_from_slice(&(MAX_PNG_SIDE + 1).to_be_bytes());
        assert_eq!(validate_png(&oversized), Err(Failure::TooLarge));

        let mut animation = TEST_PNG.to_vec();
        animation[37..41].copy_from_slice(b"acTL");
        assert_eq!(validate_png(&animation), Err(Failure::InvalidPng));

        let mut trailing = TEST_PNG.to_vec();
        trailing.push(0);
        assert_eq!(validate_png(&trailing), Err(Failure::InvalidPng));
    }

    #[test]
    fn caches_failures_and_evicts_the_least_recently_used_entry() {
        let fake = FakeRenderer::new("exit 1");
        let executable = fake.executable();
        let mut renderer = MermaidRenderer::default();
        assert_eq!(renderer.request(&executable, ""), Preview::Pending);
        settle(&mut renderer);
        assert_eq!(renderer.request(&executable, ""), Preview::Failed(Failure::Invalid));

        for index in 1..MAX_CACHE_ENTRIES {
            assert_eq!(renderer.request(&executable, &index.to_string()), Preview::Pending);
            settle(&mut renderer);
        }
        assert_eq!(renderer.request(&executable, ""), Preview::Failed(Failure::Invalid));
        assert_eq!(renderer.request(&executable, "new"), Preview::Pending);
        settle(&mut renderer);
        assert_eq!(renderer.caches[Format::Unicode as usize].len(), MAX_CACHE_ENTRIES);
        assert_eq!(renderer.request(&executable, ""), Preview::Failed(Failure::Invalid));
        assert_eq!(renderer.request(&executable, "1"), Preview::Pending);
        settle(&mut renderer);
    }

    #[test]
    fn bounds_source_and_renderer_streams() {
        let mut renderer = MermaidRenderer::default();
        assert_eq!(
            renderer.request("unused", &"x".repeat(MAX_SOURCE_BYTES + 1)),
            Preview::Failed(Failure::TooLarge)
        );
        assert!(renderer.jobs.is_empty());

        for script in ["exec yes growing", "exec yes growing >&2"] {
            let fake = FakeRenderer::new(script);
            assert_eq!(run(fake.executable(), TIMEOUT), Preview::Failed(Failure::TooLarge));
        }
        assert_eq!(
            literal_rows("x\n".repeat(MAX_ROWS + 1).as_bytes()),
            Err(Failure::TooLarge)
        );
        assert_eq!(
            literal_rows(&vec![b'x'; MAX_ROW_BYTES + 1]),
            Err(Failure::TooLarge)
        );
    }

    #[test]
    fn times_out_even_when_a_wrapper_child_holds_pipes() {
        let fake = FakeRenderer::new("sleep 30 & wait");
        let start = Instant::now();
        assert_eq!(
            run(fake.executable(), Duration::from_millis(100)),
            Preview::Failed(Failure::Timeout)
        );
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn reports_safe_failures() {
        let fake = FakeRenderer::new("printf 'unsupported diagram\n' >&2; exit 1");
        assert_eq!(run(fake.executable(), TIMEOUT), Preview::Failed(Failure::Unsupported));
        assert_eq!(
            run(format!("{}/missing", fake.0.display()), TIMEOUT),
            Preview::Failed(Failure::MissingExecutable)
        );

        for bytes in [
            b"\x1b]52;malicious\x07".as_slice(),
            b"a\rB",
            b"a\tb",
            b"\xFF",
            "a\u{202e}b".as_bytes(),
            "a\u{0085}b".as_bytes(),
        ] {
            assert_eq!(literal_rows(bytes), Err(Failure::UnsafeOutput));
        }
        assert_eq!(literal_rows(b" \n"), Err(Failure::Invalid));
    }
}
