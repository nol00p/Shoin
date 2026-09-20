//! Text alignment WITHIN the writing column — a different axis from
//! `layout.align`, which places the column itself on screen (SPEC.md §6) and
//! is never touched by this. The column stays put; this decides how each row
//! sits inside it.
//!
//! Pure text math, no ratatui: callers turn `justify_gaps`'s output into
//! styled spans, and `align_offset`/`justify_offset` into screen columns.

use crate::render::layout::display_width;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justified,
}

impl TextAlign {
    pub fn next(self) -> Self {
        match self {
            TextAlign::Left => TextAlign::Center,
            TextAlign::Center => TextAlign::Right,
            TextAlign::Right => TextAlign::Justified,
            TextAlign::Justified => TextAlign::Left,
        }
    }

    /// `None` for a word this does not know, so a typo in `:align` or
    /// `layout.text_align` is reported rather than silently read as `left`.
    pub fn parse(s: &str) -> Option<TextAlign> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "left" | "l" => TextAlign::Left,
            "center" | "centre" | "c" => TextAlign::Center,
            "right" | "r" => TextAlign::Right,
            "justified" | "justify" | "j" => TextAlign::Justified,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
            TextAlign::Justified => "justified",
        }
    }
}

/// Extra columns needed at each interior word-gap so a wrapped row's visible
/// content exactly fills `measure` — the stretch `Justified` applies.
///
/// Returns one entry per gap, in order, as `(row-local char index of the
/// gap's leading space, extra spaces to insert immediately after it)`. Empty
/// when the row already fills `measure`, or has no interior space to stretch
/// (a single word too long to wrap) — either way the row draws exactly as
/// `Left` would draw it, which is the only sane fallback for an unstretchable
/// line.
///
/// Trailing whitespace is ignored on both ends of the measurement: the word
/// wrapper (`layout::wrap_line`) leaves a row's own trailing space attached to
/// it, and stretching space that is already invisible would waste a column.
pub fn justify_gaps(text: &str, measure: u16) -> Vec<(usize, u16)> {
    let trimmed = text.trim_end();
    let chars: Vec<char> = trimmed.chars().collect();
    let content_width = display_width(trimmed);
    if content_width >= measure {
        return Vec::new();
    }
    let shortfall = measure - content_width;

    // A gap is the leading space of a run of one or more spaces between two
    // non-space characters — never at position 0, which would be leading
    // whitespace, not a word boundary.
    let mut gaps = Vec::new();
    let mut i = 1;
    while i < chars.len() {
        if chars[i] == ' ' {
            gaps.push(i);
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    if gaps.is_empty() {
        return Vec::new();
    }

    let base = shortfall / gaps.len() as u16;
    let extra = shortfall % gaps.len() as u16;
    gaps.into_iter()
        .enumerate()
        .map(|(idx, pos)| (pos, base + u16::from((idx as u16) < extra)))
        .collect()
}

/// Where a row's content starts, in columns from the left of an
/// `area_width`-wide area — mirrors ratatui's own `Paragraph`/`Line` offset
/// math (`get_line_offset`), so the cursor and mouse land where the text was
/// actually drawn. `Justified` and `Left` both start flush left.
pub fn align_offset(align: TextAlign, content_width: u16, area_width: u16) -> u16 {
    match align {
        TextAlign::Center => (area_width / 2).saturating_sub(content_width / 2),
        TextAlign::Right => area_width.saturating_sub(content_width),
        TextAlign::Left | TextAlign::Justified => 0,
    }
}

/// Screen columns consumed by justification stretch before row-local char
/// index `before` — added to a cursor's x so it still sits right after the
/// word it follows once the gaps ahead of it have been pulled apart.
pub fn justify_offset(gaps: &[(usize, u16)], before: usize) -> u16 {
    gaps.iter()
        .filter(|&&(pos, _)| pos < before)
        .map(|&(_, extra)| extra)
        .sum()
}

/// The inverse of the stretch rendering applied: map a screen column inside
/// an already-stretched row back to its row-local column, so a click still
/// lands on the character under the pointer rather than drifting into the
/// padding a later gap opened up.
pub fn unstretch_col(gaps: &[(usize, u16)], col: u16) -> u16 {
    let mut consumed = 0u16;
    for &(pos, extra) in gaps {
        let insertion_at = pos as u16 + 1 + consumed;
        if col < insertion_at {
            return col - consumed;
        }
        if col < insertion_at + extra {
            // Clicked inside the inserted padding itself: snap to the space
            // that grew it, same as clicking just past the word before it.
            return pos as u16 + 1;
        }
        consumed += extra;
    }
    col.saturating_sub(consumed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_the_documented_words_and_rejects_the_rest() {
        assert_eq!(TextAlign::parse("left"), Some(TextAlign::Left));
        assert_eq!(TextAlign::parse("CENTER"), Some(TextAlign::Center));
        assert_eq!(TextAlign::parse(" centre "), Some(TextAlign::Center));
        assert_eq!(TextAlign::parse("right"), Some(TextAlign::Right));
        assert_eq!(TextAlign::parse("justified"), Some(TextAlign::Justified));
        assert_eq!(TextAlign::parse("justify"), Some(TextAlign::Justified));
        assert_eq!(TextAlign::parse("banana"), None);
    }

    #[test]
    fn next_cycles_through_all_four_and_back() {
        assert_eq!(TextAlign::Left.next(), TextAlign::Center);
        assert_eq!(TextAlign::Center.next(), TextAlign::Right);
        assert_eq!(TextAlign::Right.next(), TextAlign::Justified);
        assert_eq!(TextAlign::Justified.next(), TextAlign::Left);
    }

    #[test]
    fn justify_gaps_distributes_the_shortfall_across_interior_spaces() {
        // "aaaa bbbb cccc" is 14 columns wide, three words, two gaps.
        let gaps = justify_gaps("aaaa bbbb cccc", 20);
        assert_eq!(gaps.len(), 2, "two interior gaps: {gaps:?}");
        let total: u16 = gaps.iter().map(|&(_, n)| n).sum();
        assert_eq!(total, 6, "20 - 14 columns short");
    }

    #[test]
    fn justify_gaps_ignores_a_trailing_space_the_wrapper_left_attached() {
        let gaps = justify_gaps("aaaa bbbb ", 12);
        // Trimmed content is "aaaa bbbb" (9 cols), one interior gap, 3 short.
        assert_eq!(gaps, vec![(4, 3)]);
    }

    #[test]
    fn justify_gaps_is_empty_when_theres_nothing_to_stretch() {
        assert!(justify_gaps("aaaa", 12).is_empty(), "single word, no gap");
        assert!(justify_gaps("aaaa bbbb", 9).is_empty(), "already full");
    }

    #[test]
    fn align_offset_matches_ratatuis_own_center_and_right_math() {
        assert_eq!(align_offset(TextAlign::Left, 10, 20), 0);
        assert_eq!(align_offset(TextAlign::Justified, 10, 20), 0);
        assert_eq!(align_offset(TextAlign::Center, 10, 20), 5);
        assert_eq!(align_offset(TextAlign::Right, 10, 20), 10);
    }

    #[test]
    fn justify_offset_counts_only_gaps_fully_before_the_cursor() {
        let gaps = vec![(4, 2), (9, 1)];
        assert_eq!(justify_offset(&gaps, 0), 0, "before any gap");
        assert_eq!(justify_offset(&gaps, 4), 0, "sitting on the gap's own space");
        assert_eq!(justify_offset(&gaps, 5), 2, "just past the first gap");
        assert_eq!(justify_offset(&gaps, 10), 3, "past both");
    }

    #[test]
    fn unstretch_col_inverts_the_stretch() {
        let gaps = vec![(4, 2), (9, 1)];
        // Rendered row: "aaaa   bbbb  cccc" — 2 extra after col 4, 1 after col 9+2=11.
        assert_eq!(unstretch_col(&gaps, 0), 0);
        assert_eq!(unstretch_col(&gaps, 4), 4, "right at the word boundary");
        assert_eq!(unstretch_col(&gaps, 6), 5, "inside the first stretch snaps forward");
        assert_eq!(unstretch_col(&gaps, 7), 5, "past the first stretch, one column back");
        assert_eq!(unstretch_col(&gaps, 13), 10, "past both stretches, three columns back");
    }
}
