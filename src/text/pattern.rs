//! Translate Vim's pattern syntax to the Rust regex engine.

use regex::{Captures, Regex, RegexBuilder};
use std::ops::Range;

#[derive(Clone)]
pub struct Pattern {
    regex: Regex,
    start: Vec<usize>,
    end: Vec<usize>,
    groups: Vec<usize>,
    eol: Vec<usize>,
    pub multiline: bool,
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
        let mut rx = String::new();
        let mut chars = pattern.chars().peekable();
        let mut markers = 0;
        let mut groups = 0;
        let mut branch_start = true;
        let mut after_anchor = false;
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
                        continue;
                    }
                    '1'..='9' | 'i' | 'I' | 'k' | 'K' | 'f' | 'F' | 'p' | 'P' | 'Z' => {
                        return Err(format!("pattern: \\{ch} not supported"));
                    }
                    '_' => match chars.next() {
                        Some('^') => {
                            rx.push('^');
                            continue;
                        }
                        Some('$') => {
                            markers += 1;
                            rx.push_str(&format!("(?P<eol{markers}>$)"));
                            continue;
                        }
                        Some(next) => return Err(format!("pattern: \\_{next} not supported")),
                        None => return Err("pattern: incomplete \\_ atom".into()),
                    },
                    'n' | 't' | 'e' | 'r' | 'b' => {
                        multiline |= ch == 'n';
                        rx.push_str(match ch {
                            'n' => r"\n",
                            't' => r"\t",
                            'r' => r"\r",
                            'e' => r"\x1b",
                            _ => r"\x08",
                        });
                        branch_start = ch == 'n';
                        after_anchor = false;
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
                    continue;
                }
            }
            let special = match ch {
                '*' if branch_start || after_anchor => false,
                '.' | '*' | '[' | '~' => match magic {
                    Magic::Very | Magic::Default => !escaped,
                    Magic::No | Magic::Literal => escaped,
                },
                '^' | '$' => match magic {
                    Magic::Very => !escaped,
                    Magic::Literal => escaped,
                    _ => !escaped && (if ch == '^' { branch_start } else {
                        let mut rest = chars.clone();
                        loop {
                            match (rest.next(), rest.next()) {
                                (None, _) | (Some('\\'), Some('|' | ')' | 'n')) => break true,
                                (Some('\\'), Some('c' | 'C' | 'm' | 'M' | 'v' | 'V')) => {}
                                _ => break false,
                            }
                        }
                    }),
                },
                '+' | '?' | '=' | '{' | '(' | ')' | '|' | '<' | '>' | '%' | '&' | '@' => {
                    escaped != (magic == Magic::Very)
                }
                _ => false,
            };
            branch_start = special && matches!(ch, '(' | '|' | '%');
            after_anchor = special && ch == '^';
            if !special {
                multiline |= ch == '\n';
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
                '$' => {
                    markers += 1;
                    rx.push_str(&format!("(?P<eol{markers}>$)"));
                }
                '<' => rx.push_str(r"\b{start}"),
                '>' => rx.push_str(r"\b{end}"),
                '=' => rx.push('?'),
                '~' => {
                    let replacement = replacement.ok_or("no previous substitute regular expression")?;
                    multiline |= replacement.contains('\n');
                    rx.push_str(&regex::escape(replacement));
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
        let mut result = Self { regex, start: Vec::new(), end: Vec::new(), groups: Vec::new(), eol: Vec::new(), multiline };
        for (index, name) in result.regex.capture_names().enumerate() {
            match name {
                Some(name) if name.starts_with("start") => result.start.push(index),
                Some(name) if name.starts_with("end") => result.end.push(index),
                Some(name) if name.starts_with("vim") => result.groups.push(index),
                Some(name) if name.starts_with("eol") => result.eol.push(index),
                _ => {}
            }
        }
        Ok(result)
    }

    pub fn captures<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Captures<'a>> + 'a {
        let mut regular = self.regex.captures_iter(text);
        let mut next = Some(0);
        let mut previous_nonempty_end = None;
        std::iter::from_fn(move || {
            if self.end.is_empty() {
                return regular.next();
            }
            let at = next?;
            let mut captures = self.regex.captures_at(text, at)?;
            let mut bounds = self.bounds(&captures);
            // A zero-width match immediately after a replacement belongs to
            // the match already consumed, as with the regular iterator.
            if previous_nonempty_end == Some(bounds.start) && bounds.is_empty() {
                let after = at + text[at..].chars().next()?.len_utf8();
                captures = self.regex.captures_at(text, after)?;
                bounds = self.bounds(&captures);
            }
            previous_nonempty_end = (!bounds.is_empty()).then_some(bounds.end);
            next = if bounds.is_empty() {
                text[bounds.end..].chars().next().map(|ch| bounds.end + ch.len_utf8())
            } else {
                Some(bounds.end)
            };
            Some(captures)
        })
    }

    pub fn char_ranges<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        let mut previous = 0;
        let mut columns = 0;
        self.captures(text).map(move |captures| {
            let range = self.bounds(&captures);
            columns += text[previous..range.start].chars().count();
            let start = columns;
            columns += text[range.clone()].chars().count();
            previous = range.end;
            start..columns
        })
    }

    pub fn bounds(&self, captures: &Captures<'_>) -> Range<usize> {
        let whole = captures.get(0).expect("every capture has a whole match");
        let marker = |indices: &[usize]| indices.iter().rev().find_map(|&i| captures.get(i));
        let start = marker(&self.start).map_or(whole.start(), |m| m.start());
        let end = marker(&self.end).map_or(whole.end(), |m| m.start());
        start..end.max(start)
    }

    /// A repeatable empty atom stops before a nonempty line's ending.
    /// An explicit end anchor or a match that consumed text can reach it.
    pub fn is_implicit_empty_end(&self, captures: &Captures<'_>, text: &str) -> bool {
        let whole = captures.get(0).expect("every capture has a whole match");
        whole.is_empty() && whole.start() > 0 && text.as_bytes()[whole.start() - 1] != b'\n'
            && (whole.start() == text.len() || text.as_bytes()[whole.start()] == b'\n')
            && self.eol.iter().all(|&group| captures.get(group).is_none())
    }

    pub fn group<'a>(&self, captures: &Captures<'a>, n: usize) -> &'a str {
        self.groups.get(n - 1).and_then(|&i| captures.get(i)).map_or("", |m| m.as_str())
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
            '[' if chars.peek() == Some(&':') => {
                out.push('[');
                out.extend(chars.by_ref().take_while(|&c| c != ']'));
                out.push(']');
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
