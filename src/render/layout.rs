//! Measure, margins, soft wrap, typewriter scroll. SPEC.md §6.
//!
//! This is the only module permitted to introduce display-only indentation
//! (hanging indent on wrapped list lines), and it does so inside the wrap map
//! where the source↔screen mapping already exists.

use unicode_width::UnicodeWidthChar;

use crate::config::schema::LayoutConfig;

/// Most blank rows line spacing will put between two lines, wherever the value
/// comes from. Spacing is air, and past a couple of rows it stops reading as
/// air and starts reading as a document with holes in it — and every row of it
/// is a row of text the screen no longer shows.
pub const MAX_LINE_SPACING: u16 = 4;

pub struct Layout {
    /// Actual text width after clamping `measure` to the terminal.
    pub measure: u16,
    /// Left margin in columns.
    pub margin_left: u16,
    /// First screen row used by text.
    pub top: u16,
    /// Number of screen rows available to text.
    pub height: u16,
}

/// Where a visual row's content comes from.
///
/// SPEC.md §14.5 — the seam that was opened before anything needed it, when the
/// mapping to rope lines was still the identity. It PAID: `Embedded` (a row of
/// transcluded text, which has no line in this rope) and `Spacer` (line
/// spacing, which has no text at all) were each one variant and its arithmetic,
/// and every consumer of `VisualRow::line()` kept working untouched.
///
/// DISCIPLINE: no code outside this module indexes the rope directly for
/// rendering. That single rule is what made both of them additive changes
/// rather than rewrites, and it is why a third would be too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowSource {
    /// A line of the buffer's own rope.
    Buffer(usize),
    /// A row of content transcluded into `line` by an `![[…]]` there — text
    /// that exists in another file and has NO line in this rope. This is the
    /// variant §14.5 said would be expensive to retrofit, and the reason
    /// nothing outside this module indexes the rope for rendering.
    Embedded { line: usize, index: usize },
    /// A blank row drawn AFTER `line` for `layout.line_spacing` — line height,
    /// on a grid that has no such thing. It holds no text at all, which makes
    /// it the cheapest of the three: it exists only so the row index counts it.
    Spacer(usize),
}

/// One visual row: a slice of one source line.
#[derive(Clone, Copy, Debug)]
pub struct VisualRow {
    pub source: RowSource,
    /// Char range within the source line.
    pub start_col: usize,
    pub end_col: usize,
    /// Display-only leading indent for wrapped list continuations.
    pub hanging: u16,
    /// False for a row of transcluded content — there is nothing in the rope
    /// behind it to change.
    pub editable: bool,
}

impl VisualRow {
    /// The rope line this row belongs to. An embedded row answers with the line
    /// holding its `![[…]]`, which is what every caller wants: it is where the
    /// cursor goes, what a click selects, and the unit motions step over.
    pub fn line(&self) -> usize {
        match self.source {
            RowSource::Buffer(l) => l,
            RowSource::Embedded { line, .. } => line,
            RowSource::Spacer(line) => line,
        }
    }

    /// Which row of an expansion this is, or `None` for ordinary buffer text.
    pub fn embedded(&self) -> Option<usize> {
        match self.source {
            RowSource::Embedded { index, .. } => Some(index),
            RowSource::Buffer(_) | RowSource::Spacer(_) => None,
        }
    }

    /// Whether this row is line-spacing air rather than content.
    pub fn is_spacer(&self) -> bool {
        matches!(self.source, RowSource::Spacer(_))
    }
}

impl Layout {
    /// Clamp `measure` to the terminal, keeping a minimum 2-column gutter each
    /// side. `reserved_rows` is the status line's allowance.
    pub fn compute(cfg: &LayoutConfig, term_cols: u16, term_rows: u16, reserved_rows: u16) -> Self {
        let available = term_cols.saturating_sub(4).max(8);
        let measure = cfg.measure.clamp(8, available);

        let margin_left = if cfg.align.trim() == "left" {
            2
        } else {
            term_cols.saturating_sub(measure) / 2
        };

        let pad_top = cfg.padding_top.min(term_rows / 4);
        let pad_bottom = cfg.padding_bottom.min(term_rows / 4);
        let height = term_rows
            .saturating_sub(pad_top)
            .saturating_sub(pad_bottom)
            .saturating_sub(reserved_rows)
            .max(1);

        Layout {
            measure,
            margin_left,
            top: pad_top,
            height,
        }
    }
}

