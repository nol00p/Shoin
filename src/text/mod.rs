//! Text layer. Knows nothing about rendering, input, or config. SPEC.md §4.
//! The exceptions are `list` and `table`, which read the block cache's line kinds
//! rather than parse list markers and table rows a second time.

pub mod buffer;
pub mod cursor;
pub mod edit;
pub mod history;
pub mod list;
pub mod motion;
pub mod object;
pub mod table;
