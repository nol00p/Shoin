//! Keeping a renumbered item's lines inside it, as other Markdown tools read
//! it.
//!
//! A number that changes width (`9.` → `10.`) moves its item's text column,
//! and CommonMark (GitHub, pandoc's `commonmark` reader) reads everything in
//! an item relative to that column: a sub-item, fence or table left at the
//! old column falls out of the item, and indented code gains or loses a
//! space. Moving every line of the item that reaches the old column by the
//! same amount leaves the item's content, as CommonMark strips it, exactly
//! as it was.
//!
//! Rather than predict when that holds, which tabs, lazy lines and blocks
//! inside items make hard, each item is checked with a parse: its events
//! after the renumber have to match its events before. An item that already
//! matches with the renumber alone is left as written; one that matches only
//! with its lines moved gets them moved; one that matches neither way, as a
//! tab can make it, gets the renumber alone. Each item is decided on its
//! own, so one that cannot be helped does not stop another.
//!
//! "Before" is the text as it is, with each changed number replaced by a
//! stand-in of its old width that is `1` exactly when the new number is:
//! the width is what the lines were laid out for, and only a list starting
//! at 1 can interrupt a paragraph, so a sub-list a renumber starts at `1.`
//! reads as the list it is meant to be.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};
use ropey::Rope;

use super::list::Renumber;

/// One line's new start: its first `cut` chars become `with`. Renumbering
/// and realigning change nothing else, and the buffer gets exactly the
/// edits that were checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineEdit {
    pub line: usize,
    pub cut: usize,
    pub with: String,
}

/// The line edits that make `edits` in `rope`: each number rewritten, and
/// where a number changes width, its item's lines moved with its text when
/// that, and only that, keeps the item reading as it did.
pub fn line_edits(rope: &Rope, edits: &[Renumber]) -> Vec<LineEdit> {
    if edits.iter().all(|r| width_change(r) == 0) {
        return edits
            .iter()
            .filter_map(|r| line_edit(r.line, &rope.line(r.line).to_string(), Some(r), 0))
            .collect();
    }
    let text = rope.to_string();
    let lines: Vec<&str> =
        (0..rope.len_lines()).map(|l| &text[rope.line_to_byte(l)..rope.line_to_byte(l + 1)]).collect();
    realign(&lines, edits)
}

/// How many columns `r` widens its number by (negative: narrows).
fn width_change(r: &Renumber) -> isize {
    r.number.chars().count() as isize - r.cols.len() as isize
}

/// A renumbered item, as it read before.
struct Item<'r, 't> {
    edit: &'r Renumber,
    /// Its text column, before.
    col: usize,
    /// Its last line, blank ones included.
    last: usize,
    events: Vec<Event<'t>>,
}

