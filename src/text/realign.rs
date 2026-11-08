//! Keeping a renumbered item's lines inside it.
//!
//! A number that changes width (`9.` → `10.`) moves its item's text column,
//! and CommonMark (GitHub, pandoc's `commonmark` reader) reads everything in
//! an item relative to that column: a sub-item or table left at the old
//! column falls out of the item, and indented code gains or loses a space.
//!
//! Each item whose number changes width gets the smallest of these that
//! makes it read as it did, tried in turn:
//!
//! 1. the new number alone, when nothing depends on the column;
//! 2. the spaces after the number taking up the change, so the column stays
//!    where it was (`10. a` → `9.  a`, `9.  a` → `10. a`);
//! 3. every line of the item that reaches the old column moving by the same
//!    amount, which leaves the item's content, as CommonMark strips it,
//!    exactly as it was.
//!
//! Only an item that CommonMark and Shoin agree ends on the same line is
//! adjusted: where they disagree, the text was already read differently by
//! other tools than Shoin shows it, and the number goes in alone, as it
//! would without realigning (which may well be what puts it right).
//!
//! "Reads as it did" is checked, not predicted. The item's parse
//! (pulldown-cmark) has to give the same events over the same lines under
//! the same parent item, and Shoin's own reading of its lines (block kinds,
//! a task's box, which item each is in) has to stay the same, since the
//! editor and HTML export read with that. When nothing works, as with a
//! fence, `$$` block, heading or rule that moving would put 4 columns in
//! (where Shoin's parser stops seeing one), the number goes in alone. Lines
//! indented with tabs never move. Each item is decided on its own, so one
//! that cannot be helped does not stop another.
//!
//! "As it did" is the text before the renumber, with each changed number
//! replaced by a stand-in of its old width that is `1` exactly when the new
//! number is: the width is what the lines were laid out for, and only a list
//! starting at 1 can interrupt a paragraph, so a sub-list a renumber starts
//! at `1.` reads as the list it is meant to be. Digits in text are left out
//! of the comparison, since a renumbered line can read as text.

use std::collections::{BTreeMap, HashMap};
use std::ops::RangeInclusive;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ropey::Rope;

use super::buffer::Buffer;
use super::list::{Lists, Renumber};
use crate::render::markdown::block::{BlockCache, BlockKind};

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
/// where a number changes width, its item kept reading as it did when one
/// of the ways in the module docs does that. `math` and `tab_width` are what
/// Shoin reads the text with.
pub fn line_edits(rope: &Rope, edits: &[Renumber], math: bool, tab_width: usize) -> Vec<LineEdit> {
    if edits.iter().all(|r| width_change(r) == 0) {
        return edits.iter().map(|r| plain_edit(&rope.line(r.line).to_string(), r)).collect();
    }
    let text = rope.to_string();
    let lines: Vec<&str> =
        (0..rope.len_lines()).map(|l| &text[rope.line_to_byte(l)..rope.line_to_byte(l + 1)]).collect();
    realign(&text, &lines, edits, |text, range| shoin_reading(text, range, math, tab_width))
}

/// How many columns `r` widens its number by (negative: narrows).
fn width_change(r: &Renumber) -> isize {
    r.number.chars().count() as isize - r.cols.len() as isize
}

/// How Shoin reads a line: its block kind, with an item's depth left out,
/// and the item it is in. Depth alone may change; nesting may not.
type ShoinLine = (BlockKind, Option<usize>);

/// How Shoin reads lines `range` of `text`, and where the item on each of
/// them ends (for lines that start no item, `None`).
fn shoin_reading(
    text: &str,
    range: RangeInclusive<usize>,
    math: bool,
    tab_width: usize,
) -> (Vec<ShoinLine>, Vec<Option<usize>>) {
    let mut buffer = Buffer::empty();
    buffer.rope = Rope::from_str(text);
    let kinds = BlockCache::build(&buffer, math).kinds;
    let lists = Lists::new(&buffer, &kinds, tab_width);
    let read = range
        .clone()
        .map(|l| {
            let kind = match &kinds[l] {
                BlockKind::ListItem { marker, checked, .. } => {
                    BlockKind::ListItem { depth: 0, marker: *marker, checked: *checked }
                }
                kind => kind.clone(),
            };
            (kind, lists.parent(l))
        })
        .collect();
    let ends = range.map(|l| lists.item_bounds(l).filter(|(start, _)| *start == l).map(|(_, end)| end)).collect();
    (read, ends)
}

