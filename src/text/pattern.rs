//! Translate Vim's pattern syntax to the Rust regex engine.

use regex::{CaptureLocations, Regex, RegexBuilder};
use std::ops::Range;
use std::time::Instant;

#[derive(Clone)]
pub struct Pattern {
    regex: Regex,
    start: Vec<usize>,
    end: Vec<usize>,
    groups: Vec<usize>,
    /// A match can take in a line break, so runs on the joined lines.
    pub multiline: bool,
    /// The pattern has a `\n` atom (Vim's `RF_HASNL`): `:s` then goes on
    /// searching at a line's end. A `[\n]` collection does not count.
    newline: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Magic {
    Very,
    Default,
    No,
    Literal,
}

impl Pattern {
    pub fn compile(pattern: &str, ignore_case: bool, replacement: Option<&str>) -> Result<Self, String> {
        let mut magic = Magic::Default;
        let mut case = None;
        let mut multiline = false;
        let mut newline = false;
        let mut rx = String::new();
        let mut chars = pattern.chars().peekable();
        let mut markers = 0;
        let mut groups = 0;
        let mut branch_start = true;
        let mut after_anchor = false;
        // After `\n` a `^` is an anchor, but a `*` repeats the `\n`.
        let mut after_newline = false;
        // Vim refuses a multi straight after another (`x**`, `x\{2}*`).
        let mut after_multi = false;
        while let Some(mut ch) = chars.next() {
            let escaped = ch == '\\';
            if escaped {
                ch = chars.next().ok_or("pattern: trailing backslash")?;
                match ch {
                    'v' | 'm' | 'M' | 'V' => {
                        magic = match ch {
                            'v' => Magic::Very,
                            'm' => Magic::Default,
                            'M' => Magic::No,
                            _ => Magic::Literal,
                        };
                        continue;
                    }
                    'c' | 'C' => {
                        case = Some(case == Some(true) || ch == 'c');
                        continue;
                    }
                    'z' => {
                        let next = chars.next().ok_or("pattern: incomplete \\z atom")?;
                        let prefix = match next {
                            's' => "start",
                            'e' => "end",
                            _ => return Err(format!("pattern: \\z{next} not supported")),
                        };
                        markers += 1;
                        rx.push_str(&format!("(?P<{prefix}{markers}>)"));
                        // Vim: a `^` after `\zs` or `\ze` is a literal.
                        (branch_start, after_anchor, after_multi) = (false, false, false);
                        continue;
                    }
                    '1'..='9' | 'i' | 'I' | 'k' | 'K' | 'f' | 'F' | 'p' | 'P' | 'Z' => {
                        return Err(format!("pattern: \\{ch} not supported"));
                    }
                    '_' => match chars.next() {
                        Some('^') => {
                            rx.push('^');
                            after_multi = false;
                            continue;
                        }
                        Some('$') => {
                            rx.push('$');
                            after_multi = false;
                            continue;
                        }
                        Some(next) => return Err(format!("pattern: \\_{next} not supported")),
                        None => return Err("pattern: incomplete \\_ atom".into()),
                    },
                    'n' | 't' | 'e' | 'r' | 'b' => {
                        multiline |= ch == 'n';
                        newline |= ch == 'n';
                        rx.push_str(match ch {
                            'n' => r"\n",
                            't' => r"\t",
                            'r' => r"\r",
                            'e' => r"\x1b",
                            _ => r"\x08",
                        });
                        branch_start = ch == 'n';
                        after_newline = ch == 'n';
                        after_anchor = false;
                        after_multi = false;
                        continue;
                    }
                    _ => {}
                }
                if let Some(class) = vim_class(ch) {
                    rx.push_str("(?-i:");
                    rx.push_str(class);
                    rx.push(')');
                    branch_start = false;
                    after_anchor = false;
                    after_multi = false;
                    continue;
                }
            }
            let special = match ch {
                '*' if (branch_start && !after_newline) || after_anchor => false,
                '.' | '*' | '[' | '~' => match magic {
                    Magic::Very | Magic::Default => !escaped,
                    Magic::No | Magic::Literal => escaped,
                },
                '^' | '$' => match magic {
                    Magic::Very => !escaped,
                    Magic::Literal => escaped,
                    _ => !escaped && (if ch == '^' { branch_start } else { ends_branch(chars.clone()) }),
                },
                '+' | '?' | '=' | '{' | '(' | ')' | '|' | '<' | '>' | '%' | '&' | '@' => {
                    escaped != (magic == Magic::Very)
                }
                _ => false,
            };
            branch_start = special && matches!(ch, '(' | '|' | '%');
            after_anchor = special && ch == '^';
            after_newline = false;
            let multi = special && matches!(ch, '*' | '+' | '=' | '?' | '{' | '@');
            if multi && after_multi {
                return Err("pattern: can't have a multi follow a multi".into());
            }
            after_multi = multi;
            if !special {
                multiline |= ch == '\n';
                newline |= ch == '\n';
                rx.push_str(&regex::escape(&ch.to_string()));
                continue;
            }
            match ch {
                '(' => {
                    groups += 1;
                    rx.push_str(&format!("(?P<vim{groups}>"));
                }
                '%' if chars.next_if_eq(&'(').is_some() => rx.push_str("(?:"),
                '%' | '&' | '@' => {
                    let mut atom = if escaped { format!("\\{ch}") } else { ch.to_string() };
                    match ch {
                        '@' => atom.extend(chars.by_ref().take_while(|c| c.is_ascii_digit() || "<=!>".contains(*c))),
                        '%' => atom.extend(chars.next()),
                        _ => {}
                    }
                    return Err(format!("pattern: {atom} not supported"));
                }
                '$' => rx.push('$'),
                '<' => rx.push_str(r"\b{start}"),
                '>' => rx.push_str(r"\b{end}"),
                '=' => rx.push('?'),
                '~' => {
                    let replacement = replacement.ok_or("no previous substitute regular expression")?;
                    multiline |= replacement.contains('\n');
                    // Grouped, so a multi after `~` repeats all of it.
                    rx.push_str(&format!("(?:{})", regex::escape(replacement)));
                }
                '[' => {
                    let mut rest = chars.clone();
                    match collection(&mut rest)? {
                        Some((set, crosses_line)) => {
                            chars = rest;
                            multiline |= crosses_line;
                            rx.push_str(&set);
                        }
                        None => rx.push_str(r"\["),
                    }
                }
                '{' => {
                    let lazy = chars.next_if_eq(&'-').is_some();
                    let mut count = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(ch) => count.push(ch),
                            None => return Err("pattern: unclosed repetition".into()),
                        }
                    }
                    if count.ends_with('\\') {
                        count.pop();
                    }
                    if !count.bytes().all(|b| b.is_ascii_digit() || b == b',') {
                        return Err("pattern: invalid repetition".into());
                    }
                    if let Some(upper) = count.strip_prefix(',') {
                        count = format!("0,{upper}");
                    }
                    if count.is_empty() {
                        rx.push('*');
                    } else {
                        rx.push_str(&format!("{{{count}}}"));
                    }
                    if lazy {
                        rx.push('?');
                    }
                }
                _ => rx.push(ch),
            }
        }
        let regex = RegexBuilder::new(&rx)
            .multi_line(true)
            .case_insensitive(case.unwrap_or(ignore_case))
            .build()
            .map_err(|error| {
                let message = error.to_string();
                let detail = message.lines().last().unwrap_or(&message);
                if detail.starts_with("error: ") { detail.replacen("error: ", "pattern: ", 1) } else {
                    format!("pattern: {detail}")
                }
            })?;
        let mut result = Self { regex, start: Vec::new(), end: Vec::new(), groups: Vec::new(), multiline, newline };
        for (index, name) in result.regex.capture_names().enumerate() {
            match name {
                Some(name) if name.starts_with("start") => result.start.push(index),
                Some(name) if name.starts_with("end") => result.end.push(index),
                Some(name) if name.starts_with("vim") => result.groups.push(index),
                _ => {}
            }
        }
        Ok(result)
    }

    /// The matches in `text`, found as Vim's `:s` finds them. A search
    /// starts where the previous match ended. An empty match there is
    /// skipped one character on. With `by_line`, `:s`'s rule for each line of
    /// `text`: once a match or a skip reaches the line's end, the line is
    /// done, unless the pattern has a `\n` or the match took a line break.
    pub fn matches<'a>(&'a self, text: &'a str, by_line: bool) -> Matches<'a> {
        Matches {
            pattern: self, text, locs: self.regex.capture_locations(), at: 0, previous_end: None, by_line,
            one_per_line: false,
        }
    }

    /// The first match at or after byte `at`.
    #[cfg(test)]
    pub fn find_at(&self, text: &str, at: usize) -> Option<Found> {
        let mut matches = self.matches(text, false);
        matches.seek(at);
        matches.next()
    }

    /// The matches' character columns, at most until `deadline`: what was
    /// found by then is returned, so a pathological line cannot stall a
    /// redraw (Vim's 'redrawtime' does the same).
    pub fn char_ranges(&self, text: &str, deadline: Option<Instant>) -> Vec<Range<usize>> {
        self.char_ranges_until(text, deadline).0
    }

    /// [`Pattern::char_ranges`], and whether they are all of them: `false`
    /// when `deadline` cut the search short.
    pub fn char_ranges_until(&self, text: &str, deadline: Option<Instant>) -> (Vec<Range<usize>>, bool) {
        let mut ranges = Vec::new();
        let mut matches = self.matches(text, false);
        let (mut previous, mut columns) = (0, 0);
        while let Some(found) = matches.next() {
            let range = found.span;
            columns += text[previous..range.start].chars().count();
            let start = columns;
            columns += text[range.clone()].chars().count();
            previous = range.end;
            ranges.push(start..columns);
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return (ranges, false);
            }
        }
        (ranges, true)
    }
}

