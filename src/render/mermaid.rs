//! Asynchronous Unicode and PNG previews from a configured local Merman CLI.

use std::collections::VecDeque;
#[cfg(unix)]
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
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
const TRANSIENT_TTL: Duration = Duration::from_secs(3);
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
type Cache = VecDeque<(Arc<Key>, Rendered, Option<Instant>)>;

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
    AssetWrite,
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
            Self::AssetWrite => "Could not write a temporary Mermaid PNG",
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
    cancelled: Arc<AtomicBool>,
}

impl MermaidRenderer {
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        for cache in &mut self.caches {
            let before = cache.len();
            let now = Instant::now();
            cache.retain(|(_, _, expires)| expires.is_none_or(|expires| now < expires));
            changed |= cache.len() != before;
        }
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

    pub(crate) fn with_cached_pngs(&self) -> Self {
        let mut renderer = Self::default();
        renderer.caches[Format::Png as usize] = self.caches[Format::Png as usize]
            .iter().filter(|(_, output, _)| matches!(output, Rendered::Ready(_))).cloned().collect();
        renderer
    }

    pub(crate) fn cached_png(&self, executable: &str, source: &str) -> Option<PicturePreview> {
        self.cached_output(executable, source, Format::Png).map(|state| state.map(Output::png))
    }

    fn cached_output(&self, executable: &str, source: &str, format: Format) -> Option<Rendered> {
        self.caches[format as usize]
            .iter()
            .find(|(key, _, expires)| key.matches(executable, source, format)
                && expires.is_none_or(|expires| Instant::now() < expires))
            .map(|(_, output, _)| output.clone())
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
        let now = Instant::now();
        cache.retain(|(_, _, expires)| expires.is_none_or(|expires| now < expires));
        if let Some(index) = cache.iter().position(|(key, _, _)| key.matches(executable, source, format)) {
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
        let cancelled = Arc::clone(&self.cancelled);
        let (send, receive) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("mermaid-render".into())
            .spawn(move || render(&worker_key, TIMEOUT, &cancelled, send))
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

impl Drop for MermaidRenderer {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        for job in self.jobs.drain(..) {
            let _ = job.thread.join();
        }
    }
}

fn remember(cache: &mut Cache, key: Arc<Key>, preview: Rendered) {
    if cache.len() == MAX_CACHE_ENTRIES {
        cache.pop_front();
    }
    let expires = matches!(preview, Rendered::Failed(
        Failure::MissingExecutable | Failure::Unavailable | Failure::Timeout
    )).then(|| Instant::now() + TRANSIENT_TTL);
    cache.push_back((key, preview, expires));
}

// Covers exits from the signal thread, where App and export stacks cannot drop.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static ACTIVE: Mutex<usize> = Mutex::new(0);
static IDLE: Condvar = Condvar::new();

pub(crate) fn shutdown() {
    let workers = ACTIVE.lock().unwrap();
    SHUTTING_DOWN.store(true, Ordering::Release);
    drop(IDLE.wait_while(workers, |workers| *workers != 0).unwrap());
}

struct Active;

impl Active {
    fn enter() -> Option<Self> {
        let mut workers = ACTIVE.lock().unwrap();
        if SHUTTING_DOWN.load(Ordering::Acquire) {
            return None;
        }
        *workers += 1;
        Some(Self)
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        *ACTIVE.lock().unwrap() -= 1;
        IDLE.notify_all();
    }
}

fn render(key: &Key, timeout: Duration, cancelled: &AtomicBool, send: mpsc::SyncSender<Rendered>) {
    let result = if let Some(_active) = Active::enter() {
        render_job(key, Instant::now() + timeout, cancelled)
    } else {
        Err(Failure::Unavailable)
    };
    let _ = send.send(match result {
        Ok(output) => Rendered::Ready(output),
        Err(failure) => Rendered::Failed(failure),
    });
}

#[cfg(not(unix))]
fn render_job(_: &Key, _: Instant, _: &AtomicBool) -> Result<Output, Failure> {
    Err(Failure::Unavailable)
}

#[cfg(unix)]
fn render_job(key: &Key, deadline: Instant, cancelled: &AtomicBool) -> Result<Output, Failure> {
    let mut command = Command::new(&key.executable);
    command.args(["render", "-", "--format"]);
    let stdout_limit = match key.format {
        Format::Unicode => {
            command.args(["unicode", "--ascii-color", "plain", "--resource-profile", "constrained"]);
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
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: this runs after fork and uses only syscalls; the parent check
        // closes the race where Shoin died before the death signal was armed.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(
                    libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong,
                    0 as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong,
                ) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }

    let stopped = || cancelled.load(Ordering::Acquire) || SHUTTING_DOWN.load(Ordering::Acquire);
    if stopped() {
        return Err(Failure::Unavailable);
    }
    // A renderer being replaced can briefly be open for writing on Unix.
    let child = command.spawn().or_else(|error| {
        if error.kind() == io::ErrorKind::ExecutableFileBusy && !stopped() {
            thread::sleep(POLL_INTERVAL * 2);
            command.spawn()
        } else {
            Err(error)
        }
    }).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => Failure::MissingExecutable,
        _ => Failure::Unavailable,
    })?;
    let mut child = RendererChild(Some(child));
    let process = child.0.as_mut().expect("spawned renderer");
    let mut stdin = process.stdin.take();
    let mut stdout = process.stdout.take();
    let mut stderr = process.stderr.take();
    nonblocking(stdin.as_ref().expect("piped stdin"))?;
    nonblocking(stdout.as_ref().expect("piped stdout"))?;
    nonblocking(stderr.as_ref().expect("piped stderr"))?;

