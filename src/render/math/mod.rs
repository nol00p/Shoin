//! Terminal-cell layout for display TeX.

mod boxes;
mod parse;
pub(super) mod symbols;

#[cfg(test)]
mod tests;

/// Lay out the TeX of one display block with its delimiters removed and
/// lines joined with newlines. Each returned row is padded to the box width.
/// Empty, malformed, or too-wide input returns `None`.
#[allow(dead_code, reason = "the display editor integration calls this in part two")]
pub fn layout(tex: &str, width: usize) -> Option<Vec<String>> {
    let formula = parse::Parser::new(tex, width).parse()?;
    (formula.width > 0 && formula.rows.iter().any(|row| !row.trim().is_empty()))
        .then_some(formula.rows)
}