/// What can keep an item reading as it did; see the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fix {
    Number,
    Gap,
    Lines,
}

/// A renumbered item whose number changes width, as it was.
struct Item<'r, 't> {
    edit: &'r Renumber,
    /// Its text column.
    col: usize,
    /// The spaces between the delimiter and the text, when there is text
    /// and they set the column.
    gap: Option<usize>,
    /// Its last line that is not blank.
    last_text: usize,
    /// Whether a line under it is indented with a tab somewhere.
    tabbed: bool,
    reading: Reading<'t>,
}

impl Item<'_, '_> {
    /// Whether `fix` can be tried: the gap has to stay 1 to 4 spaces, and
    /// lines indented with tabs never move, since Shoin's editor and its
    /// HTML export count a tab differently, and a shift would leave the tab
    /// where it is while spaces around it move.
    fn allows(&self, fix: Fix) -> bool {
        match fix {
            Fix::Number => true,
            Fix::Gap => self.gap.is_some_and(|g| (1..=4).contains(&(g as isize - width_change(self.edit)))),
            Fix::Lines => !self.tabbed,
        }
    }
}

/// Where the search for fixes stands: each item's fix, the line edits they
/// make, and which items read as they did with them.
struct State {
    fixes: Vec<Fix>,
    edits: Vec<LineEdit>,
    good: Vec<bool>,
}

