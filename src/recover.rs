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
//! next one beside the same file is `<file>.shoin-recover-2`, then `-3`, each
//! one past the highest number already there; the unnamed ones skip any `<n>`
//! already taken. Each is written in full under a `.partial` name first, so a
//! dump cut off part way never leaves a recovery file missing its end.
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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
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
        // Two stats for a file with no recovery file, the common case: the
        // folder is listed only when the first or second name is there. A
        // lone `-3` left after deleting both is not announced, but a new crash
        // re-takes the free plain name (`dump_to`), so the newest always is.
        if !beside_nth(path, 1).exists() && !beside_nth(path, 2).exists() {
            continue;
        }
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
/// and any `<file>.shoin-recover-<n>`. Newest is by the time each was
/// written, then by `<n>`: a new one takes the plain name when it is free and
/// otherwise the number past the highest already there, so a tie of coarse
/// timestamps still puts it first unless it re-took the plain name (which
/// needs the user to have deleted the older files in between).
pub fn recovery_files(path: &Path) -> Vec<PathBuf> {
    let mut found: Vec<_> = existing(path)
        .into_iter()
        .map(|(n, p, meta)| (meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH), n, p))
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse((a.0, a.1)));
    found.into_iter().map(|(_, _, p)| p).collect()
}

/// The recovery files beside `path` with their numbers. From a listing of the
/// folder, which sees every number however many were deleted in between; a
/// folder that lets you reach its files but not list them is asked name by
/// name instead.
fn existing(path: &Path) -> Vec<(u32, PathBuf, std::fs::Metadata)> {
    listed(path).unwrap_or_else(|| probe(path))
}

fn listed(path: &Path) -> Option<Vec<(u32, PathBuf, std::fs::Metadata)>> {
    let first = beside(path);
    let name = first.file_name()?.as_encoded_bytes().to_vec();
    let dir = match first.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let file = entry.file_name();
        let n = match file.as_encoded_bytes().strip_prefix(&name[..]) {
            Some([]) => 1,
            // Only the names `beside_nth` makes: `-2` and up, no leading zero.
            Some([b'-', digits @ ..]) if digits.first().is_some_and(|d| (b'1'..=b'9').contains(d)) => {
                match std::str::from_utf8(digits).ok().and_then(|d| d.parse::<u32>().ok()) {
                    Some(n) if n >= 2 => n,
                    _ => continue,
                }
            }
            _ => continue,
        };
        // `metadata` follows a link, as opening the file would.
        match std::fs::metadata(dir.join(&file)) {
            Ok(meta) if meta.is_file() => found.push((n, dir.join(&file), meta)),
            _ => {}
        }
    }
    Some(found)
}

/// Name by name, for a folder that cannot be listed: the search ends after
/// [`MISSES`] free numbers in a row, so files past a longer gap stay unseen
/// there.
fn probe(path: &Path) -> Vec<(u32, PathBuf, std::fs::Metadata)> {
    let mut found = Vec::new();
    let mut misses = 0;
    for n in 1..=MAX_TRIES {
        let p = beside_nth(path, n);
        match std::fs::metadata(&p) {
            Ok(meta) if meta.is_file() => {
                misses = 0;
                found.push((n, p, meta));
            }
            // A folder of that name is no recovery file, but the number is taken.
            Ok(_) => misses = 0,
            Err(_) => {
                misses += 1;
                if misses == MISSES {
                    break;
                }
            }
        }
    }
    found
}

/// How many free numbers in a row end the search in a folder that cannot be
/// listed.
const MISSES: u32 = 8;

/// The `n`th recovery file name beside `path`: the plain one first, so a
/// single crash still leaves `notes.md.shoin-recover`.
fn beside_nth(path: &Path, n: u32) -> PathBuf {
    let first = beside(path);
    if n <= 1 {
        return first;
    }
    with_suffix(&first, &format!("-{n}"))
}

/// How many names [`dump_to`] tries before it gives a file up: past this many
/// unread recovery files, something else is wrong.
const MAX_TRIES: u32 = 1000;