/// The line edits for `edits` on `lines` (each with its line ending), each
/// width-changing item realigned when that makes it read as before.
fn realign(lines: &[&str], edits: &[Renumber]) -> Vec<LineEdit> {
    let numbers: HashMap<usize, &Renumber> = edits.iter().map(|r| (r.line, r)).collect();
    let stand_ins: Vec<LineEdit> = edits
        .iter()
        .filter_map(|r| line_edit(r.line, lines[r.line], Some(&Renumber { number: stand_in(r), ..r.clone() }), 0))
        .collect();
    let (before, starts) = apply(lines, &stand_ins);
    let line_of = |byte: usize| starts.partition_point(|&s| s <= byte).saturating_sub(1);
    let parsed = parse(&before);

    let mut changing: Vec<&Renumber> = edits.iter().filter(|r| width_change(r) != 0).collect();
    changing.sort_by_key(|r| r.line);
    let items: Vec<Option<Item>> = changing
        .iter()
        .map(|&edit| {
            let col = text_column(lines[edit.line], edit)?;
            // The indent is spaces (`text_column` checked), so chars are bytes.
            let at = item_at(&parsed, starts[edit.line], starts[edit.line] + edit.cols.start)?;
            let last = line_of(parsed[at].1.end.saturating_sub(1)).min(lines.len() - 1);
            Some(Item { edit, col, last, events: item_events(&parsed, at) })
        })
        .collect();

    // The edits with the items `on` realigned; `None` if a line has fewer
    // leading spaces than a narrowing takes away.
    let build = |on: &[bool]| -> Option<Vec<LineEdit>> {
        let mut shift: BTreeMap<usize, isize> = BTreeMap::new();
        for item in items.iter().zip(on).filter(|(_, on)| **on).filter_map(|(item, _)| item.as_ref()) {
            let reach = " ".repeat(item.col);
            let inside = lines.iter().enumerate().take(item.last + 1).skip(item.edit.line + 1);
            for (k, _) in inside.filter(|(_, l)| l.starts_with(&reach)) {
                *shift.entry(k).or_default() += width_change(item.edit);
            }
        }
        let touched: BTreeSet<usize> = numbers.keys().copied().chain(shift.keys().copied()).collect();
        touched
            .into_iter()
            .filter_map(|k| {
                let by = shift.get(&k).copied().unwrap_or(0);
                match numbers.get(&k) {
                    None if by == 0 => None,
                    r => Some(line_edit(k, lines[k], r.copied(), by)),
                }
            })
            .collect()
    };
    // Which items read as they did with `edits` made: the same events, and
    // the same lines, so none fell out of the item to be read after it, and
    // none came in.
    let reads_as_before = |edits: &[LineEdit]| -> Vec<bool> {
        let (text, starts) = apply(lines, edits);
        let line_of = |byte: usize| starts.partition_point(|&s| s <= byte).saturating_sub(1);
        let parsed = parse(&text);
        let at: HashMap<usize, &LineEdit> = edits.iter().map(|e| (e.line, e)).collect();
        items
            .iter()
            .map(|item| {
                item.as_ref().is_some_and(|item| {
                    let line = item.edit.line;
                    // The number ends the edit's `with`, after spaces.
                    let marker = at[&line].with.len() - item.edit.number.len();
                    item_at(&parsed, starts[line], starts[line] + marker).is_some_and(|i| {
                        line_of(parsed[i].1.end.saturating_sub(1)).min(lines.len() - 1) == item.last
                            && item_events(&parsed, i) == item.events
                    })
                })
            })
            .collect()
    };

    let mut on = vec![false; items.len()];
    // With nothing shifted, every line edit is a plain renumber.
    let mut best: Vec<LineEdit> =
        edits.iter().filter_map(|r| line_edit(r.line, lines[r.line], Some(r), 0)).collect();
    let mut good = reads_as_before(&best);
    // Realign an item when that makes more items read as before and none
    // stop; again, since realigning an inner item can be what lets the
    // item around it read as before.
    let count = |v: &[bool]| v.iter().filter(|&&b| b).count();
    for _ in 0..items.len() {
        let mut changed = false;
        for i in 0..items.len() {
            if on[i] || good[i] || items[i].is_none() {
                continue;
            }
            on[i] = true;
            if let Some(edits) = build(&on) {
                let now = reads_as_before(&edits);
                if now.iter().zip(&good).all(|(n, g)| *n || !*g) && count(&now) > count(&good) {
                    (best, good, changed) = (edits, now, true);
                    continue;
                }
            }
            on[i] = false;
        }
        if !changed {
            break;
        }
    }
    best
}

/// A number of `r`'s old width that is `1` exactly when its new one is.
fn stand_in(r: &Renumber) -> String {
    let width = r.cols.len();
    if r.number.chars().count() == width {
        r.number.clone()
    } else if r.number.trim_start_matches('0') == "1" {
        format!("{:0>width$}", "1")
    } else {
        format!("{:0>width$}", "2")
    }
}

