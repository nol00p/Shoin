//! The random-session test: drives a real `App` with random keys, pastes,
//! mouse events, `:` commands, overlays, resizes and real files with embeds,
//! and after every step renders a frame, checks the block cache against a full
//! rebuild and checks the cursor is inside the text. In read-only mode a key
//! that scrolls must move the view unless it is at that end. At the end it undoes
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
use crate::test_support::app_with_clean_history as app_with;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
    "> ---\n> - a\n>   - a1\n>\t- b\n>- c\n> 1. x\n> 2. y\n> > - deeper\n>\n> 10. j\n>    - k\nlazy\n\n- out\n  > - in an item\n",
    "a\u{2028}b\u{c}c\rd\n\u{feff}bom\n\u{200b}zero width\u{301}combining\n",
    // Display math: closed, one-line, run together, under list items, left
    // open, in a quote, under a heading to fold, and closed on the last line.
    "Intro\n\n$$\n\\frac{a}{b}\n$$\n$$x^2$$\n$$ a\nb $$\n\n- item\n  $$\n  \\sum_{i=1}^{n} i\n  $$\n  - nested\n\n    $$y$$\n\n$$\nunclosed\n\n> $$\n> q\n> $$\n\n# H\n$$\n\\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}\n$$\ntext\n$$\nlast\n$$",
    // A matrix taller than most of the sizes, then lines, and a long wrapped
    // line: read-only mode has to scroll through all of their rows.
    "top\n\n$$\n\\begin{bmatrix}\na \\\\\nb \\\\\nc \\\\\nd \\\\\ne \\\\\nf \\\\\ng \\\\\nh \\\\\ni \\\\\nj \\\\\nk \\\\\nl\n\\end{bmatrix}\n$$\nafter one\nwords words words words words words words words words words words words words words words words words words words words words\n\n$$\n\\begin{pmatrix}\n1 \\\\\n2 \\\\\n3 \\\\\n4 \\\\\n5 \\\\\n6\n\\end{pmatrix}\n$$",
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
    // `z` is there to fold and scroll: `za` `zc` `zo` `zM` `zR` `zt` `zz`
    // `zb` come up as pairs too.
    let normal = "zazczMzRzoztzzzbhjklwbeWBE0$^GggxXdDcCyYpPuoOiIaAvVJ~.;,nN{}()[]<>\"'`-+*#=|123456789fFtTrzgZ%&_/?hjklhjkldduu";
    let typed = "abc xyz\t-*#`[]()|$>1.!=~_\\\"jk日🙂";
    let ex = [
        "sp", "vs", "close", "only", "focus", "focus sentence", "typewriter", "number", "number relative", "align",
        "align justified", "embed", "embed full", "embed rec", "embed short", "view", "zen", "fixlist", "u", "red", "bn",
        "bp", "set conceal off", "set conceal on", "set measure 20", "set measure 200", "set tab_width 8", "set math off",
        "set math on", "diff", "revert", "ls", "help", "set hints off", "b 1", "3", "+2", "d", "2,4d", "y 3",
        ".,+2>", "<", "%<", "m 0", "m $", "1,3m $", "t 0", "$-1,$t 0", "'<,'>d", "'<,'>m 0", "%fixlist",
    ];
    let mut term = Terminal::new(TestBackend::new(w.max(1), h.max(1))).unwrap();
    for step in 0..steps {
        let roll = rng.below(100);
        let overlay = app.help.is_some() || app.finder.is_some() || app.diff.is_some();
        let insertish = matches!(app.mode, Mode::Insert | Mode::Command(_) | Mode::Search { .. } | Mode::Prompt(_));
        // Where read-only mode's view stood before a key that scrolls it.
        let reading = app.view && !overlay && matches!(app.mode, Mode::Normal) && app.pending.is_clean();
        let before = if reading { reading_rows(&app) } else { None };
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
                Action::Fold(crate::input::action::FoldCmd::Toggle),
                Action::Fold(crate::input::action::FoldCmd::Close),
                Action::Fold(crate::input::action::FoldCmd::Open),
                Action::Fold(crate::input::action::FoldCmd::CloseAll),
                Action::Fold(crate::input::action::FoldCmd::CloseAll),
                Action::Fold(crate::input::action::FoldCmd::OpenAll),
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
        // Down (true) or up (false), for a key that scrolls read-only mode.
        let mut scrolls = None;
        if let Some(ev) = ev {
            let mods = if ev.modifiers.is_empty() { String::new() } else { format!("+{:?}", ev.modifiers) };
            trace.push(format!("{:?}{mods}", ev.code));
            let ctrl = ev.modifiers == KeyModifiers::CONTROL;
            scrolls = match ev.code {
                KeyCode::Char('j') | KeyCode::Down if ev.modifiers.is_empty() => Some(true),
                KeyCode::Char('k') | KeyCode::Up if ev.modifiers.is_empty() => Some(false),
                KeyCode::Char('d' | 'f') if ctrl => Some(true),
                KeyCode::Char('u' | 'b') if ctrl => Some(false),
                _ => None,
            };
            app.on_key(ev);
            app.sync_after_input();
        }
        // Read-only mode: a scrolling key moves the view a row or more,
        // unless it already shows that end of the text. Every row is
        // reachable, a tall layout's, an embed's and a wrapped line's too.
        if let (Some(down), Some((row, last))) = (scrolls, before) {
            if app.view {
                if let Some((now, _)) = reading_rows(&app) {
                    let stuck = if down { row < last && now <= row } else { row > 0 && now >= row };
                    if stuck {
                        let dir = if down { "down" } else { "up" };
                        return Err(format!(
                            "step {step}: read-only view stuck going {dir} at row {row} (last top {last}), \
                             {w}x{h}, cursor {:?}, {} panes\ntext: {:?}",
                            app.buffer.cursor,
                            app.layout.ids().len(),
                            text(&app)
                        ));
                    }
                }
            }
        }
        app.tree = None;
        app.quit = false;
        app.refresh_blocks();
        app.refresh_focus();
        let fresh = BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width);
        if fresh.kinds != app.blocks.kinds
            || fresh.carry != app.blocks.carry
            || fresh.math_open_at_end != app.blocks.math_open_at_end
        {
            return Err(format!("step {step}: block cache differs from a rebuild\ntext: {:?}", text(&app)));
        }
        // The closed math blocks kept with the kinds are the ones a rebuild finds.
        let (kept, rebuilt) = (crate::render::hidden::math_blocks(&app.blocks), crate::render::hidden::math_blocks(&fresh));
        if kept != rebuilt {
            return Err(format!("step {step}: kept math blocks {kept:?}, a rebuild finds {rebuilt:?}\ntext: {:?}", text(&app)));
        }
        let c = app.buffer.cursor;
        if c.line >= app.buffer.line_count().max(1) || c.col > app.buffer.line_len(c.line) {
            return Err(format!("step {step}: cursor {c:?} out of bounds\ntext: {:?}", text(&app)));
        }
        // Folds are per pane. In every pane, every fold is inside the text
        // and still measures as an item or a section; the focused pane's
        // never hide the cursor; and no fold outlives its pane.
        app.sync_folds();
        let n = app.buffer.line_count();
        let panes = app.layout.ids();
        for (pane, folds) in &app.docs[app.current()].folds {
            if !panes.contains(pane) {
                return Err(format!("step {step}: folds kept for closed pane {pane}"));
            }
            for f in folds.closed() {
                if f.end >= n || f.start >= f.end {
                    return Err(format!("step {step}: pane {pane} fold {f:?} outside {n} lines\ntext: {:?}", text(&app)));
                }
                let synced = app.blocks.revision == app.buffer.revision;
                if *pane == app.focus_pane && f.hides(c.line) && !insertish && synced {
                    return Err(format!("step {step}: cursor {c:?} inside fold {f:?}\ntext: {:?}", text(&app)));
                }
            }
        }
        term.draw(|f| crate::render::frame::render(f, &app)).unwrap();
        index_invariant(&app).map_err(|e| format!("step {step}: {e}\ntext: {:?}", text(&app)))?;
        let _ = app.visible_pictures();
        math_invariants(&app).map_err(|e| format!("step {step}: {e}\ntext: {:?}", text(&app)))?;
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

