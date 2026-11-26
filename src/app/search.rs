use super::*;
use crate::text::pattern::Pattern;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long one line's (or a multi-line pattern's whole document's)
/// highlights may take before the rest of them is left out, as Vim's
/// 'redrawtime'. What was found is kept until the text changes.
const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(50);

/// Lines whose highlights are kept per document before the cache restarts.
const CACHED_LINES: usize = 4096;

/// Scans of a multi-line pattern's highlights kept per document before the
/// cache restarts: about one per viewport, when the document is too large
/// for one scan to find all its matches in time.
const CACHED_SCANS: usize = 16;

/// Lines above a viewport that its scan starts at, so that a match begun a
/// little above the top line is still highlighted there.
const SCAN_MARGIN: usize = 8;

#[derive(Clone, Copy, Default)]
enum Offset {
    #[default]
    Start,
    End,
    Lines(isize),
}

/// The last search, shared by repeat motions and highlighting.
#[derive(Clone)]
pub struct Search {
    pub pattern: String,
    pub reverse: bool,
    compiled: Pattern,
    offset: Offset,
    matches: RefCell<HashMap<u64, DocMatches>>,
}

/// One document's matches at one revision: each drawn line's columns for a
/// line-local pattern; for a multi-line one, the scans that highlight it
/// and where `n` can stop.
#[derive(Clone, Default)]
struct DocMatches {
    revision: u64,
    lines: HashMap<usize, Vec<Range<usize>>>,
    scans: Vec<Scan>,
    stops: Option<Vec<(usize, Range<usize>)>>,
}

/// The matches of a multi-line pattern from character `start` on, as
/// highlighted: all of them up to character `covered`, where the scan ran
/// out of [`HIGHLIGHT_BUDGET`] (or the end of the document).
#[derive(Clone)]
struct Scan {
    start: usize,
    covered: usize,
    ranges: Vec<Range<usize>>,
}

impl Search {
    pub(crate) fn new(pattern: String, reverse: bool, compiled: Pattern) -> Self {
        Self { pattern, reverse, compiled, offset: Offset::Start, matches: RefCell::default() }
    }

    /// Drop what is kept for the document `id`, which has closed.
    pub(super) fn forget(&self, id: u64) {
        self.matches.borrow_mut().remove(&id);
    }

    /// How many documents have matches cached.
    #[cfg(test)]
    pub(super) fn cached_docs(&self) -> usize {
        self.matches.borrow().len()
    }

    /// `doc`'s cached matches, restarted when its text changed.
    fn doc_matches<'a>(&self, cache: &'a mut HashMap<u64, DocMatches>, doc: &BufferState) -> &'a mut DocMatches {
        let entry = cache.entry(doc.id).or_default();
        if entry.revision != doc.buffer.revision {
            *entry = DocMatches { revision: doc.buffer.revision, ..DocMatches::default() };
        }
        entry
    }

    /// Run `f` on the matches of a multi-line pattern in `doc` to highlight
    /// on `line`, as character ranges, from a scan that covers the line. A
    /// line no scan covered starts a new one a few lines above it, with its
    /// own [`HIGHLIGHT_BUDGET`], as Vim searches from the top of the window;
    /// if that runs out first, one from the line itself, which then serves
    /// it however far it got.
    fn with_whole<R>(&self, doc: &BufferState, line: usize, f: impl FnOnce(&[Range<usize>]) -> R) -> R {
        let rope = &doc.buffer.rope;
        let first = rope.line_to_char(line);
        let end = first + doc.buffer.line_len(line);
        let serves = |scan: &Scan| scan.start <= first && (end < scan.covered || scan.start == first);
        let scan = |from: usize| {
            let start = rope.line_to_char(from);
            let deadline = Some(Instant::now() + HIGHLIGHT_BUDGET);
            let (mut ranges, all) = self.compiled.char_ranges_until(&multiline_text(&doc.buffer, from), deadline);
            for range in &mut ranges {
                *range = range.start + start..range.end + start;
            }
            // Past the last match found, the rest is unknown, unless there is none.
            let covered = if all { usize::MAX } else { ranges.last().map_or(start, |range| range.end) };
            Scan { start, covered, ranges }
        };
        let mut cache = self.matches.borrow_mut();
        let entry = self.doc_matches(&mut cache, doc);
        let index = entry.scans.iter().rposition(serves).unwrap_or_else(|| {
            if entry.scans.len() >= CACHED_SCANS {
                entry.scans.clear();
            }
            let mut new = scan(line.saturating_sub(SCAN_MARGIN));
            if !serves(&new) {
                new = scan(line);
            }
            entry.scans.push(new);
            entry.scans.len() - 1
        });
        f(&entry.scans[index].ranges)
    }

    /// Run `f` on where `n` and `N` can stop for a multi-line pattern in
    /// `doc` ([`stops`]): each with the line whose search found it, as
    /// character ranges in that order. One index per revision, so each
    /// repeat is a binary search.
    fn with_stops<R>(&self, doc: &BufferState, f: impl FnOnce(&[(usize, Range<usize>)]) -> R) -> R {
        let mut cache = self.matches.borrow_mut();
        let entry = self.doc_matches(&mut cache, doc);
        f(entry.stops.get_or_insert_with(|| {
            let text = multiline_text(&doc.buffer, 0);
            let mut chars = CharCounter::new(&text);
            let (mut line_byte, mut line) = (0, 0);
            stops(&self.compiled, &text)
                .map(|(start, span)| {
                    line += text.as_bytes()[line_byte..start].iter().filter(|&&b| b == b'\n').count();
                    line_byte = start;
                    (line, chars.at(span.start)..chars.at(span.end))
                })
                .collect()
        }))
    }
}

