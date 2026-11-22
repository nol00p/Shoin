use super::*;
use crate::text::pattern::{Matches, Pattern};
use range::{Range as LineRange, UndoEntry};

#[derive(Clone, Copy, Default)]
struct Flags {
    global: bool,
    confirm: bool,
    count: bool,
    quiet: bool,
    ignore_case: bool,
}

#[derive(Clone)]
pub(super) struct Substitute {
    pattern: String,
    replacement: String,
    flags: Flags,
}

struct Edit {
    range: Range<usize>,
    replacement: String,
    line: usize,
    end_line: usize,
}

pub(super) struct Confirmation {
    doc: DotMark,
    replacement: String,
    matches: Vec<Edit>,
    change: Changes,
    next: usize,
    /// The cursor the command started from, whose goal is left as it was
    /// unless `a` is pressed, as Neovim's.
    start: Cursor,
    /// The command reused the previous pattern after `$`: see
    /// [`App::land_after_substitute`].
    end_column: bool,
}

#[derive(Default)]
struct Changes {
    entries: Vec<UndoEntry>,
    count: usize,
    char_delta: isize,
    /// Lines added (or, negative, joined) by the edits applied so far.
    line_delta: isize,
    edited: bool,
}

impl Changes {
    fn range(&self, edit: &Edit) -> Range<usize> {
        edit.range.start.saturating_add_signed(self.char_delta)..edit.range.end.saturating_add_signed(self.char_delta)
    }

    /// One entry per run of lines the edits touch: an edit that starts on a
    /// line the previous one reached (joined into it) extends its entry. Vim
    /// counts the lines of its message the same way.
    fn record(&mut self, edit: &Edit, added: usize) {
        self.count += 1;
        let joined = edit.end_line - edit.line;
        match self.entries.last_mut() {
            Some(entry) if edit.line < entry.top + entry.before => {
                let before = entry.before.max(edit.end_line - entry.top + 1);
                entry.after = (entry.after + added + before).saturating_sub(entry.before + joined);
                entry.before = before;
            }
            _ => self.entries.push(UndoEntry { top: edit.line, before: joined + 1, after: added + 1, diff: 0 }),
        }
    }
}

pub(super) fn command(rest: &str) -> Option<(&str, bool)> {
    if let Some(flags) = rest.strip_prefix('&') {
        return Some((flags, true));
    }
    let end = rest.find(|ch: char| !ch.is_ascii_alphabetic()).unwrap_or(rest.len());
    matches!(&rest[..end], "s" | "su" | "sub" | "subs" | "subst" | "substi" | "substit" | "substitu" | "substitut" | "substitute")
        .then_some((&rest[end..], false))
}

impl App {
    pub(super) fn previous_replacement(&self) -> Option<&str> {
        self.substitute.as_ref().map(|s| s.replacement.as_str())
    }

    pub(super) fn repeat_substitute(&mut self, all: bool) {
        if all {
            if let Some(anchor) = self.visual_anchor() {
                self.command_selection = Some((anchor.line, self.buffer.cursor.line));
                self.end_recording();
                self.run_command("'<,'>s//~/&");
                self.command_selection = None;
            } else {
                self.run_command("%s//~/&");
            }
        } else {
            self.run_command("&");
        }
    }

    pub(super) fn run_substitute(&mut self, range: Option<LineRange>, args: &str, repeat: bool) {
        if let Err(error) = self.prepare_substitute(range, args, repeat) {
            self.notify(error, FlashKind::Error);
        }
    }

