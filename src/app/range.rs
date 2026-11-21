//! Line addresses, ranges and the commands that take them on the `:` line,
//! read the way vim reads them.
//!
//! An address is `N`, `.` (the cursor's line), `$` (the last line) or `'<`
//! / `'>` (the first and last line of the Visual selection `:` was typed
//! from), followed by any number of offsets: `+N`, `-N`, a bare `+` or `-`
//! for one, or a bare `N` for `+N`, with spaces between them allowed. An
//! offset with no address before it counts from the cursor's line. A range
//! is one address, two joined by `,` (either may be left out for the
//! cursor's line), or `%` for every line.
//!
//! Out of range values clamp to the first or last line, where vim refuses
//! some of them, and a range given backwards is turned round without asking.
//!
//! On its own a range goes to its last line, as `NG` does
//! (`App::run_command`). Before a command it names the lines the command
//! works on: `:d`, `:y`, `:>`, `:<`, `:m`, `:t` here, `:fixlist` and `:w`
//! in `commands.rs`.

use super::*;
use super::commands::{canonical, COMMANDS};

/// What an address counts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Base {
    /// `.`, or an offset written with nothing before it.
    Current,
    /// `$`.
    Last,
    /// A line number as typed, counted from 1. `0` is allowed: it is the
    /// line above the first for `:m` and `:t`, and the first line elsewhere.
    Number(usize),
    /// `'<`.
    SelStart,
    /// `'>`.
    SelEnd,
}

/// One address: where it counts from and the sum of its offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Address {
    pub base: Base,
    pub offset: i64,
}

impl Address {
    const CURRENT: Address = Address { base: Base::Current, offset: 0 };
}

/// The lines a command was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Range {
    One(Address),
    Two(Address, Address),
    /// `%`.
    Whole,
}

/// The longest run of ASCII digits at the start of `s`, and what follows.
fn digits(s: &str) -> (&str, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s.split_at(end)
}

/// A number as typed; one too big to hold is as big as it gets, which every
/// caller clamps to the last line anyway.
fn number(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(i64::MAX / 4).min(i64::MAX / 4)
}

/// One address at the start of `s` (spaces before it allowed), and the rest
/// of the line after it. `None`, with `s` handed back, when there is none.
pub(crate) fn address(s: &str) -> (Option<Address>, &str) {
    let t = s.trim_start();
    let (mut base, mut rest) = match t.as_bytes().first() {
        Some(b'.') => (Some(Base::Current), &t[1..]),
        Some(b'$') => (Some(Base::Last), &t[1..]),
        Some(b) if b.is_ascii_digit() => {
            let (n, rest) = digits(t);
            (Some(Base::Number(n.parse().unwrap_or(usize::MAX))), rest)
        }
        _ if t.starts_with("'<") => (Some(Base::SelStart), &t[2..]),
        _ if t.starts_with("'>") => (Some(Base::SelEnd), &t[2..]),
        _ => (None, t),
    };
    // Vim's offsets: each `+` or `-` is followed by its number or stands
    // for one, and a number after a space is added, so `3 + 2` is 6.
    let mut offset = 0i64;
    loop {
        let r = rest.trim_start();
        let (sign, after) = match r.as_bytes().first() {
            Some(b'+') => (1, &r[1..]),
            Some(b'-') => (-1, &r[1..]),
            Some(b) if b.is_ascii_digit() => (1, r),
            _ => break,
        };
        base.get_or_insert(Base::Current);
        let (n, after) = digits(after);
        let n = if n.is_empty() { 1 } else { number(n) };
        offset = offset.saturating_add(sign * n);
        rest = after;
    }
    match base {
        Some(base) => (Some(Address { base, offset }), rest),
        None => (None, s),
    }
}