/// Where Vim's `searchit` can stop in `text`: on each line, the first match
/// from the line's start, then each one from the end of the one before (a
/// character on after an empty one), as Neovim's default 'cpoptions' flag
/// `c` has it, until that is the line's end or a match took a line break.
/// Each comes with the byte its line starts at, the line whose search found
/// it: `\zs` can put it on a later line, which still has its own search.
/// Byte ranges of the spans `\zs`/`\ze` report, found as they are asked
/// for: `n` usually needs one or two of a line's.
fn stops<'a>(pattern: &'a Pattern, text: &'a str) -> impl Iterator<Item = (usize, Range<usize>)> + 'a {
    let mut matches = pattern.matches(text, false);
    let mut at = 0;
    // The line the last match began on, looked up once per line, so a long
    // line is not scanned again for each of its matches.
    let mut line: Option<Range<usize>> = None;
    std::iter::from_fn(move || {
        if at > text.len() {
            return None;
        }
        matches.seek(at);
        let found = matches.next()?;
        let span = found.span;
        if found.whole.start == text.len() && text.ends_with('\n') {
            return None; // Vim never searches a line after the last one.
        }
        let line = match &line {
            Some(line) if found.whole.start <= line.end => line.clone(),
            _ => {
                let start = text[..found.whole.start].rfind('\n').map_or(0, |i| i + 1);
                let end = text[found.whole.start..].find('\n').map_or(text.len(), |i| found.whole.start + i);
                line.insert(start..end).clone()
            }
        };
        if span.start == text.len() && text.ends_with('\n') {
            // After the last line's break, where no line is: Vim puts the
            // cursor at the end of the last line.
            at = text.len() + 1;
            return Some((line.start, text.len() - 1..text.len() - 1));
        }
        at = span.end;
        if span.is_empty() && at < line.end {
            at += text[at..].chars().next().map_or(1, char::len_utf8);
        }
        if at >= line.end || text[span.clone()].contains('\n') {
            // The next line starts afresh, from its start even when this
            // match took some of it.
            at = line.end + 1;
        }
        Some((line.start, span))
    })
}

/// Turns byte offsets in a text into character offsets, counting from the
/// last one asked for.
struct CharCounter<'a> {
    text: &'a str,
    byte: usize,
    char: usize,
}

impl<'a> CharCounter<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, byte: 0, char: 0 }
    }

    fn at(&mut self, byte: usize) -> usize {
        if byte >= self.byte {
            self.char += self.text[self.byte..byte].chars().count();
        } else {
            self.char -= self.text[byte..self.byte].chars().count();
        }
        self.byte = byte;
        self.char
    }
}

/// The char index where `line`'s text ends and its break begins.
fn line_end(buffer: &crate::text::buffer::Buffer, line: usize) -> usize {
    buffer.rope.line_to_char(line) + buffer.line_len(line)
}

/// Whether `range` (char indices) takes `line`'s line break: some of the
/// chars after its text and before the next line, or the one Vim matches
/// after the last line when the file ends without one.
pub(super) fn takes_line_break(buffer: &crate::text::buffer::Buffer, line: usize, range: &Range<usize>) -> bool {
    let end = line_end(buffer, line);
    let next = if line + 1 < buffer.line_count() { buffer.rope.line_to_char(line + 1) } else { end + 1 };
    range.start < next.max(end + 1) && range.end > end
}