    fn prepare_substitute(&mut self, range: Option<LineRange>, args: &str, repeat: bool) -> Result<(), String> {
        let previous = self.substitute.as_ref();
        let args = args.trim_start();
        let separator = args.chars().next().filter(|ch| ch.is_ascii_punctuation() && !matches!(ch, '\\' | '"' | '|'));
        let mut reused = false;
        let (pattern, replacement, flags) = if let Some(separator) = separator.filter(|_| !repeat) {
            let (pattern, rest) = search::pattern_field(&args[1..], separator, false);
            let (replacement, flags) = rest.map_or_else(|| (String::new(), None), |rest| search::field(rest, separator, false));
            let pattern = if pattern.is_empty() {
                self.search.as_ref().ok_or("no previous search pattern")?.pattern.clone()
            } else {
                pattern
            };
            (pattern, expand_tilde(&replacement, self.previous_replacement().unwrap_or("")), flags.unwrap_or(""))
        } else {
            reused = true;
            let previous = previous.ok_or("no previous substitute")?;
            (previous.pattern.clone(), previous.replacement.clone(), args)
        };
        if replacement.starts_with(r"\=") {
            return Err("replacement: \\= not supported".into());
        }
        let (flags, count) = parse_flags(flags, previous.map_or_else(Flags::default, |s| s.flags))?;
        if !flags.count && self.refuse_read_only() {
            return Ok(());
        }
        // Vim's `endcolumn`: only a command that reuses the pattern (`&`,
        // `:&&`, a bare `:s`) keeps `$`'s line end.
        let end_column = reused && self.buffer.cursor.goal_col == Cursor::LINE_END;
        let compiled = Pattern::compile(&pattern, flags.ignore_case, self.previous_replacement())?;
        let Some((first, last)) = self.range_span(range, count) else { return Ok(()) };
        let searched = if flags.ignore_case {
            Pattern::compile(&pattern, false, self.previous_replacement())?
        } else {
            compiled.clone()
        };
        self.set_substitute_pattern(pattern.clone(), searched);
        self.substitute = Some(Substitute { pattern: pattern.clone(), replacement: replacement.clone(), flags });
        self.flash = None;

        // One pass in order over the range: running byte, character and
        // line positions, never a seek into the rope per match.
        let rope = &self.buffer.rope;
        let pieces = parse_replacement(&replacement);
        let last_line = self.buffer.last_line();
        let content_end = rope.line_to_char(last_line) + self.buffer.line_len(last_line);
        let mut edits = Vec::new();
        // The edits after which the search went back to an earlier line.
        let mut rewound = Vec::new();
        let (mut matches, mut lines) = (0, 0);
        let mut last_match_line = None;
        // Something matched, if only after the last line: no error then, as Vim.
        let mut got_match = false;
        let mut collect = |text: &str, base: usize, line: usize, limit: usize| {
            let mut found = compiled.matches(text, true);
            found.one_per_line = !flags.global;
            let mut at = Position { byte: 0, char: base, line };
            while let Some(m) = found.next() {
                // Vim takes a match whose start is in the range, even when
                // `\zs` moves what it replaces past the range's end.
                at.advance(text, m.whole.start);
                if at.char > limit {
                    break;
                }
                let whole_line = at.line;
                at.advance(text, m.span.start);
                got_match = true;
                let (start, line) = (at.char, at.line);
                // `\n\zs` can put it after the last line's break: no line there.
                if line > last_line {
                    break;
                }
                matches += 1;
                if last_match_line != Some(line) {
                    lines += 1;
                    last_match_line = Some(line);
                }
                at.advance(text, m.span.end);
                // Without `g`, one match a line, except that one taking a
                // line break searches the rest joined to it again, while the
                // next line is in the range (Vim's `do_again`). Counting
                // joins nothing: the next line is searched from its start.
                let joins = at.line > line;
                let again = joins && at.line <= last;
                if flags.count && joins {
                    found.next_line(m.span.start);
                } else if !(flags.global || again) {
                    // After the lines it joined; else from the line the match
                    // began on: one that `\zs` moved into the next line leaves
                    // that line its own search.
                    found.next_line(if joins { m.span.end } else { m.whole.start });
                    if !joins && whole_line < line && !flags.count {
                        rewound.push(edits.len());
                    }
                }
                if !flags.count {
                    // A match through the last line's break (which the file
                    // may not have) replaces up to the end of that line.
                    let end = at.char.min(content_end.max(start));
                    let mut out = String::new();
                    expand(&pieces, &text[m.span.clone()], &found, &mut out);
                    edits.push(Edit { range: start..end, line, end_line: at.line.min(last_line), replacement: out });
                }
            }
        };
        if compiled.multiline {
            let limit = rope.line_to_char(last) + self.buffer.line_len(last);
            collect(&search::multiline_text(&self.buffer, first), rope.line_to_char(first), first, limit);
        } else {
            let mut base = rope.line_to_char(first);
            for (line, slice) in (first..=last).zip(rope.lines_at(first)) {
                collect(&search::searchable(slice), base, line, usize::MAX);
                base += slice.len_chars();
            }
        }
        in_source_order(&mut edits, &rewound);
        if matches == 0 {
            if !flags.quiet && !got_match {
                return Err(format!("pattern not found: {pattern}"));
            }
        } else if flags.count {
            self.notify(summary(matches, lines, "match", "matches"), FlashKind::Info);
        } else if !self.buffer.is_editable(&(edits[0].range.start..edits[edits.len() - 1].range.end)) {
            return Err("substitution crosses read-only text".into());
        } else if flags.confirm {
            let start = self.buffer.cursor;
            self.confirmation = Some(Confirmation {
                doc: self.dot_mark(), replacement, matches: edits,
                change: Changes::default(), next: 0, start, end_column,
            });
            self.mode = Mode::SubstituteConfirm;
            self.buffer.history.begin_group(Some(Cursor::new(start.line, start.col)));
            self.show_substitute_confirmation();
        } else {
            let mut change = Changes::default();
            self.buffer.history.begin_group(None);
            self.apply_substitutions(&edits, &mut change);
            self.land_after_substitute(end_column);
            self.finish_substitutions(change);
        }
        Ok(())
    }

