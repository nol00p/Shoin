//! What a quote line holds past its markers.
//!
//! A quote line is drawn as one bar per level, then the rest of the line as
//! it would be drawn outside the quote. The rest is read with the list
//! commands' own cut (`text::list::QuoteLine`): a space after the last `>` is
//! the marker's, and a tab is the marker's space plus `tab_width - 1`
//! columns of indent. So `>   - b` is the item `  - b`, at the depth the
//! commands give it, with its bullet, guides and hanging indent.

use crate::text::list::QuoteLine;

use super::markdown::block::{classify, BlockKind, Carry};

/// A quote line cut after its markers, and the kind of line the rest is.
pub struct Inner {
    pub cut: QuoteLine,
    /// The view text's kind on its own. Only a list item is drawn as one;
    /// any other kind keeps the quote's styling.
    pub block: BlockKind,
}

impl Inner {
    /// The quote line `line`, whose kind is `Quote(depth)`.
    pub fn of(line: &str, depth: u8, tab_width: usize) -> Option<Inner> {
        let cut = QuoteLine::new(line, depth, tab_width)?;
        let (block, _) = classify(&cut.view, &Carry::default(), false, false, tab_width);
        Some(Inner { cut, block })
    }

    /// Whether the rest of the line is a list item.
    pub fn is_item(&self) -> bool {
        matches!(self.block, BlockKind::ListItem { .. })
    }
}

/// The bars a quote of `depth` levels draws in place of its markers, each
/// followed by a space.
pub fn bars(glyph: &str, depth: u8) -> String {
    format!("{glyph} ").repeat((depth as usize).max(1))
}
