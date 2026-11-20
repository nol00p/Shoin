use super::boxes::{grid, Align, Grid, MathBox};
use super::symbols::{accent_command, big_operator, env_delims, math_escape, symbol, text_of, OPERATORS};
use crate::render::conceal::render_latex;

// Limit recursive groups and commands before they exhaust the caller's stack.
const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Char(char),
    Command(&'a str),
    End,
}

fn token(input: &str) -> (Token<'_>, &str) {
    let mut chars = input.chars();
    match chars.next() {
        None => (Token::End, ""),
        Some('\\') => {
            let rest = chars.as_str();
            let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            let len = if letters > 0 { letters } else { rest.chars().next().map_or(0, char::len_utf8) };
            (Token::Command(&rest[..len]), &rest[len..])
        }
        Some(ch) => (Token::Char(ch), chars.as_str()),
    }
}

pub(super) struct Parser<'a> {
    rest: &'a str,
    width: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ordinary,
    Opening,
    Closing,
    Space,
    Relation,
    Binary,
}

impl Kind {
    fn of(text: &str) -> Self {
        match text {
            "=" | "<" | ">" | ":" | "≤" | "≥" | "≠" | "≈" | "≡" | "∼" | "∝" |
            "→" | "←" | "↔" | "⇒" | "⇐" | "⇔" | "↑" | "↓" |
            "∈" | "∉" | "⊂" | "⊃" | "⊆" | "⊇" | "⊥" | "∥" => Self::Relation,
            "+" | "-" | "−" | "×" | "·" | "÷" | "±" | "∓" |
            "∪" | "∩" | "∧" | "∨" | "∘" | "•" | "⋆" => Self::Binary,
            "(" | "[" | "{" => Self::Opening,
            ")" | "]" | "}" => Self::Closing,
            _ if !text.is_empty() && text.trim().is_empty() => Self::Space,
            _ => Self::Ordinary,
        }
    }

    fn spaced(self) -> bool {
        matches!(self, Self::Relation | Self::Binary)
    }
}

#[derive(Clone, Copy)]
enum Limits {
    Side,
    Below,
    Both,
}

struct Atom {
    box_: MathBox,
    kind: Kind,
    limits: Limits,
    named: bool,
}

impl Atom {
    fn text(text: &str) -> Self {
        let mut atom = Self::boxed(MathBox::text(text));
        atom.kind = Kind::of(text);
        atom
    }

    fn boxed(box_: MathBox) -> Self {
        Self { box_, kind: Kind::Ordinary, limits: Limits::Side, named: false }
    }

    fn named(name: &str, limits: Limits) -> Self {
        Self { named: true, limits, ..Self::text(name) }
    }
}

impl<'a> Parser<'a> {
    pub fn new(tex: &'a str, width: usize) -> Self {
        Self { rest: tex, width }
    }