    let mut source = key.source.as_bytes();
    let mut out = Vec::new();
    let mut err = Vec::new();
    loop {
        if stopped() {
            return Err(Failure::Unavailable);
        }
        if Instant::now() >= deadline {
            return Err(Failure::Timeout);
        }
        let mut progressed = false;
        if let Some(pipe) = &mut stdin {
            if !source.is_empty() {
                match pipe.write(source) {
                    Ok(written) => {
                        source = &source[written..];
                        progressed = written > 0;
                    }
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => source = &[],
                    Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) => {}
                    Err(_) => return Err(Failure::Invalid),
                }
            }
            if source.is_empty() {
                stdin = None;
            }
        }
        progressed |= drain(&mut stdout, &mut out, stdout_limit)?;
        progressed |= drain(&mut stderr, &mut err, MAX_DIAGNOSTIC_BYTES)?;
        if stdout.is_none() && stderr.is_none() && child.exited()? {
            break;
        }
        if !progressed {
            thread::sleep(POLL_INTERVAL);
        }
    }
    let status = child.finish().map_err(|_| Failure::Invalid)?;
    if status.success() {
        return match key.format {
            Format::Unicode => literal_rows(&out).map(Output::Rows),
            Format::Png => validate_png(&out).map(|()| Output::Png(out.into())),
        };
    }

    let diagnostic = String::from_utf8_lossy(&err).to_ascii_lowercase();
    Err(if diagnostic.contains("resource limit") {
        Failure::TooLarge
    } else if diagnostic.contains("unsupported")
        || diagnostic.contains("not supported")
        || diagnostic.contains("does not support")
    {
        Failure::Unsupported
    } else {
        Failure::Invalid
    })
}

#[cfg(unix)]
struct RendererChild(Option<Child>);

#[cfg(unix)]
impl RendererChild {
    fn exited(&self) -> Result<bool, Failure> {
        let child = self.0.as_ref().expect("unreaped renderer");
        // SAFETY: waitid initializes the record on success. A zero pid means
        // there is nothing to report. WNOWAIT keeps the group ID reserved
        // until finish signals any descendants and reaps the group leader.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(libc::P_PID, child.id() as libc::id_t, &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
        };
        if result == 0 {
            Ok(unsafe { info.si_pid() } != 0)
        } else if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(Failure::Invalid)
        }
    }

    fn finish(&mut self) -> io::Result<ExitStatus> {
        let mut child = self.0.take().expect("unreaped renderer");
        // SAFETY: process_group(0) gave the unreaped child its own group.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
        let _ = child.kill();
        child.wait()
    }
}