/// One match: the bytes the regex consumed and the span `\zs`/`\ze` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub whole: Range<usize>,
    pub span: Range<usize>,
}

/// The matches of a [`Pattern`]; [`Matches::group`] reads the current
/// match's groups, so this lends rather than iterates.
pub struct Matches<'a> {
    pattern: &'a Pattern,
    text: &'a str,
    locs: CaptureLocations,
    at: usize,
    previous_end: Option<usize>,
    by_line: bool,
    /// `:s` without `g`: a skipped empty match ends its line too.
    pub one_per_line: bool,
}

impl Matches<'_> {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Found> {
        let pattern = self.pattern;
        loop {
            if self.at > self.text.len() {
                return None;
            }
            let whole = pattern.regex.captures_read_at(&mut self.locs, self.text, self.at)?.range();
            // The last `\zs` and `\ze` that took part; an `\ze` before that
            // `\zs` is undone by it, as in Vim.
            let marker = |indices: &[usize]| indices.iter().rev().find_map(|&i| Some((i, self.locs.get(i)?.0)));
            let zs = marker(&pattern.start);
            let start = zs.map_or(whole.start, |(_, at)| at);
            let end = match marker(&pattern.end) {
                Some((ze, at)) if zs.is_none_or(|(zs, _)| ze > zs) => at.max(start),
                _ => whole.end.max(start),
            };
            if self.previous_end == Some(self.at) && end == self.at {
                // An empty match where the last one ended: try one character
                // on (past a line break, the next line starts afresh).
                if self.one_per_line {
                    self.next_line(self.at);
                } else {
                    self.at += self.text[self.at..].chars().next().map_or(1, char::len_utf8);
                    self.end_line();
                }
                continue;
            }
            self.at = end;
            self.previous_end = Some(end);
            if !self.text[start..end].contains('\n') {
                self.end_line();
            }
            return Some(Found { whole, span: start..end });
        }
    }

    /// Go on searching at byte `at`, as afresh: an empty match there counts.
    pub fn seek(&mut self, at: usize) {
        self.at = at;
        self.previous_end = None;
    }

    /// Go on searching at the start of the line after the one byte `from` is in.
    pub fn next_line(&mut self, from: usize) {
        self.at = self.text[from..].find('\n').map_or(self.text.len() + 1, |i| from + i + 1);
        self.previous_end = None;
    }

    /// `:s`'s `lastone`: at a line's end the line is done. The next search
    /// starts on the next line, or nowhere after the last one.
    fn end_line(&mut self) {
        if !self.by_line || self.pattern.newline {
            return;
        }
        match self.text.as_bytes().get(self.at) {
            None => self.at = self.text.len() + 1,
            Some(b'\n') => {
                self.at += 1;
                self.previous_end = None;
            }
            Some(_) => {}
        }
    }

    /// Vim group `n` (1–9) of the current match, empty when it did not take part.
    pub fn group(&self, n: usize) -> &str {
        self.pattern.groups.get(n - 1).and_then(|&i| self.locs.get(i)).map_or("", |(a, b)| &self.text[a..b])
    }
}