    fn peek(&self) -> Token<'a> {
        token(self.rest.trim_start()).0
    }

    fn next(&mut self) -> Token<'a> {
        let (next, rest) = token(self.rest.trim_start());
        self.rest = rest;
        next
    }

    fn take(&mut self, expected: Token<'_>) -> bool {
        if self.peek() == expected {
            self.next();
            true
        } else {
            false
        }
    }

    pub fn parse(mut self) -> Option<MathBox> {
        let rows = self.equations(0, false)?;
        (self.peek() == Token::End).then_some(())?;
        self.center(rows)
    }

    fn equations(&mut self, depth: usize, script: bool) -> Option<Vec<Vec<MathBox>>> {
        let mut rows = Vec::new();
        loop {
            let mut columns = Vec::new();
            loop {
                columns.push(self.sequence(depth, script, None)?);
                if !self.take(Token::Char('&')) {
                    break;
                }
            }
            let more = self.take(Token::Command("\\"));
            // A final TeX row separator can terminate the last equation.
            if !rows.is_empty() && !more && columns.len() == 1 && columns[0].rows == [""] {
                break;
            }
            rows.push(columns);
            if !more {
                break;
            }
        }
        Some(rows)
    }

    fn sequence(&mut self, depth: usize, script: bool, closing: Option<char>) -> Option<MathBox> {
        if depth > MAX_DEPTH {
            return None;
        }
        let mut parts = Vec::new();
        let mut used = 0;
        let mut previous: Option<Kind> = None;
        let mut previous_named = false;
        let mut previous_alphanumeric = false;
        loop {
            let had_space = self.rest.starts_with(char::is_whitespace);
            let next = self.peek();
            if matches!(next, Token::End | Token::Char('}' | '&') | Token::Command("\\" | "end" | "right"))
                || closing.is_some_and(|ch| next == Token::Char(ch))
            {
                break;
            }
            let mut atom = self.atom(depth, script)?;
            // A leading sign or a sign following an operator is unary.
            if atom.kind == Kind::Binary && matches!(next, Token::Char('+' | '-' | '−') | Token::Command("pm" | "mp"))
                && matches!(previous, None | Some(Kind::Opening | Kind::Relation | Kind::Binary))
            {
                atom.kind = Kind::Ordinary;
            }
            if atom.box_.width == 0 && atom.box_.rows[0].is_empty() {
                continue;
            }
            let first_alphanumeric = atom.box_.rows[atom.box_.baseline].chars().next().is_some_and(char::is_alphanumeric);
            let separated = !script && previous.is_some_and(|kind: Kind| {
                kind != Kind::Space && atom.kind != Kind::Space && (
                    (kind.spaced() && atom.kind != Kind::Closing)
                    || (atom.kind.spaced() && kind != Kind::Opening)
                )
            });
            let operator_space = (atom.named && previous_alphanumeric)
                || (previous_named && (had_space && first_alphanumeric
                    || matches!(next, Token::Char(c) if c.is_alphanumeric())
                    || matches!(next, Token::Command(name) if name != "left" && name.starts_with(char::is_alphabetic))));
            if separated || (operator_space && atom.kind != Kind::Space && previous != Some(Kind::Space)) {
                parts.push(MathBox::text(" "));
                used += 1;
            }
            previous = Some(atom.kind);
            previous_named = atom.named;
            previous_alphanumeric = atom.box_.rows[atom.box_.baseline].chars().last().is_some_and(char::is_alphanumeric);
            used += atom.box_.width;
            if used > self.width {
                return None;
            }
            parts.push(atom.box_);
        }
        MathBox::join(parts, self.width)
    }

    fn atom(&mut self, depth: usize, script: bool) -> Option<Atom> {
        let mut atom = self.argument(depth, script)?;
        let (mut upper, mut lower) = (None, None);
        loop {
            match self.peek() {
                Token::Command("limits" | "nolimits") => {
                    self.next();
                }
                Token::Char('^' | '_') => {
                    let target = if self.next() == Token::Char('^') { &mut upper } else { &mut lower };
                    if target.is_some() {
                        return None;
                    }
                    *target = Some(self.argument(depth + 1, true)?.box_);
                }
                _ => break,
            }
        }
        atom.box_ = match atom.limits {
            Limits::Side => atom.box_.side_scripts(upper, lower, self.width)?,
            Limits::Both => MathBox::stack(upper, atom.box_, lower),
            Limits::Below => MathBox::stack(None, atom.box_, lower).side_scripts(upper, None, self.width)?,
        };
        Some(atom)
    }

    // An unbraced argument is one token or a command with its own arguments.
    fn argument(&mut self, depth: usize, script: bool) -> Option<Atom> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.next() {
            Token::Char('{') => {
                let value = self.sequence(depth + 1, script, None)?;
                self.take(Token::Char('}')).then_some(Atom::boxed(value))
            }
            Token::Char('}' | '^' | '_' | '&') | Token::End => None,
            Token::Char('-') if !script => Some(Atom::text("−")),
            Token::Char(c) => Some(Atom::text(c.encode_utf8(&mut [0; 4]))),
            Token::Command(name) => self.command(name, depth + 1, script),
        }
    }

    fn command(&mut self, name: &str, depth: usize, script: bool) -> Option<Atom> {
        let boxed = match name {
            "frac" | "dfrac" | "tfrac" => MathBox::fraction(
                self.argument(depth, script)?.box_, self.argument(depth, script)?.box_,
            ),
            "sqrt" => {
                let index = if self.take(Token::Char('[')) {
                    let index = self.sequence(depth, true, Some(']'))?;
                    self.take(Token::Char(']')).then_some(())?;
                    Some(index)
                } else {
                    None
                };
                self.argument(depth, script)?.box_.root(index, self.width)?
            }
            "text" | "mathrm" => MathBox::text(one_line(&text_of(self.raw_group()?))),
            "operatorname" => {
                self.take(Token::Char('*'));
                return Some(Atom::named(&one_line(&text_of(self.raw_group()?)), Limits::Side));
            }
            "mathbf" | "bm" | "boldsymbol" => self.argument(depth, script)?.box_,
            "vec" | "hat" => {
                let mut value = self.argument(depth, script)?.box_;
                let row = &mut value.rows[value.baseline];
                *row = accent_command(name, row);
                value
            }
            "left" => {
                let open = self.delimiter()?;
                let value = self.sequence(depth, script, None)?;
                self.take(Token::Command("right")).then_some(())?;
                let close = self.delimiter()?;
                value.enclose(open, close, self.width)?
            }
            "begin" => self.environment(depth, script)?,
            "end" | "right" | "\\" | "" => return None,
            "limits" | "nolimits" => MathBox::text(""),
            name if OPERATORS.contains(&name) => {
                let limits = if matches!(name, "lim" | "max" | "min" | "sup" | "inf") {
                    Limits::Below
                } else {
                    Limits::Side
                };
                return Some(Atom::named(name, limits));
            }
            _ => {
                if let Some(glyph) = big_operator(name) {
                    return Some(Atom { limits: Limits::Both, ..Atom::text(glyph) });
                }
                if let Some(escaped) = name.chars().next().filter(|_| name.chars().count() == 1).and_then(math_escape) {
                    return Some(Atom::text(&escaped));
                }
                let replacement = symbol(name);
                if replacement.is_empty() {
                    return Some(Atom::text(&one_line(&format!("\\{name}"))));
                }
                return Some(Atom::text(replacement));
            }
        };
        (boxed.width <= self.width).then_some(Atom::boxed(boxed))
    }

    fn delimiter(&mut self) -> Option<&'static str> {
        Some(match self.next() {
            Token::Char('.') => "",
            Token::Char('(') => "(",
            Token::Char(')') => ")",
            Token::Char('[') => "[",
            Token::Char(']') => "]",
            Token::Command("{" | "lbrace") => "{",
            Token::Command("}" | "rbrace") => "}",
            Token::Char('|') | Token::Command("vert" | "lvert" | "rvert") => "|",
            Token::Char('‖') | Token::Command("|" | "Vert" | "lVert" | "rVert") => "‖",
            Token::Char('<' | '⟨') | Token::Command("langle") => "⟨",
            Token::Char('>' | '⟩') | Token::Command("rangle") => "⟩",
            _ => return None,
        })
    }

    fn raw_group(&mut self) -> Option<&'a str> {
        self.take(Token::Char('{')).then_some(())?;
        let start = self.rest;
        let mut nesting = 1;
        loop {
            let before = self.rest;
            let (next, rest) = token(before);
            self.rest = rest;
            match next {
                Token::Char('{') => {
                    nesting += 1;
                    if nesting > MAX_DEPTH {
                        return None;
                    }
                }
                Token::Char('}') => {
                    nesting -= 1;
                    if nesting == 0 {
                        return Some(&start[..start.len() - before.len()]);
                    }
                }
                Token::End => return None,
                _ => {}
            }
        }
    }

    fn environment(&mut self, depth: usize, script: bool) -> Option<MathBox> {
        let name = self.raw_group()?.trim();
        if name.is_empty() {
            return None;
        }
        let (kind, open, close) = match name {
            "matrix" | "pmatrix" | "bmatrix" | "Bmatrix" | "vmatrix" | "Vmatrix" => {
                let (open, close) = env_delims(name);
                (Some(Grid::Centered), open, close)
            }
            "cases" => (Some(Grid::Cases), "{", ""),
            "aligned" | "align" | "align*" => (Some(Grid::Aligned), "", ""),
            "gather" => (None, "", ""),
            _ => {
                let body = self.inline_body(name)?;
                return Some(MathBox::text(one_line(&render_latex(body))));
            }
        };
        let rows = self.equations(depth, script)?;
        self.take(Token::Command("end")).then_some(())?;
        (self.raw_group()?.trim() == name).then_some(())?;
        match kind {
            Some(kind) => grid(rows, kind, self.width)?.enclose(open, close, self.width),
            None => self.center(rows),
        }
    }

    // Unknown environments use the existing inline renderer. Scan their
    // structure without applying display width or display command rules.
    fn inline_body(&mut self, name: &'a str) -> Option<&'a str> {
        let start = self.rest;
        let mut environments = vec![(name, 0)];
        let mut braces = 0usize;
        loop {
            let before = self.rest;
            let (next, rest) = token(before);
            self.rest = rest;
            match next {
                Token::Char('{') => braces += 1,
                Token::Char('}') => braces = braces.checked_sub(1)?,
                Token::Command("begin") => {
                    let nested = self.raw_group()?.trim();
                    environments.push((nested, braces));
                }
                Token::Command("end") => {
                    let (expected, open_braces) = environments.pop()?;
                    if self.raw_group()?.trim() != expected || braces != open_braces {
                        return None;
                    }
                    if environments.is_empty() {
                        return Some(&start[..start.len() - before.len()]);
                    }
                }
                Token::End => return None,
                _ => {}
            }
            // The inline renderer can revisit nested arguments on malformed
            // commands, so use a tighter bound for its permissive fallback.
            if braces + environments.len() > 12 {
                return None;
            }
        }
    }

    fn center(&self, rows: Vec<Vec<MathBox>>) -> Option<MathBox> {
        let equations = rows.into_iter().map(|cells| {
            let mut parts = Vec::new();
            for cell in cells {
                if !parts.is_empty() {
                    parts.push(MathBox::text("  "));
                }
                parts.push(cell);
            }
            MathBox::join(parts, self.width)
        }).collect::<Option<Vec<_>>>()?;
        Some(center_rows(equations))
    }
}

fn center_rows(mut equations: Vec<MathBox>) -> MathBox {
    if equations.len() == 1 {
        return equations.pop().expect("one equation");
    }
    let width = equations.iter().map(|equation| equation.width).max().unwrap_or(0);
    let rows: Vec<_> = equations.into_iter().flat_map(|equation| equation.pad(width, Align::Center).rows).collect();
    let baseline = (rows.len() - 1) / 2;
    MathBox { width, baseline, rows }
}

fn one_line(text: &str) -> String {
    text.replace(['\r', '\n', '\t'], " ")
}
