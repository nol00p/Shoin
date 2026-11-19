//! Depth-colored indent guides. SPEC.md §5.4.
//!
//! Leading whitespace on list items is replaced by a guide glyph colored by
//! nesting depth, cycling through `theme.indent_colors`. The glyph occupies
//! exactly the column the whitespace occupied, so SPEC.md §2 holds.
//!
//! The feature is split in two: this module marks and colors the columns, and
//! the conceal map swaps the glyph in (concealed lines only, like every other
//! conceal op — the line under the cursor keeps its literal spaces).

use crate::config::schema::GlyphConfig;

use super::markdown::block::BlockKind;
use super::theme::{Style, Theme};
use super::StyledSpan;

/// Guide positions and colors for one line's leading whitespace.
pub struct Guides {
    /// (column, depth) pairs — one per guide glyph.
    pub marks: Vec<(usize, u8)>,
}

/// One guide per completed indent level in a list item's leading whitespace.
///
/// Levels are `tab_width` columns, counted as the classifier counts them
/// (`block::indent_cols`: a tab is `tab_width` columns). Each mark is the
/// CHAR offset where its level starts, and the char there may be a tab; a
/// level that starts inside a tab gets no mark, as the tab already covers it.
///
/// A quote line gets the guides of the list item past its markers, as that
/// item would have them outside the quote, each at the source char its
/// column stands for (`QuoteLine::doc_col`): a guide in the columns of a tab
/// after the last `>` is on that tab.
pub fn guides_for(line: &str, block: &BlockKind, tab_width: usize) -> Guides {
    let mut marks = Vec::new();
    if let BlockKind::Quote(depth) = block {
        if let Some(inner) = super::quote::Inner::of(line, *depth, tab_width).filter(|i| i.is_item()) {
            marks = guides_for(&inner.cut.view, &inner.block, tab_width)
                .marks
                .into_iter()
                .map(|(col, depth)| (inner.cut.doc_col(col), depth))
                .collect();
        }
        return Guides { marks };
    }
    if !matches!(block, BlockKind::ListItem { .. }) {
        return Guides { marks };
    }
    let t = tab_width.max(1);
    let total = crate::render::markdown::block::indent_cols(line, t);
    let mut col = 0;
    for (i, c) in line.chars().enumerate() {
        let w = match c {
            ' ' => 1,
            '\t' => t,
            _ => break,
        };
        // Only a complete level gets a guide: the stray columns past the last
        // one are left.
        if col % t == 0 && col + t <= total {
            marks.push((i, (col / t) as u8));
        }
        col += w;
    }
    Guides { marks }
}