/// Read-only mode's view of the focused pane: the row it starts at and the
/// last row it can start at. `None` when there is no view to move.
fn reading_rows(app: &App) -> Option<(usize, usize)> {
    let area = ratatui::layout::Rect { x: 0, y: 0, width: app.term_size.0, height: app.term_size.1 };
    let mut got = None;
    crate::render::frame::reading_view(app, area, |row, last, _| {
        got = Some((row.min(last), last));
        None
    });
    got
}

/// The row index the cache keeps from the frame, the focused pane's, is the
/// one a fresh cache builds at the same measure: a sync that keeps it
/// (`IndexKey`) never misses a change to what it is built from.
fn index_invariant(app: &App) -> Result<(), String> {
    let mut cache = app.cache.borrow_mut();
    let measure = cache.measure();
    cache.sync(app, measure);
    let mut fresh = crate::render::cache::RenderCache::default();
    fresh.sync(app, measure);
    for line in 0..app.buffer.line_count() {
        let kept = (cache.line_span(line), cache.is_concealed(line));
        let built = (fresh.line_span(line), fresh.is_concealed(line));
        if kept != built {
            return Err(format!("the kept row index has line {line} (rows, concealed) {kept:?}, a fresh one {built:?}"));
        }
    }
    if cache.total_rows() != fresh.total_rows() {
        return Err(format!("the kept row index has {} rows, a fresh one {}", cache.total_rows(), fresh.total_rows()));
    }
    Ok(())
}

