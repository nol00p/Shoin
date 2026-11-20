//! Display math laid out over several rows (#77).
//!
//! STUB. The real layout engine is written on the `math-layout` branch and
//! replaces this whole module when it merges. Until then `layout` keeps the
//! agreed signature and returns the block's source lines, trimmed and padded
//! to one width, so the editor side (`render::cache`, `render::hidden`,
//! `render::frame`) can be built and tested against it.

use unicode_width::UnicodeWidthStr;

/// Lay out the TeX of one display block: the text between its `$$`
/// delimiters, delimiters removed, the block's lines joined with '\n'.
/// `Some(rows)` when it fits in `width` cells: every row has exactly the
/// same display width, the box's width. `None` when it is wider than
/// `width`, empty, or can't be parsed: fall back to today's form.
pub fn layout(tex: &str, width: usize) -> Option<Vec<String>> {
    // STUB: the trimmed non-empty source lines, padded to equal width.
    let lines: Vec<&str> = tex.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let w = lines.iter().map(|l| l.width()).max()?;
    if w > width {
        return None;
    }
    Some(lines.iter().map(|l| format!("{l}{}", " ".repeat(w - l.width()))).collect())
}
