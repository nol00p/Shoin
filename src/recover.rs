//! Crash recovery: the unsaved text of a dying editor goes to a file.
//!
//! Shoin writes nothing you did not ask for while it runs. The one exception is
//! when it dies with unsaved work: a panic, `SIGTERM`, or `SIGHUP` (the terminal
//! window closed). Then each modified buffer is written to
//! `<file>.shoin-recover` beside its file, or to
//! `$XDG_STATE_HOME/shoin/recover-<pid>-<n>.md` for a buffer with no name (or
//! a folder that refuses the write). The paths go to stderr. Nothing ever
//! applies a recovery file: opening a file that has one beside it only says so.
//!
//! The panic hook has no `App` to read, so the event loop keeps a process-wide
//! [`REGISTRY`] of the modified buffers up to date through [`tick`]. A rope
//! clone is a reference-count bump that shares its chunks, and it is taken
//! only when a buffer's revision moved, so an idle loop pays one comparison
//! per modified buffer.
//!
//! A signal handler may do almost nothing safely (no allocation, no lock), so
//! [`on_signal`] only writes the signal's number down a socket. A thread that
//! sleeps on the other end does the work in ordinary code: dump, restore the
//! terminal, exit. It cannot be the event loop: after `SIGHUP` the terminal is
//! gone, and crossterm's reader spins on the dead tty without ever returning.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, TryLockError};

use ropey::Rope;

use crate::text::buffer::{Buffer, LineEnding};

/// What a recovery file is called: `<file>.shoin-recover`.
pub const SUFFIX: &str = ".shoin-recover";

/// The text a dying editor would lose.
struct Snapshot {
    path: Option<PathBuf>,
    rope: Rope,
    crlf: bool,
    revision: u64,
}

impl Snapshot {
    fn of(b: &Buffer) -> Snapshot {
        Snapshot {
            path: b.path.clone(),
            rope: b.rope.clone(),
            crlf: b.line_ending == LineEnding::Crlf,
            revision: b.revision,
        }
    }

    fn is_of(&self, b: &Buffer) -> bool {
        self.revision == b.revision && self.rope.len_chars() == b.rope.len_chars() && self.path == b.path
    }
}

/// The modified buffers as of the last [`tick`].
static REGISTRY: Mutex<Vec<Snapshot>> = Mutex::new(Vec::new());

/// Files already checked for a recovery file beside them, so each one is
/// looked for once per session rather than once per tick.
static CHECKED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Bring the registry up to date with `buffers`, and look for a recovery file
/// beside any file not seen before: `Some` is the notice to show. The event
/// loop calls this once per pass.
pub fn tick<'a>(buffers: impl Iterator<Item = &'a Buffer> + Clone) -> Option<String> {
    refresh(buffers.clone());
    notice(buffers)
}

fn refresh<'a>(buffers: impl Iterator<Item = &'a Buffer>) {
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let mut n = 0;
    for b in buffers.filter(|b| b.modified) {
        match reg.get_mut(n) {
            Some(s) if s.is_of(b) => {}
            Some(s) => *s = Snapshot::of(b),
            None => reg.push(Snapshot::of(b)),
        }
        n += 1;
    }
    reg.truncate(n);
}

fn notice<'a>(buffers: impl Iterator<Item = &'a Buffer>) -> Option<String> {
    let mut checked = CHECKED.lock().unwrap_or_else(|e| e.into_inner());
    let checked = checked.get_or_insert_with(HashSet::new);
    for path in buffers.filter_map(|b| b.path.as_deref()) {
        if checked.contains(path) {
            continue;
        }
        checked.insert(path.to_path_buf());
        let beside = beside(path);
        // One notice per pass: any other file is looked at on the next one,
        // after this flash has had its turn.
        if beside.is_file() {
            return Some(format!("{} holds unsaved text from a crash", beside.display()));
        }
    }
    None
}

/// `notes.md` → `notes.md.shoin-recover`, in the same folder.
pub fn beside(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(SUFFIX);
    path.with_file_name(name)
}

/// Where a buffer with no file, or a file in a folder that refuses the write,
/// is recovered to.
fn state_dir() -> PathBuf {
    let base = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => match std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            Some(home) => PathBuf::from(home).join(".local/state"),
            None => std::env::temp_dir(),
        },
    };
    base.join("shoin")
}

