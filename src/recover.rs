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
//! A recovery file is never overwritten: it may be the only copy of that text,
//! and a second crash before anyone looked at the first must not take it. The
//! next one beside the same file is `<file>.shoin-recover-2`, then `-3`, and
//! so on; the unnamed ones skip any `<n>` already taken.
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
use std::sync::atomic::AtomicI32;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};

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
        // One notice per pass: any other file is looked at on the next one,
        // after this flash has had its turn.
        match recovery_files(path).as_slice() {
            [] => {}
            [one] => return Some(format!("{} holds unsaved text from a crash", one.display())),
            several => {
                // Names only: they all sit beside the open file, and a full
                // path each would push the later ones off a flash that is cut
                // to the width of the screen.
                let names: Vec<_> =
                    several.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy()).collect();
                return Some(format!("unsaved text from a crash, newest first: {}", names.join(", ")));
            }
        }
    }
    None
}

/// Every recovery file beside `path`, newest first: `<file>.shoin-recover`
/// and any `<file>.shoin-recover-<n>`. Newest is by the time each was written,
/// not by `<n>`: once the plain name is deleted, the next crash takes it again.
pub fn recovery_files(path: &Path) -> Vec<PathBuf> {
    let first = beside(path);
    let Some(name) = first.file_name() else { return Vec::new() };
    let name = name.as_encoded_bytes();
    let dir = match first.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let file = entry.file_name();
        let n = match file.as_encoded_bytes().strip_prefix(name) {
            Some([]) => 1,
            Some([b'-', digits @ ..]) if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) => {
                std::str::from_utf8(digits).ok().and_then(|d| d.parse::<u64>().ok()).unwrap_or(u64::MAX)
            }
            _ => continue,
        };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let written = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        found.push((written, n, dir.join(file)));
    }
    found.sort_by_key(|a| std::cmp::Reverse((a.0, a.1)));
    found.into_iter().map(|(_, _, p)| p).collect()
}

/// The `n`th recovery file name beside `path`: the plain one first, so a
/// single crash still leaves `notes.md.shoin-recover`.
fn beside_nth(path: &Path, n: u32) -> PathBuf {
    let first = beside(path);
    if n <= 1 {
        return first;
    }
    let mut name = first.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!("-{n}"));
    first.with_file_name(name)
}

/// How many names [`dump_to`] tries before it gives a file up: past this many
/// unread recovery files, something else is wrong.
const MAX_TRIES: u32 = 1000;

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
    // Shared by every unnamed buffer of this dump, and moved past any name a
    // dead editor with the same pid left behind.
    let mut unnamed = 0;
    let pid = std::process::id();
    for s in snaps {
        if let Some(path) = &s.path {
            if let Some(target) = write_new(s, (1..=MAX_TRIES).map(|n| beside_nth(path, n))) {
                written.push(target);
                continue;
            }
        }
        if std::fs::create_dir_all(state).is_err() {
            continue;
        }
        let names = std::iter::from_fn(|| {
            unnamed += 1;
            Some(state.join(format!("recover-{pid}-{unnamed}.md")))
        });
        if let Some(target) = write_new(s, names.take(MAX_TRIES as usize)) {
            written.push(target);
        }
    }
    written
}