/// The edit that renumbers `line` (number `k`) by `r`, if given, and moves
/// it `shift` columns (negative: takes that many leading spaces away);
/// `None` when it has fewer leading spaces than that.
fn line_edit(k: usize, line: &str, r: Option<&Renumber>, shift: isize) -> Option<LineEdit> {
    let Some(r) = r else {
        return Some(match shift {
            s if s >= 0 => LineEdit { line: k, cut: 0, with: " ".repeat(s as usize) },
            s => {
                let n = s.unsigned_abs();
                line.starts_with(&" ".repeat(n)).then_some(LineEdit { line: k, cut: n, with: String::new() })?
            }
        });
    };
    let indent: String = line.chars().take(r.cols.start).collect();
    let indent = match shift {
        s if s >= 0 => " ".repeat(s as usize) + &indent,
        s => indent.strip_prefix(&" ".repeat(s.unsigned_abs()))?.to_string(),
    };
    Some(LineEdit { line: k, cut: r.cols.end, with: indent + &r.number })
}

/// `lines` with `edits` made, joined, and where each line now starts.
fn apply(lines: &[&str], edits: &[LineEdit]) -> (String, Vec<usize>) {
    let at: HashMap<usize, &LineEdit> = edits.iter().map(|e| (e.line, e)).collect();
    let mut text = String::with_capacity(lines.iter().map(|l| l.len()).sum::<usize>() + 16);
    let mut starts = Vec::with_capacity(lines.len());
    for (k, line) in lines.iter().enumerate() {
        starts.push(text.len());
        match at.get(&k) {
            Some(e) => {
                text.push_str(&e.with);
                text.push_str(line.char_indices().nth(e.cut).map_or("", |(b, _)| &line[b..]));
            }
            None => text.push_str(line),
        }
    }
    (text, starts)
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
}

/// Every event with the bytes it came from.
fn parse(text: &str) -> Vec<(Event<'_>, Range<usize>)> {
    Parser::new_ext(text, options()).into_offset_iter().collect()
}

/// The item whose number is at byte `marker` on the line starting at byte
/// `line`. The parser starts an item at its number, or at the line's indent
/// past the text column of what holds it, never before the line.
fn item_at(parsed: &[(Event, Range<usize>)], line: usize, marker: usize) -> Option<usize> {
    parsed.iter().rposition(|(e, r)| matches!(e, Event::Start(Tag::Item)) && (line..=marker).contains(&r.start))
}