/// Whether a `$` ends its branch, so is an anchor: Vim's rule looks past
/// `\c \C \m \M \v \V \Z`, taking the mode they set, for the end, `\|`,
/// `\)`, `\&` or `\n` (or `|`, `)`, `&` under `\v`).
fn ends_branch(mut rest: std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut very = false;
    loop {
        let mut ahead = rest.clone();
        match (ahead.next(), ahead.next()) {
            (Some('\\'), Some(mode @ ('c' | 'C' | 'm' | 'M' | 'v' | 'V' | 'Z'))) => {
                match mode {
                    'v' => very = true,
                    'm' | 'M' | 'V' => very = false,
                    _ => {}
                }
                rest = ahead;
            }
            (None, _) | (Some('\\'), Some('|' | '&' | ')' | 'n')) => return true,
            (Some('|' | '&' | ')'), _) => return very,
            _ => return false,
        }
    }
}

fn collection(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<Option<(String, bool)>, String> {
    let mut out = String::from("[");
    let negated = chars.next_if_eq(&'^').is_some();
    let mut newline = false;
    if negated {
        out.push('^');
    }
    if chars.next_if_eq(&']').is_some() {
        out.push_str(r"\]");
    }
    while let Some(ch) = chars.next() {
        match ch {
            ']' => {
                out.push(']');
                return Ok(Some(if newline && !negated {
                    (out, true)
                } else {
                    (format!(r"[{out}&&[^\n]]"), false)
                }));
            }
            '[' if chars.peek().is_some_and(|ch| matches!(ch, '.' | '=')) => {
                return Err("pattern: collating and equivalence classes not supported".into());
            }
            // `[:name:]`; any other `[` is literal, as Vim's `get_char_class`.
            '[' if chars.peek() == Some(&':') => {
                let mut ahead = chars.clone();
                ahead.next();
                let mut name = String::new();
                while let Some(ch) = ahead.next_if(char::is_ascii_lowercase) {
                    name.push(ch);
                }
                if ahead.next_if_eq(&':').is_some() && ahead.next_if_eq(&']').is_some() {
                    out.push_str(&format!("[:{name}:]"));
                    *chars = ahead;
                } else {
                    out.push_str(r"\[");
                }
            }
            '\\' => {
                let next = chars.next().ok_or("pattern: trailing backslash")?;
                match next {
                    't' | 'r' => {
                        out.push('\\');
                        out.push(next);
                    }
                    'e' => out.push_str(r"\x1b"),
                    'b' => out.push_str(r"\x08"),
                    'n' => {
                        out.push_str(r"\n");
                        newline = true;
                    }
                    _ => out.push_str(&regex::escape(&next.to_string())),
                }
            }
            '-' => out.push('-'),
            _ => out.push_str(&regex::escape(&ch.to_string())),
        }
    }
    Ok(None)
}

fn vim_class(ch: char) -> Option<&'static str> {
    Some(match ch {
        's' => "[ \\t]", 'S' => "[^ \\t\\n]",
        'd' => "[0-9]", 'D' => "[^0-9\\n]",
        'x' => "[0-9A-Fa-f]", 'X' => "[^0-9A-Fa-f\\n]",
        'o' => "[0-7]", 'O' => "[^0-7\\n]",
        'w' => "[0-9A-Za-z_]", 'W' => "[^0-9A-Za-z_\\n]",
        'h' => "[A-Za-z_]", 'H' => "[^A-Za-z_\\n]",
        'a' => "[A-Za-z]", 'A' => "[^A-Za-z\\n]",
        'l' => "[a-z]", 'L' => "[^a-z\\n]",
        'u' => "[A-Z]", 'U' => "[^A-Z\\n]",
        _ => return None,
    })
}

#[cfg(test)]
#[path = "pattern_tests.rs"]
mod tests;