/// Write `s` to the first of `names` that does not exist yet. Any failure
/// other than a taken name ends the search: the folder refuses the write.
fn write_new(s: &Snapshot, names: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    for target in names {
        match write(&target, s) {
            Ok(()) => return Some(target),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

/// Write one snapshot to a file that must not exist yet, readable by its
/// owner only: it is somebody's draft.
fn write(target: &Path, s: &Snapshot) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    // `create_new` is atomic: a name taken between the check and the write is
    // still never overwritten.
    opts.write(true).create_new(true);
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

// ------------------------------------------------------------------ ending
//
// The signal thread ends the editor while the event loop may be halfway
// through a frame. Restoring the terminal under a frame lands the rest of that
// frame on the user's shell, and a frame drawn after it does the same. So the
// loop writes to the terminal only while it holds [`frame`], and the signal
// thread, through [`stop`], keeps any new frame from starting and waits for the
// one in flight before it touches the terminal. The same claim makes the
// ending happen once: whichever of the signal thread and the loop's own exit
// claims it first dumps and restores; the other stands aside.

const RUNNING: u8 = 0;
const QUITTING: u8 = 1;
const SIGNALLED: u8 = 2;

/// Who is ending the editor, and the lock a frame holds while it is written.
struct Gate {
    ending: AtomicU8,
    frame: Mutex<()>,
}

impl Gate {
    const fn new() -> Gate {
        Gate { ending: AtomicU8::new(RUNNING), frame: Mutex::new(()) }
    }

    fn frame(&self) -> Option<MutexGuard<'_, ()>> {
        let guard = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        // Read under the lock: `stop` sets the state before it takes the lock,
        // so a frame that gets the lock after `stop` has waited sees it.
        (self.ending.load(Ordering::SeqCst) != SIGNALLED).then_some(guard)
    }

    fn claim(&self, by: u8) -> bool {
        self.ending.compare_exchange(RUNNING, by, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }

    fn stop(&self) -> bool {
        if !self.claim(SIGNALLED) {
            return false;
        }
        // Bounded, like `dump`: a frame stuck on a terminal that stopped
        // reading must not keep the unsaved text from its file.
        for _ in 0..50 {
            match self.frame.try_lock() {
                Ok(_) | Err(TryLockError::Poisoned(_)) => break,
                Err(TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        true
    }
}

static GATE: Gate = Gate::new();

/// Held by the event loop while it writes to the terminal. `None`
/// once a signal is ending the editor: the loop must then stop writing and
/// return, and leave the terminal to the signal thread.
pub fn frame() -> Option<MutexGuard<'static, ()>> {
    GATE.frame()
}

/// For the signal thread: claim the ending, keep frames from starting and wait
/// (briefly) for the one being written. `false` when the loop's own exit got
/// there first; that path dumps and restores, so the signal thread does not.
pub fn stop() -> bool {
    GATE.stop()
}

/// For the event loop's own exit: claim the ending. `false` when a signal got
/// there first; its thread then dumps, restores and exits the process.
pub fn quit() -> bool {
    GATE.claim(QUITTING)
}

/// Whether a signal is ending the editor.
pub fn signalled() -> bool {
    GATE.ending.load(Ordering::SeqCst) == SIGNALLED
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

    #[test]
    fn a_second_crash_never_overwrites_the_first_recovery_file() {
        let d = dir("twice");
        let path = d.join("t.md");
        std::fs::write(&path, "saved\n").unwrap();
        let first = dump_to(&[snap(Some(path.clone()), "first crash\n", false)], &d.join("state"));
        let second = dump_to(&[snap(Some(path.clone()), "second crash\n", false)], &d.join("state"));
        assert_eq!(first, vec![d.join("t.md.shoin-recover")]);
        assert_eq!(second, vec![d.join("t.md.shoin-recover-2")]);
        assert_eq!(std::fs::read_to_string(&first[0]).unwrap(), "first crash\n");
        assert_eq!(std::fs::read_to_string(&second[0]).unwrap(), "second crash\n");
        assert_eq!(recovery_files(&path), vec![second[0].clone(), first[0].clone()], "newest first");
        // Nothing else in the folder counts: not the file, not another
        // file's recovery, not a name that only starts the same way.
        std::fs::write(d.join("t.md.shoin-recover-x"), "").unwrap();
        std::fs::write(d.join("t.md.shoin-recover-"), "").unwrap();
        std::fs::write(d.join("tt.md.shoin-recover"), "").unwrap();
        assert_eq!(recovery_files(&path).len(), 2);

        let b = Buffer::open(path.clone(), &[]).unwrap();
        let text = notice([&b].into_iter()).expect("a notice");
        assert_eq!(text, "unsaved text from a crash, newest first: t.md.shoin-recover-2, t.md.shoin-recover");

        // With the plain name deleted, the next crash takes it again, and it
        // is still listed first: newest is by time, not by number.
        std::fs::remove_file(&first[0]).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&second[0]).unwrap().set_modified(old).unwrap();
        let third = dump_to(&[snap(Some(path.clone()), "third crash\n", false)], &d.join("state"));
        assert_eq!(third, vec![d.join("t.md.shoin-recover")]);
        assert_eq!(recovery_files(&path), vec![third[0].clone(), second[0].clone()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unnamed_buffer_skips_a_name_an_earlier_crash_left() {
        let d = dir("unnamed-twice");
        let state = d.join("state");
        let pid = std::process::id();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join(format!("recover-{pid}-1.md")), "earlier\n").unwrap();
        let written = dump_to(&[snap(None, "a\n", false), snap(None, "b\n", false)], &state);
        assert_eq!(written, vec![state.join(format!("recover-{pid}-2.md")), state.join(format!("recover-{pid}-3.md"))]);
        assert_eq!(std::fs::read_to_string(state.join(format!("recover-{pid}-1.md"))).unwrap(), "earlier\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_signal_stops_frames_before_the_terminal_is_touched() {
        let gate = Gate::new();
        assert!(gate.frame().is_some(), "frames run until something ends the editor");
        assert!(gate.stop(), "the first to end the editor claims it");
        assert!(gate.frame().is_none(), "no frame starts once a signal has claimed the ending");
        assert!(!gate.stop() && !gate.claim(QUITTING), "and it is ended once");

        let gate = Gate::new();
        assert!(gate.claim(QUITTING));
        assert!(!gate.stop(), "a loop already on its way out restores by itself");
        assert!(gate.frame().is_some());
    }

    #[test]
    fn a_signal_waits_for_the_frame_being_written() {
        static GATE: Gate = Gate::new();
        let frame = GATE.frame().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let stopper = std::thread::spawn(move || {
            assert!(GATE.stop());
            tx.send(()).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(rx.try_recv().is_err(), "stop returned while a frame was still being written");
        drop(frame);
        rx.recv_timeout(std::time::Duration::from_secs(2)).expect("stop returns once the frame is done");
        stopper.join().unwrap();
        assert!(GATE.frame().is_none());
    }
}