/// The events of the item starting at `parsed[at]`, through its end, with
/// the numbers lists start at left out: renumbering is meant to change them.
fn item_events<'t>(parsed: &[(Event<'t>, Range<usize>)], at: usize) -> Vec<Event<'t>> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (e, _) in &parsed[at..] {
        match e {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
        out.push(match e {
            Event::Start(Tag::List(Some(_))) => Event::Start(Tag::List(Some(1))),
            e => e.clone(),
        });
        if depth == 0 {
            break;
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole document's reading, list start numbers left out.
    fn shape(text: &str) -> Vec<Event<'_>> {
        parse(text)
            .into_iter()
            .map(|(e, _)| match e {
                Event::Start(Tag::List(Some(_))) => Event::Start(Tag::List(Some(1))),
                e => e,
            })
            .collect()
    }

    /// `text` with `edits` made through `line_edits`, as the buffer gets them.
    fn edited(text: &str, edits: &[Renumber]) -> String {
        let rope = Rope::from_str(text);
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        apply(&lines, &line_edits(&rope, edits)).0
    }

    /// The renumber that makes the number on `line` `new`.
    fn to(text: &str, line: usize, new: u64) -> Renumber {
        let l = text.split('\n').nth(line).unwrap();
        let indent = l.len() - l.trim_start_matches(' ').len();
        let digits = l[indent..].chars().take_while(char::is_ascii_digit).count();
        Renumber { line, cols: indent..indent + digits, number: new.to_string() }
    }

    /// `text` after the number on `line` becomes `new`, realigned.
    fn renumbered(text: &str, line: usize, new: u64) -> String {
        edited(text, &[to(text, line, new)])
    }

    #[test]
    fn what_still_reads_the_same_is_left_as_written() {
        // Continuation text, and a list nested by 4 as `>al` makes it.
        assert_eq!(renumbered("9. a\n   more\n", 0, 10), "10. a\n   more\n");
        // A sub-list a renumber starts at `1.` counts as one: in the text
        // before, `2. t` could not interrupt `s` and read as more of it.
        assert_eq!(edited("1. s\n   2. t\n", &[to("1. s", 0, 10), to("x\n   2. t", 1, 1)]), "10. s\n    1. t\n");
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
        // Once anything has to move, the whole item does, text too.
        assert_eq!(renumbered("9. a\n   more\n   - x\n", 0, 10), "10. a\n    more\n    - x\n");
    }

    /// Review of #86: each still fell out of its item.
    #[test]
    fn review_cases() {
        // An indented list: the parser starts the item before its number.
        assert_eq!(renumbered("  8. h\n  9. i\n     - x\n", 1, 10), "  8. h\n  10. i\n      - x\n");
        assert_eq!(
            renumbered("- a\n    1. s\n    9. i\n       ```\n       make\n       ```\n", 2, 10),
            "- a\n    1. s\n    10. i\n        ```\n        make\n        ```\n"
        );
        // A whitespace-only line in a fence is code too.
        assert_eq!(
            renumbered("9. i\n   ```py\n   def f():\n       x = 1\n       \n       y = 2\n   ```\n", 0, 10),
            "10. i\n    ```py\n    def f():\n        x = 1\n        \n        y = 2\n    ```\n"
        );
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
        assert_eq!(renumbered(text, 0, 10), "10.\n     \t- z\n");
        assert!(shape(&renumbered(text, 0, 10)) != shape(text));
    }

    /// Two items in one renumber are decided apart: one that reads the
    /// same keeps its indents, and one no shift can help stops nothing.
    #[test]
    fn each_item_is_decided_on_its_own() {
        let text = "10. b\n    - y\n9. a\n   - x\n";
        assert_eq!(edited(text, &[to(text, 0, 9), to(text, 2, 10)]), "9. b\n    - y\n10. a\n    - x\n");
        let text = "10. b\n\n    \tcode\n9. a\n   - x\n";
        assert_eq!(edited(text, &[to(text, 0, 9), to(text, 3, 10)]), "9. b\n\n    \tcode\n10. a\n    - x\n");
    }

    /// Nested numbers that both widen: the inner item moves with the outer
    /// one and by its own widening too.
    #[test]
    fn nested_widenings_add_up() {
        let text = "9. a\n   1. s\n   9. t\n      - x\n";
        assert_eq!(edited(text, &[to(text, 0, 10), to(text, 2, 10)]), "10. a\n    1. s\n    10. t\n        - x\n");
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

    /// The claim the design rests on: for tab-free lists without tables, at
    /// any indent and with whitespace-only lines, the renumber alone or the
    /// shift always reads as it did. Tabs, and a table delimiter row after
    /// lazy text, can defeat it; those get the renumber alone.
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
            let mut text = " ".repeat(rnd(4));
            text.push_str(["9. a\n", "9. - x\n", "9.\n", "9.  a\n", "9. ```\n"][rnd(5)]);
            for _ in 0..1 + rnd(6) {
                // An empty fragment makes a blank or whitespace-only line.
                let f = frags[rnd(frags.len())];
                text.push_str(&" ".repeat(rnd(10)));
                text.push_str(f);
                text.push('\n');
            }
            let indent = text.len() - text.trim_start_matches(' ').len();
            let after = edited(&text, &[Renumber { line: 0, cols: indent..indent + 1, number: "10".into() }]);
            assert!(shape(&after) == shape(&text), "{text:?} became {after:?}");
        }
    }
}