fn realign(
    text: &str,
    lines: &[&str],
    edits: &[Renumber],
    shoin: impl Fn(&str, RangeInclusive<usize>) -> (Vec<ShoinLine>, Vec<Option<usize>>),
) -> Vec<LineEdit> {
    let numbers: BTreeMap<usize, &Renumber> = edits.iter().map(|r| (r.line, r)).collect();
    let stand_ins: Vec<LineEdit> = edits
        .iter()
        .map(|r| plain_edit(lines[r.line], &Renumber { number: stand_in(r), ..r.clone() }))
        .collect();
    let (before_text, before_starts) = apply(lines, &stand_ins);
    let before = Parsed::new(&before_text, before_starts);

    let items: Vec<Option<Item>> = numbers
        .values()
        .filter(|r| width_change(r) != 0)
        .map(|&edit| {
            let (col, gap) = layout(lines[edit.line], edit)?;
            // `layout` checked the indent is spaces, so chars are bytes.
            let reading = before.reading(before.item_at(edit.line, edit.cols.start)?);
            let last_text = (edit.line..=reading.last).rev().find(|&k| !lines[k].trim().is_empty())?;
            let tabbed = lines[edit.line + 1..=last_text].iter().any(|l| l.trim_start_matches([' ', '\t']).len() < l.trim_start_matches(' ').len());
            Some(Item { edit, col, gap, last_text, tabbed, reading })
        })
        .collect();
    // The lines any fix could change, and Shoin's reading of them before,
    // worked out only once a fix is tried.
    let first = items.iter().flatten().map(|i| i.edit.line).min().unwrap_or(0);
    let last = items.iter().flatten().map(|i| i.reading.last).max().unwrap_or(0);
    let shoin_before = std::cell::OnceCell::new();
    let shoin_before = || shoin_before.get_or_init(|| shoin(text, first..=last));

    // The line edits for `edits` with each item fixed by its `fixes`.
    let build = |fixes: &[Fix]| -> Option<Vec<LineEdit>> {
        let mut shift: BTreeMap<usize, isize> = BTreeMap::new();
        let mut gaps: HashMap<usize, usize> = HashMap::new();
        for (item, fix) in items.iter().zip(fixes) {
            let Some(item) = item else { continue };
            let change = width_change(item.edit);
            match fix {
                Fix::Number => {}
                Fix::Gap => {
                    gaps.insert(item.edit.line, (item.gap? as isize - change) as usize);
                }
                Fix::Lines => {
                    let reach = " ".repeat(item.col);
                    let inside = lines.iter().enumerate().take(item.last_text + 1).skip(item.edit.line + 1);
                    for (k, _) in inside.filter(|(_, l)| l.starts_with(&reach)) {
                        *shift.entry(k).or_default() += change;
                    }
                }
            }
        }
        let numbered = numbers.iter().map(|(&k, r)| {
            number_edit(lines[k], r, shift.get(&k).copied().unwrap_or(0), gaps.get(&k).copied())
        });
        let shifted = shift
            .iter()
            .filter(|&(k, by)| !numbers.contains_key(k) && *by != 0)
            .map(|(&k, &by)| shift_edit(k, lines[k], by));
        numbered.chain(shifted).collect()
    };
    // Which items read as they did with `edits` made. Only a fix can change
    // what Shoin reads, so without one that is not checked.
    let check = |edits: &[LineEdit], fixed: bool| -> Vec<bool> {
        let (text, starts) = apply(lines, edits);
        let after = Parsed::new(&text, starts);
        let shoin_after = fixed.then(|| shoin(&text, first..=last).0);
        let numbered: HashMap<usize, &LineEdit> = edits.iter().map(|e| (e.line, e)).collect();
        let same_for_shoin = |range: RangeInclusive<usize>| {
            shoin_after.as_ref().is_none_or(|now| {
                let was = &shoin_before().0;
                range.into_iter().all(|l| now[l - first] == was[l - first])
            })
        };
        items
            .iter()
            .map(|item| {
                item.as_ref().is_some_and(|item| {
                    let line = item.edit.line;
                    // The number follows the spaces its edit starts with.
                    let marker = numbered[&line].with.chars().take_while(|&c| c == ' ').count();
                    after.item_at(line, marker).is_some_and(|s| after.reading(s) == item.reading)
                        && same_for_shoin(line..=item.reading.last)
                })
            })
            .collect()
    };
    // Fix the items `set` (those not already reading as they did) with
    // `fix`, and keep it if they all then read as they did and no other item
    // stops.
    let attempt = |state: &mut State, set: &[usize], fix: Fix| {
        let mut fixes = state.fixes.clone();
        for &i in set.iter().filter(|&&i| !state.good[i]) {
            fixes[i] = fix;
        }
        let Some(edits) = build(&fixes) else { return };
        let good = check(&edits, true);
        let kept = state.good.iter().zip(&good).all(|(was, now)| *now || !*was);
        if kept && set.iter().all(|&i| good[i]) {
            *state = State { fixes, edits, good };
        }
    };

    let mut state = State {
        fixes: vec![Fix::Number; items.len()],
        edits: numbers.values().map(|r| plain_edit(lines[r.line], r)).collect(),
        good: Vec::new(),
    };
    state.good = check(&state.edits, false);
    for fix in [Fix::Gap, Fix::Lines] {
        // Only an item CommonMark and Shoin agree ends on the same line.
        let trying: Vec<usize> = (0..items.len())
            .filter(|&i| {
                items[i].as_ref().is_some_and(|item| {
                    !state.good[i]
                        && item.allows(fix)
                        && shoin_before().1[item.edit.line - first] == Some(item.last_text)
                })
            })
            .collect();
        if trying.is_empty() {
            continue;
        }
        // All at once usually does it. If not, an item with the items inside
        // it, which have to move together; then one at a time.
        attempt(&mut state, &trying, fix);
        for group in nested_groups(&trying, &items) {
            if group.len() > 1 {
                attempt(&mut state, &group, fix);
            }
            for &i in &group {
                attempt(&mut state, &[i], fix);
            }
        }
    }
    state.edits
}

