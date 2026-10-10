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
//! Each program gets `TOOL_TIMEOUT` to answer and is killed after it, so a
//! clipboard daemon that hangs costs the editor a second, not the session.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long a clipboard program may take before it is given up on.
const TOOL_TIMEOUT: Duration = Duration::from_secs(1);

/// Somewhere text can be copied to and pasted from. `System` in the editor;
/// `Memory` in the tests, which must never touch the developer's clipboard.
pub trait Clipboard {
    fn copy(&mut self, text: &str);
    /// The clipboard's text, or `None` when it cannot be read.
    fn paste(&mut self) -> Option<String>;
}

/// The clipboard for this session: the real one, except under `cargo test`.
pub fn session() -> Box<dyn Clipboard> {
    if cfg!(test) {
        Box::new(Memory::default())
    } else {
        Box::new(System::new())
    }
}

/// The operating system's clipboard, through whichever program it has.
pub struct System {
    /// Over SSH, the programs on THIS machine would fill the remote host's
    /// clipboard, which nobody is looking at. Decided once, at startup.
    remote: bool,
}

impl System {
    pub fn new() -> Self {
        let set = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        Self {
            remote: set("SSH_TTY") || set("SSH_CONNECTION"),
        }
    }
}

impl Clipboard for System {
    fn copy(&mut self, text: &str) {
        if !self.remote && tools().iter().any(|t| run_copy(t.copy, text)) {
            return;
        }
        osc52(text);
    }

    fn paste(&mut self) -> Option<String> {
        if self.remote {
            return None;
        }
        tools().iter().find_map(|t| run_paste(t.paste))
    }
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
    let set = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if set("WAYLAND_DISPLAY") {
        out.push(Tool {
            copy: &["wl-copy"],
            paste: &["wl-paste", "--no-newline"],
        });
    }
    if set("DISPLAY") {
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

/// Wait for `child` to exit, killing it at the deadline. True on success.
fn finish(mut child: Child) -> bool {
    let deadline = Instant::now() + TOOL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn run_copy(argv: &[&str], text: &str) -> bool {
    let Some(mut child) = spawn(argv, Stdio::piped(), Stdio::null()) else {
        return false;
    };
    // Dropping stdin closes it, which is what tells the program the text has
    // ended. `xclip` and `wl-copy` then fork to keep serving the selection,
    // so the process we wait on still exits promptly.
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut pipe| pipe.write_all(text.as_bytes()).is_ok());
    finish(child) && wrote
}

fn run_paste(argv: &[&str]) -> Option<String> {
    let mut child = spawn(argv, Stdio::null(), Stdio::piped())?;
    // Read on a thread: a program that hangs with its pipe open would block a
    // read here forever, and `finish` could never reach its deadline.
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    });
    if !finish(child) {
        return None;
    }
    let bytes = reader.join().ok()?.ok()?;
    String::from_utf8(bytes).ok().map(|s| normalize(&s))
}

/// Hand `text` to the terminal's own clipboard with an OSC 52 escape.
fn osc52(text: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
}

/// Line endings as the buffer keeps them. A Windows-sourced clipboard says
/// `\r\n`, and an old Mac one, or a terminal's bracketed paste, a bare `\r`.
pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Standard base64 with padding — all OSC 52 needs, and twenty lines rather
/// than a dependency.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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
    fn base64_matches_the_rfc_vectors() {
        // RFC 4648 §10.
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in cases {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
    }

    #[test]
    fn base64_carries_utf8_bytes() {
        assert_eq!(base64("é".as_bytes()), "w6k=");
    }

    #[test]
    fn line_endings_become_newlines() {
        assert_eq!(normalize("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn a_missing_program_is_a_failed_copy_not_a_panic() {
        assert!(!run_copy(&["shoin-no-such-clipboard-tool"], "x"));
        assert_eq!(run_paste(&["shoin-no-such-clipboard-tool"]), None);
    }
}
