//! The random-session test: drives a real `App` with random keys, pastes,
//! mouse events, `:` commands, overlays, resizes and real files with embeds,
//! and after every step renders a frame, checks the block cache against a full
//! rebuild and checks the cursor is inside the text. At the end it undoes
//! everything and checks the original text is back, then redoes it all.
//!
//! It changes the working directory and `HOME` (to a temp folder), so it is
//! `#[ignore]`d and runs on its own, in one thread:
//!
//! ```sh
//! cargo test -- --ignored --test-threads=1 fuzz
//! FUZZ_N=2000 FUZZ_SEED=1 FUZZ_STEPS=250 cargo test -- --ignored --test-threads=1 fuzz
//! ```
//!
//! Safety rules it depends on — keep them:
//! - the working directory and `HOME` are a temp folder;
//! - the leader key is never sent in Normal mode, and `app.tree` is cleared
//!   after every step: the tree's `d` + `y` deletes real files and `-` climbs
//!   out of the folder;
//! - `x` never follows `g` (`gx` runs the desktop opener; `spawn_opener` also
//!   refuses under `cfg!(test)`).

use super::*;
use crate::render::markdown::block::BlockCache;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn app_with(text: &str) -> App {
    let mut app = App::new(Config::default(), None, None).unwrap();
    app.buffer.insert_str(Cursor::new(0, 0), text);
    app.buffer.cursor = Cursor::new(0, 0);
    app.buffer.history = Default::default();
    app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math);
    app
}

fn text(app: &App) -> String {
    app.buffer.rope.to_string()
}

fn key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    app.sync_after_input();
}

/// xorshift64: tiny, deterministic, and a seed reproduces a session exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const DOCS: &[&str] = &[
    "# Title\n\nSome *prose* with **bold**, `code`, [a link](https://example.com) and [[wiki|alias]] #tag.\n\n- one\n- two\n  - nested\n    - deeper\n- [ ] task\n- [x] done\n\n1. first\n2. second\n\n> quote\n> more\n\n```rust\nfn main() { let s = \"x\"; /* c */ }\n```\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\n\n$$\nx^2 + \\frac{a}{b}\n$$\n\nInline $a_i^2$ math. ==mark== ~~gone~~\n\n---\n\nlast line without newline",
    "",
    "\n\n\n",
    "x",
    "日本語のテキスト、全角。🙂 emoji 👨‍👩‍👧 family\n\tTabbed\tline\n    - indented item\nA very long line that goes on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on.\n",
    "---\ntitle: fm\n---\n# After front matter\n![[missing-note]]\n![[missing.png]]\n![alt](nothing.png)\n\n1. a\n2. b\n3. c\n4. d\n5. e\n6. f\n7. g\n8. h\n9. i\n10. j\n    ```\n    code in ten\n    ```\n    - sub\n\n$$ unclosed\n\n``` unclosed fence\nstill code\n",
    "| only | header |\n| - | - |\n\n|x|\n|-|\n|\\||\n\n> - quoted list\n> > nested quote\n\n-\n- \n1.\n* * *\n   ***\n#\n## \n###### six\n####### seven\n",
    "a\u{2028}b\u{c}c\rd\n\u{feff}bom\n\u{200b}zero width\u{301}combining\n",
];