/// Word-boundary wrap with a HANGING INDENT: every row after the first is
/// narrowed by `hang` cells, because the renderer will push it right by that
/// much so it lines up under the item's content (SPEC.md §6).
///
/// `hang` is clamped so a continuation row always keeps some width — a deeply
/// indented item in a narrow pane must still wrap rather than loop forever.
pub fn wrap_hanging(text: &str, measure: u16, hang: u16) -> Vec<(usize, usize)> {
    if hang == 0 {
        return wrap_line(text, measure);
    }
    let hang = hang.min(measure.saturating_sub(MIN_WRAP_WIDTH));
    let first = wrap_line(text, measure);
    if first.len() < 2 || hang == 0 {
        return first;
    }
    // The first row is whatever the full measure took; the rest are re-wrapped
    // at the narrower width, so the break points shift as they must.
    let (start, end) = first[0];
    let mut out = vec![(start, end)];
    let chars: Vec<char> = text.chars().collect();
    let rest: String = chars[end..].iter().collect();
    for (a, b) in wrap_line(&rest, measure - hang) {
        out.push((end + a, end + b));
    }
    out
}

/// The narrowest a hanging continuation row may become.
const MIN_WRAP_WIDTH: u16 = 8;

/// The display column an item's content starts at — the width of its leading
/// marker run, which is what a wrapped continuation lines up under.
///
/// Read off the DISPLAY text, not the source: a concealed `- [ ] ` is one
/// checkbox cell wide and a revealed one is six, and the continuation has to
/// follow whichever is on screen.
pub fn content_column(text: &str, tab_width: u16) -> u16 {
    let mut width = 0u16;
    let mut chars = text.chars().peekable();
    // Leading whitespace.
    while let Some(c) = chars.peek() {
        match c {
            ' ' => width += 1,
            '\t' => width += tab_width.max(1),
            _ => break,
        }
        chars.next();
    }
    // The marker itself: an ordinal runs to its `.`/`)`, anything else is one
    // cell (a bullet, a checkbox, a `-`).
    let mut marker = 0u16;
    if chars.peek().is_some_and(|c| c.is_ascii_digit()) {
        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            chars.next();
            marker += 1;
        }
        if chars.peek().is_some_and(|c| matches!(c, '.' | ')')) {
            chars.next();
            marker += 1;
        }
    } else if let Some(c) = chars.next() {
        marker += UnicodeWidthChar::width(c).unwrap_or(1) as u16;
    }
    // A task's `[ ]` box, when the markers are showing raw.
    let rest: String = chars.clone().collect();
    let boxed = rest.strip_prefix(' ').filter(|r| r.starts_with("[")).is_some()
        && rest.chars().nth(3) == Some(']');
    if boxed {
        for _ in 0..4 {
            chars.next();
        }
        marker += 4;
    }
    // And the space(s) between marker and content.
    let mut gap = 0u16;
    while chars.peek() == Some(&' ') {
        chars.next();
        gap += 1;
    }
    // A marker with no content after it is not a hanging indent, it is a line.
    if chars.peek().is_none() {
        return 0;
    }
    width + marker + gap
}

/// Word-boundary wrap of one line, returning char ranges.
///
/// Measured in display cells via `unicode-width`, so CJK and emoji occupy their
/// true width. Always returns at least one range, so an empty line still
/// produces a row.
pub fn wrap_line(text: &str, measure: u16) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![(0, 0)];
    }
    let max = measure.max(1) as usize;

    let mut out = Vec::new();
    let mut start = 0usize;
    let mut width = 0usize;
    let mut last_space: Option<usize> = None;
    let mut i = 0usize;

    while i < chars.len() {
        let w = UnicodeWidthChar::width(chars[i]).unwrap_or(0);
        if width + w > max && i > start {
            // Break at the last space if there was one inside this row,
            // otherwise hard-break at the current character.
            let brk = match last_space {
                Some(s) if s > start => s + 1,
                _ => i,
            };
            out.push((start, brk));
            start = brk;
            width = 0;
            last_space = None;
            i = brk;
            continue;
        }
        if chars[i] == ' ' {
            last_space = Some(i);
        }
        width += w;
        i += 1;
    }
    out.push((start, chars.len()));
    out
}

