//! Timing on a large document: what one keystroke costs at 100,000 lines.
//!
//! Every edit used to walk the whole document several times (the render
//! cache re-verified every line, the block cache rescanned to the end when
//! the line count moved, the word count re-read the rope), so a typed
//! character cost ~50 ms at 4.5 MB. The work is now proportional to the
//! edit, and this holds it there.
//!
//! Timings mean little in a debug build, so it is `#[ignore]`d:
//!
//! ```sh
//! cargo test --release -- --ignored perf_large_doc --nocapture
//! ```

use super::*;
use crate::render::markdown::block::BlockCache;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

fn app_with(text: &str) -> App {
    let mut app = App::new(Config::default(), None, None).unwrap();
    app.buffer.insert_str(Cursor::new(0, 0), text);
    app.buffer.cursor = Cursor::new(0, 0);
    app.buffer.history = Default::default();
    app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math);
    app
}

/// One keystroke and the frame after it, as the event loop runs them: the
/// key, then the caches, then the draw (which syncs the render cache and
/// draws the status line, word count included).
fn keystroke(app: &mut App, term: &mut Terminal<TestBackend>, code: KeyCode) -> Duration {
    let t = Instant::now();
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    app.sync_after_input();
    app.refresh_blocks();
    app.refresh_focus();
    term.draw(|f| crate::render::frame::render(f, app)).unwrap();
    t.elapsed()
}

/// The slowest of a few runs would be noise-bound; the median of several is
/// what a user feels while typing.
fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

#[test]
#[ignore]
fn perf_large_doc() {
    let doc: String = (0..100_000)
        .map(|i| format!("Line {i} with *some* text and a [link](x) #tag\n"))
        .collect();
    let mut app = app_with(&doc);
    app.term_size = (100, 40);
    let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
    app.refresh_blocks();
    term.draw(|f| crate::render::frame::render(f, &app)).unwrap();

    // Halfway down, so neither end of the document is the cheap side.
    for _ in 0..50_000 {
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    }
    app.sync_after_input();
    keystroke(&mut app, &mut term, KeyCode::Char('i'));

    let typed = median((0..21).map(|_| keystroke(&mut app, &mut term, KeyCode::Char('x'))).collect());
    let enter = median((0..9).map(|_| keystroke(&mut app, &mut term, KeyCode::Enter)).collect());
    keystroke(&mut app, &mut term, KeyCode::Esc);
    let dd = median(
        (0..9)
            .map(|_| {
                keystroke(&mut app, &mut term, KeyCode::Char('d'));
                keystroke(&mut app, &mut term, KeyCode::Char('d'))
            })
            .collect(),
    );
    let u = median((0..9).map(|_| keystroke(&mut app, &mut term, KeyCode::Char('u'))).collect());
    let j = median((0..9).map(|_| keystroke(&mut app, &mut term, KeyCode::Char('j'))).collect());
    eprintln!("100,000 lines: typed char {typed:?}, Enter {enter:?}, dd {dd:?}, u {u:?}, j {j:?}");

    // The target is 5 ms a keystroke; the margin covers a slow or busy machine.
    let limit = Duration::from_millis(10);
    for (what, took) in [("typed char", typed), ("Enter", enter), ("dd", dd), ("u", u)] {
        assert!(took < limit, "{what} took {took:?} at 100,000 lines");
    }
}