/// The range at the start of a `:` line, if there is one, and the rest of
/// the line after it with its leading spaces dropped.
pub(crate) fn parse(line: &str) -> (Option<Range>, &str) {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix('%') {
        return (Some(Range::Whole), rest.trim_start());
    }
    let (first, rest) = address(t);
    let rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix(',') {
        let (second, rest) = address(after);
        let range = Range::Two(first.unwrap_or(Address::CURRENT), second.unwrap_or(Address::CURRENT));
        return (Some(range), rest.trim_start());
    }
    match first {
        Some(a) => (Some(Range::One(a)), rest),
        None => (None, t),
    }
}

/// How many bytes of `line` its range takes, spaces after it included: where
/// the command's name starts, for Tab completion. 0 with no range.
pub(crate) fn prefix_len(line: &str) -> usize {
    match parse(line) {
        (Some(_), rest) => line.len() - rest.len(),
        (None, _) => 0,
    }
}

/// What the addresses of a range are measured against. Lines count from 0.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lines {
    pub cursor: usize,
    pub last: usize,
    /// The first and last line of the Visual selection `:` was typed from.
    pub selection: Option<(usize, usize)>,
}

impl Lines {
    /// An address as a line counted from 1, unclamped: 0 or less is above
    /// the first line, more than `last + 1` below the last.
    pub fn value(&self, a: Address) -> Result<i64, String> {
        let base = match a.base {
            Base::Current => self.cursor as i64 + 1,
            Base::Last => self.last as i64 + 1,
            Base::Number(n) => i64::try_from(n).unwrap_or(i64::MAX).min(i64::MAX / 4),
            Base::SelStart | Base::SelEnd => {
                let Some((lo, hi)) = self.selection else {
                    return Err("'< and '> are the lines of a Visual selection: select, then :".into());
                };
                (if a.base == Base::SelStart { lo } else { hi }) as i64 + 1
            }
        };
        Ok(base.saturating_add(a.offset))
    }

    /// The line an address names, counted from 0 and clamped to the text.
    pub fn line(&self, a: Address) -> Result<usize, String> {
        Ok((self.value(a)?.clamp(1, self.last as i64 + 1) - 1) as usize)
    }

    /// A `:m` or `:t` target: how many lines are above the place the lines
    /// go, from 0 (the top) to `last + 1` (the end).
    pub fn target(&self, a: Address) -> Result<usize, String> {
        Ok(self.value(a)?.clamp(0, self.last as i64 + 1) as usize)
    }

    /// The first and last line of a range, in order; the cursor's line
    /// without one.
    pub fn span(&self, range: Option<Range>) -> Result<(usize, usize), String> {
        Ok(match range {
            None => (self.cursor, self.cursor),
            Some(Range::Whole) => (0, self.last),
            Some(Range::One(a)) => {
                let l = self.line(a)?;
                (l, l)
            }
            Some(Range::Two(a, b)) => {
                let (a, b) = (self.line(a)?, self.line(b)?);
                (a.min(b), a.max(b))
            }
        })
    }
}

/// A command that works on lines, as typed after its range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineCommand {
    /// `:d [x] [count]`.
    Delete { register: Option<char>, count: Option<usize> },
    /// `:y [x] [count]`.
    Yank { register: Option<char>, count: Option<usize> },
    /// `:>` and `:<`, one level for each `>` or `<`, then `[count]`.
    Shift { outdent: bool, levels: usize, count: Option<usize> },
    /// `:m {address}`.
    Move(Address),
    /// `:t {address}`, `:co {address}`.
    Copy(Address),
}

/// The names `:d`, `:y`, `:m` and `:t` answer to; each is in `COMMANDS`.
const LINE_COMMANDS: &[&str] = &["delete", "yank", "move", "copy"];