/// `trying` (in line order) split into an item with the items inside it.
fn nested_groups(trying: &[usize], items: &[Option<Item>]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut end = 0;
    for &i in trying {
        let Some(item) = &items[i] else { continue };
        match groups.last_mut() {
            Some(group) if item.edit.line <= end => group.push(i),
            _ => groups.push(vec![i]),
        }
        end = end.max(item.reading.last);
    }
    groups
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

/// The edit that puts `r`'s number on its line, and nothing else.
fn plain_edit(line: &str, r: &Renumber) -> LineEdit {
    let indent: String = line.chars().take(r.cols.start).collect();
    LineEdit { line: r.line, cut: r.cols.end, with: indent + &r.number }
}

/// `plain_edit` with the line moved `shift` columns (negative: that many
/// leading spaces taken away), and with `gap` spaces after the delimiter
/// when given. `None` when the line has fewer leading spaces than that.
fn number_edit(line: &str, r: &Renumber, shift: isize, gap: Option<usize>) -> Option<LineEdit> {
    let mut e = plain_edit(line, r);
    if shift < 0 {
        e.with = e.with.strip_prefix(&" ".repeat(shift.unsigned_abs()))?.to_string();
    } else {
        e.with.insert_str(0, &" ".repeat(shift as usize));
    }
    if let Some(gap) = gap {
        let after: Vec<char> = line.chars().skip(r.cols.end).collect();
        e.with.push(*after.first()?);
        e.with.push_str(&" ".repeat(gap));
        e.cut += 1 + after[1..].iter().take_while(|&&c| c == ' ').count();
    }
    Some(e)
}

/// The edit that moves line `k` by `shift` columns; `None` when it has
/// fewer leading spaces than a negative shift takes away.
fn shift_edit(k: usize, line: &str, shift: isize) -> Option<LineEdit> {
    if shift >= 0 {
        return Some(LineEdit { line: k, cut: 0, with: " ".repeat(shift as usize) });
    }
    let n = shift.unsigned_abs();
    line.starts_with(&" ".repeat(n)).then(|| LineEdit { line: k, cut: n, with: String::new() })
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

/// Where `r`'s item text starts, as CommonMark counts it: past the indent,
/// the number and its delimiter, and the spaces after it, or just one of
/// them when the item is empty or five or more follow (the rest is then
/// indented code). With it, those spaces when they set the column. `None`
/// unless the indent and the spaces are spaces.
fn layout(line: &str, r: &Renumber) -> Option<(usize, Option<usize>)> {
    let chars: Vec<char> = line.trim_end_matches(['\n', '\r']).chars().collect();
    if !chars.get(..r.cols.start)?.iter().all(|&c| c == ' ') || !matches!(chars.get(r.cols.end), Some('.' | ')')) {
        return None;
    }
    let after = r.cols.end + 1;
    let gap = chars[after..].iter().take_while(|&&c| c == ' ').count();
    match chars.get(after + gap) {
        None => Some((after + 1, None)),
        Some('\t') => None,
        Some(_) if gap == 0 => None,
        Some(_) if gap >= 5 => Some((after + 1, None)),
        Some(_) => Some((after + gap, Some(gap))),
    }
}

/// What an item reads as: its events, with the numbers lists start at and
/// digits in text left out, its last line, and the line of the item it is
/// in.
#[derive(Debug, PartialEq)]
struct Reading<'t> {
    events: Vec<Event<'t>>,
    last: usize,
    parent: Option<usize>,
}

/// An item in a parse: its bytes, its events, and the item it is in.
struct Span {
    bytes: std::ops::Range<usize>,
    events: std::ops::RangeInclusive<usize>,
    parent: Option<usize>,
}

/// A text's parse, with its items in order.
struct Parsed<'t> {
    events: Vec<Event<'t>>,
    items: Vec<Span>,
    starts: Vec<usize>,
}

impl<'t> Parsed<'t> {
    /// `text`, whose lines start at `starts`.
    fn new(text: &'t str, starts: Vec<usize>) -> Self {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_MATH
            | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
        let (mut events, mut items, mut open) = (Vec::new(), Vec::new(), Vec::new());
        for (e, bytes) in Parser::new_ext(text, options).into_offset_iter() {
            match e {
                Event::Start(Tag::Item) => {
                    let at = events.len();
                    items.push(Span { bytes, events: at..=at, parent: open.last().copied() });
                    open.push(items.len() - 1);
                }
                Event::End(TagEnd::Item) => {
                    if let Some(i) = open.pop() {
                        items[i].events = *items[i].events.start()..=events.len();
                    }
                }
                _ => {}
            }
            events.push(e);
        }
        Parsed { events, items, starts }
    }

    fn line_of(&self, byte: usize) -> usize {
        self.starts.partition_point(|&s| s <= byte).saturating_sub(1)
    }

