use super::*;
use crate::text::pattern::Pattern;
use range::{Range as LineRange, UndoEntry};
use regex::Captures;

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
}

#[derive(Default)]
struct Changes {
    entries: Vec<UndoEntry>,
    count: usize,
    char_delta: isize,
    edited: bool,
}

impl Changes {
    fn range(&self, edit: &Edit) -> Range<usize> {
        edit.range.start.saturating_add_signed(self.char_delta)..edit.range.end.saturating_add_signed(self.char_delta)
    }

    fn record(&mut self, edit: &Edit, added: usize) {
        self.count += 1;
        let before = edit.end_line - edit.line + 1;
        if let Some(entry) = self.entries.last_mut().filter(|entry| entry.top == edit.line) {
            entry.before = entry.before.max(before);
            entry.after += added;
        } else {
            self.entries.push(UndoEntry { top: edit.line, before, after: added + 1, diff: 0 });
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
        let (pattern, replacement, flags) = if let Some(separator) = separator.filter(|_| !repeat) {
            let (pattern, rest) = search::field(&args[1..], separator, false);
            let (replacement, flags) = rest.map_or_else(|| (String::new(), None), |rest| search::field(rest, separator, false));
            let pattern = if pattern.is_empty() {
                self.search.as_ref().ok_or("no previous search pattern")?.pattern.clone()
            } else {
                pattern
            };
            (pattern, expand_tilde(&replacement, self.previous_replacement().unwrap_or("")), flags.unwrap_or(""))
        } else {
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

        let rope = &self.buffer.rope;
        let mut edits = Vec::new();
        let (mut matches, mut lines) = (0, 0);
        let mut last_match_line = None;
        let mut collect = |text: &str, base: usize, line: Option<usize>, limit: usize| {
            let mut previous_byte = 0;
            let mut offset = base;
            for captures in compiled.captures(text) {
                if compiled.is_implicit_empty_end(&captures, text) {
                    continue;
                }
                let bounds = compiled.bounds(&captures);
                offset += text[previous_byte..bounds.start].chars().count();
                previous_byte = bounds.start;
                if offset > limit {
                    break;
                }
                let line = line.unwrap_or_else(|| rope.char_to_line(offset));
                if last_match_line == Some(line) && !flags.global {
                    continue;
                }
                matches += 1;
                if last_match_line != Some(line) {
                    lines += 1;
                    last_match_line = Some(line);
                }
                if !flags.count {
                    let end = offset + text[bounds.clone()].chars().count();
                    edits.push(Edit {
                        range: offset..end,
                        line,
                        end_line: rope.char_to_line(end),
                        replacement: replace(&replacement, &compiled, &captures, &text[bounds]),
                    });
                }
                if !flags.global && !compiled.multiline {
                    break;
                }
            }
        };
        if compiled.multiline {
            let base = rope.line_to_char(first);
            let last_line = self.buffer.last_line();
            let end = rope.line_to_char(last_line) + self.buffer.line_len(last_line);
            let limit = rope.line_to_char(last) + self.buffer.line_len(last);
            collect(&rope.slice(base..end).to_string(), base, None, limit);
        } else {
            for line in first..=last {
                collect(&search::searchable_line(rope, line), rope.line_to_char(line), Some(line), usize::MAX);
            }
        }
        if matches == 0 {
            if !flags.quiet {
                return Err(format!("pattern not found: {pattern}"));
            }
        } else if flags.count {
            self.notify(summary(matches, lines, "match", "matches"), FlashKind::Info);
        } else if !self.buffer.is_editable(&(edits[0].range.start..edits[edits.len() - 1].range.end)) {
            return Err("substitution crosses read-only text".into());
        } else if flags.confirm {
            self.confirmation = Some(Confirmation {
                doc: self.dot_mark(), replacement, matches: edits,
                change: Changes::default(), next: 0,
            });
            self.mode = Mode::SubstituteConfirm;
            let start = self.buffer.cursor;
            self.buffer.history.begin_group(Some(start));
            self.show_substitute_confirmation();
        } else {
            let mut change = Changes::default();
            self.buffer.history.begin_group(None);
            self.apply_substitutions(&edits, &mut change);
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
            if matches!(cmd, KeyCode::Char('y' | 'l')) {
                self.buffer.cursor = Cursor::new(self.buffer.cursor.line, 0);
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
        let begin = change.range(first).start;
        let end = change.range(last).end;
        let mut previous = begin;
        let mut text = String::new();
        let mut delta = 0isize;
        let mut destination = 0;
        for edit in edits {
            let range = change.range(edit);
            for chunk in self.buffer.rope.slice(previous..range.start).chunks() {
                text.push_str(chunk);
            }
            text.push_str(&edit.replacement);
            let added = edit.replacement.bytes().filter(|&b| b == b'\n').count();
            destination = self.buffer.rope.char_to_line(range.start).saturating_add_signed(delta) + added;
            delta += added as isize - (edit.end_line - edit.line) as isize;
            change.record(edit, added);
            previous = range.end;
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

#[derive(Clone, Copy)]
enum Case {
    Keep,
    Upper,
    Lower,
}

fn replace(raw: &str, pattern: &Pattern, captures: &Captures<'_>, whole: &str) -> String {
    let mut out = String::new();
    let mut case = Case::Keep;
    let mut once = None;
    let mut chars = raw.chars();
    while let Some(mut ch) = chars.next() {
        let expansion = if ch == '&' {
            Some(whole)
        } else if ch == '\\' {
            match chars.next() {
                Some('0') => Some(whole),
                Some(n @ '1'..='9') => Some(pattern.group(captures, n as usize - '0' as usize)),
                Some(next) => {
                    match next {
                        'u' => once = Some(Case::Upper),
                        'l' => once = Some(Case::Lower),
                        'U' => case = Case::Upper,
                        'L' => case = Case::Lower,
                        'e' | 'E' => { case = Case::Keep; once = None; }
                        other => {
                            ch = match other {
                                'r' => '\n', 'n' => '\0', 't' => '\t', 'b' => '\x08',
                                literal => literal,
                            };
                            append_case(&mut out, ch, once.take().unwrap_or(case));
                        }
                    }
                    continue;
                }
                None => None,
            }
        } else {
            None
        };
        if let Some(text) = expansion {
            for item in text.chars() {
                append_case(&mut out, item, once.take().unwrap_or(case));
            }
        } else {
            append_case(&mut out, ch, once.take().unwrap_or(case));
        }
    }
    out
}

fn append_case(out: &mut String, ch: char, case: Case) {
    match case {
        Case::Keep => out.push(ch),
        Case::Upper if ch == 'ß' => out.push('ẞ'),
        Case::Upper => {
            let mut upper = ch.to_uppercase();
            let first = upper.next().unwrap_or(ch);
            out.push(if upper.next().is_none() { first } else { ch });
        }
        Case::Lower => out.push(ch.to_lowercase().next().unwrap_or(ch)),
    }
}
