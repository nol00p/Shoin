//! Measure, margins, soft wrap, typewriter scroll.
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
    /// Columns the line-number gutter reserves — digits plus one column of
    /// air before the text, 0 when numbers are off. `compute` does not know
    /// the buffer's line count, so it starts at 0; the caller sets it once
    /// that is known, before drawing the gutter (`render::frame`).
    pub number_gutter: u16,
}

/// Where a visual row's content comes from.
///
/// A seam opened before anything needed it, when the
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
    /// variant that would have been expensive to retrofit, and the reason
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
            number_gutter: 0,
        }
    }
}

/// Word-boundary wrap with a HANGING INDENT: every row after the first is
/// narrowed by `hang` cells, because the renderer will push it right by that
/// much so it lines up under the item's content.
///
/// `hang` is clamped so a continuation row always keeps some width — a deeply
/// indented item in a narrow pane must still wrap rather than loop forever.
///
/// Tabs reach their stops counted from the line's first cell, row after row
/// ([`cell_width_at`]): the hang moves a row on screen, not its stops.
pub fn wrap_hanging(text: &str, measure: u16, hang: u16, tab: usize) -> Vec<(usize, usize)> {
    wrap_with_inset(text, measure, hang, tab, 0, &[])
}

/// Wrap a line whose first row starts `inset` cells inside the measure.
/// `breaks` are char indices a row may also start at, as if a space came
/// before them.
pub(super) fn wrap_with_inset(
    text: &str,
    measure: u16,
    hang: u16,
    tab: usize,
    inset: u16,
    breaks: &[usize],
) -> Vec<(usize, usize)> {
    let hang = hang.min(measure.saturating_sub(MIN_WRAP_WIDTH));
    let chars: Vec<char> = text.chars().collect();
    let first = wrap_chars_at(&chars, 0, measure.saturating_sub(inset), tab, breaks);
    if first.len() < 2 || (hang == 0 && inset == 0) {
        return first;
    }
    // Keep the first break. Wrap the remaining text at the continuation width.
    let (start, end) = first[0];
    let mut out = vec![(start, end)];
    let rest_breaks: Vec<usize> = breaks.iter().filter_map(|&b| b.checked_sub(end)).collect();
    let at = phase(&chars, end, tab);
    for (a, b) in wrap_chars_at(&chars[end..], at, measure - hang, tab, &rest_breaks) {
        out.push((end + a, end + b));
    }
    out
}

/// The narrowest a hanging continuation row may become.
pub(super) const MIN_WRAP_WIDTH: u16 = 8;