/// Recolor each guide column with its depth color (cycling `indent_colors`).
///
/// This half only COLORS the columns. The width-preserving glyph substitution
/// (`│` in place of the space) is a `Conceal::Replace` from
/// `conceal::indent_guide_ops`, which is why `glyphs` is not consulted here —
/// that replacement is `plain`, so it inherits the color set below.
pub fn apply(spans: &mut Vec<StyledSpan>, guides: &Guides, theme: &Theme, _glyphs: &GlyphConfig) {
    if guides.marks.is_empty() || spans.is_empty() || theme.indent_colors.is_empty() {
        return;
    }
    let mut rebuilt: Vec<StyledSpan> = Vec::with_capacity(spans.len() + guides.marks.len());
    for sp in spans.iter() {
        let mut col = sp.range.start;
        for &(mark, depth) in &guides.marks {
            if mark < sp.range.start || mark >= sp.range.end {
                continue;
            }
            if mark > col {
                rebuilt.push(StyledSpan {
                    range: col..mark,
                    style: sp.style,
                });
            }
            let color = theme.indent_colors[depth as usize % theme.indent_colors.len()];
            rebuilt.push(StyledSpan {
                range: mark..mark + 1,
                style: Style::fg(color),
            });
            col = mark + 1;
        }
        if col < sp.range.end {
            rebuilt.push(StyledSpan {
                range: col..sp.range.end,
                style: sp.style,
            });
        }
    }
    *spans = rebuilt;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::markdown::block::Marker;

    fn list(depth: u8) -> BlockKind {
        BlockKind::ListItem {
            depth,
            marker: Marker::Dash,
            checked: None,
        }
    }

    #[test]
    fn one_guide_per_level() {
        // 4 spaces, tab_width 2 -> two levels at columns 0 and 2.
        let g = guides_for("    - x", &list(4), 2);
        assert_eq!(g.marks, vec![(0, 0), (2, 1)]);
    }

    #[test]
    fn partial_indent_yields_no_leftover_guide() {
        // 3 spaces, tab_width 2 -> one complete level; the stray column is left.
        let g = guides_for("   - x", &list(3), 2);
        assert_eq!(g.marks, vec![(0, 0)]);
    }

    /// Levels are counted in columns, a tab as `tab_width`, and marked on
    /// the char their level starts at.
    #[test]
    fn a_tab_is_tab_width_columns_of_levels() {
        assert_eq!(guides_for("\t\t- x", &list(8), 4).marks, vec![(0, 0), (1, 1)]);
        assert_eq!(guides_for("\t  - x", &list(4), 2).marks, vec![(0, 0), (1, 1)]);
        assert_eq!(guides_for("\t  - x", &list(6), 4).marks, vec![(0, 0)]);
        // A level that starts inside a tab has no char of its own.
        assert_eq!(guides_for(" \t- x", &list(5), 4).marks, vec![(0, 0)]);
        assert_eq!(guides_for("\t- x", &list(8), 8).marks, vec![(0, 0)]);
    }

    #[test]
    fn only_lists_are_guided() {
        assert!(guides_for("  > quote", &BlockKind::Quote(1), 2).marks.is_empty());
        assert!(guides_for(">     quote", &BlockKind::Quote(1), 2).marks.is_empty());
        assert!(guides_for("no indent", &BlockKind::Paragraph, 2).marks.is_empty());
    }

    /// A list item in a quote has the guides it has outside, on the chars
    /// past the markers; a tab after `>` holds the guide its columns get.
    #[test]
    fn a_quoted_item_has_its_guides_past_the_markers() {
        let q = |line: &str, depth| guides_for(line, &BlockKind::Quote(depth), 2).marks;
        assert_eq!(q(">   - x", 1), vec![(2, 0)]);
        assert_eq!(q("> >     - x", 2), vec![(4, 0), (6, 1)]);
        assert_eq!(q("> - x", 1), vec![]);
        // `>` then a tab at width 4 is the marker's space and 3 columns.
        let t = |line: &str| guides_for(line, &BlockKind::Quote(1), 4).marks;
        assert_eq!(t(">\t- x"), vec![]);
        assert_eq!(t(">\t  - x"), vec![(1, 0)]);
        assert_eq!(t(">\t      - x"), vec![(1, 0), (3, 1)]);
    }

    #[test]
    fn apply_splits_and_colors_guide_columns() {
        let base = Style::fg(crate::render::theme::Color::None);
        let mut spans = vec![StyledSpan { range: 0..7, style: base }];
        let guides = guides_for("    - x", &list(4), 2);
        let theme = Theme::default();
        apply(&mut spans, &guides, &theme, &GlyphConfig::default());
        // Guide columns 0 and 2 become single-char spans in indent colors.
        assert_eq!(spans[0].range, 0..1);
        assert_eq!(spans[0].style.fg, theme.indent_colors[0]);
        assert_eq!(spans[1].range, 1..2);
        assert_eq!(spans[2].range, 2..3);
        assert_eq!(spans[2].style.fg, theme.indent_colors[1]);
        // Coverage is preserved and gap-free.
        assert_eq!(spans.last().unwrap().range.end, 7);
    }
}
