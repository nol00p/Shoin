//! Timing for one very long line (F25, part 1).
//!
//! Ignored by default: it builds 200 KB lines and only means something in a
//! release build. Run it with
//! `cargo test --release perf_long_line -- --ignored --nocapture`.
//!
//! Drawing such a line once took 625 ms per frame when it held inline markup,
//! because the style of every display char was looked up by scanning the
//! line's spans from the start; search matches and wrapped rows each walked
//! the whole line again in the same way. The limit below is generous so a slow
//! machine does not fail it, while any of those walks still would by a wide
//! margin. A debug build only prints its timings: they say nothing about the
//! release one and would make an unfiltered `--ignored` run flaky.

use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::app::{App, Search};
use crate::config::Config;
use crate::render::markdown::block::BlockCache;
use crate::text::cursor::Cursor;

/// The slowest frame allowed in a release build. The target is 5 ms.
const LIMIT: Duration = Duration::from_millis(25);

const BOLD: &str = "word and **bold** thing ";
const MIXED: &str = "a *b* `c` ~~d~~ [e](f) **g** ";
const PLAIN: &str = "plain words only here ok ";

/// `first`, a 200 KB line of `unit`, `last`, with the cursor on `cursor_line`.
fn app_with(unit: &str, cursor_line: usize) -> App {
    let text = format!("first\n{}\nlast\n", unit.repeat(200_000 / unit.len()));
    let mut app = App::new(Config::default(), None, None).unwrap();
    app.buffer.insert_str(Cursor::new(0, 0), &text);
    app.buffer.cursor = Cursor::new(cursor_line, 0);
    app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math);
    app
}

/// The first frame, which builds the line's cache entry, and the slowest of
/// the next few, which redraw from it.
fn frames(app: &App) -> (Duration, Duration) {
    let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
    let mut draw = || {
        let t = Instant::now();
        term.draw(|f| super::frame::render(f, app)).unwrap();
        t.elapsed()
    };
    let first = draw();
    let later = (0..4).map(|_| draw()).max().unwrap();
    (first, later)
}

#[test]
#[ignore]
fn perf_long_line() {
    let search = |unit: &str, pattern: &str| {
        let mut app = app_with(unit, 0);
        app.search = Some(Search { pattern: pattern.into(), reverse: false });
        app
    };
    let cases = [
        // The cursor on the first line, so the long one is concealed.
        ("bold", app_with(BOLD, 0)),
        ("mixed", app_with(MIXED, 0)),
        ("plain", app_with(PLAIN, 0)),
        // The cursor on the long line itself, so it is drawn raw.
        ("bold, cursor on it", app_with(BOLD, 1)),
        // About 8,000 matches, each splitting a span.
        ("bold, searching `bold`", search(BOLD, "bold")),
    ];
    for (name, app) in cases {
        let (first, later) = frames(&app);
        eprintln!("200 KB line ({name}): first frame {first:?}, later frames {later:?}");
        let took = first.max(later);
        if cfg!(not(debug_assertions)) {
            assert!(took < LIMIT, "200 KB line ({name}): {took:?} per frame, limit {LIMIT:?}");
        }
    }
}
