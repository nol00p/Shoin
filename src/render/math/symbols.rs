//! Unicode spellings shared by inline and display math.

#[derive(Clone, Copy)]
pub(in crate::render) enum MathAlphabet {
    DoubleStruck,
    Script,
    Fraktur,
}

impl MathAlphabet {
    pub(in crate::render) fn from_command(name: &str) -> Option<Self> {
        match name {
            "mathbb" => Some(Self::DoubleStruck),
            "mathcal" | "mathscr" => Some(Self::Script),
            "mathfrak" => Some(Self::Fraktur),
            _ => None,
        }
    }

    pub(in crate::render) fn render(self, argument: &str) -> Option<String> {
        let letters: String = argument.chars()
            .filter(|c| !c.is_ascii_whitespace())
            .map(|c| self.letter(c))
            .collect::<Option<_>>()?;
        (!letters.is_empty()).then_some(letters)
    }

    fn letter(self, c: char) -> Option<char> {
        use MathAlphabet::*;
        // The Letterlike Symbols fill the holes in the alphanumeric ranges.
        let glyph = match (self, c) {
            (DoubleStruck, 'C') => 'ℂ',
            (DoubleStruck, 'H') => 'ℍ',
            (DoubleStruck, 'N') => 'ℕ',
            (DoubleStruck, 'P') => 'ℙ',
            (DoubleStruck, 'Q') => 'ℚ',
            (DoubleStruck, 'R') => 'ℝ',
            (DoubleStruck, 'Z') => 'ℤ',
            (Script, 'B') => 'ℬ',
            (Script, 'E') => 'ℰ',
            (Script, 'F') => 'ℱ',
            (Script, 'H') => 'ℋ',
            (Script, 'I') => 'ℐ',
            (Script, 'L') => 'ℒ',
            (Script, 'M') => 'ℳ',
            (Script, 'R') => 'ℛ',
            (Script, 'e') => 'ℯ',
            (Script, 'g') => 'ℊ',
            (Script, 'o') => 'ℴ',
            (Fraktur, 'C') => 'ℭ',
            (Fraktur, 'H') => 'ℌ',
            (Fraktur, 'I') => 'ℑ',
            (Fraktur, 'R') => 'ℜ',
            (Fraktur, 'Z') => 'ℨ',
            (DoubleStruck, 'A'..='Z') => return char::from_u32(0x1D538 + (c as u32 - 'A' as u32)),
            (DoubleStruck, 'a'..='z') => return char::from_u32(0x1D552 + (c as u32 - 'a' as u32)),
            (DoubleStruck, '0'..='9') => return char::from_u32(0x1D7D8 + (c as u32 - '0' as u32)),
            (Script, 'A'..='Z') => return char::from_u32(0x1D49C + (c as u32 - 'A' as u32)),
            (Script, 'a'..='z') => return char::from_u32(0x1D4B6 + (c as u32 - 'a' as u32)),
            (Fraktur, 'A'..='Z') => return char::from_u32(0x1D504 + (c as u32 - 'A' as u32)),
            (Fraktur, 'a'..='z') => return char::from_u32(0x1D51E + (c as u32 - 'a' as u32)),
            _ => return None,
        };
        Some(glyph)
    }
}