#[cfg(unix)]
impl Drop for RendererChild {
    fn drop(&mut self) {
        if self.0.is_some() {
            let _ = self.finish();
        }
    }
}

#[cfg(unix)]
fn nonblocking(pipe: &impl AsRawFd) -> Result<(), Failure> {
    // SAFETY: the parent owns this end of the pipe. The renderer has the
    // other end, which keeps its normal blocking semantics.
    if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
        return Err(Failure::Unavailable);
    }
    Ok(())
}

#[cfg(unix)]
fn drain(pipe: &mut Option<impl Read>, output: &mut Vec<u8>, limit: usize) -> Result<bool, Failure> {
    let Some(reader) = pipe else { return Ok(false) };
    let mut buffer = [0; 8192];
    // Read at most one chunk per turn so a busy stream cannot starve stdin,
    // cancellation, timeouts, or the other output stream.
    match reader.read(&mut buffer) {
        Ok(0) => {
            *pipe = None;
            Ok(false)
        }
        Ok(n) if n <= limit - output.len() => {
            output.extend_from_slice(&buffer[..n]);
            Ok(true)
        }
        Ok(_) => Err(Failure::TooLarge),
        Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) => Ok(false),
        Err(_) => Err(Failure::Invalid),
    }
}

/// Accept the static 8-bit RGBA PNG emitted by Merman. Dimensions are
/// bounded before decompression. The chunk whitelist excludes metadata,
/// animation, and any trailing data.
#[cfg(unix)]
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

    if !valid_png_crc(&header[12..]) {
        return Err(Failure::InvalidPng);
    }

    let mut chunks = &bytes[33..];
    let mut compressed = Vec::new();
    while let Some(header) = chunks.get(..8) {
        let size = u32::from_be_bytes(header[..4].try_into().expect("chunk size")) as usize;
        let end = size.checked_add(12).ok_or(Failure::InvalidPng)?;
        let chunk = chunks.get(..end).ok_or(Failure::InvalidPng)?;
        if !valid_png_crc(&chunk[4..]) {
            return Err(Failure::InvalidPng);
        }
        chunks = &chunks[end..];
        match &header[4..8] {
            b"IDAT" => compressed.extend_from_slice(&chunk[8..end - 4]),
            b"IEND" if size == 0 && chunks.is_empty() && !compressed.is_empty() => {
                return decode_scanlines(&compressed, info.width, info.height);
            }
            _ => break,
        }
    }
    Err(Failure::InvalidPng)
}

#[cfg(unix)]
#[link(name = "z")]
unsafe extern "C" {
    fn crc32(crc: libc::c_ulong, bytes: *const u8, len: libc::c_uint) -> libc::c_ulong;
    fn uncompress(dest: *mut u8, len: *mut libc::c_ulong, source: *const u8, size: libc::c_ulong) -> libc::c_int;
}

#[cfg(unix)]
fn valid_png_crc(chunk: &[u8]) -> bool {
    let (bytes, checksum) = chunk.split_at(chunk.len() - 4);
    let expected = u32::from_be_bytes(checksum.try_into().expect("PNG checksum"));
    // SAFETY: the bytes are valid for the supplied length, capped at 1 MiB.
    unsafe { crc32(0, bytes.as_ptr(), bytes.len() as libc::c_uint) == expected.into() }
}