    fn show_substitute_confirmation(&mut self) {
        let Some(pending) = &self.confirmation else { return };
        if let Some(edit) = pending.matches.get(pending.next) {
            self.buffer.cursor = self.idx_to_cursor(pending.change.range(edit).start);
            self.forget_reading();
        }
    }

    pub(crate) fn substitute_prompt(&self) -> Option<String> {
        let pending = self.confirmation.as_ref()?;
        Some(format!("replace with {} (y/n/a/q/l/^E/^Y)?", pending.replacement))
    }

    pub(crate) fn substitute_highlight(&self, line: usize) -> Option<Range<usize>> {
        let pending = self.confirmation.as_ref()?;
        if pending.doc != self.dot_mark() {
            return None;
        }
        let edit = pending.matches.get(pending.next)?;
        let range = pending.change.range(edit);
        let rope = &self.buffer.rope;
        let base = rope.line_to_char(line);
        let end = base + self.buffer.line_len(line);
        if range.is_empty() && rope.char_to_line(range.start) == line {
            let at = range.start.min(end.saturating_sub(1)).saturating_sub(base);
            return Some(at..(at + 1).min(end - base));
        }
        let start = range.start.max(base);
        let stop = range.end.min(end);
        (start < stop).then(|| start - base..stop - base)
    }

    pub(super) fn on_key_substitute(&mut self, key: KeyEvent) {
        let Some(mut pending) = self.confirmation.take() else {
            self.mode = Mode::Normal;
            return;
        };
        if pending.doc != self.dot_mark() {
            self.mode = Mode::Normal;
            self.buffer.history.end_group();
            return self.notify("buffer changed during substitution", FlashKind::Error);
        }
        let cmd = if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() { key.code } else { KeyCode::Null };
        let done = match cmd {
            KeyCode::Char('y' | 'l' | 'a') => {
                let end = if cmd == KeyCode::Char('a') { pending.matches.len() } else { pending.next + 1 };
                self.apply_substitutions(&pending.matches[pending.next..end], &mut pending.change);
                pending.doc = self.dot_mark();
                pending.next = end;
                cmd == KeyCode::Char('l')
            }
            KeyCode::Char('n') => {
                pending.next += 1;
                false
            }
            KeyCode::Char('q') | KeyCode::Esc => true,
            _ => false,
        };
        if done || pending.next == pending.matches.len() {
            self.mode = Mode::Normal;
            if cmd == KeyCode::Char('a') {
                self.land_after_substitute(pending.end_column);
            } else {
                if matches!(cmd, KeyCode::Char('y' | 'l')) {
                    self.buffer.cursor = Cursor::new(self.buffer.cursor.line, 0);
                }
                // While asking, Neovim moves the cursor without setting the
                // goal: the one the command started with still holds.
                self.buffer.cursor.goal_col = pending.start.goal_col;
                self.buffer.cursor.goal_cells = pending.start.goal_cells;
            }
            self.finish_substitutions(pending.change);
        } else {
            self.confirmation = Some(pending);
            self.show_substitute_confirmation();
        }
    }

