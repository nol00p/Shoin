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
//! A dump that is killed outright (`SIGKILL`) leaves that `.partial` file
//! behind. It may hold the only copy of most of the text, so nothing deletes
//! it or writes over it, not even a later process that has the same pid:
//! opening the file names it with the recovery files, as cut off. A dump is
//! taken to be over when its process is gone, when the pid is ours and no
//! dump of ours is writing, or when nothing has written to it for three
//! minutes. The one `.partial` that does go is a second name for a recovery
//! file (its dump was cut off between the two): a later dump removes it once
//! all its own text is written, and opening the file does not name it.
//!
//! A file name too long to take these endings (names stop at 255 bytes) is
//! shortened in them, with a hash of the whole name so two long names that
//! start the same way do not share recovery files. Recovery files that 0.2.4
//! wrote under the full name are still found.
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
        // One listing of the folder answers both questions, and only a name
        // that is one of this file's is looked at further: a folder with none
        // costs the listing alone.
        let found = scan(path);
        let files = newest_first(found.recovery);
        let cut = cut_off(found.partials, WRITING.load(Ordering::SeqCst));
        // One notice per pass: any other file is looked at on the next one,
        // after this flash has had its turn.
        if let Some(text) = notice_text(&files, &cut) {
            return Some(text);
        }
    }
    None
}

/// What opening a file says about the recovery files beside it, newest first,
/// and the `.partial` files dumps left when they were cut off.
fn notice_text(files: &[PathBuf], cut: &[PathBuf]) -> Option<String> {
    // Names only when there are several: they all sit beside the open file,
    // and a full path each would push the later ones off a flash that is cut
    // to the width of the screen.
    let names = |ps: &[PathBuf]| -> String {
        let names: Vec<_> = ps.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy()).collect();
        names.join(", ")
    };
    match (files, cut) {
        ([], []) => None,
        ([one], []) => Some(format!("{} holds unsaved text from a crash", one.display())),
        ([], [one]) => Some(format!("{} holds unsaved text from a crash, cut off part way", one.display())),
        (files, []) => Some(format!("unsaved text from a crash, newest first: {}", names(files))),
        ([], cut) => Some(format!("unsaved text from a crash, cut off part way: {}", names(cut))),
        (files, cut) => Some(format!(
            "unsaved text from a crash, newest first: {}; cut off part way: {}",
            names(files),
            names(cut)
        )),
    }
}

/// The `.partial` files among `partials` whose dump is over and whose text is
/// nowhere else, newest first: each is what a dump had written when it was
/// killed. `own_writing`: a dump of this process is writing now.
fn cut_off(partials: Vec<Found>, own_writing: bool) -> Vec<PathBuf> {
    let mut found: Vec<_> = partials
        .into_iter()
        .filter(|(pid, _, meta)| !duplicate(meta) && over(*pid, meta, own_writing))
        .map(|(_, p, meta)| (meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH), p))
        .collect();
    found.sort();
    found.into_iter().rev().map(|(_, p)| p).collect()
}

/// Whether the dump that wrote a `.partial` is over. Its process is gone; or
/// the pid is this process's and no dump of ours is writing (ours exist only
/// while one is, so this one is an earlier process's that had the pid); or
/// nothing has written to it for [`CUT_OFF_AFTER`], whatever has the pid now.
fn over(pid: u32, meta: &std::fs::Metadata, own_writing: bool) -> bool {
    let quiet = meta.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > CUT_OFF_AFTER);
    let gone = if pid == std::process::id() { !own_writing } else { alive(pid) != Some(true) };
    quiet || gone
}

/// A dump writes as it goes, and the waits around one give up at two
/// minutes (`lock_for_dump`, `wait_for_owner`): a `.partial` nothing has
/// written to for a minute past that is not coming back.
const CUT_OFF_AFTER: std::time::Duration = std::time::Duration::from_secs(180);