    /// The item whose number is `marker` bytes into `line`. The parser
    /// starts an item at its number, or at the line's indent past the text
    /// column of what holds it, never before the line.
    fn item_at(&self, line: usize, marker: usize) -> Option<&Span> {
        let (from, to) = (self.starts[line], self.starts[line] + marker);
        let i = self.items.partition_point(|s| s.bytes.start <= to).checked_sub(1)?;
        (self.items[i].bytes.start >= from).then(|| &self.items[i])
    }

    fn reading(&self, span: &Span) -> Reading<'t> {
        let events = self.events[span.events.clone()]
            .iter()
            .map(|e| match e {
                Event::Start(Tag::List(Some(_))) => Event::Start(Tag::List(Some(1))),
                Event::Text(t) if t.bytes().any(|b| b.is_ascii_digit()) => {
                    Event::Text(t.chars().filter(|c| !c.is_ascii_digit()).collect::<String>().into())
                }
                e => e.clone(),
            })
            .collect();
        Reading {
            events,
            last: self.line_of(span.bytes.end.saturating_sub(1)).min(self.starts.len() - 1),
            parent: span.parent.map(|p| self.line_of(self.items[p].bytes.start)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str(text);
        b
    }

    /// The whole document's reading, as an item's is compared.
    fn shape(text: &str) -> Reading<'_> {
        let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        let parsed = Parsed::new(text, starts);
        let all = Span { bytes: 0..text.len(), events: 0..=parsed.events.len().saturating_sub(1), parent: None };
        parsed.reading(&all)
    }

    /// `text` with `edits` made through `line_edits`, as the buffer gets them.
    fn edited(text: &str, edits: &[Renumber]) -> String {
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        apply(&lines, &line_edits(&Rope::from_str(text), edits, true, 4)).0
    }

    /// Whether CommonMark and Shoin end `edit`'s item on the same line.
    fn agree(text: &str, edit: &Renumber) -> bool {
        let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        let parsed = Parsed::new(text, starts);
        let Some(span) = parsed.item_at(edit.line, edit.cols.start) else { return false };
        let last = parsed.reading(span).last;
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let last_text = (edit.line..=last).rev().find(|&k| !lines[k].trim().is_empty());
        let b = buffer(text);
        let cache = BlockCache::build(&b, true);
        Lists::new(&b, &cache.kinds, 4).item_bounds(edit.line).map(|(_, end)| end) == last_text
    }

    /// The renumber that makes the number on `line` `new`.
    fn to(text: &str, line: usize, new: u64) -> Renumber {
        let l = text.split('\n').nth(line).unwrap();
        let indent = l.len() - l.trim_start_matches(' ').len();
        let digits = l[indent..].chars().take_while(char::is_ascii_digit).count();
        Renumber { line, cols: indent..indent + digits, number: new.to_string() }
    }