    /// Build one replacement span for this batch. Confirmation uses a one-match
    /// batch for `y` or `l`, then one batch for everything selected by `a`.
    fn apply_substitutions(&mut self, edits: &[Edit], change: &mut Changes) {
        let (Some(first), Some(last)) = (edits.first(), edits.last()) else { return };
        debug_assert!(edits.windows(2).all(|pair| pair[0].range.end <= pair[1].range.start), "edits out of order");
        let begin = change.range(first).start;
        let end = change.range(last).end;
        let mut text = String::new();
        let mut destination = 0;
        // The unchanged text between matches, read in order from the rope's chunks.
        let mut source = RopeReader::new(self.buffer.rope.slice(begin..end), begin);
        for edit in edits {
            let range = change.range(edit);
            source.read_to(range.start, Some(&mut text));
            source.read_to(range.end, None);
            text.push_str(&edit.replacement);
            let added = edit.replacement.bytes().filter(|&b| b == b'\n').count();
            destination = edit.line.saturating_add_signed(change.line_delta) + added;
            change.line_delta += added as isize - (edit.end_line - edit.line) as isize;
            change.record(edit, added);
        }
        // A blank last line needs a terminator to be a line in the rope,
        // even when the file was opened without a final newline.
        let rope = &self.buffer.rope;
        if end == rope.len_chars()
            && (text.ends_with('\n') || (text.is_empty() && begin > 0 && rope.char(begin - 1) == '\n'))
        {
            text.push('\n');
        }
        change.edited |= begin != end || !text.is_empty();
        change.char_delta += text.chars().count() as isize - (end - begin) as isize;
        self.buffer.delete_chars(begin, end);
        if !text.is_empty() {
            let at = self.idx_to_cursor(begin);
            self.buffer.insert_str(at, &text);
        }
        self.buffer.cursor = self.first_non_blank_on(destination.min(self.buffer.last_line()));
    }

    /// Where a substitute that did not ask leaves the cursor: on the first
    /// non-blank of the last line it changed (`apply_substitutions`), or,
    /// for a repeat after `$` (`end_column`), on that line's last character
    /// with `$`'s goal kept, as Neovim's `do_sub`.
    fn land_after_substitute(&mut self, end_column: bool) {
        if end_column {
            let line = self.buffer.cursor.line;
            let col = self.buffer.line_len(line).saturating_sub(1);
            self.buffer.cursor = Cursor { goal_col: Cursor::LINE_END, goal_cells: Some(Cursor::LINE_END), ..Cursor::new(line, col) };
            self.goal_kept = true;
        }
    }

    fn finish_substitutions(&mut self, mut change: Changes) {
        if change.edited {
            // Vim saves the undo position on the first line actually changed,
            // at column zero, even when the range began earlier.
            let undo = Cursor::new(change.entries[0].top, 0);
            change.entries.reverse();
            let redo = self.undo_cursor(&change.entries, undo, 0, false);
            self.buffer.history.set_undo_cursor(undo);
            self.buffer.history.set_redo_cursor(redo);
        }
        if change.count > 2 && change.entries.len() > 1 {
            self.notify(summary(change.count, change.entries.len(), "substitution", "substitutions"), FlashKind::Info);
        }
        self.buffer.history.end_group();
        self.end_recording();
        self.forget_reading();
    }
}

