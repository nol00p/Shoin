//! Keeping a renumbered item's lines inside it, as other Markdown tools read
//! it.
//!
//! A number that changes width (`9.` → `10.`) moves its item's text column,
//! and CommonMark (GitHub, pandoc) reads everything in an item relative to
//! that column: a sub-item, fence or table left at the old column falls out
//! of the item, and indented code gains or loses a space. Moving every line
//! of the item that reaches the old column by the same amount leaves the
//! item's content, as CommonMark strips it, exactly as it was.
//!
//! Rather than predict when that holds, which tabs, lazy lines and blocks
//! inside items make hard, each candidate is parsed and kept only if the
//! document reads the same as before the renumber. First the renumber alone,
//! so a file whose indents already fit is left as written; then the renumber
//! with the shift; and when neither reads the same, the renumber alone.

use std::collections::{BTreeMap, HashMap};

use pulldown_cmark::{Event, Options, Parser, Tag};

use super::list::Renumber;

/// Whether `r` changes its number's width, the only case with anything to
/// realign.
pub fn changes_width(r: &Renumber) -> bool {
    delta(r) != 0
}

/// The lines to shift alongside `edits`, by columns (negative: remove that
/// many leading spaces), so the document reads the same to CommonMark as
/// `lines` did. `lines` is the text before the edits, each line with its
/// line ending. Empty when no number changes width, when the renumber alone
/// already reads the same, or when shifting would not make it read the same.
pub fn shifts(lines: &[String], edits: &[Renumber]) -> Vec<(usize, isize)> {
    if !edits.iter().any(changes_width) {
        return Vec::new();
    }
    let text = lines.concat();
    let before = shape(&text);
    if apply(lines, edits, &[]).is_some_and(|t| shape(&t) == before) {
        return Vec::new();
    }
    let shifts = item_shifts(lines, &text, edits);
    if shifts.is_empty() {
        return Vec::new();
    }
    match apply(lines, edits, &shifts) {
        Some(t) if shape(&t) == before => shifts,
        _ => Vec::new(),
    }
}

/// How many columns `r` widens its number by (negative: narrows).
fn delta(r: &Renumber) -> isize {
    r.number.chars().count() as isize - r.cols.len() as isize
}

/// The parse that has to stay the same: the events, with the numbers lists
/// start at left out, since renumbering is meant to change those.
fn shape(text: &str) -> Vec<Event<'_>> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH;
    Parser::new_ext(text, options)
        .map(|e| match e {
            Event::Start(Tag::List(Some(_))) => Event::Start(Tag::List(Some(1))),
            e => e,
        })
        .collect()
}

/// Every widened or narrowed item's lines that reach its old text column,
/// with how far each moves; an item inside another that also changes width
/// moves by both. The item's extent is CommonMark's, from the parser, so
/// lazy lines and blank-separated paragraphs count as they do there. An item
/// whose text column a tab decides is skipped.
fn item_shifts(lines: &[String], text: &str, edits: &[Renumber]) -> Vec<(usize, isize)> {
    let mut starts = Vec::with_capacity(lines.len());
    let mut at = 0;
    for l in lines {
        starts.push(at);
        at += l.len();
    }
    let line_of = |byte: usize| starts.partition_point(|&s| s <= byte).saturating_sub(1);
    // Each item's range, by the byte its marker starts at.
    let items: HashMap<usize, usize> = Parser::new(text)
        .into_offset_iter()
        .filter_map(|(e, r)| matches!(e, Event::Start(Tag::Item)).then_some((r.start, r.end)))
        .collect();

    let mut by: BTreeMap<usize, isize> = BTreeMap::new();
    for r in edits.iter().filter(|r| changes_width(r)) {
        let Some(line) = lines.get(r.line) else { continue };
        let Some(col) = text_column(line, r) else { continue };
        // `text_column` checked the indent is all spaces, so chars are bytes.
        let Some(&end) = items.get(&(starts[r.line] + r.cols.start)) else { continue };
        let last = line_of(end.saturating_sub(1));
        let reach = " ".repeat(col);
        let inside = lines.iter().enumerate().take(last + 1).skip(r.line + 1);
        for (k, l) in inside {
            if !l.trim().is_empty() && l.starts_with(&reach) {
                *by.entry(k).or_default() += delta(r);
            }
        }
    }
    by.into_iter().filter(|&(_, d)| d != 0).collect()
}

