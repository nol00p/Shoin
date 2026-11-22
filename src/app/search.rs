use super::*;
use crate::text::pattern::Pattern;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

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
    multiline_highlights: RefCell<HighlightCache>,
}

type HighlightCache = HashMap<u64, (Option<u64>, Vec<Range<usize>>)>;

impl Search {
    pub(crate) fn new(pattern: String, reverse: bool, compiled: Pattern) -> Self {
        Self { pattern, reverse, compiled, offset: Offset::Start, multiline_highlights: RefCell::new(HashMap::new()) }
    }
}

pub(super) fn searchable_line(rope: &ropey::Rope, line: usize) -> Cow<'_, str> {
    let mut slice = rope.line(line);
    let len = slice.len_chars();
    if len > 0 && slice.char(len - 1) == '\n' {
        let cr = usize::from(len > 1 && slice.char(len - 2) == '\r');
        slice = slice.slice(..len - 1 - cr);
    }
    slice.as_str().map_or_else(|| Cow::Owned(slice.to_string()), Cow::Borrowed)
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
                let (pattern, suffix) = field(&query, if reverse { '?' } else { '/' }, reverse);
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
            search.multiline_highlights.get_mut().clear();
        } else {
            self.search = Some(Search::new(pattern, false, compiled));
        }
    }

    pub(super) fn search_move(&mut self, backward: bool) {
        let Some(search) = &self.search else { return };
        let rope = &self.buffer.rope;
        let last = self.buffer.last_line();
        let cur = self.buffer.cursor;
        let absolute = match search.offset {
            Offset::Lines(_) if backward => rope.line_to_char(cur.line),
            Offset::Lines(_) => rope.line_to_char((cur.line + 1).min(rope.len_lines())),
            _ => self.buffer.char_index(cur),
        };
        let positions = |text: &str, col: usize, current: bool| {
            let at = text.char_indices().nth(col).map_or(text.len(), |(byte, _)| byte);
            let end_offset = matches!(search.offset, Offset::End);
            let line_offset = matches!(search.offset, Offset::Lines(_));
            if current && line_offset && !search.compiled.multiline {
                return None;
            }
            let mut found = None;
            for bounds in search.compiled.captures(text).map(|m| search.compiled.bounds(&m)) {
                let mut pos = if end_offset {
                    bounds.end - text[bounds.clone()].chars().next_back().map_or(0, char::len_utf8)
                } else {
                    bounds.start
                };
                if pos == text.len() && !search.compiled.multiline {
                    pos -= text.chars().next_back().map_or(0, char::len_utf8);
                }
                if current && (if backward { pos >= at } else if line_offset { pos < at } else { pos <= at }) {
                    if backward {
                        break;
                    }
                    continue;
                }
                found = Some(pos);
                if !backward {
                    break;
                }
            }
            found.map(|byte| text[..byte].chars().count())
        };
        let found = if search.compiled.multiline {
            let text = rope.slice(..rope.line_to_char(last) + self.buffer.line_len(last)).to_string();
            positions(&text, absolute, true).or_else(|| positions(&text, 0, false))
        } else {
            let on_line = |line, current| {
                positions(&searchable_line(rope, line), cur.col, current)
                    .map(|col| rope.line_to_char(line) + col)
            };
            on_line(cur.line.min(last), true).or_else(|| {
                if backward {
                    (0..cur.line.min(last)).rev().chain((cur.line.min(last)..=last).rev())
                        .find_map(|line| on_line(line, false))
                } else {
                    (cur.line.min(last) + 1..=last).chain(0..=cur.line.min(last))
                        .find_map(|line| on_line(line, false))
                }
            })
        };
        let pattern = &search.pattern;
        match found {
            Some(idx) => {
                let mut target = self.idx_to_cursor(idx);
                if let Offset::Lines(delta) = search.offset {
                    target = Cursor::new(target.line.saturating_add_signed(delta).min(last), 0);
                }
                let sigil = if backward { "?" } else { "/" };
                let message = format!("{sigil}{pattern}");
                self.buffer.cursor = target;
                self.forget_reading();
                self.notify(message, FlashKind::Info);
            }
            None => self.notify(format!("pattern not found: {pattern}"), FlashKind::Error),
        }
    }

    pub(crate) fn search_highlights(&self, doc: &BufferState, line: usize, text: &str) -> Vec<Range<usize>> {
        let Some(search) = &self.search else { return Vec::new() };
        if !search.compiled.multiline {
            return search.compiled.char_ranges(text).collect();
        }
        let mut cache = search.multiline_highlights.borrow_mut();
        let rope = &doc.buffer.rope;
        let (revision, matches) = cache.entry(doc.id).or_default();
        if *revision != Some(doc.buffer.revision) {
            let last = doc.buffer.last_line();
            let text = rope.slice(..rope.line_to_char(last) + doc.buffer.line_len(last)).to_string();
            *matches = search.compiled.char_ranges(&text).collect();
            *revision = Some(doc.buffer.revision);
        }
        let first = rope.line_to_char(line);
        let end = first + doc.buffer.line_len(line);
        matches[matches.partition_point(|m| m.end <= first)..].iter()
            .take_while(|m| m.start < end)
            .map(|m| m.start.saturating_sub(first)..m.end.min(end) - first)
            .collect()
    }
}
