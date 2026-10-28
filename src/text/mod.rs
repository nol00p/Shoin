//! Text layer. Knows nothing about rendering, input, or config. SPEC.md §4.
//! The one exception is `list`, which reads the block cache's line kinds
//! rather than parse list markers a second time.

pub mod buffer;
pub mod cursor;
pub mod edit;
pub mod history;
pub mod list;
pub mod motion;
pub mod object;