    /// `text` after the number on `line` becomes `new`.
    fn renumbered(text: &str, line: usize, new: u64) -> String {
        edited(text, &[to(text, line, new)])
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
    fn the_gap_after_the_number_takes_the_change_when_it_can() {
        // Narrowing: the column stays, so nothing else moves.
        assert_eq!(renumbered("10. a\n\n        code\n", 0, 9), "9.  a\n\n        code\n");
        // Widening, with a space to spare.
        assert_eq!(renumbered("9.  a\n    - x\n", 0, 10), "10. a\n    - x\n");
        // An empty item has no gap to give: its lines move.
        assert_eq!(renumbered("10.\n        code\n", 0, 9), "9.\n       code\n");
    }

    #[test]
    fn what_would_fall_out_moves_with_the_whole_item() {
        // A sub-list nested at the text column, as Prettier writes it.
        assert_eq!(renumbered("9. a\n   - x\n     - y\n", 0, 10), "10. a\n    - x\n      - y\n");
        // Indented code in a loose item, a table, a quote, a paragraph after
        // a blank line.
        assert_eq!(renumbered("9. a\n\n       code\n", 0, 10), "10. a\n\n        code\n");
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
        // Indented lists: the parser starts the item before its number.
        assert_eq!(renumbered("  8. h\n  9. i\n     - x\n", 1, 10), "  8. h\n  10. i\n      - x\n");
        assert_eq!(
            renumbered("- a\n    1. s\n    9. i\n       - x\n", 2, 10),
            "- a\n    1. s\n    10. i\n        - x\n"
        );
        // Whitespace-only lines past the item's text stay as they are.
        assert_eq!(renumbered("9. a\n   - x\n     \n\n   \npara\n", 0, 10), "10. a\n    - x\n     \n\n   \npara\n");
    }

    /// A fence, `$$` block or heading 4 columns in is prose to Shoin, so an
    /// item holding one keeps its place: the number goes in alone, and
    /// CommonMark tools read the block outside the item, as they did before
    /// realigning existed.
    #[test]
    fn what_shoin_would_stop_seeing_stays() {
        for text in [
            "9. a\n   ```\n   x = 1\n   ```\n",
            "9. a\n   $$\n   x^2\n   $$\n",
            "9. i\n   ```py\n   def f():\n       \n   ```\n",
            "9. a\n   - x\n   # h\n",
        ] {
            assert_eq!(renumbered(text, 0, 10), format!("1{text}").replacen("19.", "10.", 1));
        }
    }

    /// The cases moving only some lines (#83) got wrong: each reads as it did.
    #[test]
    fn the_cases_that_partial_moves_got_wrong() {
        // Text 4+ columns past the text column is paragraph text, not a
        // sub-item, and must stay 4+ past.
        assert_eq!(renumbered("9. a\n       - x\n", 0, 10), "10. a\n        - x\n");
        // An item whose text opens a list of its own.
        assert_eq!(renumbered("9. - x\n     - y\n", 0, 10), "10. - x\n      - y\n");
        // Raw HTML, a fence after a sub-item's marker (prose to Shoin either
        // way), a sub-item ending in a heading or a rule 5 columns in.
        assert_eq!(renumbered("9. a\n   <div>\n     x\n   </div>\n", 0, 10), "10. a\n    <div>\n      x\n    </div>\n");
        assert_eq!(
            renumbered("9. a\n   - ```\n     code\n     ```\n", 0, 10),
            "10. a\n    - ```\n      code\n      ```\n"
        );
        assert_eq!(renumbered("9. a\n   - x\n     # h\n", 0, 10), "10. a\n    - x\n      # h\n");
        assert_eq!(renumbered("9. a\n   - x\n     ***\n", 0, 10), "10. a\n    - x\n      ***\n");
    }

    /// A sub-list a renumber starts at `1.` counts as one, and a numbered
    /// line that reads as text compares without its digits.
    #[test]
    fn numbers_that_change_what_reads_as_a_list() {
        // Before, `2. t` could not interrupt `s` and read as more of it.
        assert_eq!(edited("1. s\n   2. t\n", &[to("1. s", 0, 10), to("x\n   2. t", 1, 1)]), "10. s\n    1. t\n");
        // `10. t` is text in `a` before and `9. t` after.
        let text = "9. a\n   10. t\n       - x\n";
        assert_eq!(edited(text, &[to(text, 0, 10), to(text, 1, 9)]), "10. a\n    9. t\n        - x\n");
    }

    /// Where CommonMark and Shoin disagree on where the item ends, the text
    /// already reads differently elsewhere than in Shoin: the number goes in
    /// alone, which here puts the fence back in the item for both.
    #[test]
    fn an_item_other_tools_already_read_differently_gets_the_number_alone() {
        assert_eq!(renumbered("10. j\n   ```sh\n   make\n   ```\n", 0, 9), "9. j\n   ```sh\n   make\n   ```\n");
        assert_eq!(renumbered("10. a\n   - x\n", 0, 9), "9. a\n   - x\n");
    }

    /// When nothing reads the same, as a tab can make it, the number goes in
    /// alone, as it would without realigning.
    #[test]
    fn when_nothing_reads_the_same_the_number_goes_in_alone() {
        let text = "9.\n     \t- z\n";
        assert_eq!(renumbered(text, 0, 10), "10.\n     \t- z\n");
        assert!(shape(&renumbered(text, 0, 10)) != shape(text));
    }

    /// Items in one renumber are decided apart: one that reads the same
    /// keeps its indents, and one nothing helps stops nothing.
    #[test]
    fn each_item_is_decided_on_its_own() {
        let text = "10. b\n    - y\n9. a\n   - x\n";
        assert_eq!(edited(text, &[to(text, 0, 9), to(text, 2, 10)]), "9. b\n    - y\n10. a\n    - x\n");
        let text = "9. b\n   ```\n   c\n   ```\n\n- sep\n\n9. a\n   - x\n";
        assert_eq!(
            edited(text, &[to(text, 0, 10), to(text, 7, 10)]),
            "10. b\n   ```\n   c\n   ```\n\n- sep\n\n10. a\n    - x\n"
        );
    }

    /// Review 3 of #86: what Shoin reads has to stay the same in full, not
    /// just the kind of block, and a blocker elsewhere stops no nested pair.
    #[test]
    fn shoin_keeps_its_tasks_nesting_and_nested_pairs() {
        // A task's box: Shoin sees it only one space past the number.
        assert_eq!(renumbered("10. [x] done\n       - x\n", 0, 9), "9. [x] done\n      - x\n");
        // A tab-indented sub-item would stay while its sibling moved, and
        // Shoin would nest them differently: nothing moves.
        assert_eq!(renumbered("9. a\n   - x\n  \t- y\n", 0, 10), "10. a\n   - x\n  \t- y\n");
        let text = "9. a\n   1. s\n   9. t\n      - x\n\n- sep\n\n9. b\n   ```\n   c\n   ```\n";
        assert_eq!(
            edited(text, &[to(text, 0, 10), to(text, 2, 10), to(text, 7, 10)]),
            "10. a\n    1. s\n    10. t\n        - x\n\n- sep\n\n10. b\n   ```\n   c\n   ```\n"
        );
    }

    /// Nested numbers that both widen move together: the inner item with
    /// the outer one, and by its own widening too.
    #[test]
    fn nested_widenings_add_up() {
        let text = "9. a\n   1. s\n   9. t\n      - x\n";
        assert_eq!(edited(text, &[to(text, 0, 10), to(text, 2, 10)]), "10. a\n    1. s\n    10. t\n        - x\n");
    }

    #[test]
    fn the_layout_follows_commonmark() {
        let layout_of = |line: &str| {
            let digits = line.trim_start().chars().take_while(char::is_ascii_digit).count();
            let indent = line.len() - line.trim_start().len();
            layout(line, &Renumber { line: 0, cols: indent..indent + digits, number: String::new() })
        };
        assert_eq!(layout_of("9. a\n"), Some((3, Some(1))));
        assert_eq!(layout_of("  10)  a"), Some((7, Some(2))));
        assert_eq!(layout_of("9.\n"), Some((3, None)));
        assert_eq!(layout_of("9.      code"), Some((3, None)));
        assert_eq!(layout_of("9.\ta"), None);
        assert_eq!(layout_of("\t9. a"), None);
    }

    /// The claims the design rests on, over random lists at any indent with
    /// whitespace-only lines: whatever is done reads as before, or else only
    /// the number changed; and without tabs, tables, or blocks Shoin stops
    /// seeing 4 columns in, a widening of an item CommonMark and Shoin agree
    /// on always reads as before.
    #[test]
    fn random_lists_read_as_before_or_only_the_number_changes() {
        let plain = ["a", "more", "- x", "* y", "1. n", "9. m", "10. w", "> q", "<div>", "code", "-", ""];
        let more = ["```", "~~~", "$$", "# h", "***", "| t |", "|---|", "\t- z", "\tcode"];
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut rnd = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for round in 0..4_000 {
            let all = round % 2 == 1;
            let frags: Vec<&str> = if all { plain.iter().chain(&more).copied().collect() } else { plain.to_vec() };
            let mut text = " ".repeat(rnd(4));
            text.push_str(["9. a\n", "9. - x\n", "9.\n", "9.  a\n", "9. *b*\n"][rnd(5)]);
            for _ in 0..1 + rnd(6) {
                // An empty fragment makes a blank or whitespace-only line.
                text.push_str(&" ".repeat(rnd(10)));
                text.push_str(frags[rnd(frags.len())]);
                text.push('\n');
            }
            let edit = to(&text, 0, 10);
            let after = edited(&text, std::slice::from_ref(&edit));
            let alone = edited(&text.replacen("9.", "10.", 1), &[]);
            let reads = shape(&after) == shape(&text);
            assert!(reads || after == alone, "{text:?} became {after:?}");
            assert!(reads || all || !agree(&text, &edit), "{text:?} became {after:?}");
        }
    }
}
