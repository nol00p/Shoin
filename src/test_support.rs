//! Fixtures for tests that start with an in-memory document.

use crate::app::App;
use crate::config::Config;
use crate::render::markdown::block::BlockCache;
use crate::text::cursor::Cursor;

pub(crate) fn app_with(text: &str) -> App {
    app_with_config(text, Config::default())
}

pub(crate) fn app_with_config(text: &str, config: Config) -> App {
    let mut app = App::new(config, None, None).unwrap();
    app.buffer.insert_str(Cursor::new(0, 0), text);
    app.buffer.cursor = Cursor::new(0, 0);
    app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width);
    app
}

/// Treat the fixture text as the starting document for undo and redo.
pub(crate) fn app_with_clean_history(text: &str) -> App {
    let mut app = app_with(text);
    app.buffer.history = Default::default();
    app
}