/// Put edits in source order, which the batch reads forward. They come out
/// of order only after `\zs` put a match on a later line than it began
/// (`rewound` holds those edits' indices): the next line then has its own
/// search, which can find edits before it. Vim searches that line as
/// already changed, so a later match that reaches into such an edit, or
/// starts on text it inserted, would have read its replacement: the first
/// found stays. An empty one at the same place goes before it, as there.
fn in_source_order(edits: &mut Vec<Edit>, rewound: &[usize]) {
    if rewound.is_empty() {
        return;
    }
    let mut found: Vec<(usize, Edit)> = edits.drain(..).enumerate().collect();
    found.sort_by_key(|(order, edit)| (edit.range.start, edit.range.end, std::cmp::Reverse(*order)));
    let mut kept: Vec<(usize, Edit)> = Vec::with_capacity(found.len());
    for (order, edit) in found {
        match kept.last_mut() {
            Some((last_order, last)) if edit.range.start < last.range.end => {
                if order < *last_order {
                    (*last_order, *last) = (order, edit);
                }
            }
            Some((last_order, last))
                if edit.range.start == last.range.start
                    && last.range.is_empty()
                    && !edit.range.is_empty()
                    && rewound.binary_search(last_order).is_ok() => {}
            _ => kept.push((order, edit)),
        }
    }
    edits.extend(kept.into_iter().map(|(_, edit)| edit));
}

fn parse_flags(text: &str, previous: Flags) -> Result<(Flags, Option<usize>), String> {
    let (mut flags, text) = text.strip_prefix('&').map_or((Flags::default(), text), |rest| (previous, rest));
    let mut tail = text;
    for (i, ch) in text.char_indices() {
        tail = &text[i..];
        match ch {
            'g' => flags.global = !flags.global,
            'c' => flags.confirm = !flags.confirm,
            'i' => flags.ignore_case = true,
            'I' => flags.ignore_case = false,
            'n' => flags.count = true,
            'e' => flags.quiet = true,
            '0'..='9' | ' ' | '\t' => break,
            'r' | '#' | 'p' | 'l' => return Err(format!("substitute: {ch} flag not supported")),
            _ => return Err(format!("substitute: invalid flag {ch}")),
        }
        tail = &text[i + ch.len_utf8()..];
    }
    let count = match tail.trim() {
        "" => None,
        number => Some(number.parse::<usize>().ok().filter(|&n| n > 0).ok_or("substitute: invalid count")?),
    };
    Ok((flags, count))
}

fn summary(count: usize, lines: usize, singular: &str, plural: &str) -> String {
    let unit = if count == 1 { singular } else { plural };
    let line = if lines == 1 { "line" } else { "lines" };
    format!("{count} {unit} on {lines} {line}")
}