/// The display column an item's content starts at — the width of its leading
/// marker run, which is what a wrapped continuation lines up under.
///
/// Read off the DISPLAY text, not the source: a concealed `- [ ] ` is one
/// checkbox cell wide and a revealed one is six, and the continuation has to
/// follow whichever is on screen. `guide` is the indent guide glyph a
/// concealed line draws in its indent (empty for a raw line): it is indent,
/// not the marker.
///
/// `at` is the line cell `text` starts at, for the stops of a tab in it:
/// 0 for a whole line, the bars' width for a quote's item past them.
pub fn content_column(text: &str, at: u16, tab_width: u16, guide: &str) -> u16 {
    let mut width = 0u16;
    let mut chars = text.chars().peekable();
    // Leading whitespace, guides included.
    let mut rest = text;
    loop {
        if !guide.is_empty() && rest.starts_with(guide) {
            width += display_width(guide);
            for _ in guide.chars() {
                chars.next();
            }
            rest = &rest[guide.len()..];
            continue;
        }
        match chars.peek() {
            Some(' ') => width += 1,
            Some('\t') => width += tab_stop((at + width).into(), tab_width.into()) as u16,
            _ => break,
        }
        rest = &rest[1..];
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
    // A tab there is drawn to its stop, so it counts the cells it reaches.
    let mut gap = 0u16;
    loop {
        match chars.peek() {
            Some(' ') => gap += 1,
            Some('\t') => gap += tab_stop((at + width + marker + gap).into(), tab_width.into()) as u16,
            _ => break,
        }
        chars.next();
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
/// true width, and a tab the cells to its next stop, counted from the line's
/// first cell through every row ([`cell_width_at`]). A row may break after a
/// space or a tab; a tab is never split across rows. One that reaches the
/// row's last cell exactly ends it; one that would run past it is wrapped
/// with the word before it, or starts the next row when the row has no
/// break, keeping its width. Always returns at least one range, so an empty
/// line still produces a row.
pub fn wrap_line(text: &str, measure: u16, tab: usize) -> Vec<(usize, usize)> {
    wrap_chars(&text.chars().collect::<Vec<_>>(), measure, tab)
}

/// `wrap_line` for a line whose chars the caller already has, so a long line
/// is not collected twice when its rows are then cut with `char_window`.
pub fn wrap_chars(chars: &[char], measure: u16, tab: usize) -> Vec<(usize, usize)> {
    wrap_chars_at(chars, 0, measure, tab, &[])
}

/// `wrap_chars` that may also start a row at any index in `breaks`. `at` is
/// the line cell `chars` start at (only its tab stop phase matters).
fn wrap_chars_at(chars: &[char], at: usize, measure: u16, tab: usize, breaks: &[usize]) -> Vec<(usize, usize)> {
    if chars.is_empty() {
        return vec![(0, 0)];
    }
    let max = measure.max(1) as usize;

    let mut out = Vec::new();
    let mut start = 0usize;
    // The line cell `start` sits at, for the tab stops.
    let mut origin = at;
    let mut width = 0usize;
    // The latest index after `start` a row may start at: past a space, or
    // one of `breaks`, with the row's width up to it.
    let mut last_break: Option<(usize, usize)> = None;
    let mut i = 0usize;

    while i < chars.len() {
        let w = cell_width_at(chars[i], origin + width, tab);
        if width + w > max && i > start {
            // Break at the last break point inside this row, otherwise
            // hard-break at the current character.
            let (brk, upto) = match last_break {
                Some((b, upto)) if b > start => (b, upto),
                _ => (i, width),
            };
            out.push((start, brk));
            start = brk;
            origin += upto;
            width = 0;
            last_break = None;
            i = brk;
            continue;
        }
        if i > start && breaks.contains(&i) {
            last_break = Some((i, width));
        }
        if i > start && matches!(chars[i], ' ' | '\t') {
            last_break = Some((i + 1, width + w));
        }
        width += w;
        i += 1;
    }
    out.push((start, chars.len()));
    out
}

/// The chars in columns `start..end` of a line, clamped to its length: the
/// text of one row `wrap_line` returned, or of any window into a line.
pub fn char_window(chars: &[char], start: usize, end: usize) -> String {
    let end = end.min(chars.len());
    chars[start.min(end)..end].iter().collect()
}

/// What a char that must not reach the terminal raw is drawn as instead.
///
/// U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR sit inside a line,
/// since lines break on `\n` only. Sent raw, terminals disagree on
/// them: some break the row, some draw nothing, and the screen drifts from
/// the layout. So each is drawn as a dim `↵` — not East Asian Ambiguous, so a
/// CJK terminal draws it one cell wide too — and the reader can see, and put
/// the caret on, a char that is really there. The C0 controls need nothing
/// here: they measure zero cells and ratatui drops them.
pub fn stand_in(c: char) -> Option<char> {
    match c {
        '\u{2028}' | '\u{2029}' => Some('\u{21b5}'),
        _ => None,
    }
}

/// The cells `c` takes on screen: a `stand_in` char is measured as the glyph
/// drawn for it, not as itself. unicode-width gives both one cell today, but
/// every screen measure — wrap, caret, click, conceal — goes through here so
/// the measure follows the draw whatever the two say. Not for the file:
/// `text::table` pads by unicode-width itself.
///
/// A tab here is one at a stop, `tab` cells: how wide it is anywhere else
/// depends on where it starts, which [`cell_width_at`] is given. A `tab` of 0
/// (a config file can say so) is 1, as the classifier reads it.
pub fn cell_width(c: char, tab: usize) -> usize {
    if c == '\t' {
        return tab.max(1);
    }
    match stand_in(c) {
        Some(g) => UnicodeWidthChar::width(g).unwrap_or(1),
        None => UnicodeWidthChar::width(c).unwrap_or(0),
    }
}

/// The cells a tab starting at line cell `at` takes: to the next multiple of
/// `tab`, as Neovim's `tabstop` draws it, so a tab at a stop is `tab` wide.
pub fn tab_stop(at: usize, tab: usize) -> usize {
    let t = tab.max(1);
    t - at % t
}

/// The cells `c` takes when drawn from line cell `at`: [`cell_width`], but a
/// tab reaches its next stop ([`tab_stop`]). Stops count from the line's
/// first drawn cell: the concealed text on a concealed line, the source on
/// the line under the cursor, through every row of a wrapped line.
///
/// Only drawing and the cursor measure this way. The classifier and the list
/// commands count an indent with a tab as `tab` columns wherever it sits
/// (`block::indent_cols`), so a list's nesting does not move with it; a line
/// indented with tabs alone draws the same either way, since each one starts
/// at a stop.
pub fn cell_width_at(c: char, at: usize, tab: usize) -> usize {
    if c == '\t' {
        tab_stop(at, tab)
    } else {
        cell_width(c, tab)
    }
}

/// A line cell count that `chars[..at]` reaches, as far as a tab after it
/// cares: the exact width when no tab comes before `at`, otherwise the width
/// past the last tab, which ends on a stop. So a row in the middle of a long
/// line finds its stops without measuring the whole line before it.
pub fn phase(chars: &[char], at: usize, tab: usize) -> usize {
    let at = at.min(chars.len());
    let from = chars[..at].iter().rposition(|&c| c == '\t').map_or(0, |t| t + 1);
    cells_from(&chars[from..at], 0, tab)
}

/// The cells `chars` take drawn from line cell `at` (a [`phase`] will do).
pub fn cells_from(chars: &[char], at: usize, tab: usize) -> usize {
    chars.iter().fold(0, |w, &c| w + cell_width_at(c, at + w, tab))
}

/// `text` with each tab replaced by the spaces it is drawn as from line cell
/// `at` ([`cell_width_at`]), for drawing: a tab must never reach the terminal
/// buffer, where ratatui drops it (a control char) and every cell after it
/// shifts left. Borrowed unchanged when there is no tab, which is nearly
/// always.
pub fn expand_tabs(text: &str, at: usize, tab: usize) -> std::borrow::Cow<'_, str> {
    if !text.contains('\t') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut w = 0;
    for c in text.chars() {
        let cw = cell_width_at(c, at + w, tab);
        if c == '\t' {
            out.extend(std::iter::repeat_n(' ', cw));
        } else {
            out.push(c);
        }
        w += cw;
    }
    std::borrow::Cow::Owned(out)
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

/// The single funnel for measuring chrome text in display cells: prompts,
/// labels, file names. A tab there measures 0: ratatui drops it.
pub fn display_width(text: &str) -> u16 {
    text.chars()
        .filter(|&c| c != '\t')
        .map(|c| cell_width(c, 0))
        .sum::<usize>() as u16
}

/// Document text in display cells from the start of its line, a tab reaching
/// its stop ([`cell_width_at`]).
pub fn text_width(text: &str, tab: usize) -> u16 {
    text_width_from(text, 0, tab)
}

/// [`text_width`] for text that starts at line cell `at` (a [`phase`] will
/// do): a row of a wrapped line, or the part of one before the cursor.
pub fn text_width_from(text: &str, at: usize, tab: usize) -> u16 {
    text.chars().fold(0usize, |w, c| w + cell_width_at(c, at + w, tab)) as u16
}

/// The char under display cell `cell` of `chars`, counted from the first,
/// which sits at line cell `from` (a [`phase`] will do): every cell of a tab
/// or a wide glyph lands on that char. A cell past the end answers
/// `chars.len()`. A zero-width char takes no cell, so a click never lands on
/// one but on the char it sits on.
pub fn char_at_cell(chars: &[char], cell: usize, from: usize, tab: usize) -> usize {
    let mut at = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        let w = cell_width_at(c, from + at, tab);
        if cell < at + w {
            return i;
        }
        at += w;
    }
    chars.len()
}

/// Scroll offset (in visual rows) such that the cursor stays visible, honoring
/// `scroll_off` and — when typewriter mode is on — pinning the cursor row to
/// `typewriter_anchor` of the text height.
///
/// ORDER MATTERS: this is computed AFTER the wrap
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
    reading: bool,
) -> usize {
    let height = layout.height.max(1) as usize;

    // Read-only mode has no caret, so the cursor is the top of the screen and
    // every motion scrolls the page, as in `less`. `App` keeps the cursor from
    // passing the row where the last screenful starts.
    if reading {
        return cursor_row.min(total_rows.saturating_sub(height));
    }

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

    /// A tab between the marker and the text counts its drawn width: the
    /// cells to its stop, counted from where the text starts on the line.
    #[test]
    fn a_tab_after_the_marker_counts_as_its_width() {
        assert_eq!(content_column("-\tword", 0, 4, ""), 4);
        assert_eq!(content_column("1.\tword", 0, 4, ""), 4);
        assert_eq!(content_column("100.\tword", 0, 4, ""), 8, "a tab at a stop is a full width");
        assert_eq!(content_column("- \tword", 0, 2, ""), 4);
        assert_eq!(content_column("-\tword", 2, 4, ""), 2, "after a quote's bar and space");
        assert_eq!(content_column(" \t- word", 0, 4, ""), 6, "a mixed indent reaches the stop");
    }

    /// `breaks` let a row start where no space precedes, and a row that
    /// starts with a space still does not break right after it.
    #[test]
    fn extra_breaks_are_break_points() {
        assert_eq!(wrap_with_inset(">>abcdefghijklmn", 16, 0, 4, 2, &[1, 2]), [(0, 2), (2, 16)]);
        assert_eq!(wrap_with_inset(">>abcdefghijklmn", 16, 0, 4, 2, &[]), [(0, 14), (14, 16)]);
        assert_eq!(wrap_chars(&" abcdefghij".chars().collect::<Vec<_>>(), 8, 4), [(0, 8), (8, 11)]);
    }

    /// The column a wrapped continuation hangs at, read off the DISPLAY form.
    #[test]
    fn content_column_measures_the_marker_run() {
        assert_eq!(content_column("- item", 0, 4, ""), 2, "bullet plus its space");
        assert_eq!(content_column("• item", 0, 4, ""), 2, "…concealed is the same width");
        assert_eq!(content_column("12. item", 0, 4, ""), 4, "an ordinal is as wide as it reads");
        assert_eq!(content_column("- [ ] task", 0, 4, ""), 6, "the raw checkbox prefix");
        assert_eq!(content_column("☐ task", 0, 4, ""), 2, "…and the concealed one");
        assert_eq!(content_column("  - nested", 0, 4, ""), 4, "indent counts toward it");
        assert_eq!(content_column("\t- tabbed", 0, 4, ""), 6, "a tab is tab_width cells");
        assert_eq!(content_column("- ", 0, 4, ""), 0, "a marker with no content is not a hang");
        // A concealed nested item draws guides in its indent; they are
        // indent, not its marker.
        assert_eq!(content_column("│   ◦ nested", 0, 4, "│"), 6, "a guide is indent");
        assert_eq!(content_column("│ │ • nested", 0, 2, "│"), 6);
        assert_eq!(content_column("│ ◦ nested", 0, 2, ""), 2, "…not when there are none");
    }

    /// Rows after the first are wrapped at the narrower width, since the
    /// renderer will push them right by `hang`.
    #[test]
    fn wrap_hanging_narrows_only_the_continuations() {
        let text = "- aaaa bbbb cccc dddd";
        let flat = wrap_line(text, 12, 4);
        let hung = wrap_hanging(text, 12, 2, 4);
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
        let rows = wrap_hanging("- aaaa bbbb cccc dddd eeee", 10, 9, 4);
        assert!(rows.len() > 1);
        assert!(rows.windows(2).all(|w| w[1].1 > w[0].1), "always makes progress");
    }
}