/// `rest` (the line after its range) as a line command. `Ok(None)` when it
/// is some other command, or none; `Err` when it is one of these with
/// something wrong after it, which is then the message to show.
pub(crate) fn line_command(rest: &str) -> Result<Option<LineCommand>, String> {
    let shift = rest.as_bytes().first().copied();
    if let Some(sign @ (b'>' | b'<')) = shift {
        let levels = rest.bytes().take_while(|&b| b == sign).count();
        let count = match count(&rest[levels..])? {
            Some(c) => c,
            None => return Ok(None),
        };
        return Ok(Some(LineCommand::Shift { outdent: sign == b'<', levels, count }));
    }
    let end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
    let (name, tail) = rest.split_at(end);
    let Some(command) = canonical(COMMANDS, name).filter(|c| LINE_COMMANDS.contains(c)) else {
        return Ok(None);
    };
    match command {
        "delete" | "yank" => {
            let tail = tail.trim_start();
            // A register is one character that is no digit; a digit starts
            // the count.
            let (register, tail) = match tail.chars().next() {
                Some(c) if c.is_ascii_alphabetic() || matches!(c, '"' | '+' | '*') => {
                    (Some(c), &tail[c.len_utf8()..])
                }
                _ => (None, tail),
            };
            let Some(count) = count(tail)? else { return Ok(None) };
            Ok(Some(match command {
                "delete" => LineCommand::Delete { register, count },
                _ => LineCommand::Yank { register, count },
            }))
        }
        _ => {
            let (target, after) = address(tail);
            let shown = if command == "move" { ":m" } else { ":t" };
            let Some(target) = target else {
                return Err(match tail.trim() {
                    "" => format!("{shown} needs a line to go below: {shown} 0 is the top, {shown} $ the end"),
                    t => format!("{shown}: not a line: {t}"),
                });
            };
            if !after.trim().is_empty() {
                return Err(format!("{shown}: not a line: {}", tail.trim()));
            }
            Ok(Some(match command {
                "move" => LineCommand::Move(target),
                _ => LineCommand::Copy(target),
            }))
        }
    }
}

/// A count after a command: `Ok(Some(None))` for none, `Ok(Some(Some(n)))`
/// for one, `Ok(None)` for something that is no count at all. A count of 0
/// is refused, as vim refuses it.
fn count(s: &str) -> Result<Option<Option<usize>>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(Some(None));
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    match s.parse::<usize>() {
        Ok(0) => Err("a count must be 1 or more".into()),
        Ok(n) => Ok(Some(Some(n))),
        Err(_) => Ok(Some(Some(usize::MAX))),
    }
}

/// One change as Neovim's undo records it: the block of lines below line
/// `top` (counted from 1, 0 for the top of the file), `before` lines long
/// before the command and `after` lines long after it. `diff` is the first
/// line of the block that differs, when it is lines changed in place.
struct UndoEntry {
    top: usize,
    before: usize,
    after: usize,
    diff: usize,
}

/// The line Neovim puts the cursor on when it undoes (or redoes) `entries`,
/// counted from 1: the line the command started on (`start`) when that is
/// in a block put back, else the first line that changed. A port of the
/// cursor rule in Neovim's `u_undoredo`, for the few shapes of change the
/// line commands make.
fn undo_line(entries: &[UndoEntry], start: usize, undo: bool) -> usize {
    let order: Vec<&UndoEntry> = match undo {
        true => entries.iter().rev().collect(),
        false => entries.iter().collect(),
    };
    let mut newlnum = usize::MAX;
    let mut line = start;
    for (k, e) in order.iter().enumerate() {
        let (newsize, oldsize) = if undo { (e.before, e.after) } else { (e.after, e.before) };
        if e.top >= newlnum {
            continue;
        }
        if start >= e.top && start <= e.top + newsize + 1 {
            line = start;
            newlnum = start.saturating_sub(1);
            continue;
        }
        let same = newsize.min(oldsize);
        let i = if same == 0 { 0 } else { e.diff.min(same) };
        if i == newsize && newlnum == usize::MAX && k + 1 == order.len() {
            newlnum = e.top;
            line = e.top + 1;
        } else if i < newsize {
            newlnum = e.top + i;
            line = newlnum + 1;
        }
    }
    line
}

impl App {
    /// What this buffer's ranges are measured against.
    pub(super) fn range_lines(&self) -> Lines {
        let selection = self.command_selection.map(|(a, b)| (a.min(b), a.max(b)));
        Lines { cursor: self.buffer.cursor.line, last: self.buffer.last_line(), selection }
    }

