//! Aligning a GFM table in the source (#50): every cell padded to its
//! column's width, so the `|`s line up in the file itself and the table reads
//! as a grid in every editor, not only this one.
//!
//! Like `list`, this reads the block cache's line kinds to find the table
//! rather than re-parse it: `BlockKind::Table` already knows where the header,
//! the delimiter row and the last body row are.
//!
//! Cells split on every unescaped `|`, the GFM rule `has_cell_separator`
//! follows too — a `|` inside a code span still splits unless written `\|`.
//! Widths are display cells, so CJK text and emoji pad correctly.

use std::ops::Range;

use crate::render::layout::display_width;
use crate::render::markdown::block::BlockKind;

/// `[editor] table_align`: whether leaving Insert after an edit in a table
/// aligns it (`Auto`), or only `ga` does (`Manual`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableAlign {
    Auto,
    Manual,
}

impl TableAlign {
    /// `None` for a spelling this does not know, so the config check can
    /// name it rather than pick one silently.
    pub fn parse(s: &str) -> Option<TableAlign> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "on" | "true" => TableAlign::Auto,
            "manual" | "off" | "false" => TableAlign::Manual,
            _ => return None,
        })
    }
}

/// The table containing `line`: its first and last lines, END INCLUSIVE.
pub fn bounds(kinds: &[BlockKind], line: usize) -> Option<(usize, usize)> {
    if kinds.get(line) != Some(&BlockKind::Table) {
        return None;
    }
    let mut s = line;
    while s > 0 && kinds[s - 1] == BlockKind::Table {
        s -= 1;
    }
    let mut e = line;
    while e + 1 < kinds.len() && kinds[e + 1] == BlockKind::Table {
        e += 1;
    }
    Some((s, e))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Align {
    None,
    Left,
    Center,
    Right,
}

/// A table's rows, aligned. `rows` are its lines as they are now; the result
/// has one line per row, in the same order. The first row's indent is kept
/// for every row, and every row gets its outer `|`s.
pub fn align(rows: &[String]) -> Vec<String> {
    let indent: String = rows
        .first()
        .map(|r| r.chars().take_while(|c| *c == ' ' || *c == '\t').collect())
        .unwrap_or_default();
    let cells: Vec<Vec<String>> = rows.iter().map(|r| split(r)).collect();
    let delim = rows.iter().position(|r| is_delimiter(r));
    let cols = cells.iter().map(Vec::len).max().unwrap_or(0);

    let aligns: Vec<Align> = (0..cols)
        .map(|c| match delim.and_then(|d| cells[d].get(c)) {
            Some(cell) => match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => Align::Center,
                (true, false) => Align::Left,
                (false, true) => Align::Right,
                (false, false) => Align::None,
            },
            None => Align::None,
        })
        .collect();
    // Three is the narrowest delimiter cell that still reads as one: `---`,
    // or `:-:` for a centered column.
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            cells
                .iter()
                .enumerate()
                .filter(|(i, _)| Some(*i) != delim)
                .filter_map(|(_, row)| row.get(c))
                .map(|cell| display_width(cell) as usize)
                .max()
                .unwrap_or(0)
                .max(3)
        })
        .collect();

    cells
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut out = indent.clone();
            out.push('|');
            for c in 0..cols {
                let w = widths[c];
                if Some(i) == delim {
                    // The dashes span the padding too: `|-----|` under `| abc |`.
                    let (l, r) = match aligns[c] {
                        Align::None => ('-', '-'),
                        Align::Left => (':', '-'),
                        Align::Center => (':', ':'),
                        Align::Right => ('-', ':'),
                    };
                    out.push(l);
                    out.push_str(&"-".repeat(w));
                    out.push(r);
                } else {
                    let cell = row.get(c).map(String::as_str).unwrap_or("");
                    let gap = w - display_width(cell) as usize;
                    let (before, after) = match aligns[c] {
                        Align::Right => (gap, 0),
                        Align::Center => (gap / 2, gap - gap / 2),
                        _ => (0, gap),
                    };
                    out.push(' ');
                    out.push_str(&" ".repeat(before));
                    out.push_str(cell);
                    out.push_str(&" ".repeat(after));
                    out.push(' ');
                }
                out.push('|');
            }
            out
        })
        .collect()
}

/// Where the cursor at `col` (chars) on `old` belongs on `new`, the same row
/// aligned: on the same character of the same cell, or on the same `|`. In
/// a cell's padding it goes to the nearest end of the text.
pub fn cursor_col(old: &str, new: &str, col: usize) -> usize {
    let (o, n) = (spans(old), spans(new));
    let Some(last) = o.len().checked_sub(1) else {
        return col;
    };
    let i = o.iter().position(|s| col <= s.raw.end).unwrap_or(last);
    if col == o[i].raw.end {
        return n.get(i).map_or(col, |s| s.raw.end);
    }
    let offset = col.saturating_sub(o[i].text.start).min(o[i].text.len());
    match n.get(i) {
        Some(s) => s.text.start + offset,
        None => col,
    }
}

/// One cell of a row, in chars: `raw` runs between its `|`s, `text` is the
/// part of it left after trimming.
struct Span {
    raw: Range<usize>,
    text: Range<usize>,
}