/// Whether a `.partial` is a second name for a recovery file: its dump was
/// cut off after naming the file and before removing the `.partial` name.
fn duplicate(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// Every recovery file beside `path`, newest first: `<file>.shoin-recover`
/// and any `<file>.shoin-recover-<n>`.
#[cfg(test)]
fn recovery_files(path: &Path) -> Vec<PathBuf> {
    newest_first(existing(path))
}

/// Newest is by the time each was written, then by `<n>`: a new one takes the
/// plain name when it is free and otherwise the number past the highest
/// already there, so a tie of coarse timestamps still puts it first unless it
/// re-took the plain name (which needs the user to have deleted the older
/// files in between).
fn newest_first(found: Vec<Found>) -> Vec<PathBuf> {
    let mut found: Vec<_> = found
        .into_iter()
        .map(|(n, p, meta)| (meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH), n, p))
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse((a.0, a.1)));
    found.into_iter().map(|(_, _, p)| p).collect()
}

/// A file found beside another: a recovery file with its number, or a
/// `.partial` with its pid.
type Found = (u32, PathBuf, std::fs::Metadata);

/// What sits beside a file.
#[derive(Default)]
struct Beside {
    recovery: Vec<Found>,
    partials: Vec<Found>,
}

/// The recovery files beside `path` with their numbers.
fn existing(path: &Path) -> Vec<Found> {
    scan(path).recovery
}

/// What the recovery names of `path` start with: the one [`beside`] makes,
/// and for a name it shortens, the full-length one 0.2.4 used, so the files
/// written then are still found.
fn bases(path: &Path) -> Vec<PathBuf> {
    let first = beside(path);
    let full = with_suffix(path, SUFFIX);
    if full == first {
        vec![first]
    } else {
        vec![first, full]
    }
}

/// The recovery files and `.partial` files beside `path`, from one listing of
/// its folder, which sees every number however many were deleted in between.
/// A folder that lets you reach its files but not list them is asked name by
/// name for the recovery files; its `.partial` files, named by a pid, stay
/// unseen there.
fn scan(path: &Path) -> Beside {
    let bases = bases(path);
    let names: Vec<_> = bases.iter().filter_map(|b| b.file_name()).map(|n| n.as_encoded_bytes().to_vec()).collect();
    match list(&folder(&bases[0]), &names) {
        Some(found) => found,
        None => Beside { recovery: bases.iter().flat_map(|b| probe(b)).collect(), partials: Vec::new() },
    }
}

fn folder(p: &Path) -> PathBuf {
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// What one listing of `dir` holds under any of `bases`: `<base>` and
/// `<base>-<n>` are recovery files, `<base>.partial-<pid>` and
/// `<base>.partial-<pid>.<k>` are `.partial` files. `None` when the folder
/// cannot be listed.
fn list(dir: &Path, bases: &[Vec<u8>]) -> Option<Beside> {
    let mut found = Beside::default();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let file = entry.file_name();
        let bytes = file.as_encoded_bytes();
        let Some(name) = bases.iter().find_map(|b| bytes.strip_prefix(&b[..]).and_then(parse)) else { continue };
        let p = dir.join(&file);
        match name {
            // `metadata` follows a link, as opening the file would.
            Name::Recovery(n) => match std::fs::metadata(&p) {
                Ok(meta) if meta.is_file() => found.recovery.push((n, p, meta)),
                _ => {}
            },
            // A dump makes plain files only.
            Name::Partial(pid) => match std::fs::symlink_metadata(&p) {
                Ok(meta) if meta.is_file() => found.partials.push((pid, p, meta)),
                _ => {}
            },
        }
    }
    Some(found)
}

enum Name {
    Recovery(u32),
    Partial(u32),
}

/// What follows a base in the names a dump makes, and nothing else: `-2` and
/// up and pids with no leading zero.
fn parse(rest: &[u8]) -> Option<Name> {
    match rest {
        [] => Some(Name::Recovery(1)),
        [b'-', digits @ ..] => number(digits).filter(|n| *n >= 2).map(Name::Recovery),
        _ => {
            let tail = rest.strip_prefix(PARTIAL.as_bytes())?;
            let (pid, k) = match tail.iter().position(|b| *b == b'.') {
                Some(dot) => (&tail[..dot], Some(&tail[dot + 1..])),
                None => (tail, None),
            };
            if let Some(k) = k {
                number(k).filter(|k| *k >= 2)?;
            }
            number(pid).map(Name::Partial)
        }
    }
}

