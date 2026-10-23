//! The system clipboard: what `"+` and `"*` read and write, and what every
//! yank and paste goes through when `[editor] clipboard` is on.
//!
//! There is no clipboard crate here on purpose. Every platform Shoin runs on
//! already ships a small program that does this job — `pbcopy`/`pbpaste` on
//! macOS, `wl-copy`/`wl-paste` under Wayland, `xclip` or `xsel` under X11 — and
//! shelling out to it costs a few milliseconds per yank, which is nothing next
//! to a keystroke. A crate would bring a display-server client into the build
//! for the same result.
//!
//! When none of those programs is there, or Shoin is running over SSH, a copy
//! goes out as an OSC 52 escape instead: the TERMINAL puts the text on its own
//! clipboard, which over SSH is the clipboard of the machine you are sitting
//! at — the one you want. OSC 52 is write-only in practice (reading is either
//! unimplemented or refused by every terminal worth naming), so a paste that
//! finds no program answers `None`, and the caller falls back to the text it
//! copied itself.
//!
//! One copy or paste gets `TOOL_TIMEOUT` in all, however many programs it
//! tries, and a program still running at the end is killed, so a clipboard
//! daemon that hangs costs the editor a second, not the session.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long one copy or paste may take before it is given up on.
const TOOL_TIMEOUT: Duration = Duration::from_secs(1);

/// Somewhere text can be copied to and pasted from. `System` in the editor;
/// `Memory` in the tests, which must never touch the developer's clipboard.
pub trait Clipboard {
    fn copy(&mut self, text: &str);
    /// The clipboard's text — empty when it holds none, or none as text — or
    /// `None` when there is no clipboard here to read.
    fn paste(&mut self) -> Option<String>;
    /// `[editor] clipboard_background`: hand copies to a worker thread rather
    /// than waiting for the clipboard program on every yank and delete.
    fn set_background(&mut self, _on: bool) {}
    /// Settle copies the worker has finished. The event loop calls this
    /// between frames, which is where an OSC 52 fallback may be written: the
    /// worker never writes to the terminal itself, or the escape could land
    /// in the middle of a frame.
    fn poll(&mut self) {}
}

/// The clipboard for this session: the real one, except under `cargo test`.
pub fn session(background: bool) -> Box<dyn Clipboard> {
    if cfg!(test) {
        Box::new(Memory::default())
    } else {
        let mut system = System::new();
        system.set_background(background);
        Box::new(system)
    }
}

/// The operating system's clipboard, through whichever program it has.
pub struct System {
    /// Over SSH, the programs on THIS machine would fill the remote host's
    /// clipboard, which nobody is looking at. Decided once, at startup.
    remote: bool,
    /// A program that hung once is not asked again this session. With
    /// `[editor] clipboard` on every `x` and `dd` copies, and each would
    /// otherwise wait out the timeout before falling back to OSC 52.
    hung: bool,
    /// The last copy went out as OSC 52 because no program took it. A paste
    /// program failing after that is the same broken clipboard, not an empty
    /// one, and Shoin's own copy is the best record of what it holds.
    copied_by_osc52: bool,
    /// The background copier, when `[editor] clipboard_background` is on.
    worker: Option<Worker>,
}

/// A thread that runs copies so the editor does not wait on them.
struct Worker {
    jobs: mpsc::Sender<String>,
    done: mpsc::Receiver<Copied>,
    /// Copies sent and not yet settled.
    pending: usize,
}

/// What the worker did with one or more queued copies.
struct Copied {
    /// How many queued copies this answers: only the newest is ever run.
    jobs: usize,
    run: Run<()>,
    text: String,
}

impl Worker {
    /// Start the thread, running each copy through `copy` — the clipboard
    /// programs in the editor, a stand-in in the tests.
    fn spawn(copy: fn(&str) -> Run<()>) -> Self {
        let (jobs, inbox) = mpsc::channel::<String>();
        let (outbox, done) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(mut text) = inbox.recv() {
                // Each copy replaces the one before it, so a burst of `x`
                // costs one program run, not one per key.
                let mut jobs = 1;
                while let Ok(newer) = inbox.try_recv() {
                    text = newer;
                    jobs += 1;
                }
                let run = copy(&text);
                if outbox.send(Copied { jobs, run, text }).is_err() {
                    break;
                }
            }
        });
        Worker {
            jobs,
            done,
            pending: 0,
        }
    }
}

impl System {
    pub fn new() -> Self {
        Self {
            remote: env_set("SSH_TTY") || env_set("SSH_CONNECTION"),
            hung: false,
            copied_by_osc52: false,
            worker: None,
        }
    }

    fn programs_usable(&self) -> bool {
        !self.remote && !self.hung
    }

    /// Record how a copy went, and send it as OSC 52 if no program took it.
    fn settle(&mut self, run: Run<()>, text: &str) {
        if run == Run::Hung {
            self.hung = true;
        }
        self.copied_by_osc52 = run != Run::Done(());
        if self.copied_by_osc52 {
            osc52(text);
        }
    }