/// The column `r`'s item text starts at, as CommonMark counts it: past the
/// indent, the number and its delimiter, and the spaces after it, or just
/// one of them when the item is empty or five or more follow (the rest is
/// then indented code). `None` unless the indent and the gap are spaces.
fn text_column(line: &str, r: &Renumber) -> Option<usize> {
    let chars: Vec<char> = line.trim_end_matches(['\n', '\r']).chars().collect();
    if !chars.get(..r.cols.start)?.iter().all(|&c| c == ' ') {
        return None;
    }
    if !matches!(chars.get(r.cols.end), Some('.' | ')')) {
        return None;
    }
    let after = r.cols.end + 1;
    let gap = chars[after..].iter().take_while(|&&c| c == ' ').count();
    match chars.get(after + gap) {
        None => Some(after + 1),
        Some('\t') => None,
        Some(_) if gap == 0 => None,
        Some(_) => Some(after + if gap >= 5 { 1 } else { gap }),
    }
}

/// `lines` with `edits` and `shifts` made, joined; `None` if a line has
/// fewer leading spaces than a negative shift takes away.
fn apply(lines: &[String], edits: &[Renumber], shifts: &[(usize, isize)]) -> Option<String> {
    let edits: HashMap<usize, &Renumber> = edits.iter().map(|r| (r.line, r)).collect();
    let shifts: HashMap<usize, isize> = shifts.iter().copied().collect();
    let mut out = String::with_capacity(lines.iter().map(String::len).sum::<usize>() + 16);
    for (k, line) in lines.iter().enumerate() {
        let mut line = std::borrow::Cow::Borrowed(line.as_str());
        if let Some(r) = edits.get(&k) {
            let byte = |c: usize| line.char_indices().nth(c).map_or(line.len(), |(b, _)| b);
            let (from, to) = (byte(r.cols.start), byte(r.cols.end));
            line = format!("{}{}{}", &line[..from], r.number, &line[to..]).into();
        }
        match shifts.get(&k).copied().unwrap_or(0) {
            0 => out.push_str(&line),
            d if d > 0 => {
                out.push_str(&" ".repeat(d as usize));
                out.push_str(&line);
            }
            d => out.push_str(line.strip_prefix(&" ".repeat(d.unsigned_abs()))?),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(text: &str) -> Vec<String> {
        text.split_inclusive('\n').map(String::from).collect()
    }

    /// `text` after the number on `line` becomes `new`, realigned.
    fn renumbered(text: &str, line: usize, new: u64) -> String {
        let lines = split(text);
        let l = &lines[line];
        let indent = l.len() - l.trim_start_matches(' ').len();
        let digits = l[indent..].chars().take_while(char::is_ascii_digit).count();
        let edits = [Renumber { line, cols: indent..indent + digits, number: new.to_string() }];
        apply(&lines, &edits, &shifts(&lines, &edits)).unwrap()
    }

    #[test]
    fn what_still_reads_the_same_is_left_as_written() {
        // Continuation text, and a list nested by 4 as `>al` makes it.
        assert_eq!(renumbered("9. a\n   more\n", 0, 10), "10. a\n   more\n");
        assert_eq!(renumbered("9. a\n    - x\n      more\n", 0, 10), "10. a\n    - x\n      more\n");
        // A tab already reaches past the new column.
        assert_eq!(renumbered("9. a\n\t- x\n", 0, 10), "10. a\n\t- x\n");
        // Narrowing leaves a sub-item deeper than it has to be, still in.
        assert_eq!(renumbered("10. a\n    - x\n", 0, 9), "9. a\n    - x\n");
        // Nothing changes width.
        assert_eq!(renumbered("3. a\n   - x\n", 0, 4), "4. a\n   - x\n");
    }

    #[test]
    fn what_would_fall_out_moves_with_the_whole_item() {
        // A sub-list nested at the text column, as Prettier writes it.
        assert_eq!(renumbered("9. a\n   - x\n     - y\n", 0, 10), "10. a\n    - x\n      - y\n");
        // Code: fenced, indented in a loose item, and `$$`.
        assert_eq!(
            renumbered("9. a\n   ```\n   x = 1\n     y\n   ```\n", 0, 10),
            "10. a\n    ```\n    x = 1\n      y\n    ```\n"
        );
        assert_eq!(renumbered("9. a\n\n       code\n", 0, 10), "10. a\n\n        code\n");
        assert_eq!(renumbered("9. a\n   $$\n   x^2\n   $$\n", 0, 10), "10. a\n    $$\n    x^2\n    $$\n");
        // A table, a quote, a paragraph after a blank line.
        assert_eq!(
            renumbered("9. a\n\n   | h |\n   |---|\n   | c |\n", 0, 10),
            "10. a\n\n    | h |\n    |---|\n    | c |\n"
        );
        assert_eq!(renumbered("9. a\n   > q\n", 0, 10), "10. a\n    > q\n");
        assert_eq!(renumbered("9. a\n\n   para\n   - x\n", 0, 10), "10. a\n\n    para\n    - x\n");
        // `99.` → `100.` takes a list nested by 4 along too.
        assert_eq!(renumbered("99. a\n    - x\n", 0, 100), "100. a\n     - x\n");
        // An item in the middle of its list, and lazy text that stays.
        assert_eq!(renumbered("8. h\n9. i\n   - x\n10. j\n", 1, 10), "8. h\n10. i\n    - x\n10. j\n");
        assert_eq!(renumbered("9. a\nlazy\n   - x\n", 0, 10), "10. a\nlazy\n    - x\n");
    }

    /// The cases rule B (#83) got wrong: each now reads as it did.
    #[test]
    fn the_cases_that_partial_moves_got_wrong() {
        // Text 4+ columns past the text column is paragraph text, not a
        // sub-item, and must stay 4+ past.
        assert_eq!(renumbered("9. a\n       - x\n", 0, 10), "10. a\n        - x\n");
        // An item whose text opens a list of its own.
        assert_eq!(renumbered("9. - x\n     - y\n", 0, 10), "10. - x\n      - y\n");
        // Raw HTML, a fence after a sub-item's marker, a sub-item ending in
        // a heading or a rule.
        assert_eq!(renumbered("9. a\n   <div>\n     x\n   </div>\n", 0, 10), "10. a\n    <div>\n      x\n    </div>\n");
        assert_eq!(
            renumbered("9. a\n   - ```\n     code\n     ```\n", 0, 10),
            "10. a\n    - ```\n      code\n      ```\n"
        );
        assert_eq!(renumbered("9. a\n   - x\n     # h\n", 0, 10), "10. a\n    - x\n      # h\n");
        assert_eq!(renumbered("9. a\n   - x\n     ***\n", 0, 10), "10. a\n    - x\n      ***\n");
    }

    #[test]
    fn narrowing_takes_back_what_code_would_gain() {
        assert_eq!(renumbered("10. a\n\n        code\n", 0, 9), "9. a\n\n       code\n");
        assert_eq!(renumbered("10. a\n\n        - x\n", 0, 9), "9. a\n\n       - x\n");
    }

    /// When neither reads the same, as a tab can make it, the renumber goes
    /// in alone, as it would without realign.
    #[test]
    fn when_nothing_reads_the_same_the_renumber_goes_in_alone() {
        let text = "9.\n     \t- z\n";
        let lines = split(text);
        let edits = [Renumber { line: 0, cols: 0..1, number: "10".into() }];
        assert!(shape(&apply(&lines, &edits, &[]).unwrap()) != shape(text));
        assert_eq!(shifts(&lines, &edits), vec![]);
    }

    #[test]
    fn the_text_column_follows_commonmark() {
        let col = |line: &str| {
            let digits = line.trim_start().chars().take_while(char::is_ascii_digit).count();
            let indent = line.len() - line.trim_start().len();
            text_column(line, &Renumber { line: 0, cols: indent..indent + digits, number: String::new() })
        };
        assert_eq!(col("9. a\n"), Some(3));
        assert_eq!(col("  10)  a"), Some(7));
        assert_eq!(col("9.\n"), Some(3));
        assert_eq!(col("9.      code"), Some(3));
        assert_eq!(col("9.\ta"), None);
        assert_eq!(col("\t9. a"), None);
    }

    /// The claim the design rests on: for tab-free lists without tables, the
    /// renumber alone or the shift always reads as it did. Tabs, and a table
    /// delimiter row after lazy text, can defeat it; those fall back to the
    /// renumber alone.
    #[test]
    fn widening_always_reads_the_same_without_tabs_or_tables() {
        let frags = [
            "a", "more", "- x", "* y", "1. n", "9. m", "10. w", "```", "~~~", "$$", "# h", "> q", "***", "<div>",
            "code", "-", "",
        ];
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut rnd = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..5_000 {
            let mut text = String::from(["9. a\n", "9. - x\n", "9.\n", "9.  a\n", "9. ```\n"][rnd(5)]);
            for _ in 0..1 + rnd(6) {
                let f = frags[rnd(frags.len())];
                if !f.is_empty() {
                    text.push_str(&" ".repeat(rnd(10)));
                    text.push_str(f);
                }
                text.push('\n');
            }
            let after = renumbered(&text, 0, 10);
            assert!(shape(&after) == shape(&text), "{text:?} became {after:?}");
        }
    }
}