/// The text a multi-line pattern matches from line `first` on: through the
/// last line's break, which Vim matches even when the file has none.
pub(super) fn multiline_text(buffer: &crate::text::buffer::Buffer, first: usize) -> String {
    let mut text = buffer.rope.slice(buffer.rope.line_to_char(first)..).to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

pub(super) fn searchable_line(rope: &ropey::Rope, line: usize) -> Cow<'_, str> {
    searchable(rope.line(line))
}

/// A line's text without its break, borrowed when the rope holds it in one piece.
pub(super) fn searchable(mut slice: ropey::RopeSlice<'_>) -> Cow<'_, str> {
    let len = slice.len_chars();
    if len > 0 && slice.char(len - 1) == '\n' {
        slice = slice.slice(..len - 1);
    }
    slice.as_str().map_or_else(|| Cow::Owned(slice.to_string()), Cow::Borrowed)
}

/// Read a delimited pattern as Vim's `skip_regexp` does: a separator inside
/// a `[]` collection does not end it, and a `[` with no `]` takes the rest of
/// the text. Only `\v` and `\V` change whether `[` opens one, as in Vim.
pub(super) fn pattern_field(text: &str, separator: char, unescape: bool) -> (String, Option<&str>) {
    let mut out = String::new();
    let mut magic = true;
    let mut i = 0;
    while let Some(ch) = text[i..].chars().next() {
        if ch == separator {
            return (out, Some(&text[i + ch.len_utf8()..]));
        }
        let open = if magic && ch == '[' {
            1
        } else if !magic && text[i..].starts_with("\\[") {
            2
        } else {
            0
        };
        if open > 0 {
            let Some(close) = collection_end(&text[i + open..]) else {
                out.push_str(&text[i..]);
                return (out, None);
            };
            let end = i + open + close + 1;
            out.push_str(&text[i..end]);
            i = end;
            continue;
        }
        if ch == '\\' {
            let Some(next) = text[i + 1..].chars().next() else {
                out.push(ch);
                break;
            };
            if !unescape || next != separator {
                out.push(ch);
            }
            out.push(next);
            match next {
                'v' => magic = true,
                'V' => magic = false,
                _ => {}
            }
            i += 1 + next.len_utf8();
        } else {
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    (out, None)
}

/// Where the `]` that closes a collection is in `text` (which follows the
/// `[`), as Vim's `skip_anyof`; `None` when there is none.
fn collection_end(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let char_len = |at: usize| text[at..].chars().next().map_or(1, char::len_utf8);
    let mut at = usize::from(bytes.first() == Some(&b'^'));
    if matches!(bytes.get(at), Some(b']' | b'-')) {
        at += 1;
    }
    while at < bytes.len() && bytes[at] != b']' {
        match bytes[at] {
            b'-' => {
                at += 1;
                if at < bytes.len() && bytes[at] != b']' {
                    at += char_len(at);
                }
            }
            b'\\' if bytes.get(at + 1).is_some_and(|b| b"]^-n\\rtebdoxuU".contains(b)) => at += 2,
            b'[' => at += class_len(&text[at..]).unwrap_or(1),
            _ => at += char_len(at),
        }
    }
    (at < bytes.len()).then_some(at)
}

/// The length of a `[:name:]`, `[=x=]` or `[.x.]` at the start of `text`.
/// Only the name and what ends it are read, so a pattern of many openers
/// is read in linear time.
fn class_len(text: &str) -> Option<usize> {
    if let Some(name) = text.strip_prefix("[:") {
        let len = name.bytes().take_while(u8::is_ascii_lowercase).count();
        return name[len..].starts_with(":]").then_some(len + 4);
    }
    let mark = text[1..].chars().next().filter(|&ch| ch == '=' || ch == '.')?;
    let ch = text[2..].chars().next()?;
    let tail = &text[2 + ch.len_utf8()..];
    (tail.starts_with(mark) && tail[1..].starts_with(']')).then_some(2 + ch.len_utf8() + 2)
}

/// Read a delimited Vim field, retaining regex/replacement escapes.
pub(super) fn field(text: &str, separator: char, unescape: bool) -> (String, Option<&str>) {
    let mut out = String::new();
    let mut chars = text.char_indices();
    while let Some((idx, ch)) = chars.next() {
        if ch == separator {
            return (out, Some(&text[idx + ch.len_utf8()..]));
        }
        if ch == '\\' {
            let Some((_, next)) = chars.next() else {
                out.push(ch);
                break;
            };
            if !unescape || next != separator {
                out.push(ch);
            }
            out.push(next);
        } else {
            out.push(ch);
        }
    }
    (out, None)
}

impl App {
    pub(super) fn on_key_search(&mut self, key: KeyEvent, mut query: String, reverse: bool) {
        match field_key(key, &mut query) {
            FieldKey::Cancel => {
                self.mode = Mode::Normal;
                self.flash = None;
            }
            FieldKey::Submit => {
                self.mode = Mode::Normal;
                let (pattern, suffix) = pattern_field(&query, if reverse { '?' } else { '/' }, reverse);
                let offset = match suffix.unwrap_or("") {
                    "" => Offset::Start,
                    "e" => Offset::End,
                    "+" => Offset::Lines(1),
                    "-" => Offset::Lines(-1),
                    n => match n.parse::<isize>() {
                        Ok(n) => Offset::Lines(n),
                        Err(_) => return self.notify("search offset not supported", FlashKind::Error),
                    },
                };
                if pattern.is_empty() {
                    let Some(previous) = &mut self.search else {
                        return self.notify("no previous search pattern", FlashKind::Error);
                    };
                    previous.reverse = reverse;
                    if suffix.is_some() {
                        previous.offset = offset;
                    }
                } else if self.set_search(pattern, reverse) {
                    self.search.as_mut().expect("search just set").offset = offset;
                } else {
                    return;
                }
                self.search_move(reverse);
            }
            FieldKey::Stay => self.mode = Mode::Search { query, reverse },
        }
    }

    pub(super) fn set_search(&mut self, pattern: String, reverse: bool) -> bool {
        match Pattern::compile(&pattern, false, self.previous_replacement()) {
            Ok(compiled) => {
                self.search = Some(Search::new(pattern, reverse, compiled));
                true
            }
            Err(err) => {
                self.notify(err, FlashKind::Error);
                false
            }
        }
    }

    /// A substitution replaces the last pattern and keeps the search
    /// direction and offset that `n` was already using.
    pub(super) fn set_substitute_pattern(&mut self, pattern: String, compiled: Pattern) {
        if let Some(search) = &mut self.search {
            search.pattern = pattern;
            search.compiled = compiled;
            search.matches.get_mut().clear();
        } else {
            self.search = Some(Search::new(pattern, false, compiled));
        }
    }

    /// `n` (or `N` as `backward`): the next match from the cursor, as
    /// Vim's `searchit`: past the cursor (a match at a line's end counts as
    /// on its last character), then the first on each line after, wrapping.
    pub(super) fn search_move(&mut self, backward: bool) {
        let Some(search) = &self.search else {
            return self.notify("no previous regular expression", FlashKind::Error);
        };
        let rope = &self.buffer.rope;
        let last = self.buffer.last_line();
        let cur = self.buffer.cursor;
        let line_offset = matches!(search.offset, Offset::Lines(_));
        // Where a match puts the cursor: its start, or its last character for `/e`.
        let pos = |m: &Range<usize>| match search.offset {
            Offset::End if !m.is_empty() => m.end - 1,
            _ => m.start,
        };
        let found = if search.compiled.multiline {
            let next_line = rope.line_to_char((cur.line + 1).min(rope.len_lines()));
            let at_end = |idx: usize| idx >= rope.len_chars() || rope.char(idx) == '\n';
            search.with_stops(self, |all| {
                // A line's stops: its matches in order, and last perhaps one
                // that `\zs` put on a later line.
                let first = all.partition_point(|(line, _)| *line < cur.line);
                let after = all.partition_point(|(line, _)| *line <= cur.line);
                let stop = if backward {
                    // On each line from the cursor's back, the last of its
                    // stops before the cursor (before its line, with a line
                    // offset); else, wrapping, the last one.
                    let before = if line_offset { rope.line_to_char(cur.line) } else { self.buffer.char_index(cur) };
                    let mut end = after;
                    loop {
                        let Some(&(line, _)) = all[..end].last() else { break all.last() };
                        let start = all[..end].partition_point(|(l, _)| *l < line);
                        let group = &all[start..end];
                        match group.partition_point(|(_, m)| pos(m) < before) {
                            0 => end = start,
                            i => break group.get(i - 1),
                        }
                    }
                } else {
                    // The first past the cursor on its line (a match at the
                    // line's end counting as on its last character; one put
                    // on a later line is past, and with a line offset only
                    // that one is); else the first a later line found; else,
                    // wrapping, the first one.
                    let absolute = self.buffer.char_index(cur);
                    let passed = |m: &Range<usize>| {
                        let p = pos(m);
                        m.start < next_line
                            && (line_offset || p as isize - isize::from(at_end(p)) <= absolute as isize)
                    };
                    let here = &all[first..after];
                    here.get(here.partition_point(|(_, m)| passed(m))).or(all.get(after)).or(all.first())
                };
                stop.map(|(_, m)| pos(m))
            })
        } else {
            // Each line's stops as cursor columns, tried one at a time.
            let line_stops = |line, f: &mut dyn FnMut(usize) -> bool| -> Option<usize> {
                let text = searchable_line(rope, line);
                let mut chars = CharCounter::new(&text);
                let mut last = None;
                for (_, span) in stops(&search.compiled, &text) {
                    let col = pos(&(chars.at(span.start)..chars.at(span.end)));
                    if !f(col) {
                        break;
                    }
                    last = Some(col);
                }
                last
            };
            // The first stop `wanted` takes, or (backward) the last one
            // before `wanted` refuses.
            let first = |line, wanted: &dyn Fn(usize) -> bool| {
                let mut found = None;
                line_stops(line, &mut |col| {
                    found = wanted(col).then_some(col);
                    found.is_none()
                });
                found
            };
            let last_before = |line, before: usize| line_stops(line, &mut |col| col < before);
            let here = cur.line.min(last);
            let len = self.buffer.line_len(here) as isize;
            let on_cursor_line = if line_offset {
                None
            } else if backward {
                last_before(here, cur.col)
            } else {
                first(here, &|col| col as isize - isize::from(col as isize >= len) > cur.col as isize)
            };
            on_cursor_line.map(|col| (here, col)).or_else(|| {
                if backward {
                    (0..here).rev().chain((here..=last).rev())
                        .find_map(|line| last_before(line, usize::MAX).map(|col| (line, col)))
                } else {
                    (here + 1..=last).chain(0..=here)
                        .find_map(|line| first(line, &|_| true).map(|col| (line, col)))
                }
            }).map(|(line, col)| rope.line_to_char(line) + col)
        };
        let pattern = &search.pattern;
        match found {
            Some(idx) => {
                let mut target = self.idx_to_cursor(idx);
                // At a line's end: on its last character, as Normal mode,
                // which is also the goal `j` and `k` keep.
                target.set_col(target.col.min(self.buffer.line_len(target.line).saturating_sub(1)));
                if let Offset::Lines(delta) = search.offset {
                    target = Cursor::new(target.line.saturating_add_signed(delta).min(last), 0);
                }
                let sigil = if backward { "?" } else { "/" };
                let message = format!("{sigil}{pattern}");
                self.buffer.cursor = target;
                self.forget_reading();
                self.notify(message, FlashKind::Info);
            }
            None => {
                let message = format!("pattern not found: {pattern}");
                // The goal is set where the cursor is even so, as Neovim's
                // `normal_search`: `$` then a failed `n` forgets the line end.
                let col = self.buffer.cursor.col;
                self.buffer.cursor.set_col(col);
                self.notify(message, FlashKind::Error);
            }
        }
    }

    pub(crate) fn search_highlights(&self, doc: &BufferState, line: usize, text: &str) -> Vec<Range<usize>> {
        let Some(search) = &self.search else { return Vec::new() };
        if !search.compiled.multiline {
            let mut cache = search.matches.borrow_mut();
            let entry = search.doc_matches(&mut cache, doc);
            if entry.lines.len() >= CACHED_LINES && !entry.lines.contains_key(&line) {
                entry.lines.clear();
            }
            return entry.lines.entry(line)
                .or_insert_with(|| search.compiled.char_ranges(text, Some(Instant::now() + HIGHLIGHT_BUDGET)))
                .clone();
        }
        let rope = &doc.buffer.rope;
        let first = rope.line_to_char(line);
        let end = first + doc.buffer.line_len(line);
        search.with_whole(doc, line, |matches| {
            matches[matches.partition_point(|m| m.end <= first)..].iter()
                .take_while(|m| m.start < end)
                .map(|m| m.start.saturating_sub(first)..m.end.min(end) - first)
                .collect()
        })
    }

    /// Whether a search match on `line` takes its line break, drawn as one
    /// highlighted cell after the line's text, as Neovim draws it. Only a
    /// multi-line pattern can match a break; the last line's counts even in
    /// a file without one ([`multiline_text`]).
    pub(crate) fn search_takes_line_break(&self, doc: &BufferState, line: usize) -> bool {
        let Some(search) = self.search.as_ref().filter(|s| s.compiled.multiline) else { return false };
        search.with_whole(doc, line, |matches| {
            let at = matches.partition_point(|m| m.end <= line_end(&doc.buffer, line));
            matches.get(at).is_some_and(|m| takes_line_break(&doc.buffer, line, m))
        })
    }
}