    /// `:{range}` with nothing after it: to the range's last line, through
    /// the motion `NG` runs. `:42` stays `42G` exactly, and `:$` is `G`.
    pub(super) fn jump_to_range(&mut self, range: Range) {
        let at = self.range_lines();
        let last = match range {
            Range::Whole => Address { base: Base::Last, offset: 0 },
            Range::One(a) => a,
            Range::Two(a, b) => {
                // Turned round when given backwards: the larger one.
                match (at.value(a), at.value(b)) {
                    (Ok(x), Ok(y)) if x > y => a,
                    _ => b,
                }
            }
        };
        let motion = match last {
            Address { base: Base::Last, offset: 0 } => Motion::BufferEnd,
            Address { base: Base::Number(n), offset: 0 } => Motion::GotoLine(n.saturating_sub(1)),
            a => match at.line(a) {
                Ok(l) => Motion::GotoLine(l),
                Err(e) => return self.notify(e, FlashKind::Error),
            },
        };
        self.run(Command::bare(Action::Move(motion)));
    }

    /// Read-only mode refuses an edit typed on the `:` line as it refuses a
    /// key's (`App::run`): with the way out, which `i` then takes.
    pub(super) fn refuse_read_only(&mut self) -> bool {
        if !self.view {
            return false;
        }
        self.edit_offered = true;
        self.notify("read-only — press i to edit", FlashKind::Info);
        true
    }

    /// The lines a range names, a closed fold at either end taken whole as
    /// vim takes it, and `count` lines from its last line when one is given.
    pub(super) fn range_span(&mut self, range: Option<Range>, count: Option<usize>) -> Option<(usize, usize)> {
        let at = self.range_lines();
        let (a, b) = match at.span(range) {
            Ok(s) => s,
            Err(e) => {
                self.notify(e, FlashKind::Error);
                return None;
            }
        };
        let (a, b) = match count {
            Some(n) => (b, b.saturating_add(n - 1).min(at.last)),
            None => (a, b),
        };
        let (a, b) = self.fold_hidden().widen(a, b);
        Some((a, b.min(at.last)))
    }

    /// Run a line command over `range`. Every one is a single undo step and
    /// none is a change `.` repeats, as in vim, where `.` repeats only
    /// Normal-mode changes: `.` keeps the one before.
    pub(super) fn run_line_command(&mut self, range: Option<Range>, command: LineCommand) {
        let count = match command {
            LineCommand::Delete { count, .. } | LineCommand::Yank { count, .. } | LineCommand::Shift { count, .. } => {
                count
            }
            LineCommand::Move(_) | LineCommand::Copy(_) => None,
        };
        let edits = !matches!(command, LineCommand::Yank { .. });
        if edits && self.refuse_read_only() {
            return;
        }
        let Some((a, b)) = self.range_span(range, count) else { return };
        match command {
            LineCommand::Delete { register, .. } => self.delete_range(a, b, register),
            LineCommand::Yank { register, .. } => {
                self.pending_register = register;
                let text = self.lines_text(a, b);
                self.store_register(text, true, false);
            }
            LineCommand::Shift { outdent, levels, .. } => self.shift_range(a, b, outdent, levels),
            LineCommand::Move(to) | LineCommand::Copy(to) => {
                let t = match self.range_lines().target(to) {
                    Ok(t) => t,
                    Err(e) => return self.notify(e, FlashKind::Error),
                };
                self.move_or_copy(a, b, t, matches!(command, LineCommand::Move(_)));
            }
        }
        if edits {
            self.end_recording();
        }
    }

    /// Lines `a..=b` as a register holds them, each ending in a newline.
    fn lines_text(&self, a: usize, b: usize) -> String {
        let start = self.buffer.rope.line_to_char(a);
        let end = self.buffer.rope.line_to_char((b + 1).min(self.buffer.rope.len_lines()));
        let mut text = self.buffer.rope.slice(start..end).to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text
    }