fn number(digits: &[u8]) -> Option<u32> {
    if !digits.first().is_some_and(|d| (b'1'..=b'9').contains(d)) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// Name by name, for a folder that cannot be listed, from the base recovery
/// name `first`: the search ends after [`MISSES`] free numbers in a row, so
/// files past a longer gap stay unseen there.
fn probe(first: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    let mut misses = 0;
    for n in 1..=MAX_TRIES {
        let p = nth(first, n);
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
    nth(&beside(path), n)
}

fn nth(first: &Path, n: u32) -> PathBuf {
    if n <= 1 {
        return first.to_path_buf();
    }
    with_suffix(first, &format!("-{n}"))
}

/// How many names [`dump_to`] tries before it gives a file up: past this many
/// unread recovery files, something else is wrong.
const MAX_TRIES: u32 = 1000;

/// `notes.md` → `notes.md.shoin-recover`, in the same folder. A name too long
/// to take that and the longest ending after it is shortened: see [`stem`].
pub fn beside(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default();
    let mut name = stem(name);
    name.push(SUFFIX);
    path.with_file_name(name)
}

/// The longest file name most file systems take, in bytes.
const NAME_MAX: usize = 255;

/// The room kept after `<file>.shoin-recover` for the longest ending a name
/// gets: `.partial-<pid>.<k>`, where a pid takes ten digits at most and `<k>`
/// three (`-<n>` takes eleven).
const ENDING: usize = PARTIAL.len() + 10 + ".100".len();

/// What a name is cut to when it is too long, with `~` and eight hex digits
/// of a hash of the whole name after it: two long names that start the same
/// way still get recovery names of their own.
fn stem(name: &std::ffi::OsStr) -> std::ffi::OsString {
    let room = NAME_MAX - SUFFIX.len() - ENDING;
    let bytes = name.as_encoded_bytes();
    if bytes.len() <= room {
        return name.to_os_string();
    }
    let tag = format!("~{:08x}", fnv1a(bytes));
    let mut keep = room - tag.len();
    match name.to_str() {
        Some(s) => {
            while !s.is_char_boundary(keep) {
                keep -= 1;
            }
            format!("{}{tag}", &s[..keep]).into()
        }
        #[cfg(unix)]
        None => {
            use std::os::unix::ffi::OsStringExt;
            let mut cut = bytes[..keep].to_vec();
            cut.extend_from_slice(tag.as_bytes());
            std::ffi::OsString::from_vec(cut)
        }
        // Not UTF-8 off Unix: no safe place to cut. Left long, the dump
        // goes to the state folder as before.
        #[cfg(not(unix))]
        None => name.to_os_string(),
    }
}

/// 32-bit FNV-1a: the same on every build and every version of Rust, as a
/// recovery name has to be found again by the next one.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// What a dump writes under before the text is whole: `<recovery
/// name>.partial-<pid>`, and `recover.partial-<pid>` in the state folder.
const PARTIAL: &str = ".partial-";

fn partial_beside(path: &Path, pid: u32) -> PathBuf {
    with_suffix(&beside(path), &format!("{PARTIAL}{pid}"))
}

/// Whether process `pid` is running: `None` where that cannot be asked.
fn alive(pid: u32) -> Option<bool> {
    #[cfg(unix)]
    {
        // Never 0 or a negative number: `kill` reads those as a group.
        let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
        // SAFETY: signal 0 sends nothing; it only checks the pid.
        if unsafe { sys::kill(pid, 0) } == 0 {
            return Some(true);
        }
        // `EPERM`: running, as somebody else.
        Some(std::io::Error::last_os_error().raw_os_error() != Some(sys::ESRCH))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// Remove each of `partials` that is a second name for a recovery file and
/// whose dump is over. Any other is kept, since it may hold the only copy of
/// the text, and opening the file names it.
fn clear_duplicates(partials: Vec<Found>) {
    for (pid, p, meta) in partials {
        if duplicate(&meta) && over(pid, &meta, false) {
            let _ = std::fs::remove_file(p);
        }
    }
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
/// The registry is not simply waited on: see [`lock_for_dump`].
pub fn dump() {
    DUMPING.fetch_add(1, Ordering::SeqCst);
    dump_registry();
    DUMPING.fetch_sub(1, Ordering::SeqCst);
}

/// How many dumps are under way: a signal that lost the ending to another
/// owner waits while it is writing.
static DUMPING: AtomicU32 = AtomicU32::new(0);

fn dump_registry() {
    let Some(reg) = lock_for_dump(&REGISTRY, &WRITING, std::time::Duration::from_millis(10)) else { return };
    let _writing = Writing::start(&WRITING);
    dump_to(&reg, &state_dir(), &mut |p| WRITTEN.lock().unwrap_or_else(|e| e.into_inner()).push(p.to_path_buf()));
}

/// Set while a dump holds the registry and writes: another dump that wants it
/// waits for that one, however long it takes up to a cap, rather than giving
/// up after the moment it gives anything else.
static WRITING: AtomicBool = AtomicBool::new(false);

/// Sets [`WRITING`] for as long as it lives, a dump that panics included.
struct Writing<'a>(&'a AtomicBool);

impl<'a> Writing<'a> {
    fn start(flag: &'a AtomicBool) -> Writing<'a> {
        flag.store(true, Ordering::SeqCst);
        Writing(flag)
    }
}

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The registry for a dump, tried in `tick`s rather than waited on: a panic
/// on a thread that holds it (the loop's [`tick`]) would never let it go, and
/// a dump that gives up is better than a hook that never returns. Held by
/// another dump (a safety copy that a signal came in on), it is waited for
/// until that dump is done, up to two minutes; otherwise for 50 ticks.
fn lock_for_dump<'a>(
    reg: &'a Mutex<Vec<Snapshot>>,
    writing: &AtomicBool,
    tick: std::time::Duration,
) -> Option<MutexGuard<'a, Vec<Snapshot>>> {
    let mut idle = 0;
    for _ in 0..12_000 {
        match reg.try_lock() {
            Ok(reg) => return Some(reg),
            Err(TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(TryLockError::WouldBlock) if writing.load(Ordering::SeqCst) => idle = 0,
            Err(TryLockError::WouldBlock) if idle < 50 => idle += 1,
            Err(TryLockError::WouldBlock) => return None,
        }
        std::thread::sleep(tick);
    }
    None
}

/// Write `snaps`, handing each file to `done` as soon as it is complete.
fn dump_to(snaps: &[Snapshot], state: &Path, done: &mut dyn FnMut(&Path)) {
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
            let names = numbers.map(|n| beside_nth(path, n));
            if let Some(target) = write_new(s, &partial_beside(path, pid), names) {
                done(&target);
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
        let partial = state.join(format!("recover{PARTIAL}{pid}"));
        if let Some(target) = write_new(s, &partial, names.take(MAX_TRIES as usize)) {
            done(&target);
        }
    }
    // Only now, with every text written: a slow folder must not hold up a
    // text still to be written, and a duplicate costs nothing to keep a
    // moment longer. One listing per folder, however many files are in it.
    let mut folders: std::collections::BTreeMap<PathBuf, Vec<Vec<u8>>> = std::collections::BTreeMap::new();
    for path in snaps.iter().filter_map(|s| s.path.as_deref()) {
        for base in bases(path) {
            if let Some(name) = base.file_name() {
                folders.entry(folder(&base)).or_default().push(name.as_encoded_bytes().to_vec());
            }
        }
    }
    folders.entry(state.to_path_buf()).or_default().push(b"recover".to_vec());
    for (dir, names) in folders {
        if let Some(found) = list(&dir, &names) {
            clear_duplicates(found.partials);
        }
    }
}

/// Write `s` in full to `partial`, then give it the first of `names` not
/// taken yet. A dump cut off part way (the process ends under it) leaves at
/// most the `.partial` file, never a recovery file that is missing its end.
///
/// The name is given by a hard link, which fails on a taken name rather than
/// replacing it, as a rename would. Where links are refused (FAT, some network
/// mounts), a rename to a name just seen free does it instead.
fn write_new(s: &Snapshot, partial: &Path, names: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    let partial = &write_partial(s, partial)?;
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

/// Write `s` in full to `partial`, or to `partial.2` and on when that is
/// taken: a `.partial` already there is not this process's, whose exist only
/// while one of its dumps writes and are gone at its end. It is an earlier
/// process's that had the same pid and was cut off, and it may hold the only
/// copy of its text, so it is never removed or written over.
fn write_partial(s: &Snapshot, partial: &Path) -> Option<PathBuf> {
    for k in 1..=PARTIAL_TRIES {
        let p = if k == 1 { partial.to_path_buf() } else { with_suffix(partial, &format!(".{k}")) };
        match write(&p, s) {
            Ok(()) => return Some(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                let _ = std::fs::remove_file(&p);
                return None;
            }
        }
    }
    None
}

/// How many `.partial` names a dump tries: see [`ENDING`].
const PARTIAL_TRIES: u32 = 100;

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
// Four libc calls, declared here rather than pulled in through a crate: std
// already links libc, and both signal numbers are fixed by POSIX (they are what
// `kill -1` and `kill -15` mean on every Unix).

#[cfg(unix)]
mod sys {
    pub const SIGHUP: i32 = 1;
    pub const SIGTERM: i32 = 15;
    /// `SIG_DFL`: the signal's default action, which for these two is to end
    /// the process.
    pub const DEFAULT: usize = 0;
    /// "No such process": the same number on Linux, macOS and the BSDs.
    pub const ESRCH: i32 = 3;

    use std::ffi::{c_int, c_void};

    extern "C" {
        /// `sighandler_t signal(int, sighandler_t)`; a handler is a pointer.
        pub fn signal(signum: c_int, handler: usize) -> usize;
        pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
        pub fn kill(pid: c_int, sig: c_int) -> c_int;
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
        let written = dump(&snaps, &d.join("state"));
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
        let written = dump(&snaps, &state);
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
        let mut written = Vec::new();
        dump_to(snaps, state, &mut |p| written.push(p.to_path_buf()));
        written
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
        let probed = probe(&beside(&path)).len();
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(probed, 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_cut_off_dump_leaves_no_recovery_file_missing_its_end() {
        let d = dir("partial");
        let path = d.join("p.md");
        // What a dump killed half way leaves: only the `.partial` file. Its
        // pid is this process's: an earlier process had it (pids come round
        // again, after a reboot most of all).
        let partial = partial_beside(&path, std::process::id());
        std::fs::write(&partial, "half of the te").unwrap();
        assert!(recovery_files(&path).is_empty(), "a .partial file is never listed as a recovery file");
        assert_eq!(cut_off(scan(&path).partials, false), vec![partial.clone()], "but named as cut off");
        assert!(cut_off(scan(&path).partials, true).is_empty(), "unless a dump of ours is writing");
        let written = dump(&[snap(Some(path.clone()), "the whole text\n", false)], &d);
        assert_eq!(written, vec![d.join("p.md.shoin-recover")]);
        assert_eq!(std::fs::read_to_string(&written[0]).unwrap(), "the whole text\n");
        assert_eq!(std::fs::read_to_string(&partial).unwrap(), "half of the te", "the earlier one is kept");
        assert!(!with_suffix(&partial, ".2").exists(), "ours went under the next name, and is gone");
        let b = Buffer::open(path.clone(), &[]).unwrap();
        let text = notice([&b].into_iter()).expect("a notice");
        assert!(text.ends_with(&format!("cut off part way: p.md.shoin-recover.partial-{}", std::process::id())), "{text}");
        // A taken name is passed over, not replaced.
        std::fs::write(beside_nth(&path, 2), "someone else's\n").unwrap();
        let next = write_new(&snap(None, "next\n", false), &partial, (2..=3).map(|n| beside_nth(&path, n)));
        assert_eq!(next, Some(beside_nth(&path, 3)));
        assert_eq!(std::fs::read_to_string(beside_nth(&path, 2)).unwrap(), "someone else's\n");
        assert_eq!(std::fs::read_to_string(&partial).unwrap(), "half of the te");
        assert!(!with_suffix(&partial, ".2").exists());
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
        dump_to(&snaps, &d, &mut |p| {
            assert!(p.is_file(), "named only once complete");
            seen.push(p.to_path_buf());
        });
        assert_eq!(seen, vec![d.join("a.md.shoin-recover"), d.join("b.md.shoin-recover")]);
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

    /// A pid no process has: a child that has been waited for.
    #[cfg(unix)]
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(alive(pid), Some(false));
        pid
    }

    #[test]
    #[cfg(unix)]
    fn a_dump_that_was_killed_leaves_a_partial_that_is_named_and_kept() {
        let d = dir("killed");
        let path = d.join("k.md");
        std::fs::write(&path, "saved\n").unwrap();
        // Killed while it wrote: nothing else holds this text.
        let dead = partial_beside(&path, dead_pid());
        std::fs::write(&dead, "most of the te").unwrap();
        // Being written by a running dump right now (pid 1 is always there).
        assert_eq!(alive(1), Some(true));
        let running = partial_beside(&path, 1);
        std::fs::write(&running, "in progress").unwrap();
        let b = Buffer::open(path.clone(), &[]).unwrap();
        let text = notice([&b].into_iter()).expect("a notice");
        assert_eq!(text, format!("{} holds unsaved text from a crash, cut off part way", dead.display()));
        // Nothing written to it for longer than a dump lasts: cut off,
        // whatever process has that pid now.
        set_mtime(&running, std::time::SystemTime::now() - std::time::Duration::from_secs(600));
        assert_eq!(cut_off(scan(&path).partials, false), vec![dead.clone(), running.clone()]);
        set_mtime(&running, std::time::SystemTime::now());

        // A crash after it: both named, the cut-off one apart.
        let written = dump(&[snap(Some(path.clone()), "whole\n", false)], &d.join("state"));
        assert_eq!(written, vec![beside(&path)]);
        assert_eq!(std::fs::read_to_string(&dead).unwrap(), "most of the te", "never deleted");
        assert_eq!(std::fs::read_to_string(&running).unwrap(), "in progress", "nor a running dump's");
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            notice_text(&recovery_files(&path), &cut_off(scan(&path).partials, false)).unwrap(),
            format!("unsaved text from a crash, newest first: k.md.shoin-recover; cut off part way: {}", name(&dead))
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[cfg(unix)]
    fn a_partial_whose_text_a_recovery_file_holds_goes_once_its_dump_is_over() {
        let d = dir("linked");
        let path = d.join("l.md");
        let state = d.join("state");
        // Cut off between naming the file and removing the `.partial`.
        let first = dump(&[snap(Some(path.clone()), "one\n", false)], &state);
        let dead = partial_beside(&path, dead_pid());
        std::fs::hard_link(&first[0], &dead).unwrap();
        let running = partial_beside(&path, 1);
        std::fs::hard_link(&first[0], &running).unwrap();
        // The same in the state folder.
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("recover-1-1.md"), "x\n").unwrap();
        let dead_unnamed = state.join(format!("recover.partial-{}", dead_pid()));
        std::fs::hard_link(state.join("recover-1-1.md"), &dead_unnamed).unwrap();

        assert!(cut_off(scan(&path).partials, false).is_empty(), "not named as cut off: its text is whole");
        let b = Buffer::open(path.clone(), &[]).unwrap();
        assert_eq!(notice([&b].into_iter()).unwrap(), format!("{} holds unsaved text from a crash", first[0].display()));
        assert!(dead.exists(), "and opening the file deletes nothing");

        let snaps = [snap(Some(path.clone()), "two\n", false), snap(None, "three\n", false)];
        dump_to(&snaps, &state, &mut |_| {
            assert!(dead.exists() && dead_unnamed.exists(), "cleared only after every text is written");
        });
        assert!(!dead.exists() && !dead_unnamed.exists(), "a second name for a whole file goes");
        assert!(running.exists(), "a running dump's is left to it");
        assert_eq!(std::fs::read_to_string(&first[0]).unwrap(), "one\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_name_too_long_for_the_endings_is_shortened_in_them() {
        let d = dir("long");
        // 250 bytes, with a two-byte character across the place it is cut.
        // It is cut to 209 bytes, and the `é` takes the 209th and 210th.
        let long = |end: &str| format!("{}é{}{end}", "a".repeat(208), "b".repeat(250 - 210 - end.len()));
        let (a, b) = (d.join(long("-one.md")), d.join(long("-two.md")));
        assert_eq!(a.file_name().unwrap().len(), 250);
        for p in [&a, &b] {
            std::fs::write(p, "saved\n").unwrap();
        }
        let state = d.join("state");
        let first = dump(&[snap(Some(a.clone()), "1\n", false)], &state);
        let second = dump(&[snap(Some(a.clone()), "2\n", false)], &state);
        let other = dump(&[snap(Some(b.clone()), "b\n", false)], &state);
        assert_eq!(first, vec![beside(&a)], "beside the file, not in the state folder");
        assert_eq!(second, vec![beside_nth(&a, 2)]);
        assert_eq!(other, vec![beside(&b)], "a name of its own");
        assert!(!state.exists());
        let longest = with_suffix(&beside_nth(&a, u32::MAX), "");
        assert!(longest.file_name().unwrap().len() <= NAME_MAX);
        let partial = with_suffix(&partial_beside(&a, u32::MAX), &format!(".{PARTIAL_TRIES}"));
        assert!(partial.file_name().unwrap().len() <= NAME_MAX);
        assert_eq!(recovery_files(&a), vec![second[0].clone(), first[0].clone()]);
        assert_eq!(recovery_files(&b), vec![other[0].clone()]);
        // Short names are as they were.
        assert_eq!(beside(&d.join("n.md")), d.join("n.md.shoin-recover"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_long_name_still_finds_what_0_2_4_wrote_under_the_full_name() {
        let d = dir("legacy");
        let path = d.join(format!("{}.md", "x".repeat(222)));
        std::fs::write(&path, "saved\n").unwrap();
        assert_ne!(beside(&path), with_suffix(&path, SUFFIX), "a name `stem` shortens");
        // What 0.2.4 wrote: the full name with the suffix after it.
        let old = with_suffix(&path, SUFFIX);
        std::fs::write(&old, "from 0.2.4\n").unwrap();
        set_mtime(&old, std::time::SystemTime::now() - std::time::Duration::from_secs(60));
        assert_eq!(recovery_files(&path), vec![old.clone()]);
        let b = Buffer::open(path.clone(), &[]).unwrap();
        let text = notice([&b].into_iter()).expect("a notice");
        assert_eq!(text, format!("{} holds unsaved text from a crash", old.display()));
        // A new crash goes under the short name, and both are listed.
        let new = dump(&[snap(Some(path.clone()), "new\n", false)], &d.join("state"));
        assert_eq!(new, vec![beside(&path)]);
        assert_eq!(recovery_files(&path), vec![new[0].clone(), old.clone()]);
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "from 0.2.4\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_dump_waits_for_another_dump_holding_the_registry() {
        static REG: Mutex<Vec<Snapshot>> = Mutex::new(Vec::new());
        static FLAG: AtomicBool = AtomicBool::new(false);
        let tick = std::time::Duration::from_millis(1);
        for writing in [true, false] {
            let (held, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
            let holder = std::thread::spawn(move || {
                let _reg = REG.lock().unwrap();
                // A safety copy writing, or the loop's tick stuck in a panic.
                let _w = writing.then(|| Writing::start(&FLAG));
                held.0.send(()).unwrap();
                let _ = release.1.recv();
            });
            held.1.recv().unwrap();
            // Released long after the 50 ticks anything else gets.
            let releaser = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(300));
                release.0.send(()).unwrap();
            });
            let got = lock_for_dump(&REG, &FLAG, tick).is_some();
            assert_eq!(got, writing, "waited for the other dump only");
            releaser.join().unwrap();
            holder.join().unwrap();
        }
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