/// A line that never wraps: one range, covering the whole line.
///
/// A table row's columns are aligned by `|`s the author placed in the source;
/// word-wrapping it the way a paragraph wraps would break that alignment onto
/// several ragged rows, which is worse than the row running past the measure
/// and getting clipped at the pane edge like any other unwrapped overflow.
pub fn no_wrap(text: &str) -> Vec<(usize, usize)> {
    vec![(0, text.chars().count())]
}

/// The single funnel for measuring a line in display cells.
pub fn display_width(text: &str) -> u16 {
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum::<usize>() as u16
}

/// Scroll offset (in visual rows) such that the cursor stays visible, honoring
/// `scroll_off` and — when typewriter mode is on — pinning the cursor row to
/// `typewriter_anchor` of the text height.
///
/// ORDER MATTERS (SPEC.md §6, "Reveal shift"): this is computed AFTER the wrap
/// map is built with the active line already revealed. Deriving scroll from
/// post-reveal geometry is what stops the cursor being pushed off-screen by its
/// own line growing when the markers reappear.
/// `scroll_off` comes from `[editor]`, not `[layout]` — it is an editing
/// preference (how much context to keep around the cursor), not a page-shape
/// one.
pub fn scroll_offset(
    cursor_row: usize,
    total_rows: usize,
    layout: &Layout,
    cfg: &LayoutConfig,
    scroll_off: u16,
    previous: usize,
) -> usize {
    let height = layout.height.max(1) as usize;

    if cfg.typewriter {
        let anchor = (height as f32 * cfg.typewriter_anchor).round() as usize;
        return cursor_row.saturating_sub(anchor.min(height.saturating_sub(1)));
    }

    let pad = (scroll_off as usize).min(height.saturating_sub(1) / 2);
    let mut top = previous.min(total_rows.saturating_sub(1));

    if cursor_row < top + pad {
        top = cursor_row.saturating_sub(pad);
    }
    let bottom = top + height;
    if cursor_row + pad >= bottom {
        top = cursor_row + pad + 1 - height;
    }
    top.min(total_rows.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The column a wrapped continuation hangs at, read off the DISPLAY form.
    #[test]
    fn content_column_measures_the_marker_run() {
        assert_eq!(content_column("- item", 4), 2, "bullet plus its space");
        assert_eq!(content_column("• item", 4), 2, "…concealed is the same width");
        assert_eq!(content_column("12. item", 4), 4, "an ordinal is as wide as it reads");
        assert_eq!(content_column("- [ ] task", 4), 6, "the raw checkbox prefix");
        assert_eq!(content_column("☐ task", 4), 2, "…and the concealed one");
        assert_eq!(content_column("  - nested", 4), 4, "indent counts toward it");
        assert_eq!(content_column("\t- tabbed", 4), 6, "a tab is tab_width cells");
        assert_eq!(content_column("- ", 4), 0, "a marker with no content is not a hang");
    }

    /// Rows after the first are wrapped at the narrower width, since the
    /// renderer will push them right by `hang`.
    #[test]
    fn wrap_hanging_narrows_only_the_continuations() {
        let text = "- aaaa bbbb cccc dddd";
        let flat = wrap_line(text, 12);
        let hung = wrap_hanging(text, 12, 2);
        assert_eq!(hung[0], flat[0], "the first row keeps the full measure");
        for (a, b) in &hung[1..] {
            let row: String = text.chars().skip(*a).take(b - a).collect();
            assert!(row.trim_end().chars().count() <= 10, "narrowed: {row:?}");
        }
        let joined: String = hung
            .iter()
            .map(|(a, b)| text.chars().skip(*a).take(b - a).collect::<String>())
            .collect();
        assert_eq!(joined, text, "wrapping loses nothing");
    }

    /// A hang wide enough to squeeze the row away is clamped, or a narrow pane
    /// would wrap forever.
    #[test]
    fn wrap_hanging_keeps_a_usable_width() {
        let rows = wrap_hanging("- aaaa bbbb cccc dddd eeee", 10, 9);
        assert!(rows.len() > 1);
        assert!(rows.windows(2).all(|w| w[1].1 > w[0].1), "always makes progress");
    }
}