    /// Settle what the worker has finished; with `wait`, everything still in
    /// flight too (up to the timeout), so a paste reads the latest copy.
    fn drain(&mut self, wait: bool) {
        let deadline = Instant::now() + TOOL_TIMEOUT;
        loop {
            let Some(worker) = self.worker.as_mut() else {
                return;
            };
            if worker.pending == 0 {
                return;
            }
            let next = if wait {
                let left = deadline.saturating_duration_since(Instant::now());
                worker.done.recv_timeout(left).ok()
            } else {
                worker.done.try_recv().ok()
            };
            let Some(copied) = next else {
                return;
            };
            worker.pending = worker.pending.saturating_sub(copied.jobs);
            self.settle(copied.run, &copied.text);
        }
    }
}

impl Clipboard for System {
    fn copy(&mut self, text: &str) {
        if !self.programs_usable() {
            return self.settle(Run::Missing, text);
        }
        if let Some(worker) = self.worker.as_mut() {
            if worker.jobs.send(text.to_string()).is_ok() {
                worker.pending += 1;
                return;
            }
        }
        let run = copy_with_programs(text);
        self.settle(run, text);
    }

    fn paste(&mut self) -> Option<String> {
        self.drain(true);
        if !self.programs_usable() {
            return None;
        }
        let deadline = Instant::now() + TOOL_TIMEOUT;
        for tool in tools() {
            match run_paste(tool.paste, deadline) {
                Run::Missing => continue,
                Run::Done(text) => return Some(text),
                // `wl-paste` and `xclip -o` both fail on an empty clipboard,
                // so a failure reads as empty — unless the copy before it
                // could not reach a program either.
                Run::Failed => return (!self.copied_by_osc52).then(String::new),
                Run::Hung => {
                    self.hung = true;
                    return None;
                }
            }
        }
        None
    }

    fn set_background(&mut self, on: bool) {
        match (on, self.worker.is_some()) {
            (true, false) => self.worker = Some(Worker::spawn(copy_with_programs)),
            (false, true) => {
                // Settle what is in flight first: a fallback still owed
                // must not be dropped with the worker.
                self.drain(true);
                self.worker = None;
            }
            _ => {}
        }
    }

    fn poll(&mut self) {
        self.drain(false);
    }
}

/// Try each program in turn within one timeout: `Done` if one took the text,
/// `Hung` if one stopped answering, `Failed` if none could.
fn copy_with_programs(text: &str) -> Run<()> {
    let deadline = Instant::now() + TOOL_TIMEOUT;
    for tool in tools() {
        match run_copy(tool.copy, text, deadline) {
            Run::Missing | Run::Failed => continue,
            done_or_hung => return done_or_hung,
        }
    }
    Run::Failed
}

/// Whether an environment variable is set to something.
fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// One clipboard program: the command line that copies (text on stdin) and
/// the one that pastes (text on stdout).
struct Tool {
    copy: &'static [&'static str],
    paste: &'static [&'static str],
}

/// The programs worth trying here, in order. A missing one fails to spawn and
/// the next is tried, so listing one that is not installed costs nothing.
fn tools() -> Vec<Tool> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        out.push(Tool {
            copy: &["pbcopy"],
            paste: &["pbpaste"],
        });
    }
    if env_set("WAYLAND_DISPLAY") {
        out.push(Tool {
            copy: &["wl-copy"],
            paste: &["wl-paste", "--no-newline"],
        });
    }
    if env_set("DISPLAY") {
        out.push(Tool {
            copy: &["xclip", "-selection", "clipboard"],
            paste: &["xclip", "-selection", "clipboard", "-o"],
        });
        out.push(Tool {
            copy: &["xsel", "--clipboard", "--input"],
            paste: &["xsel", "--clipboard", "--output"],
        });
    }
    out
}

/// How one run of a clipboard program went.
#[derive(Debug, PartialEq)]
enum Run<T> {
    /// Not installed, or out of time before it could start: try the next.
    Missing,
    /// Ran and exited with an error.
    Failed,
    /// Still running at the deadline, and killed.
    Hung,
    Done(T),
}

fn spawn(argv: &[&str], stdin: Stdio, stdout: Stdio) -> Option<Child> {
    let (prog, args) = argv.split_first()?;
    Command::new(prog)
        .args(args)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// Wait for `child` to exit, killing it at the deadline.
fn finish(mut child: Child, deadline: Instant) -> Run<()> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Run::Done(()),
            Ok(Some(_)) | Err(_) => return Run::Failed,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Run::Hung;
            }
        }
    }
}

fn run_copy(argv: &[&str], text: &str, deadline: Instant) -> Run<()> {
    if Instant::now() >= deadline {
        return Run::Missing;
    }
    let Some(mut child) = spawn(argv, Stdio::piped(), Stdio::null()) else {
        return Run::Missing;
    };
    let Some(mut pipe) = child.stdin.take() else {
        return Run::Failed;
    };
    // Write on a thread: a program that hangs without reading would block a
    // write bigger than the pipe here forever, and `finish` could never reach
    // its deadline. Killing the program breaks the pipe and ends the thread.
    // The thread drops stdin when done, which is what tells the program the
    // text has ended; `xclip` and `wl-copy` then fork to keep serving the
    // selection, so the process we wait on still exits promptly.
    let (tx, rx) = mpsc::channel();
    let bytes = text.as_bytes().to_vec();
    std::thread::spawn(move || {
        let _ = tx.send(pipe.write_all(&bytes).is_ok());
    });
    match finish(child, deadline) {
        Run::Done(()) => {
            let left = deadline.saturating_duration_since(Instant::now());
            if rx.recv_timeout(left).unwrap_or(false) {
                Run::Done(())
            } else {
                Run::Failed
            }
        }
        other => other,
    }
}