#[cfg(unix)]
fn decode_scanlines(compressed: &[u8], width: u32, height: u32) -> Result<(), Failure> {
    // Each row has one filter byte and one byte per channel. Every filter is
    // reversible for a complete row, so there is no need to retain pixels.
    let stride = 1 + width as usize * 4;
    let mut filtered = vec![0; stride * height as usize];
    let mut len = filtered.len() as libc::c_ulong;
    // SAFETY: both slices are live and disjoint, and the arguments give zlib
    // their full lengths. zlib checks the deflate stream and Adler checksum.
    let status = unsafe {
        uncompress(filtered.as_mut_ptr(), &mut len, compressed.as_ptr(), compressed.len() as libc::c_ulong)
    };
    if status != 0 || len as usize != filtered.len()
        || filtered.chunks_exact(stride).any(|row| row[0] > 4)
    {
        return Err(Failure::InvalidPng);
    }
    Ok(())
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
        render(&key, timeout, &AtomicBool::new(false), send);
        receive.recv().unwrap().map(Output::rows)
    }

    #[test]
    fn passes_literal_stdin_and_reuses_exact_source_and_executable() {
        let fake = FakeRenderer::new(
            r#"[ "$*" = 'render - --format unicode --ascii-color plain --resource-profile constrained --output -' ] || exit 2
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

        let png_snapshot = renderer.with_cached_pngs();
        assert!(png_snapshot.jobs.is_empty());
        assert_eq!(png_snapshot.cached_png(&executable, source), Some(PicturePreview::Ready(TEST_PNG.into())));
        assert_eq!(png_snapshot.cached(&executable, source), None);

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
    fn rejects_corrupt_compressed_pixels() {
        let mut corrupt = TEST_PNG.to_vec();
        corrupt[41..52].fill(0);
        let checksum = unsafe { crc32(0, corrupt[37..52].as_ptr(), 15) } as u32;
        corrupt[52..56].copy_from_slice(&checksum.to_be_bytes());
        assert_eq!(validate_png(&corrupt), Err(Failure::InvalidPng));
    }

    #[test]
    fn dropping_renderer_kills_and_reaps_active_child() {
        let fake = FakeRenderer::new(r#"echo $$ > "$(dirname "$0")/pid"; exec sleep 30"#);
        let mut renderer = MermaidRenderer::default();
        renderer.request(&fake.executable(), "");
        let deadline = Instant::now() + TIMEOUT;
        let pid = loop {
            if let Ok(pid) = fs::read_to_string(fake.0.join("pid")) {
                if !pid.trim().is_empty() {
                    break pid;
                }
            }
            assert!(Instant::now() < deadline, "renderer did not start");
            thread::sleep(POLL_INTERVAL);
        };
        drop(renderer);
        let alive = Command::new("kill").args(["-0", pid.trim()])
            .stderr(Stdio::null()).status().unwrap().success();
        if alive {
            let _ = Command::new("kill").args(["-9", pid.trim()]).status();
        }
        assert!(!alive, "renderer survived drop");
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
    fn transient_failures_expire_and_can_be_retried() {
        let fake = FakeRenderer::new("cat");
        let executable = fake.executable();
        let source = "graph LR";
        let mut renderer = MermaidRenderer::default();
        let cache = &mut renderer.caches[Format::Unicode as usize];
        let key = Arc::new(Key { executable: executable.clone(), source: source.into(), format: Format::Unicode });
        remember(cache, key, Rendered::Failed(Failure::MissingExecutable));
        assert!(cache[0].2.is_some());
        cache[0].2 = Some(Instant::now());
        assert!(renderer.poll());
        assert_eq!(renderer.cached(&executable, source), None);
        assert_eq!(renderer.request(&executable, source), Preview::Pending);
        settle(&mut renderer);
        assert_eq!(renderer.cached(&executable, source), Some(Preview::Ready(vec![source.into()].into())));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn detached_descendant_holding_pipes_does_not_keep_a_job_slot() {
        let fake = FakeRenderer::new(r#"
setsid sh -c 'echo $$ > "$1"; exec sleep 30' sh "$0.detached" &
wait
"#);
        let start = Instant::now();
        let result = run(fake.executable(), Duration::from_millis(200));
        if let Ok(pid) = fs::read_to_string(fake.0.join("renderer.detached")) {
            let _ = Command::new("kill").args(["-9", pid.trim()]).status();
        }
        assert_eq!(result, Preview::Failed(Failure::Timeout));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn maps_merman_resource_limit_errors_to_too_large() {
        let fake = FakeRenderer::new("printf 'ASCII resource limit exceeded\n' >&2; exit 1");
        assert_eq!(run(fake.executable(), TIMEOUT), Preview::Failed(Failure::TooLarge));
    }

    #[test]
    fn reports_safe_failures() {
        let fake = FakeRenderer::new("printf 'ASCII rendering does not support diagram type pie\n' >&2; exit 1");
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