/// One session of `steps` random events from `seed`. Every event is pushed to
/// `trace` before it runs, so a panic still leaves the full trace behind.
fn session(seed: u64, steps: usize, trace: &mut Vec<String>) -> Result<(), String> {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut rng = Rng(seed | 1);
    let mut app = app_with(DOCS[rng.below(DOCS.len())]);
    let original = text(&app);
    let sizes = [(80u16, 24u16), (40, 10), (20, 5), (10, 3), (3, 2), (1, 1), (200, 60), (0, 0), (5, 30), (120, 4)];
    let (mut w, mut h) = sizes[rng.below(sizes.len())];
    app.term_size = (w, h);
    trace.push(format!("<size {w}x{h}>"));
    // No space here: Space is the leader in Normal mode, and the leader opens
    // the file tree and other things that reach outside the temp folder.
    let normal = "hjklwbeWBE0$^GggxXdDcCyYpPuoOiIaAvVJ~.;,nN{}()[]<>\"'`-+*#=|123456789fFtTrzgZ%&_/?hjklhjkldduu";
    let typed = "abc xyz\t-*#`[]()|$>1.!=~_\\\"jk日🙂";
    let ex = [
        "sp", "vs", "close", "only", "focus", "focus sentence", "typewriter", "number", "number relative", "align",
        "align justified", "embed", "embed full", "embed rec", "embed short", "view", "zen", "fixlist", "u", "red", "bn",
        "bp", "set conceal off", "set conceal on", "set measure 20", "set measure 200", "set tab_width 8", "set math off",
        "set math on", "diff", "revert", "ls", "help", "set hints off", "b 1",
    ];
    let mut term = Terminal::new(TestBackend::new(w.max(1), h.max(1))).unwrap();
    for step in 0..steps {
        let roll = rng.below(100);
        let overlay = app.help.is_some() || app.finder.is_some() || app.diff.is_some();
        let insertish = matches!(app.mode, Mode::Insert | Mode::Command(_) | Mode::Search { .. } | Mode::Prompt(_));
        let ev: Option<KeyEvent> = if roll < 3 {
            let c = ex[rng.below(ex.len())];
            trace.push(format!(":{c}"));
            if !overlay {
                app.run_command(c);
                app.sync_after_input();
            }
            None
        } else if roll < 5 {
            (w, h) = sizes[rng.below(sizes.len())];
            app.term_size = (w, h);
            term = Terminal::new(TestBackend::new(w.max(1), h.max(1))).unwrap();
            trace.push(format!("<resize {w}x{h}>"));
            None
        } else if roll < 12 {
            Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        } else if roll < 20 {
            let codes = [
                KeyCode::Enter, KeyCode::Backspace, KeyCode::Tab, KeyCode::BackTab, KeyCode::Left, KeyCode::Right,
                KeyCode::Up, KeyCode::Down, KeyCode::Home, KeyCode::End, KeyCode::PageUp, KeyCode::PageDown,
                KeyCode::Delete,
            ];
            Some(KeyEvent::new(codes[rng.below(codes.len())], KeyModifiers::NONE))
        } else if roll < 27 {
            let cs: Vec<char> = "dufbreywvo^hjkl".chars().collect();
            let c = cs[rng.below(cs.len())];
            let m = if rng.below(3) == 0 { KeyModifiers::ALT } else { KeyModifiers::CONTROL };
            Some(KeyEvent::new(KeyCode::Char(c), m))
        } else if roll < 29 {
            trace.push("<paste>".into());
            app.on_paste("pasted **text**\nsecond line\n".into());
            app.sync_after_input();
            None
        } else if roll < 34 {
            use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
            let kinds = [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Drag(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollUp,
                MouseEventKind::Drag(MouseButton::Left),
            ];
            let kind = kinds[rng.below(kinds.len())];
            let (c, r) = (rng.below(w as usize + 3) as u16, rng.below(h as usize + 3) as u16);
            trace.push(format!("<mouse {kind:?} {c},{r}>"));
            app.on_mouse(MouseEvent { kind, column: c, row: r, modifiers: KeyModifiers::NONE });
            app.sync_after_input();
            None
        } else if roll < 37 {
            let acts = [
                Action::Help,
                Action::ToggleZen,
                Action::FindBuffer,
                Action::ToggleSplit { vertical: true },
                Action::ToggleSplit { vertical: false },
                Action::ToggleConceal,
                Action::CycleFocus,
                Action::AlternateBuffer,
                Action::FindFile { root: Root::Cwd },
                Action::ToggleView,
            ];
            let a = acts[rng.below(acts.len())].clone();
            trace.push(format!("<{a:?}>"));
            if !overlay && !matches!(app.mode, Mode::Insert) {
                app.run(Command::bare(a));
                app.sync_after_input();
            }
            None
        } else if roll < 39 {
            let files = ["a.md", "b.md", "c.txt", "pic.png", "sub/d.md"];
            let f = files[rng.below(files.len())];
            trace.push(format!("<open {f}>"));
            if !overlay {
                app.open_file(PathBuf::from(f));
                app.sync_after_input();
            }
            None
        } else {
            let pool: Vec<char> = if insertish { typed } else { normal }.chars().collect();
            let mut c = pool[rng.below(pool.len())];
            if c == 'x' && app.pending.prefix() == Some('g') {
                c = 'j';
            }
            Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
        };
        if let Some(ev) = ev {
            let mods = if ev.modifiers.is_empty() { String::new() } else { format!("+{:?}", ev.modifiers) };
            trace.push(format!("{:?}{mods}", ev.code));
            app.on_key(ev);
            app.sync_after_input();
        }
        app.tree = None;
        app.quit = false;
        app.refresh_blocks();
        app.refresh_focus();
        let fresh = BlockCache::build(&app.buffer, app.config.markdown.math);
        if fresh.kinds != app.blocks.kinds || fresh.carry != app.blocks.carry {
            return Err(format!("step {step}: block cache differs from a rebuild\ntext: {:?}", text(&app)));
        }
        let c = app.buffer.cursor;
        if c.line >= app.buffer.line_count().max(1) || c.col > app.buffer.line_len(c.line) {
            return Err(format!("step {step}: cursor {c:?} out of bounds\ntext: {:?}", text(&app)));
        }
        term.draw(|f| crate::render::frame::render(f, &app)).unwrap();
        let _ = app.visible_pictures();
    }
    // Undo everything: the text must come back. Only checked when the session
    // ends on the buffer it started with.
    app.help = None;
    app.finder = None;
    app.diff = None;
    app.set_view(false);
    for _ in 0..3 {
        key(&mut app, KeyCode::Esc);
    }
    if app.current() != 0 || app.docs.len() != 1 {
        return Ok(());
    }
    let fin = text(&app);
    let mut n = 0;
    while app.buffer.undo() {
        n += 1;
        if n > 100_000 {
            break;
        }
    }
    if text(&app) != original {
        return Err(format!("undoing everything did not restore the text\nwant {original:?}\n got {:?}", text(&app)));
    }
    for _ in 0..n {
        app.buffer.redo();
    }
    if text(&app) != fin {
        return Err(format!("redoing everything did not restore the final text\nundo steps {n}\nwant {fin:?}\n got {:?}", text(&app)));
    }
    Ok(())
}

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

/// `FUZZ_N` sessions (default 50) from `FUZZ_SEED` (default 1), each
/// `FUZZ_STEPS` long (default 250). A failure prints its seed and full trace;
/// `FUZZ_N=1 FUZZ_SEED=<seed>` replays it. `FUZZ_LOUD` keeps panic messages.
#[test]
#[ignore = "changes the working directory and HOME; run alone: cargo test -- --ignored --test-threads=1 fuzz"]
fn fuzz_random_sessions() {
    let dir = std::env::temp_dir().join(format!("shoin-fuzz-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.md"), "# A\n\n![[b]]\n\n![[pic.png]]\n\n![[b#Two]]\n![[sub/d#^blk]]\n![[a]]\ntext [[b]] end\n").unwrap();
    std::fs::write(dir.join("b.md"), "---\nt: 1\n---\n# B\n\n![[c.txt]]\n\n## Two\n\n- x\n- y\n\n```\n# not heading\n```\n").unwrap();
    std::fs::write(dir.join("c.txt"), "plain text file\nwith ![[a]] a loop\n").unwrap();
    std::fs::write(dir.join("sub/d.md"), "para one\nline ^blk\n\nother\n").unwrap();
    // A 1x1 PNG header: enough for the probe to size it.
    std::fs::write(
        dir.join("pic.png"),
        [
            0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xDE,
        ],
    )
    .unwrap();
    let old_cwd = std::env::current_dir().ok();
    let old_home = std::env::var_os("HOME");
    std::env::set_current_dir(&dir).unwrap();
    // Safe only because this test runs alone in its process
    // (`--test-threads=1` and a name filter): nothing else reads the
    // environment or the working directory meanwhile.
    std::env::set_var("HOME", &dir);

    let sessions: u64 = env_or("FUZZ_N", 50);
    let base: u64 = env_or("FUZZ_SEED", 1);
    let steps: usize = env_or("FUZZ_STEPS", 250);
    let quiet = std::env::var_os("FUZZ_LOUD").is_none();
    let old_hook = quiet.then(|| {
        let old = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        old
    });

    let mut failures = Vec::new();
    for s in base..base + sessions {
        let seed = s.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut trace = Vec::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session(seed, steps, &mut trace)));
        let msg = match r {
            Ok(Ok(())) => continue,
            Ok(Err(e)) => e,
            Err(p) => {
                let m = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                format!("panic: {m}")
            }
        };
        failures.push(format!("FUZZ_SEED={s} (seed {seed:#x}): {msg}\ntrace: {}", trace.join(" ")));
    }

    if let Some(old) = old_hook {
        std::panic::set_hook(old);
    }
    if let Some(cwd) = old_cwd {
        let _ = std::env::set_current_dir(cwd);
    }
    match old_home {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }
    let _ = std::fs::remove_dir_all(&dir);

    for f in &failures {
        eprintln!("{f}\n");
    }
    assert!(failures.is_empty(), "{} of {sessions} random sessions failed (traces above)", failures.len());
}