    /// Delete lines `a..=b`, the file's last line without a newline
    /// included: the newline before it goes instead, so the text still ends
    /// as it did.
    fn delete_lines(&mut self, a: usize, b: usize) {
        let rope = &self.buffer.rope;
        let len = rope.len_chars();
        let start = rope.line_to_char(a);
        let (from, to) = if b + 1 < rope.len_lines() {
            (start, rope.line_to_char(b + 1))
        } else if a > 0 && rope.char(len - 1) != '\n' {
            (start - 1, len)
        } else {
            (start, len)
        };
        self.buffer.delete_chars(from, to);
    }

    /// Put `text` (whole lines, each ending in a newline) in above line `at`,
    /// or after the last line when `at` is past it. After a last line with
    /// no newline, the newline goes before the new lines instead.
    fn insert_lines(&mut self, at: usize, text: &str) {
        let last = self.buffer.last_line();
        if at <= last {
            self.buffer.insert_str(Cursor::new(at, 0), text);
            return;
        }
        let len = self.buffer.rope.len_chars();
        let ends = len == 0 || self.buffer.rope.char(len - 1) == '\n';
        let end = self.idx_to_cursor(len);
        match ends {
            true => self.buffer.insert_str(end, text),
            false => self.buffer.insert_str(end, &format!("\n{}", text.strip_suffix('\n').unwrap_or(text))),
        };
    }

    /// The column a Normal-mode cursor takes for `col` on `line`: on its
    /// last character at most.
    fn clamp_col(&self, line: usize, col: usize) -> usize {
        col.min(self.buffer.line_len(line).saturating_sub(1))
    }

    /// Where Neovim's undo or redo of this command leaves the cursor: on
    /// `start` when it lands on that line, else at the goal's screen cell
    /// (`cells`) of the line it lands on. Measured in the text as it is when
    /// this runs.
    fn undo_cursor(&self, entries: &[UndoEntry], start: Cursor, cells: usize, undo: bool) -> Cursor {
        // Past the end, the line is the last one, but still the start's:
        // its column is the start's too.
        let landed = undo_line(entries, start.line + 1, undo);
        let line = landed.clamp(1, self.buffer.last_line() + 1) - 1;
        let col = if landed == start.line + 1 {
            self.clamp_col(line, start.col)
        } else {
            motion::col_at_cells(&self.buffer, line, cells, self.config.editor.tab_width)
        };
        Cursor::new(line, col)
    }

    /// The goal's screen cell, and where Neovim's undo starts from for a
    /// command that first goes to line `a`: that cell on it.
    fn goal_on(&self, a: usize) -> (usize, Cursor) {
        let tw = self.config.editor.tab_width;
        let cells = motion::goal_cells(&self.buffer, self.buffer.cursor, tw);
        (cells, Cursor::new(a, motion::col_at_cells(&self.buffer, a, cells, tw)))
    }

    /// `:d`: the lines into the register, the cursor on the first non-blank
    /// of the line after them, as Neovim.
    fn delete_range(&mut self, a: usize, b: usize, register: Option<char>) {
        let (_, start) = self.goal_on(a);
        let entries = [UndoEntry { top: a, before: b - a + 1, after: 0, diff: 0 }];
        let text = self.lines_text(a, b);
        self.buffer.history.begin_group(Some(start));
        self.delete_lines(a, b);
        self.pending_register = register;
        self.store_register(text, true, true);
        let line = a.min(self.buffer.last_line());
        let col = self.clamp_col(line, self.first_non_blank(line));
        self.buffer.cursor = Cursor::new(line, col);
        let redo = self.undo_cursor(&entries, start, 0, false);
        self.buffer.history.set_redo_cursor(redo);
        self.buffer.history.end_group();
    }