pub(in crate::render) fn symbol(name: &str) -> &'static str {
    match name {
        "nabla" => "∇",
        "partial" => "∂",
        "oint" => "∮",
        "oiint" => "∯",
        "iint" => "∬",
        "iiint" => "∭",
        "int" => "∫",
        "sum" => "∑",
        "prod" => "∏",
        "coprod" => "∐",
        "rightarrow" | "to" => "→",
        "leftarrow" | "gets" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow" | "implies" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" | "iff" => "⇔",
        "downarrow" => "↓",
        "uparrow" => "↑",
        "times" => "×",
        "cdot" => "·",
        "div" => "÷",
        "pm" => "±",
        "mp" => "∓",
        "neq" | "ne" => "≠",
        "leq" | "le" => "≤",
        "geq" | "ge" => "≥",
        "approx" => "≈",
        "equiv" => "≡",
        "sim" => "∼",
        "propto" => "∝",
        "infty" => "∞",
        "hbar" => "ℏ",
        "forall" => "∀",
        "exists" => "∃",
        "nexists" => "∄",
        "in" => "∈",
        "notin" => "∉",
        "subset" => "⊂",
        "supset" => "⊃",
        "subseteq" => "⊆",
        "supseteq" => "⊇",
        "cap" => "∩",
        "cup" => "∪",
        "land" | "wedge" => "∧",
        "lor" | "vee" => "∨",
        "neg" | "lnot" => "¬",
        "circ" => "∘",
        "bullet" => "•",
        "star" => "⋆",
        "perp" => "⊥",
        "parallel" => "∥",
        "dots" | "ldots" => "…",
        "cdots" => "⋯",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "quad" => "  ",
        "qquad" => "    ",
        // Greek lowercase
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ϵ",
        "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "varpi" => "ϖ",
        "rho" => "ρ",
        "varrho" => "ϱ",
        "sigma" => "σ",
        "varsigma" => "ς",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "ϕ",
        "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        // Greek uppercase
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        _ => "",
    }
}

/// Operator names, written upright as themselves (`\sin` → `sin`).
pub(in crate::render) const OPERATORS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh",
    "tanh", "coth", "log", "ln", "lg", "exp", "lim", "liminf", "limsup", "max", "min", "sup",
    "inf", "det", "dim", "ker", "deg", "gcd", "arg", "hom", "Pr", "mod", "bmod",
];

/// `value` with a combining `mark` on its first character.
fn accent(value: &str, mark: char) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => format!("{first}{mark}{}", chars.as_str()),
        None => String::new(),
    }
}

/// The delimiters a matrix environment is drawn between.
pub(in crate::render) fn env_delims(env: &str) -> (&'static str, &'static str) {
    match env {
        "vmatrix" => ("|", "|"),
        "Vmatrix" => ("‖", "‖"),
        "pmatrix" => ("(", ")"),
        "bmatrix" => ("[", "]"),
        "Bmatrix" => ("{", "}"),
        _ => ("", ""),
    }
}

/// The characters that `\` escapes into themselves, in math and in text.
const SPECIALS: &str = "{}&%$#_";

/// The text of a `\text{…}`-style argument: escaped specials become
/// themselves and the spacing commands become spaces.
pub(in crate::render) fn text_of(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value.chars().peekable();
    while let Some(c) = rest.next() {
        match (c, rest.peek().copied()) {
            ('\\', Some(n)) if SPECIALS.contains(n) => {
                out.push(n);
                rest.next();
            }
            ('\\', Some(',' | ' ' | ';')) => {
                out.push(' ');
                rest.next();
            }
            ('\\', Some('!')) => {
                rest.next();
            }
            _ => out.push(c),
        }
    }
    out
}