/// Write every buffer the registry holds and return where each went. Called
/// from the panic hook and after a signal, never while the editor runs.
///
/// The panic may have happened on another thread while the loop held the
/// lock, so it is tried for a moment rather than waited on: a dump that gives
/// up is better than a hook that never returns.
pub fn dump() -> Vec<PathBuf> {
    let mut tries = 0;
    let reg = loop {
        match REGISTRY.try_lock() {
            Ok(reg) => break reg,
            Err(TryLockError::Poisoned(e)) => break e.into_inner(),
            Err(TryLockError::WouldBlock) if tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(TryLockError::WouldBlock) => return Vec::new(),
        }
    };
    dump_to(&reg, &state_dir())
}

fn dump_to(snaps: &[Snapshot], state: &Path) -> Vec<PathBuf> {
    let mut written = Vec::new();
    let mut unnamed = 0;
    for s in snaps {
        if let Some(path) = &s.path {
            let target = beside(path);
            if write(&target, s).is_ok() {
                written.push(target);
                continue;
            }
        }
        unnamed += 1;
        let target = state.join(format!("recover-{}-{unnamed}.md", std::process::id()));
        if std::fs::create_dir_all(state).is_ok() && write(&target, s).is_ok() {
            written.push(target);
        }
    }
    written
}

/// Write one snapshot, readable by its owner only: it is somebody's draft.
fn write(target: &Path, s: &Snapshot) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut out = std::io::BufWriter::new(opts.open(target)?);
    for chunk in s.rope.chunks() {
        if s.crlf {
            out.write_all(chunk.replace('\n', "\r\n").as_bytes())?;
        } else {
            out.write_all(chunk.as_bytes())?;
        }
    }
    out.into_inner().map_err(|e| e.into_error())?.sync_all()
}

/// Print where the text went, after the terminal is back to normal. Not
/// `eprintln!`: after `SIGHUP` stderr is a dead terminal, and a failed print
/// must not become a panic.
pub fn report(written: &[PathBuf]) {
    let mut err = std::io::stderr().lock();
    for p in written {
        let _ = writeln!(err, "shoin: unsaved text written to {}", p.display());
    }
}

// ------------------------------------------------------------------ signals
//
// Three libc calls, declared here rather than pulled in through a crate: std
// already links libc, and both signal numbers are fixed by POSIX (they are what
// `kill -1` and `kill -15` mean on every Unix).

#[cfg(unix)]
mod sys {
    pub const SIGHUP: i32 = 1;
    pub const SIGTERM: i32 = 15;
    /// `SIG_DFL`: the signal's default action, which for these two is to end
    /// the process.
    pub const DEFAULT: usize = 0;

    use std::ffi::{c_int, c_void};

    extern "C" {
        /// `sighandler_t signal(int, sighandler_t)`; a handler is a pointer.
        pub fn signal(signum: c_int, handler: usize) -> usize;
        pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    }
}

/// The write end of the socket the signal thread sleeps on, or -1.
#[cfg(unix)]
static WAKE: AtomicI32 = AtomicI32::new(-1);

/// Only async-signal-safe work here (POSIX lists both `write` and `signal`):
/// one byte down the socket, and the default action back, so a second
/// `kill -TERM` still ends an editor whose signal thread is stuck. Neither
/// call changes `errno` when it succeeds, and a one-byte write to an empty
/// socket does.
#[cfg(unix)]
extern "C" fn on_signal(sig: i32) {
    let fd = WAKE.load(Ordering::Relaxed);
    let byte = sig as u8;
    // SAFETY: both calls are async-signal-safe; `byte` outlives the write.
    unsafe {
        if fd >= 0 {
            sys::write(fd, (&byte as *const u8).cast(), 1);
        }
        sys::signal(sig, sys::DEFAULT);
    }
}