/// `notes.md` → `notes.md.shoin-recover`, in the same folder.
pub fn beside(path: &Path) -> PathBuf {
    with_suffix(path, SUFFIX)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(suffix);
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

/// Every recovery file written so far in this process, and the lines kept for
/// later: [`report`] prints both once the terminal is back. Kept here rather
/// than handed back, so a dump cut short by a panic still reports the files it
/// finished.
static WRITTEN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Write every buffer the registry holds. Called when the editor dies, and
/// for a safety copy after a panic that leaves it running (a background
/// thread, in a build that unwinds).
///
/// The panic may have happened on another thread while the loop held the
/// lock, so it is tried for a moment rather than waited on: a dump that gives
/// up is better than a hook that never returns.
pub fn dump() {
    DUMPING.fetch_add(1, Ordering::SeqCst);
    dump_registry();
    DUMPING.fetch_sub(1, Ordering::SeqCst);
}

/// How many dumps are under way: a signal that lost the ending to another
/// owner waits while it is writing.
static DUMPING: AtomicU32 = AtomicU32::new(0);

fn dump_registry() {
    let mut tries = 0;
    let reg = loop {
        match REGISTRY.try_lock() {
            Ok(reg) => break reg,
            Err(TryLockError::Poisoned(e)) => break e.into_inner(),
            Err(TryLockError::WouldBlock) if tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(TryLockError::WouldBlock) => return,
        }
    };
    dump_to(&reg, &state_dir(), &mut |p| WRITTEN.lock().unwrap_or_else(|e| e.into_inner()).push(p.to_path_buf()));
}

/// Write `snaps`, handing each file to `done` as soon as it is complete.
fn dump_to(snaps: &[Snapshot], state: &Path, done: &mut dyn FnMut(&Path)) -> Vec<PathBuf> {
    let mut written = Vec::new();
    // Shared by every unnamed buffer of this dump, and moved past any name a
    // dead editor with the same pid left behind.
    let mut unnamed = 0;
    let pid = std::process::id();
    for s in snaps {
        if let Some(path) = &s.path {
            // The plain name when it is free, so `notice` (which looks only
            // when the plain name or `-2` is there) always finds the newest.
            // Otherwise past the highest number there, never into a gap below
            // it: see `recovery_files` for why.
            let next = existing(path).iter().map(|e| e.0).max().map_or(1, |n| n.saturating_add(1)).max(2);
            let numbers = std::iter::once(1).chain(next..next.saturating_add(MAX_TRIES));
            let partial = with_suffix(&beside(path), &format!(".partial-{pid}"));
            let names = numbers.map(|n| beside_nth(path, n));
            if let Some(target) = write_new(s, &partial, names) {
                done(&target);
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
        let partial = state.join(format!("recover.partial-{pid}"));
        if let Some(target) = write_new(s, &partial, names.take(MAX_TRIES as usize)) {
            done(&target);
            written.push(target);
        }
    }
    written
}

/// Write `s` in full to `partial`, then give it the first of `names` not
/// taken yet. A dump cut off part way (the process ends under it) leaves at
/// most the `.partial` file, never a recovery file that is missing its end.
///
/// The name is given by a hard link, which fails on a taken name rather than
/// replacing it, as a rename would. Where links are refused (FAT, some network
/// mounts), a rename to a name just seen free does it instead.
fn write_new(s: &Snapshot, partial: &Path, names: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    // A `.partial` left by an earlier dump of this pid that was cut off goes:
    // that dump's recovery files are named in full or not at all.
    let _ = std::fs::remove_file(partial);
    if write(partial, s).is_err() {
        let _ = std::fs::remove_file(partial);
        return None;
    }
    let mut placed = None;
    for target in names {
        match std::fs::hard_link(partial, &target) {
            Ok(()) => {
                placed = Some(target);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) if std::fs::symlink_metadata(&target).is_err() => {
                if std::fs::rename(partial, &target).is_ok() {
                    return Some(target);
                }
                break;
            }
            Err(_) => {}
        }
    }
    let _ = std::fs::remove_file(partial);
    placed
}

/// Write one snapshot, readable by its owner only: it is somebody's draft.
fn write(target: &Path, s: &Snapshot) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
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

/// Give the terminal back: raw mode off, every mode `main` turned on undone,
/// the main screen back and the caret shown. Each is harmless when it was
/// never on. Every error is ignored and nothing is printed: after `SIGHUP`
/// stdout and stderr are a dead terminal, and `ratatui::restore` would
/// `eprintln!` its error, which panics there; in a panic hook, that is an
/// abort with nothing reported.
pub fn restore_terminal() {
    // Raw mode first, as `ratatui::restore` does: it has more side effects
    // than leaving the alternate screen.
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
    restore_to(&mut std::io::stdout());
}

fn restore_to(out: &mut impl Write) {
    use ratatui::crossterm::cursor::{SetCursorStyle, Show};
    use ratatui::crossterm::event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, PopKeyboardEnhancementFlags,
    };
    use ratatui::crossterm::terminal::LeaveAlternateScreen;
    let _ = ratatui::crossterm::queue!(
        out,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show,
        SetCursorStyle::DefaultUserShape
    );
    // The caret's colour (OSC 112), which Insert and `:` set with OSC 12.
    let _ = write!(out, "\x1b]112\x07");
    let _ = out.flush();
}

/// Keep a line for [`report`]: a panic message that could not be printed
/// when it happened, because the screen was the editor's.
pub fn note(line: String) {
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).push(line);
}

/// Print what was kept and where the text went, after the terminal is back
/// to normal. Not `eprintln!`: after `SIGHUP` stderr is a dead terminal, and a
/// failed print must not become a panic.
pub fn report() {
    let notes = std::mem::take(&mut *NOTES.lock().unwrap_or_else(|e| e.into_inner()));
    let written = std::mem::take(&mut *WRITTEN.lock().unwrap_or_else(|e| e.into_inner()));
    let mut err = std::io::stderr().lock();
    for line in notes {
        let _ = writeln!(err, "shoin: {line}");
    }
    for p in written {
        let _ = writeln!(err, "shoin: unsaved text written to {}", p.display());
    }
}

// ------------------------------------------------------------------ ending
//
// Three things can end the editor: the loop's own exit (`:q`, or a terminal
// that went away), a signal, and a panic that takes the process down (any
// panic in a release build, which aborts; one on the main thread otherwise).
// The first to come claims the ending, and only the owner writes the recovery
// files and restores the terminal, so each happens once. A panic on a
// background thread of a build that unwinds leaves the editor running: it
// writes a safety copy but claims nothing, so a later ending still dumps.
//
// The signal thread ends the editor while the event loop may be halfway
// through a frame. Restoring the terminal under a frame lands the rest of it
// on the user's shell, and a frame drawn after it does the same. So the loop
// writes to the terminal only while it holds a [`Frame`]; [`stop`] keeps any
// new frame from starting, the recovery files are written, and [`settle`]
// waits for the frame in flight before the terminal is touched. The dump comes
// before the wait: the text is what cannot be lost.

const RUNNING: u8 = 0;
const QUITTING: u8 = 1;
const SIGNALLED: u8 = 2;
const PANICKED: u8 = 3;

/// The name of the thread [`catch_signals`] starts.
const SIGNAL_THREAD: &str = "signals";

/// How the event loop ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// It quit, or its terminal went away: the caller ends the editor.
    Quit,
    /// A signal is ending the editor from its own thread, which restores the
    /// terminal and exits with the signal's status.
    Stopped,
}

