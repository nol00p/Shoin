//! Timing for one very long line with inline markup (F25, part 1).
//!
//! Ignored by default: it builds 200 KB lines and only means something in a
//! release build. Run it with
//! `cargo test --release perf_long_line -- --ignored --nocapture`.
//!
//! Concealing such a line once took 625 ms per frame, because the style of
//! every display char was looked up by scanning the line's spans from the
//! start. The limits below are generous so a slow machine does not fail them,
//! while the quadratic walk still would by a wide margin.

use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::app::App;
use crate::config::Config;
use crate::render::markdown::block::BlockCache;
use crate::text::cursor::Cursor;

/// The slowest frame allowed in a release build. The target is 5 ms.
const LIMIT_RELEASE: Duration = Duration::from_millis(25);
/// A debug build runs many times slower, but still far below the quadratic walk.
const LIMIT_DEBUG: Duration = Duration::from_millis(500);

fn app_with(text: &str) -> App {
    let mut app = App::new(Config::default(), None, None).unwrap();
    app.buffer.insert_str(Cursor::new(0, 0), text);
    app.buffer.cursor = Cursor::new(0, 0);
    app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math);
    app
}

/// The slowest of a few frames, the first included: the first builds the
/// line's cache entry, the rest redraw from it.
fn slowest_frame(app: &App) -> Duration {
    let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
    (0..5)
        .map(|_| {
            let t = Instant::now();
            term.draw(|f| super::frame::render(f, app)).unwrap();
            t.elapsed()
        })
        .max()
        .unwrap()
}

#[test]
#[ignore]
fn perf_long_line() {
    let limit = if cfg!(debug_assertions) { LIMIT_DEBUG } else { LIMIT_RELEASE };
    for (name, unit) in [
        ("bold", "word and **bold** thing "),
        ("mixed", "a *b* `c` ~~d~~ [e](f) **g** "),
        ("plain", "plain words only here ok "),
    ] {
        // The cursor sits on the first line, so the long one is concealed.
        let doc = format!("first\n{}\nlast\n", unit.repeat(200_000 / unit.len()));
        let app = app_with(&doc);
        let took = slowest_frame(&app);
        eprintln!("200 KB line ({name}): slowest frame {took:?}");
        assert!(took < limit, "200 KB line ({name}): {took:?} per frame, limit {limit:?}");
    }
}