/// Display math laid out over several rows (#77): the active set never cuts
/// a closed block (it holds all of it or none), the cursor is never on a
/// laid-out line, and the row index maps every row to a line, in order, with
/// every line's rows where `line_rows` says.
fn math_invariants(app: &App) -> Result<(), String> {
    let markdown = app.buffer.syntax == crate::text::buffer::Syntax::Markdown;
    let fresh = app.blocks.revision == app.buffer.revision;
    let a = &app.active;
    let blocks = crate::render::hidden::math_blocks(&app.blocks).to_vec();
    if fresh && markdown && app.config.layout.conceal && !app.view {
        for b in &blocks {
            let touches = a.start < b.end && a.end >= b.start;
            if touches && (a.start > b.start || a.end + 1 < b.end) {
                return Err(format!("active {}..={} cuts math block {b:?}", a.start, a.end));
            }
        }
    }
    // A sync of its own, so the index describes this text whatever the frame
    // drew (a pane squeezed to nothing draws none).
    let mut cache = app.cache.borrow_mut();
    cache.sync(app, 40);
    let c = app.buffer.cursor;
    if !app.view {
        if let Some(b) = blocks.iter().find(|b| b.contains(&c.line)) {
            if cache.math_row(b.start, 0).is_some() {
                return Err(format!("cursor {c:?} is on laid-out block {b:?}"));
            }
        }
    }
    let mut prev = 0;
    for r in 0..cache.total_rows() {
        let row = cache.row(r).ok_or(format!("row {r} of {} is missing", cache.total_rows()))?;
        if row.line() < prev {
            return Err(format!("row {r} goes back to line {} from {prev}", row.line()));
        }
        prev = row.line();
        let (first, last) = cache.line_rows(row.line());
        if !row.is_spacer() && !(first..=last).contains(&r) {
            return Err(format!("row {r} of line {} outside its rows {first}..={last}", row.line()));
        }
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