fn expand_tilde(raw: &str, previous: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '~' => out.push_str(previous),
            '\\' => {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Where a walk through matched text is: its byte, and the character and
/// line those are in the buffer.
struct Position {
    byte: usize,
    char: usize,
    line: usize,
}

impl Position {
    /// Move to byte `to`; back too, where a search starts again on a line
    /// that the last match went into.
    fn advance(&mut self, text: &str, to: usize) {
        let passed = &text[self.byte.min(to)..self.byte.max(to)];
        let (chars, lines) = (passed.chars().count(), passed.bytes().filter(|&b| b == b'\n').count());
        if to >= self.byte {
            (self.char, self.line) = (self.char + chars, self.line + lines);
        } else {
            (self.char, self.line) = (self.char - chars, self.line - lines);
        }
        self.byte = to;
    }
}

/// Reads a rope slice front to back by character index, a chunk at a time.
struct RopeReader<'a> {
    chunks: ropey::iter::Chunks<'a>,
    chunk: &'a str,
    /// Where in `chunk` the next character is, and its index in the rope.
    byte: usize,
    char: usize,
}

impl<'a> RopeReader<'a> {
    fn new(slice: ropey::RopeSlice<'a>, start: usize) -> Self {
        Self { chunks: slice.chunks(), chunk: "", byte: 0, char: start }
    }

    /// Move to character `to`, appending what is passed to `out`.
    fn read_to(&mut self, to: usize, mut out: Option<&mut String>) {
        while self.char < to {
            if self.byte == self.chunk.len() {
                let Some(chunk) = self.chunks.next() else { return };
                (self.chunk, self.byte) = (chunk, 0);
                continue;
            }
            let rest = &self.chunk[self.byte..];
            let wanted = to - self.char;
            let (bytes, chars) = match rest.char_indices().nth(wanted) {
                Some((byte, _)) => (byte, wanted),
                None => (rest.len(), rest.chars().count()),
            };
            if let Some(out) = out.as_deref_mut() {
                out.push_str(&rest[..bytes]);
            }
            self.byte += bytes;
            self.char += chars;
        }
    }
}

#[derive(Clone, Copy)]
enum Case {
    Keep,
    Upper,
    Lower,
}

/// One part of a parsed replacement string.
enum Piece {
    Char(char),
    Whole,
    Group(usize),
    Once(Case),
    All(Case),
    /// `\e`, `\E`: back to the matched case.
    Reset,
}

fn parse_replacement(raw: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        pieces.push(match ch {
            '&' => Piece::Whole,
            '\\' => match chars.next() {
                Some('0') => Piece::Whole,
                Some(n @ '1'..='9') => Piece::Group(n as usize - '0' as usize),
                Some('u') => Piece::Once(Case::Upper),
                Some('l') => Piece::Once(Case::Lower),
                Some('U') => Piece::All(Case::Upper),
                Some('L') => Piece::All(Case::Lower),
                Some('e' | 'E') => Piece::Reset,
                Some('r') => Piece::Char('\n'),
                Some('n') => Piece::Char('\0'),
                Some('t') => Piece::Char('\t'),
                Some('b') => Piece::Char('\x08'),
                Some(literal) => Piece::Char(literal),
                None => Piece::Char('\\'),
            },
            _ => Piece::Char(ch),
        });
    }
    pieces
}

fn expand(pieces: &[Piece], whole: &str, found: &Matches<'_>, out: &mut String) {
    let mut case = Case::Keep;
    let mut once = None;
    let mut append = |text: &str, case: Case, once: &mut Option<Case>| {
        for ch in text.chars() {
            append_case(out, ch, once.take().unwrap_or(case));
        }
    };
    for piece in pieces {
        match *piece {
            Piece::Char(ch) => append(ch.encode_utf8(&mut [0; 4]), case, &mut once),
            Piece::Whole => append(whole, case, &mut once),
            Piece::Group(n) => append(found.group(n), case, &mut once),
            Piece::Once(next) => once = Some(next),
            Piece::All(next) => case = next,
            Piece::Reset => (case, once) = (Case::Keep, None),
        }
    }
}

/// Neovim's one-to-one case mappings: where Unicode's full mapping is more
/// than one character, the simple mapping (or none), as `utf_toupper`.
fn append_case(out: &mut String, ch: char, case: Case) {
    fn single(mut mapped: impl Iterator<Item = char>, ch: char) -> char {
        let first = mapped.next();
        first.filter(|_| mapped.next().is_none()).unwrap_or(ch)
    }
    out.push(match case {
        Case::Keep => ch,
        Case::Upper => match ch as u32 {
            0xdf => '\u{1e9e}',
            // Greek with ypogegrammeni: the capital with prosgegrammeni.
            0x1f80..=0x1f87 | 0x1f90..=0x1f97 | 0x1fa0..=0x1fa7 => char::from_u32(ch as u32 + 8).unwrap_or(ch),
            0x1fb3 | 0x1fc3 | 0x1ff3 => char::from_u32(ch as u32 + 9).unwrap_or(ch),
            _ => single(ch.to_uppercase(), ch),
        },
        Case::Lower if ch == '\u{130}' => 'i',
        Case::Lower => single(ch.to_lowercase(), ch),
    });
}