    /// `:>` and `:<`: `levels` indents in or out, as `>>` and `<<`. The
    /// cursor ends on the range's last line: on the first non-blank of the
    /// last line whose indent changed, carried down to it, as Neovim; the
    /// goal column stays what it was.
    fn shift_range(&mut self, a: usize, b: usize, outdent: bool, levels: usize) {
        let (cells, start) = self.goal_on(a);
        let goal = self.buffer.cursor;
        let before: Vec<String> = (a..=b).map(|l| self.buffer.line_text(l)).collect();
        let revision = self.buffer.revision;
        self.buffer.history.begin_group(Some(start));
        for _ in 0..levels {
            self.shift_lines(a, b, outdent);
        }
        let mut col = start.col;
        for (i, l) in (a..=b).enumerate() {
            if self.buffer.line_text(l) != before[i] {
                col = self.first_non_blank(l);
            }
        }
        self.buffer.cursor = Cursor {
            line: b,
            col: self.clamp_col(b, col),
            goal_col: goal.goal_col,
            goal_cells: Some(cells),
        };
        if self.buffer.revision != revision {
            let redo = Cursor::new(a, self.clamp_col(a, start.col));
            self.buffer.history.set_redo_cursor(redo);
        }
        self.buffer.history.end_group();
    }

    /// `:m` and `:t`: lines `a..=b` below the first `t` lines (`t` = 0 is
    /// the top). The cursor goes to the last line moved or copied, on the
    /// column it had, and keeps its goal, as Neovim.
    fn move_or_copy(&mut self, a: usize, b: usize, t: usize, moving: bool) {
        let n = b - a + 1;
        if moving && t > a && t <= b {
            return self.notify("can't move lines below one of themselves", FlashKind::Error);
        }
        let tw = self.config.editor.tab_width;
        let cur = self.buffer.cursor;
        let cells = motion::goal_cells(&self.buffer, cur, tw);
        let line = match (moving, t > b) {
            (true, true) => t - 1,
            _ => t + n - 1,
        };
        let done = |app: &mut App| {
            let len_line = line.min(app.buffer.last_line());
            app.buffer.cursor = Cursor {
                line: len_line,
                col: app.clamp_col(len_line, cur.col),
                goal_col: cur.goal_col,
                goal_cells: Some(cells),
            };
        };
        if moving && (t == a || t == b + 1) {
            // Where they are already: nothing changes but the cursor.
            return done(self);
        }
        // Neovim's undo blocks: the lines put in below line `t`, then, for
        // a move, the old ones taken out.
        let mut entries = vec![UndoEntry { top: t, before: 0, after: n, diff: 0 }];
        if moving {
            let extra = if t <= a { n } else { 0 };
            entries.push(UndoEntry { top: a + extra, before: n, after: 0, diff: 0 });
        }
        let undo = self.undo_cursor(&entries, cur, cells, true);
        let undo_cells = motion::goal_cells(&self.buffer, undo, tw);
        let text = self.lines_text(a, b);
        self.buffer.history.begin_group(Some(undo));
        if moving {
            self.delete_lines(a, b);
            self.insert_lines(if t > b { t - n } else { t }, &text);
        } else {
            self.insert_lines(t, &text);
        }
        done(self);
        let redo = self.undo_cursor(&entries, cur, undo_cells, false);
        self.buffer.history.set_redo_cursor(redo);
        self.buffer.history.end_group();
    }

    /// `:{range}w path`: only those lines, written to another file.
    pub(super) fn write_range(&mut self, a: usize, b: usize, typed: &str, force: bool) {
        if typed.is_empty() {
            return self.notify("a range writes part of the buffer to another file: give its path", FlashKind::Error);
        }
        let p = self.typed_path(typed);
        if self.buffer.is_own_file(&p) {
            return self.notify(
                format!("{} is this buffer's own file: a range writes to another one", self.shown(&p)),
                FlashKind::Error,
            );
        }
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        match self.buffer.write_lines_copy(&p, a, b, policy, force) {
            Ok(()) => {
                self.disk_changed();
                let n = b - a + 1;
                let lines = if n == 1 { "line" } else { "lines" };
                self.notify(format!("{} written · {n} {lines}", self.shown(&p)), FlashKind::Info);
            }
            Err(e) => self.notify(format!("{e}"), FlashKind::Error),
        }
    }
}

#[cfg(test)]
#[path = "range_tests.rs"]
mod tests;