/// Who is ending the editor, whether frames may still start, and the lock a
/// frame holds while it is written.
struct Gate {
    ending: AtomicU8,
    stopping: AtomicBool,
    /// Set by a thread that panicked while holding the frame and now waits
    /// for the signal thread: its frame will never finish.
    abandoned: AtomicBool,
    /// Whether a panic that left the editor running has written its copy.
    safety_copied: AtomicBool,
    frame: Mutex<()>,
}

thread_local! {
    /// How many [`Frame`]s this thread holds: a write nested in a frame (the
    /// clipboard's OSC 52 during a pass of the loop) takes no second lock.
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Permission to write to the terminal; see [`frame`].
pub struct Frame<'a> {
    _guard: Option<MutexGuard<'a, ()>>,
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get() - 1));
    }
}

impl Gate {
    const fn new() -> Gate {
        Gate {
            ending: AtomicU8::new(RUNNING),
            stopping: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
            safety_copied: AtomicBool::new(false),
            frame: Mutex::new(()),
        }
    }

    fn frame(&self) -> Option<Frame<'_>> {
        if DEPTH.with(|d| d.get()) > 0 {
            DEPTH.with(|d| d.set(d.get() + 1));
            return Some(Frame { _guard: None });
        }
        let guard = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        // Read under the lock: `stop` sets the flag before `settle` takes the
        // lock, so a frame that gets the lock after that sees it.
        if self.stopping.load(Ordering::SeqCst) {
            return None;
        }
        DEPTH.with(|d| d.set(d.get() + 1));
        Some(Frame { _guard: Some(guard) })
    }

    fn claim(&self, by: u8) -> bool {
        self.ending.compare_exchange(RUNNING, by, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }

    fn is(&self, by: u8) -> bool {
        self.ending.load(Ordering::SeqCst) == by
    }

    /// Frames stop only for a signal that wins: one that loses to the loop's
    /// exit must not keep that exit from resetting the caret.
    fn stop(&self) -> bool {
        if !self.claim(SIGNALLED) {
            return false;
        }
        self.stopping.store(true, Ordering::SeqCst);
        true
    }

    fn settle(&self) {
        // This thread's own frame cannot finish while it waits for it.
        if DEPTH.with(|d| d.get()) > 0 {
            return;
        }
        // Bounded: a frame stuck on a terminal that stopped reading must not
        // keep the editor from ending.
        for _ in 0..200 {
            if self.abandoned.load(Ordering::SeqCst) {
                return;
            }
            match self.frame.try_lock() {
                Ok(_) | Err(TryLockError::Poisoned(_)) => return,
                Err(TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
    }

    fn on_panic(&self, ends_process: bool, signal_thread: bool, main_thread: bool) -> OnPanic {
        // First: the signal thread ends the process whatever the build, and
        // nobody else would once it is gone.
        if signal_thread {
            return OnPanic::SignalThread;
        }
        if !ends_process {
            // One safety copy per process: a thread that keeps failing must
            // not fill the folder.
            let first = !self.safety_copied.swap(true, Ordering::SeqCst);
            return OnPanic::SafetyCopy { dump: first && self.is(RUNNING) };
        }
        if self.claim(PANICKED) {
            // As for a signal: the restore must not land under a frame.
            self.stopping.store(true, Ordering::SeqCst);
            OnPanic::Owner
        } else if self.is(QUITTING) && main_thread {
            // `quit` is claimed on the main thread: its own panic on the way out.
            OnPanic::Restore
        } else {
            // Another owner is dumping or restoring; returning would end the
            // process (`panic = "abort"`) under it.
            OnPanic::Wait
        }
    }

    fn abandon(&self) {
        if DEPTH.with(|d| d.get()) > 0 {
            self.abandoned.store(true, Ordering::SeqCst);
        }
    }
}

static GATE: Gate = Gate::new();

/// Held by the event loop while it writes to the terminal. `None` once a
/// signal is ending the editor: the loop must then write nothing more and
/// return [`Ended::Stopped`].
pub fn frame() -> Option<Frame<'static>> {
    GATE.frame()
}

/// Whether frames have stopped: a signal is ending the editor.
pub fn stopping() -> bool {
    GATE.stopping.load(Ordering::SeqCst)
}

/// The signal being handled, for the exit status.
static SIGNAL: AtomicU8 = AtomicU8::new(0);

/// For the signal thread: keep frames from starting, and claim the ending.
/// `false` when the loop's exit or a panic got there first, and is writing
/// the recovery files.
pub fn stop(sig: i32) -> bool {
    SIGNAL.store(sig as u8, Ordering::SeqCst);
    GATE.stop()
}

/// The exit status for the signal being handled.
pub fn signal_status() -> i32 {
    128 + i32::from(SIGNAL.load(Ordering::SeqCst))
}

/// After the dump: wait (up to two seconds) for the frame being written to
/// finish, so the terminal can be restored. Not for a frame this thread holds.
pub fn settle() {
    GATE.settle()
}

/// For the signal thread when another ending got there first: wait while it
/// writes recovery files, however long that takes up to a two-minute cap, and
/// five seconds more for it to end the process; then end it anyway, so a quit
/// stuck on a terminal that stopped reading cannot outlive the signal.
pub fn wait_for_owner() -> ! {
    let tick = std::time::Duration::from_millis(100);
    wait_for_dumps(&|| DUMPING.load(Ordering::SeqCst) > 0, tick, 50, 1200);
    std::process::exit(signal_status());
}

/// Sleep in `tick`s until `dumping` has been false for `grace` ticks in a
/// row, or `cap` ticks in all. Returns the ticks slept.
fn wait_for_dumps(dumping: &dyn Fn() -> bool, tick: std::time::Duration, grace: u32, cap: u32) -> u32 {
    let mut idle = 0;
    for slept in 0..cap {
        if dumping() {
            idle = 0;
        } else {
            idle += 1;
            if idle > grace {
                return slept;
            }
        }
        std::thread::sleep(tick);
    }
    cap
}

/// For the event loop's exit: claim the ending, or find it already claimed by
/// an earlier call. `false` when a signal or a panic got there first.
pub fn quit() -> bool {
    GATE.claim(QUITTING) || GATE.is(QUITTING)
}

/// What the panic hook does. Decided here, with the claim, so the rules
/// can be held to in a test.
#[derive(Debug, PartialEq, Eq)]
pub enum OnPanic {
    /// The editor keeps running (a background thread, in a build that
    /// unwinds): claim nothing, so the real ending still writes the text as
    /// it is then. One safety copy per process, when no ending is writing
    /// one already, and the message kept for the exit, as printed now it
    /// would land on the editor's screen.
    SafetyCopy { dump: bool },
    /// The signal thread's own dump or restore failed part way: wait for the
    /// frame, restore, name what was written, and exit with the signal's
    /// status.
    SignalThread,
    /// This panic ends the editor: frames have stopped; dump, wait for the
    /// frame in flight, restore, report.
    Owner,
    /// Another thread owns the ending (a signal, a panic, or the loop's own
    /// exit) and will restore and end the process: keep the message for its
    /// report, give up the frame, and wait.
    Wait,
    /// The loop's own exit owns the ending (a panic while it restores):
    /// restore and report, but no second dump.
    Restore,
}

/// For the panic hook: what to do about a panic on this thread.
pub fn on_panic() -> OnPanic {
    GATE.on_panic(panic_ends_process(), on_signal_thread(), on_main_thread())
}

/// For a thread that panicked and now waits for the signal thread to end the
/// process: if it held the frame, that frame will never finish.
pub fn abandon_frame() {
    GATE.abandon()
}

/// Whether this is the thread [`catch_signals`] runs `on_stop` on.
fn on_signal_thread() -> bool {
    std::thread::current().name() == Some(SIGNAL_THREAD)
}

/// Whether a panic on this thread ends the process: always in a build that
/// aborts on panic (release), otherwise only on the main thread, where it
/// unwinds out of `main`.
fn panic_ends_process() -> bool {
    cfg!(panic = "abort") || on_main_thread()
}

fn on_main_thread() -> bool {
    std::thread::current().name() == Some("main")
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
        let spawned = std::thread::Builder::new().name(SIGNAL_THREAD.into()).spawn(move || {
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
        let written = dump_to(&snaps, &d.join("state"), &mut |_| {});
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
        let written = dump_to(&snaps, &state, &mut |_| {});
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

    fn dump(snaps: &[Snapshot], state: &Path) -> Vec<PathBuf> {
        dump_to(snaps, state, &mut |_| {})
    }

    fn set_mtime(p: &Path, t: std::time::SystemTime) {
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn a_second_crash_never_overwrites_the_first_recovery_file() {
        let d = dir("twice");
        let path = d.join("t.md");
        std::fs::write(&path, "saved\n").unwrap();
        let first = dump(&[snap(Some(path.clone()), "first crash\n", false)], &d.join("state"));
        let second = dump(&[snap(Some(path.clone()), "second crash\n", false)], &d.join("state"));
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
        let names: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(
            !names.iter().any(|n| n.to_string_lossy().contains("partial")),
            "no .partial file is left behind: {names:?}"
        );

        let b = Buffer::open(path.clone(), &[]).unwrap();
        let text = notice([&b].into_iter()).expect("a notice");
        assert_eq!(text, "unsaved text from a crash, newest first: t.md.shoin-recover-2, t.md.shoin-recover");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_crash_takes_the_free_plain_name_so_opening_the_file_names_it() {
        let d = dir("plain-again");
        let path = d.join("p.md");
        std::fs::write(&path, "saved\n").unwrap();
        let state = d.join("state");
        let first = dump(&[snap(Some(path.clone()), "1\n", false)], &state);
        let _ = dump(&[snap(Some(path.clone()), "2\n", false)], &state);
        let third = dump(&[snap(Some(path.clone()), "3\n", false)], &state);
        // The user reads and deletes the first two, keeping `-3`.
        std::fs::remove_file(&first[0]).unwrap();
        std::fs::remove_file(beside_nth(&path, 2)).unwrap();
        let fourth = dump(&[snap(Some(path.clone()), "4\n", false)], &state);
        assert_eq!(fourth, vec![first[0].clone()], "the plain name, free again");
        set_mtime(&third[0], std::time::SystemTime::now() - std::time::Duration::from_secs(60));
        let b = Buffer::open(path.clone(), &[]).unwrap();
        let text = notice([&b].into_iter()).expect("the new crash is announced");
        assert_eq!(text, "unsaved text from a crash, newest first: p.md.shoin-recover, p.md.shoin-recover-3");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_recovery_file_goes_past_the_highest_number_and_is_listed_first() {
        let d = dir("order");
        let path = d.join("o.md");
        let first = dump(&[snap(Some(path.clone()), "1\n", false)], &d);
        let second = dump(&[snap(Some(path.clone()), "2\n", false)], &d);
        // A gap below the highest is not filled: `-2` deleted, the next
        // goes past `-3`, so the number orders them too.
        let extra = dump(&[snap(Some(path.clone()), "x\n", false)], &d);
        assert_eq!(extra, vec![d.join("o.md.shoin-recover-3")]);
        std::fs::remove_file(&second[0]).unwrap();
        std::fs::rename(&extra[0], &second[0]).unwrap();
        let third = dump(&[snap(Some(path.clone()), "3\n", false)], &d);
        assert_eq!(third, vec![d.join("o.md.shoin-recover-3")]);
        std::fs::remove_file(&first[0]).unwrap();
        // Timestamps that cannot tell them apart (FAT keeps two seconds).
        let t = std::time::SystemTime::now();
        set_mtime(&second[0], t);
        set_mtime(&third[0], t);
        assert_eq!(recovery_files(&path), vec![third[0].clone(), second[0].clone()]);
        // A few deleted from the middle of the run do not hide later ones.
        for n in 4..=9 {
            std::fs::write(beside_nth(&path, n), "").unwrap();
        }
        for n in 3..=8 {
            std::fs::remove_file(beside_nth(&path, n)).unwrap();
        }
        assert_eq!(recovery_files(&path).len(), 2, "-2 and -9");
        // The plain name is free here, so the next takes it; then past `-9`.
        assert_eq!(dump(&[snap(Some(path.clone()), "4\n", false)], &d), vec![d.join("o.md.shoin-recover")]);
        assert_eq!(dump(&[snap(Some(path.clone()), "5\n", false)], &d), vec![d.join("o.md.shoin-recover-10")]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[cfg(unix)]
    fn recovery_files_are_found_in_a_folder_that_cannot_be_listed() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("nolist");
        let inner = d.join("inner");
        std::fs::create_dir(&inner).unwrap();
        let path = inner.join("x.md");
        dump(&[snap(Some(path.clone()), "x\n", false)], &d);
        dump(&[snap(Some(path.clone()), "y\n", false)], &d);
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o300)).unwrap();
        let found = recovery_files(&path);
        let probed = probe(&path).len();
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(probed, 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_cut_off_dump_leaves_no_recovery_file_missing_its_end() {
        let d = dir("partial");
        let path = d.join("p.md");
        // What a dump killed half way leaves: only the `.partial` file.
        let partial = with_suffix(&beside(&path), &format!(".partial-{}", std::process::id()));
        std::fs::write(&partial, "half of the te").unwrap();
        assert!(recovery_files(&path).is_empty(), "a .partial file is never listed");
        let written = dump(&[snap(Some(path.clone()), "the whole text\n", false)], &d);
        assert_eq!(written, vec![d.join("p.md.shoin-recover")]);
        assert_eq!(std::fs::read_to_string(&written[0]).unwrap(), "the whole text\n");
        assert!(!partial.exists(), "the next dump of this pid clears it");
        // A taken name is passed over, not replaced.
        std::fs::write(beside_nth(&path, 2), "someone else's\n").unwrap();
        let next = write_new(&snap(None, "next\n", false), &partial, (2..=3).map(|n| beside_nth(&path, n)));
        assert_eq!(next, Some(beside_nth(&path, 3)));
        assert_eq!(std::fs::read_to_string(beside_nth(&path, 2)).unwrap(), "someone else's\n");
        assert!(!partial.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unnamed_buffer_skips_a_name_an_earlier_crash_left() {
        let d = dir("unnamed-twice");
        let state = d.join("state");
        let pid = std::process::id();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join(format!("recover-{pid}-1.md")), "earlier\n").unwrap();
        let written = dump(&[snap(None, "a\n", false), snap(None, "b\n", false)], &state);
        assert_eq!(written, vec![state.join(format!("recover-{pid}-2.md")), state.join(format!("recover-{pid}-3.md"))]);
        assert_eq!(std::fs::read_to_string(state.join(format!("recover-{pid}-1.md"))).unwrap(), "earlier\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_dump_names_each_file_as_soon_as_it_is_written() {
        let d = dir("progress");
        let mut seen = Vec::new();
        let snaps = [snap(Some(d.join("a.md")), "a\n", false), snap(Some(d.join("b.md")), "b\n", false)];
        let written = dump_to(&snaps, &d, &mut |p| {
            assert!(p.is_file(), "named only once complete");
            seen.push(p.to_path_buf());
        });
        assert_eq!(seen, written);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_signal_stops_frames_before_the_terminal_is_touched() {
        let gate = Gate::new();
        assert!(gate.frame().is_some(), "frames run until something ends the editor");
        assert!(gate.stop(), "the first to end the editor claims it");
        assert!(gate.frame().is_none(), "no frame starts once a signal is ending the editor");
        assert!(!gate.stop() && !gate.claim(QUITTING) && !gate.claim(PANICKED), "and it is ended once");

        let gate = Gate::new();
        assert!(gate.claim(QUITTING));
        assert!(!gate.stop(), "a loop already on its way out writes the files itself");
        assert!(gate.is(QUITTING));
        assert!(gate.frame().is_some(), "and still resets the caret: a losing signal stops no frames");
    }

    #[test]
    fn a_frame_nested_in_a_frame_takes_no_second_lock() {
        let gate = Gate::new();
        let outer = gate.frame().unwrap();
        let inner = gate.frame().expect("the clipboard's OSC 52 inside a pass of the loop");
        drop(inner);
        assert!(gate.frame.try_lock().is_err(), "the outer frame still holds the lock");
        drop(outer);
        assert!(gate.frame.try_lock().is_ok());
    }

    #[test]
    fn a_background_panic_leaves_the_ending_to_a_later_signal() {
        let gate = Gate::new();
        // In a build that unwinds, a background thread's panic leaves the
        // editor running: it may copy, but it claims nothing.
        assert_eq!(gate.on_panic(false, false, true), OnPanic::SafetyCopy { dump: true });
        assert!(gate.is(RUNNING));
        assert!(gate.stop(), "the signal that comes later still owns the ending, and dumps");
        assert_eq!(gate.on_panic(false, false, true), OnPanic::SafetyCopy { dump: false }, "no copy over its dump");
        // In a build that unwinds the signal thread's panic still ends the
        // process; nothing else would once that thread is gone.
        assert_eq!(gate.on_panic(false, true, true), OnPanic::SignalThread);
        // A main-thread panic now waits for the signal thread.
        assert_eq!(gate.on_panic(true, false, true), OnPanic::Wait);
        assert_eq!(gate.on_panic(true, true, true), OnPanic::SignalThread);

        let gate = Gate::new();
        assert_eq!(gate.on_panic(true, false, true), OnPanic::Owner, "a panic that ends the process owns it");
        assert!(!gate.stop(), "and a signal after it does not dump again");
        assert!(gate.frame().is_none(), "but frames stop all the same");

        let gate = Gate::new();
        assert!(gate.claim(QUITTING));
        assert_eq!(gate.on_panic(true, false, true), OnPanic::Restore, "a panic while the loop restores");
        // Another thread's panic (a build that aborts) must not end the
        // process under the loop's own dump or restore: it waits.
        assert_eq!(gate.on_panic(true, false, false), OnPanic::Wait);

        let gate = Gate::new();
        assert_eq!(gate.on_panic(true, false, false), OnPanic::Owner, "a background thread's panic, aborting");
        assert_eq!(gate.on_panic(true, false, true), OnPanic::Wait, "main waits while that owner dumps");
        assert_eq!(gate.on_panic(true, false, false), OnPanic::Wait, "and so does a third thread");
    }

    #[test]
    fn a_background_panic_writes_one_safety_copy_per_process() {
        let gate = Gate::new();
        assert_eq!(gate.on_panic(false, false, true), OnPanic::SafetyCopy { dump: true });
        assert_eq!(gate.on_panic(false, false, true), OnPanic::SafetyCopy { dump: false });
        assert!(gate.is(RUNNING));
    }

    #[test]
    fn a_panic_that_ends_the_process_stops_frames_for_its_restore() {
        let gate = Gate::new();
        assert_eq!(gate.on_panic(true, false, true), OnPanic::Owner);
        assert!(gate.frame().is_none(), "no frame lands under the restore");
        let gate = Gate::new();
        let frame = gate.frame().unwrap();
        let start = std::time::Instant::now();
        gate.settle();
        assert!(start.elapsed() < std::time::Duration::from_millis(500), "not for the frame this thread holds");
        drop(frame);
    }

    #[test]
    fn restoring_a_terminal_that_is_gone_does_not_panic() {
        struct Dead;
        impl Write for Dead {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from_raw_os_error(5))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::from_raw_os_error(5))
            }
        }
        restore_to(&mut Dead);
        let mut out = Vec::new();
        restore_to(&mut out);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("\x1b[?1049l"), "the main screen back: {out:?}");
        assert!(out.contains("\x1b[?25h"), "the caret shown: {out:?}");
        assert!(out.contains("\x1b[0 q"), "the caret's own shape back: {out:?}");
        assert!(out.contains("\x1b]112\x07"), "the caret's own colour back: {out:?}");
    }

    #[test]
    fn a_losing_signal_waits_while_the_owner_writes() {
        let tick = std::time::Duration::from_millis(1);
        let calls = std::cell::Cell::new(0);
        // Dumping for 30 ticks, far past the grace of 5: still waited for.
        let dumping = || {
            calls.set(calls.get() + 1);
            calls.get() <= 30
        };
        let slept = wait_for_dumps(&dumping, tick, 5, 1000);
        assert!((35..=37).contains(&slept), "{slept}");
        assert_eq!(wait_for_dumps(&|| true, tick, 5, 50), 50, "capped");
        assert_eq!(wait_for_dumps(&|| false, tick, 5, 50), 5, "an owner that never dumps gets the grace only");
    }

    #[test]
    fn a_listable_folder_shows_files_past_any_gap() {
        let d = dir("gap");
        let path = d.join("g.md");
        std::fs::write(beside_nth(&path, 1), "").unwrap();
        std::fs::write(beside_nth(&path, 20), "").unwrap();
        // Names `beside_nth` never makes are not recovery files.
        std::fs::write(with_suffix(&beside(&path), "-1"), "").unwrap();
        std::fs::write(with_suffix(&beside(&path), "-020"), "").unwrap();
        std::fs::write(with_suffix(&beside(&path), ".partial-1"), "").unwrap();
        assert_eq!(recovery_files(&path).len(), 2);
        let next = dump(&[snap(Some(path.clone()), "new\n", false)], &d);
        assert_eq!(next, vec![beside_nth(&path, 21)], "past every number there");
        let b = Buffer::open(path.clone(), &[]).unwrap();
        let text = notice([&b].into_iter()).expect("a notice");
        assert!(text.starts_with("unsaved text from a crash, newest first: g.md.shoin-recover-21"), "{text}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn on_this_test_thread_a_panic_does_not_end_the_process() {
        // Tests unwind and run off the main thread: a background thread.
        assert!(!panic_ends_process() || cfg!(panic = "abort"));
        assert!(!on_signal_thread());
    }

    #[test]
    fn a_signal_waits_for_the_frame_being_written() {
        static GATE: Gate = Gate::new();
        let frame = GATE.frame().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let stopper = std::thread::spawn(move || {
            assert!(GATE.stop(), "claiming does not wait for the frame: the dump comes first");
            GATE.settle();
            tx.send(()).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(rx.try_recv().is_err(), "settle returned while a frame was still being written");
        drop(frame);
        rx.recv_timeout(std::time::Duration::from_secs(5)).expect("settle returns once the frame is done");
        stopper.join().unwrap();
        assert!(GATE.frame().is_none());
    }

    #[test]
    fn settle_does_not_wait_for_a_frame_its_panicked_holder_gave_up() {
        static GATE: Gate = Gate::new();
        let (held, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        let holder = std::thread::spawn(move || {
            let _frame = GATE.frame().unwrap();
            // What the panic hook does on a thread that waits for the signal.
            GATE.abandon();
            held.0.send(()).unwrap();
            let _ = release.1.recv();
        });
        held.1.recv().unwrap();
        assert!(GATE.stop());
        let start = std::time::Instant::now();
        GATE.settle();
        assert!(start.elapsed() < std::time::Duration::from_millis(500), "{:?}", start.elapsed());
        release.0.send(()).unwrap();
        holder.join().unwrap();
    }
}