pub(in crate::render) fn superscript_char(ch: char) -> Option<char> {
    match ch {
        '0' => Some('⁰'), '1' => Some('¹'), '2' => Some('²'), '3' => Some('³'), '4' => Some('⁴'),
        '5' => Some('⁵'), '6' => Some('⁶'), '7' => Some('⁷'), '8' => Some('⁸'), '9' => Some('⁹'),
        '+' => Some('⁺'), '-' => Some('⁻'), '=' => Some('⁼'), '(' => Some('⁽'), ')' => Some('⁾'),
        'a' => Some('ᵃ'), 'b' => Some('ᵇ'), 'c' => Some('ᶜ'), 'd' => Some('ᵈ'), 'e' => Some('ᵉ'),
        'f' => Some('ᶠ'), 'g' => Some('ᵍ'), 'h' => Some('ʰ'), 'i' => Some('ⁱ'), 'j' => Some('ʲ'),
        'k' => Some('ᵏ'), 'l' => Some('ˡ'), 'm' => Some('ᵐ'), 'n' => Some('ⁿ'), 'o' => Some('ᵒ'),
        'p' => Some('ᵖ'), 'r' => Some('ʳ'), 's' => Some('ˢ'), 't' => Some('ᵗ'), 'u' => Some('ᵘ'),
        'v' => Some('ᵛ'), 'w' => Some('ʷ'), 'x' => Some('ˣ'), 'y' => Some('ʸ'), 'z' => Some('ᶻ'),
        'A' => Some('ᴬ'), 'B' => Some('ᴮ'), 'D' => Some('ᴰ'), 'E' => Some('ᴱ'), 'G' => Some('ᴳ'),
        'H' => Some('ᴴ'), 'I' => Some('ᴵ'), 'J' => Some('ᴶ'), 'K' => Some('ᴷ'), 'L' => Some('ᴸ'),
        'M' => Some('ᴹ'), 'N' => Some('ᴺ'), 'O' => Some('ᴼ'), 'P' => Some('ᴾ'), 'R' => Some('ᴿ'),
        'T' => Some('ᵀ'), 'U' => Some('ᵁ'), 'V' => Some('ⱽ'), 'W' => Some('ᵂ'),
        'α' => Some('ᵅ'), 'β' => Some('ᵝ'), 'γ' => Some('ᵞ'), 'δ' => Some('ᵟ'),
        'ϵ' | 'ε' => Some('ᵋ'), 'θ' => Some('ᶿ'), 'ι' => Some('ᶥ'),
        'ϕ' | 'φ' => Some('ᵠ'), 'χ' => Some('ᵡ'), _ => None,
    }
}

pub(in crate::render) fn subscript_char(ch: char) -> Option<char> {
    match ch {
        '0' => Some('₀'), '1' => Some('₁'), '2' => Some('₂'), '3' => Some('₃'), '4' => Some('₄'),
        '5' => Some('₅'), '6' => Some('₆'), '7' => Some('₇'), '8' => Some('₈'), '9' => Some('₉'),
        '+' => Some('₊'), '-' => Some('₋'), '=' => Some('₌'), '(' => Some('₍'), ')' => Some('₎'),
        'a' => Some('ₐ'), 'e' => Some('ₑ'), 'h' => Some('ₕ'), 'i' => Some('ᵢ'), 'j' => Some('ⱼ'),
        'k' => Some('ₖ'), 'l' => Some('ₗ'), 'm' => Some('ₘ'), 'n' => Some('ₙ'), 'o' => Some('ₒ'),
        'p' => Some('ₚ'), 'r' => Some('ᵣ'), 's' => Some('ₛ'), 't' => Some('ₜ'), 'u' => Some('ᵤ'),
        'v' => Some('ᵥ'), 'x' => Some('ₓ'),
        'β' => Some('ᵦ'), 'γ' => Some('ᵧ'), 'ρ' => Some('ᵨ'),
        'ϕ' | 'φ' => Some('ᵩ'), 'χ' => Some('ᵪ'), _ => None,
    }
}


pub(in crate::render) fn accent_command(name: &str, value: &str) -> String {
    match (name, value) {
        // Precomposed, so the dot of i and j does not sit under the hat.
        ("hat", "i") => "î".into(),
        ("hat", "j") => "ĵ".into(),
        ("hat", _) => accent(value, '\u{0302}'),
        _ => accent(value, '\u{20D7}'),
    }
}

pub(super) fn big_operator(name: &str) -> Option<&'static str> {
    Some(match name {
        "sum" | "prod" | "coprod" => symbol(name),
        "bigcup" => "⋃",
        "bigcap" => "⋂",
        "bigoplus" => "⨁",
        "bigotimes" => "⨂",
        _ => return None,
    })
}

/// A spacing command or escaped math character after a backslash.
pub(in crate::render) fn math_escape(ch: char) -> Option<String> {
    Some(match ch {
        ',' | ' ' => " ".into(),
        ';' => "  ".into(),
        '!' => String::new(),
        '|' => "‖".into(),
        ch if SPECIALS.contains(ch) => ch.to_string(),
        _ => return None,
    })
}