/// Catch `SIGTERM` and `SIGHUP`. When one arrives, `on_stop` runs on a thread
/// of its own with the signal's number, and is expected to end the process.
pub fn catch_signals(on_stop: impl FnOnce(i32) + Send + 'static) {
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::fd::IntoRawFd;
        use std::os::unix::net::UnixStream;
        let Ok((mut rx, tx)) = UnixStream::pair() else { return };
        // Never closed: the handler may write to it until the process ends.
        WAKE.store(tx.into_raw_fd(), Ordering::Relaxed);
        let spawned = std::thread::Builder::new().name("signals".into()).spawn(move || {
            let mut sig = [0u8];
            if rx.read_exact(&mut sig).is_ok() {
                on_stop(i32::from(sig[0]));
            }
        });
        if spawned.is_err() {
            return;
        }
        // SAFETY: `on_signal` is an `extern "C" fn(i32)` doing only
        // async-signal-safe work.
        unsafe {
            let handler = on_signal as extern "C" fn(i32) as usize;
            sys::signal(sys::SIGTERM, handler);
            sys::signal(sys::SIGHUP, handler);
        }
    }
    #[cfg(not(unix))]
    let _ = on_stop;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("shoin-recover-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn snap(path: Option<PathBuf>, text: &str, crlf: bool) -> Snapshot {
        Snapshot { path, rope: Rope::from_str(text), crlf, revision: 1 }
    }

    #[test]
    fn two_modified_buffers_are_written_beside_their_files() {
        let d = dir("two");
        let snaps = [
            snap(Some(d.join("a.md")), "# A\n\nunsaved line\n", false),
            snap(Some(d.join("b.txt")), "one\ntwo\n", true),
        ];
        let written = dump_to(&snaps, &d.join("state"));
        assert_eq!(written, vec![d.join("a.md.shoin-recover"), d.join("b.txt.shoin-recover")]);
        assert_eq!(std::fs::read_to_string(&written[0]).unwrap(), "# A\n\nunsaved line\n");
        assert_eq!(std::fs::read_to_string(&written[1]).unwrap(), "one\r\ntwo\r\n");
        assert!(!d.join("a.md").exists(), "the file itself is never touched");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&written[0]).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unnamed_buffer_or_a_refused_folder_goes_to_the_state_folder() {
        let d = dir("unnamed");
        let state = d.join("state");
        let snaps = [
            snap(None, "piped text\n", false),
            snap(Some(d.join("no-such-folder/c.md")), "c\n", false),
        ];
        let written = dump_to(&snaps, &state);
        let pid = std::process::id();
        assert_eq!(written, vec![state.join(format!("recover-{pid}-1.md")), state.join(format!("recover-{pid}-2.md"))]);
        assert_eq!(std::fs::read_to_string(&written[0]).unwrap(), "piped text\n");
        assert_eq!(std::fs::read_to_string(&written[1]).unwrap(), "c\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_registry_holds_modified_buffers_only() {
        let d = dir("registry");
        let mut clean = Buffer::open(d.join("clean.md"), &[]).unwrap();
        clean.modified = false;
        let mut dirty = Buffer::open(d.join("dirty.md"), &[]).unwrap();
        dirty.insert_str(crate::text::cursor::Cursor::new(0, 0), "typed");
        assert!(dirty.modified);
        refresh([&clean, &dirty].into_iter());
        {
            let reg = REGISTRY.lock().unwrap();
            assert_eq!(reg.len(), 1);
            assert_eq!(reg[0].path.as_deref(), Some(&*d.join("dirty.md")));
            assert_eq!(reg[0].rope.to_string(), "typed");
        }
        dirty.insert_str(crate::text::cursor::Cursor::new(0, 5), " more");
        refresh([&clean, &dirty].into_iter());
        assert_eq!(REGISTRY.lock().unwrap()[0].rope.to_string(), "typed more");
        refresh(std::iter::empty());
        assert!(REGISTRY.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_recovery_file_beside_an_open_file_is_named_once() {
        let d = dir("notice");
        std::fs::write(d.join("n.md"), "saved\n").unwrap();
        std::fs::write(d.join("n.md.shoin-recover"), "unsaved\n").unwrap();
        std::fs::write(d.join("m.md.shoin-recover"), "unsaved\n").unwrap();
        let n = Buffer::open(d.join("n.md"), &[]).unwrap();
        let m = Buffer::open(d.join("m.md"), &[]).unwrap();
        let first = notice([&n, &m].into_iter()).expect("a notice");
        assert!(first.ends_with("n.md.shoin-recover holds unsaved text from a crash"), "{first}");
        let second = notice([&n, &m].into_iter()).expect("the other file's turn");
        assert!(second.contains("m.md.shoin-recover"), "{second}");
        assert_eq!(notice([&n, &m].into_iter()), None, "each named once per session");
        let _ = std::fs::remove_dir_all(&d);
    }
}