/// A row's cells, split the way `split` splits them.
fn spans(row: &str) -> Vec<Span> {
    let chars: Vec<char> = row.chars().collect();
    let mut bars = Vec::new();
    let mut escaped = false;
    for (i, c) in chars.iter().enumerate() {
        match c {
            '\\' => escaped = !escaped,
            '|' if !escaped => bars.push(i),
            _ => escaped = false,
        }
    }
    let ws = |c: &char| *c == ' ' || *c == '\t';
    let first = chars.iter().position(|c| !ws(c));
    let last = chars.iter().rposition(|c| !ws(c));
    let mut start = 0;
    let mut end = chars.len();
    if let (Some(&b), Some(f)) = (bars.first(), first) {
        if b == f {
            start = b + 1;
            bars.remove(0);
        }
    }
    if let (Some(&b), Some(l)) = (bars.last(), last) {
        if b == l {
            end = b;
            bars.pop();
        }
    }
    let mut out = Vec::new();
    let mut from = start;
    for to in bars.into_iter().chain(std::iter::once(end)) {
        let mut a = from;
        let mut z = to;
        while a < z && ws(&chars[a]) {
            a += 1;
        }
        while z > a && ws(&chars[z - 1]) {
            z -= 1;
        }
        out.push(Span { raw: from..to, text: a..z });
        from = to + 1;
    }
    out
}

/// A row's cells, trimmed. The outer `|`s are optional in GFM, so a leading
/// or trailing one bounds the row rather than starting an empty cell.
fn split(row: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    for c in row.trim().chars() {
        match c {
            '\\' => {
                escaped = !escaped;
                cur.push(c);
            }
            '|' if !escaped => cells.push(std::mem::take(&mut cur)),
            _ => {
                escaped = false;
                cur.push(c);
            }
        }
    }
    cells.push(cur);
    let t = row.trim();
    if t.starts_with('|') {
        cells.remove(0);
    }
    if t.len() > 1 && t.ends_with('|') && !t.ends_with("\\|") {
        cells.pop();
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

/// The `|---|:--:|` row. The same test the block classifier makes.
fn is_delimiter(row: &str) -> bool {
    let mut dashes = false;
    let mut bars = false;
    for c in row.trim().chars() {
        match c {
            '-' => dashes = true,
            '|' => bars = true,
            ':' | ' ' | '\t' => {}
            _ => return false,
        }
    }
    dashes && bars
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aligned(text: &str) -> String {
        let rows: Vec<String> = text.lines().map(str::to_string).collect();
        align(&rows).join("\n")
    }

    #[test]
    fn pads_every_cell_to_its_column() {
        let messy = "| Name | Role |\n|---|---|\n| Alice | Maintainer of the parser |\n| Bob | Docs |";
        assert_eq!(
            aligned(messy),
            "| Name  | Role                     |\n\
             |-------|--------------------------|\n\
             | Alice | Maintainer of the parser |\n\
             | Bob   | Docs                     |"
        );
    }

    #[test]
    fn already_aligned_is_unchanged() {
        let table = "| a   | b   |\n|-----|-----|\n| 1   | 2   |";
        assert_eq!(aligned(table), table);
    }

    #[test]
    fn keeps_alignment_markers_and_pads_by_them() {
        let t = "| l | c | r |\n|:--|:-:|--:|\n| left | mid | 1 |";
        assert_eq!(
            aligned(t),
            "| l    |  c  |   r |\n\
             |:-----|:---:|----:|\n\
             | left | mid |   1 |"
        );
    }

    #[test]
    fn adds_outer_bars_and_fills_short_rows() {
        let t = "a | b\n--|--\n1";
        assert_eq!(aligned(t), "| a   | b   |\n|-----|-----|\n| 1   |     |");
    }

    #[test]
    fn wide_characters_pad_by_display_width() {
        let t = "| 名前 | x |\n|---|---|\n| ab | y |";
        assert_eq!(aligned(t), "| 名前 | x   |\n|------|-----|\n| ab   | y   |");
    }

    #[test]
    fn escaped_bar_stays_in_its_cell() {
        let t = "| a \\| b | c |\n|---|---|";
        assert_eq!(aligned(t), "| a \\| b | c   |\n|--------|-----|");
    }

    #[test]
    fn keeps_the_first_rows_indent() {
        let t = "  | a | b |\n|---|---|";
        assert_eq!(aligned(t), "  | a   | b   |\n  |-----|-----|");
    }

    #[test]
    fn cursor_keeps_its_character() {
        let old = "| Alice | Maintainer |";
        let new = "| Alice | Maintainer      |";
        assert_eq!(cursor_col(old, new, 12), 12, "the n of Maintainer");
        let old = "|a|bcd|";
        let new = "| a   | bcd |";
        assert_eq!(cursor_col(old, new, 4), 9, "the c of bcd");
        assert_eq!(cursor_col(old, new, 6), 12, "on the closing bar");
        assert_eq!(cursor_col("a | b", "| a   | b   |", 4), 8, "no leading bar");
    }

    #[test]
    fn bounds_cover_the_whole_table() {
        use BlockKind::*;
        let kinds = [Paragraph, Table, Table, Table, Blank];
        assert_eq!(bounds(&kinds, 2), Some((1, 3)));
        assert_eq!(bounds(&kinds, 0), None);
    }
}