/// What a paste program printed. Text that is not UTF-8 (an image, say) is
/// no text at all, so it comes back empty.
fn run_paste(argv: &[&str], deadline: Instant) -> Run<String> {
    if Instant::now() >= deadline {
        return Run::Missing;
    }
    let Some(mut child) = spawn(argv, Stdio::null(), Stdio::piped()) else {
        return Run::Missing;
    };
    // Read on a thread: a program that hangs with its pipe open would block a
    // read here forever, and `finish` could never reach its deadline.
    let Some(mut pipe) = child.stdout.take() else {
        return Run::Failed;
    };
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    });
    match finish(child, deadline) {
        Run::Done(()) => {
            let text = reader
                .join()
                .ok()
                .and_then(Result::ok)
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .map(|s| normalize(&s));
            Run::Done(text.unwrap_or_default())
        }
        Run::Failed => Run::Failed,
        Run::Hung => Run::Hung,
        Run::Missing => Run::Missing,
    }
}

/// Hand `text` to the terminal's own clipboard with an OSC 52 escape.
fn osc52(text: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", crate::image::base64(text.as_bytes()));
    let _ = out.flush();
}

/// Line endings as the buffer keeps them. A Windows-sourced clipboard says
/// `\r\n`, and an old Mac one, or a terminal's bracketed paste, a bare `\r`.
pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// A clipboard that is only a string, shared between its clones so a test can
/// hand one to the editor and keep one to look at.
#[derive(Clone, Default)]
pub struct Memory(std::rc::Rc<std::cell::RefCell<Option<String>>>);

#[cfg(test)]
impl Memory {
    pub fn holding(text: &str) -> Self {
        let m = Self::default();
        m.0.replace(Some(text.to_string()));
        m
    }

    pub fn text(&self) -> Option<String> {
        self.0.borrow().clone()
    }
}

impl Clipboard for Memory {
    fn copy(&mut self, text: &str) {
        self.0.replace(Some(text.to_string()));
    }

    fn paste(&mut self) -> Option<String> {
        self.0.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_become_newlines() {
        assert_eq!(normalize("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn a_missing_program_is_reported_missing_not_a_panic() {
        let deadline = Instant::now() + TOOL_TIMEOUT;
        assert_eq!(run_copy(&["shoin-no-such-clipboard-tool"], "x", deadline), Run::Missing);
        assert_eq!(run_paste(&["shoin-no-such-clipboard-tool"], deadline), Run::Missing);
    }

    /// A program that never reads its input is killed at the deadline, not
    /// waited on forever, however much text there is to write.
    #[cfg(unix)]
    #[test]
    fn a_copy_program_that_never_reads_times_out() {
        let big = "x".repeat(1 << 20);
        let started = Instant::now();
        let deadline = started + Duration::from_millis(200);
        assert_eq!(run_copy(&["sleep", "5"], &big, deadline), Run::Hung);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// A paste program's failure, its text, and output that is not text are
    /// told apart: only the caller knows whether a failure means empty.
    #[cfg(unix)]
    #[test]
    fn paste_programs_report_failure_text_and_non_text() {
        let deadline = Instant::now() + TOOL_TIMEOUT;
        assert_eq!(run_paste(&["false"], deadline), Run::Failed);
        assert_eq!(run_paste(&["printf", "\\377"], deadline), Run::Done(String::new()));
        assert_eq!(run_paste(&["printf", "hi"], deadline), Run::Done("hi".into()));
    }

    /// A burst of background copies costs one program run, for the newest
    /// text, and a paste-time drain settles every one of them.
    #[test]
    fn background_copies_coalesce_and_drain() {
        use std::sync::Mutex;
        static RAN: Mutex<Vec<String>> = Mutex::new(Vec::new());
        fn slow_copy(text: &str) -> Run<()> {
            std::thread::sleep(Duration::from_millis(50));
            RAN.lock().unwrap().push(text.to_string());
            Run::Done(())
        }
        let mut clip = System {
            remote: false,
            hung: false,
            copied_by_osc52: false,
            worker: Some(Worker::spawn(slow_copy)),
        };
        for text in ["one", "two", "three", "four"] {
            clip.copy(text);
        }
        clip.drain(true);
        assert_eq!(clip.worker.as_ref().unwrap().pending, 0);
        let ran = RAN.lock().unwrap().clone();
        assert_eq!(ran.last().map(String::as_str), Some("four"), "the newest text wins");
        assert!(ran.len() < 4, "the burst was coalesced: {ran:?}");
        assert!(!clip.copied_by_osc52);
    }
}
