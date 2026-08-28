//! Syntax highlighting inside fenced code. SPEC.md §5.3.
//!
//! The extension seam §5.3 reserved, filled without a dependency. `syntect`
//! and `tree-sitter` both carry a grammar database — megabytes of binary and a
//! startup load — to answer a question this editor asks only about the handful
//! of lines a fenced block occupies, and both come with their own theme model
//! that would have to be mapped back onto `[theme]` anyway. So this is a small
//! table-driven lexer: one `Spec` per language, one scanner over all of them.
//!
//! What it deliberately does NOT do: parse. It has no notion of scope, type
//! inference or macro expansion, and it will color a keyword inside an
//! identifier-shaped context the same way everywhere. For prose interrupted by
//! ten lines of code that is the right trade — the reader wants strings,
//! comments and keywords to separate, not a compiler.
//!
//! The one thing that DOES cross a line boundary is `Cont` — an open block
//! comment or triple-quoted string. It rides in `Carry::InFence` beside the
//! fence delimiter, so the block cache's incremental rescan covers it for free.

use std::ops::Range;

/// What a run of characters is, in the only granularity a theme has colors for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Comment,
    /// A string or character literal, quotes included.
    Str,
    /// A number, or a language constant (`true`, `nil`, `None`) — both read as
    /// literal values and Tokyo Night gives them one color.
    Literal,
    Keyword,
    /// A type name, and in a key/value language the key.
    Type,
    /// An identifier used as a call.
    Function,
    /// Operators and delimiters.
    Punct,
}

/// The lexer state entering a line. Small and `Copy` on purpose: it is stored
/// per line in the block cache's carry and compared on every incremental
/// rescan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cont {
    #[default]
    None,
    /// Inside a `/* … */`-style block comment.
    Block,
    /// Inside a `"""…"""` / `'''…'''` string opened with this quote character.
    Triple(char),
}

/// One classified run, in CHAR indices — the same coordinates `StyledSpan`
/// uses, so the caller can hand these straight to the styler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub class: Class,
}

/// A language, identified by a fence's info string.
///
/// A closed enum rather than a `&'static Spec`: this rides inside `BlockKind`,
/// which is compared for cache validity once per line per sync, and comparing
/// two discriminants is free where comparing two keyword tables is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    C,
    Cpp,
    Go,
    Java,
    JavaScript,
    TypeScript,
    Python,
    Ruby,
    Lua,
    Shell,
    Sql,
    Json,
    Yaml,
    Toml,
    Html,
    Css,
}

impl Lang {
    /// Resolve a fence's info string. Only the first word counts — `rust,no_run`
    /// and `python title="x"` are both common — and the match is
    /// case-insensitive. An unknown language returns `None` and its block
    /// renders exactly as it did before this module existed.
    pub fn from_info(info: &str) -> Option<Lang> {
        let word = info
            .trim()
            .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
            .next()
            .unwrap_or("")
            .trim_start_matches('.')
            .to_ascii_lowercase();
        Some(match word.as_str() {
            "rust" | "rs" => Lang::Rust,
            "c" | "h" => Lang::C,
            "cpp" | "c++" | "cc" | "cxx" | "hpp" => Lang::Cpp,
            "go" | "golang" => Lang::Go,
            "java" | "kotlin" | "kt" => Lang::Java,
            "javascript" | "js" | "mjs" | "cjs" | "jsx" | "node" => Lang::JavaScript,
            "typescript" | "ts" | "tsx" => Lang::TypeScript,
            "python" | "py" => Lang::Python,
            "ruby" | "rb" => Lang::Ruby,
            "lua" => Lang::Lua,
            "sh" | "bash" | "zsh" | "shell" | "console" | "fish" => Lang::Shell,
            "sql" | "postgres" | "postgresql" | "mysql" | "sqlite" => Lang::Sql,
            "json" | "jsonc" | "json5" => Lang::Json,
            "yaml" | "yml" => Lang::Yaml,
            "toml" | "ini" | "cfg" => Lang::Toml,
            "html" | "xml" | "svg" | "vue" | "svelte" => Lang::Html,
            "css" | "scss" | "sass" | "less" => Lang::Css,
            _ => return None,
        })
    }

    fn spec(self) -> &'static Spec {
        match self {
            Lang::Rust => &RUST,
            Lang::C => &C,
            Lang::Cpp => &CPP,
            Lang::Go => &GO,
            Lang::Java => &JAVA,
            Lang::JavaScript => &JAVASCRIPT,
            Lang::TypeScript => &TYPESCRIPT,
            Lang::Python => &PYTHON,
            Lang::Ruby => &RUBY,
            Lang::Lua => &LUA,
            Lang::Shell => &SHELL,
            Lang::Sql => &SQL,
            Lang::Json => &JSON,
            Lang::Yaml => &YAML,
            Lang::Toml => &TOML,
            Lang::Html => &HTML,
            Lang::Css => &CSS,
        }
    }
}

/// Everything the one scanner needs to know about a language.
struct Spec {
    /// Prefixes that comment out the rest of the line.
    line_comments: &'static [&'static str],
    /// Block comment delimiters, if the language has them.
    block_comment: Option<(&'static str, &'static str)>,
    /// Characters that open a string.
    quotes: &'static [char],
    /// Whether a tripled quote opens a string that may span lines.
    triple: bool,
    keywords: &'static [&'static str],
    types: &'static [&'static str],
    /// Literal-valued words: `true`, `nil`, `None`.
    constants: &'static [&'static str],
    /// An identifier beginning with an uppercase letter is a type.
    capital_types: bool,
    /// An identifier immediately followed by `(` is a call.
    call_paren: bool,
    /// Characters that, following a string or identifier, make it a KEY —
    /// `:` in YAML and JSON, `=` in TOML. Empty for everything else.
    key_before: &'static [char],
    /// Markup: the identifier after `<` or `</` is a tag name.
    tags: bool,
    /// Whether `'` delimits a CHARACTER literal rather than a string. It
    /// changes what an unmatched `'` means: in Rust `&'a str` and in C `it's`
    /// inside a comment, a lone quote is not the start of anything.
    char_quote: bool,
    /// Whether `-` is a word character. True for the languages whose names are
    /// hyphenated (`font-size`, `on-failure`); false everywhere else, where a
    /// `-` between two words is subtraction.
    hyphens: bool,
}

/// The default shape, so each language below states only what it differs in.
const BASE: Spec = Spec {
    line_comments: &[],
    block_comment: None,
    quotes: &['"'],
    triple: false,
    keywords: &[],
    types: &[],
    constants: &[],
    capital_types: false,
    call_paren: true,
    key_before: &[],
    tags: false,
    char_quote: false,
    hyphens: false,
};

/// Scan one line, given the state entering it. Returns the state leaving it.
///
/// Tokens cover only what is classified: whitespace and unremarkable
/// identifiers produce none, and the caller leaves those in the base code
/// color. That keeps the span list short for the common line.
pub fn scan(line: &str, lang: Lang, cont: Cont, out: &mut Vec<Token>) -> Cont {
    let spec = lang.spec();
    let ch: Vec<char> = line.chars().collect();
    let n = ch.len();
    let mut i = 0;

    // Finish what the previous line left open before anything else can match:
    // a `*/` inside a string is not a string, and vice versa.
    match cont {
        Cont::Block => {
            let close = spec.block_comment.map(|(_, c)| c).unwrap_or("*/");
            match find(&ch, 0, close) {
                Some(end) => {
                    push(out, 0..end, Class::Comment);
                    i = end;
                }
                None => {
                    push(out, 0..n, Class::Comment);
                    return Cont::Block;
                }
            }
        }
        Cont::Triple(q) => {
            let close: String = std::iter::repeat_n(q, 3).collect();
            match find(&ch, 0, &close) {
                Some(end) => {
                    push(out, 0..end, Class::Str);
                    i = end;
                }
                None => {
                    push(out, 0..n, Class::Str);
                    return Cont::Triple(q);
                }
            }
        }
        Cont::None => {}
    }

    // The last classified token, so a `(` or a `:` can revise what came before
    // it — that is how a call and a key are told from a plain identifier.
    let mut prev_punct = '\0';

    while i < n {
        let c = ch[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Only the token IMMEDIATELY before matters, so it is taken and cleared
        // in one move: `<div class` must see `<` for `div` and nothing for
        // `class`.
        let prev = std::mem::replace(&mut prev_punct, '\0');

        if spec.line_comments.iter().any(|p| starts(&ch, i, p)) {
            push(out, i..n, Class::Comment);
            return Cont::None;
        }

        if let Some((open, close)) = spec.block_comment {
            if starts(&ch, i, open) {
                match find(&ch, i + open.chars().count(), close) {
                    Some(end) => {
                        push(out, i..end, Class::Comment);
                        i = end;
                        continue;
                    }
                    None => {
                        push(out, i..n, Class::Comment);
                        return Cont::Block;
                    }
                }
            }
        }

        if spec.triple && spec.quotes.contains(&c) && ch.get(i + 1) == Some(&c) && ch.get(i + 2) == Some(&c) {
            let close: String = std::iter::repeat_n(c, 3).collect();
            match find(&ch, i + 3, &close) {
                Some(end) => {
                    push(out, i..end, Class::Str);
                    i = end;
                }
                None => {
                    push(out, i..n, Class::Str);
                    return Cont::Triple(c);
                }
            }
            continue;
        }

        if spec.quotes.contains(&c) {
            // A character literal is `'c'` or `'\n'` and nothing longer. Rust's
            // lifetimes are the reason this is a rule rather than a search for
            // the next quote: `&'a str` has TWO apostrophes on most lines, so
            // "scan to the next one" swallows the code between them.
            let end = if c == '\'' && spec.char_quote {
                char_literal_end(&ch, i)
            } else {
                string_end(&ch, i, c)
            };
            match end {
                Some(end) => {
                    let class = if spec.key_before.is_empty() || !key_follows(&ch, end, spec.key_before) {
                        Class::Str
                    } else {
                        Class::Type
                    };
                    push(out, i..end, class);
                    i = end;
                    continue;
                }
                // No closing quote on this line. For `'` that is the common
                // case, not a broken line: a Rust lifetime, an English
                // apostrophe in a shell comment's neighbour. Treat the quote as
                // punctuation and keep scanning rather than painting the rest
                // of the line as a string.
                None if c == '\'' => {
                    push(out, i..i + 1, Class::Punct);
                    i += 1;
                    continue;
                }
                None => {
                    push(out, i..n, Class::Str);
                    return Cont::None;
                }
            }
        }

        if c.is_ascii_digit() {
            let end = number_end(&ch, i);
            push(out, i..end, Class::Literal);
            i = end;
            continue;
        }

        if is_ident_start(c) {
            let mut end = i;
            while end < n && is_ident(ch[end], spec.hyphens) {
                end += 1;
            }
            let word: String = ch[i..end].iter().collect();
            let class = classify_word(&word, spec, &ch, end, prev);
            if let Some(class) = class {
                push(out, i..end, class);
            }
            i = end;
            continue;
        }

        // Everything left is an operator or a delimiter. Runs merge, so `=>` or
        // `!==` is one span rather than three.
        let start = i;
        while i < n && !ch[i].is_whitespace() && !is_ident_start(ch[i]) && !ch[i].is_ascii_digit() && !spec.quotes.contains(&ch[i]) {
            // Stop before anything that opens a comment or a string, so the
            // next turn of the loop can see it.
            if spec.line_comments.iter().any(|p| starts(&ch, i, p)) {
                break;
            }
            if spec.block_comment.is_some_and(|(o, _)| starts(&ch, i, o)) {
                break;
            }
            i += 1;
        }
        if i == start {
            i += 1;
        }
        prev_punct = ch[i - 1];
        push(out, start..i, Class::Punct);
    }

    Cont::None
}

/// A word's class, or `None` to leave it in the base code color.
fn classify_word(word: &str, spec: &Spec, ch: &[char], end: usize, prev_punct: char) -> Option<Class> {
    if spec.tags && (prev_punct == '<' || prev_punct == '/') {
        return Some(Class::Keyword);
    }
    if spec.keywords.contains(&word) {
        return Some(Class::Keyword);
    }
    if spec.constants.contains(&word) {
        return Some(Class::Literal);
    }
    if spec.types.contains(&word) {
        return Some(Class::Type);
    }
    if !spec.key_before.is_empty() && key_follows(ch, end, spec.key_before) {
        return Some(Class::Type);
    }
    if spec.capital_types && word.starts_with(|c: char| c.is_uppercase()) {
        return Some(Class::Type);
    }
    if spec.call_paren && next_nonspace(ch, end) == Some('(') {
        return Some(Class::Function);
    }
    // A macro is a call with a `!` in the way: `println!(…)`, `vec![…]`.
    if spec.call_paren && ch.get(end) == Some(&'!') && matches!(ch.get(end + 1), Some('(' | '[' | '{')) {
        return Some(Class::Function);
    }
    None
}

/// Whether the next non-space character makes what precedes it a key.
fn key_follows(ch: &[char], from: usize, keys: &[char]) -> bool {
    next_nonspace(ch, from).is_some_and(|c| keys.contains(&c))
}

fn next_nonspace(ch: &[char], from: usize) -> Option<char> {
    ch[from..].iter().find(|c| !c.is_whitespace()).copied()
}

/// One past the closing quote, or `None` if the line ends first. Backslash
/// escapes the next character.
fn string_end(ch: &[char], start: usize, quote: char) -> Option<usize> {
    let mut i = start + 1;
    while i < ch.len() {
        match ch[i] {
            '\\' => i += 2,
            c if c == quote => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// One past a numeric literal: `0xff_u8`, `1_000`, `3.14e-2`, `10u8`.
///
/// The subtle case is the dot. `1.max(2)` and `0..n` are a method call and a
/// range, not decimals, so a `.` extends the number only when a DIGIT follows
/// it — which is also why this cannot be "consume everything word-shaped".
fn number_end(ch: &[char], start: usize) -> usize {
    let n = ch.len();
    let radix = matches!(ch.get(start + 1), Some('x' | 'X' | 'b' | 'B' | 'o' | 'O')) && ch[start] == '0';
    let mut i = if radix { start + 2 } else { start };
    while i < n {
        let c = ch[i];
        // The `.` rides with the digits: what makes it part of the number is
        // the digit AFTER it, not the run before it.
        let decimal = c == '.' && !radix && ch.get(i + 1).is_some_and(|d| d.is_ascii_digit());
        if c == '_' || c.is_ascii_digit() || (radix && c.is_ascii_hexdigit()) || decimal {
            i += 1;
        } else if matches!(c, 'e' | 'E')
            && !radix
            && exponent_follows(ch, i + 1)
        {
            i += if matches!(ch.get(i + 1), Some('+' | '-')) { 2 } else { 1 };
        } else if c.is_alphabetic() {
            // A type suffix — `u8`, `f64`, `L`, `px`. It ends the number.
            while i < n && (ch[i].is_alphanumeric() || ch[i] == '_') {
                i += 1;
            }
            break;
        } else {
            break;
        }
    }
    i
}

/// Whether what follows an `e` is an exponent rather than the start of a word.
fn exponent_follows(ch: &[char], at: usize) -> bool {
    let at = if matches!(ch.get(at), Some('+' | '-')) { at + 1 } else { at };
    ch.get(at).is_some_and(|c| c.is_ascii_digit())
}

/// One past a character literal, or `None` if this quote does not open one.
fn char_literal_end(ch: &[char], start: usize) -> Option<usize> {
    match ch.get(start + 1)? {
        // `'\n'`, `'\u{1f}'` — an escape, so scan for the close, but only
        // within the span an escape can occupy.
        '\\' => {
            let limit = (start + 12).min(ch.len());
            (start + 2..limit).find(|i| ch[*i] == '\'').map(|i| i + 1)
        }
        '\'' => None,
        _ if ch.get(start + 2) == Some(&'\'') => Some(start + 3),
        _ => None,
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$' || c == '@'
}

fn is_ident(c: char, hyphens: bool) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || (hyphens && c == '-')
}

/// Whether `pat` sits at `at`.
fn starts(ch: &[char], at: usize, pat: &str) -> bool {
    pat.chars().enumerate().all(|(k, p)| ch.get(at + k) == Some(&p))
}

/// One past the first occurrence of `pat` at or after `from`.
fn find(ch: &[char], from: usize, pat: &str) -> Option<usize> {
    let len = pat.chars().count();
    (from..=ch.len().saturating_sub(len)).find(|i| starts(ch, *i, pat)).map(|i| i + len)
}

/// Append a token, merging with the previous one when they touch and agree —
/// an operator run split across turns of the loop should read as one span.
fn push(out: &mut Vec<Token>, range: Range<usize>, class: Class) {
    if range.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.class == class && last.range.end == range.start => last.range.end = range.end,
        _ => out.push(Token { range, class }),
    }
}

// ---------------------------------------------------------------------------
// The language table.
//
// Keyword lists are the words a reader scans FOR, not the language's full
// reserved set: control flow, declaration, visibility. Anything missing simply
// renders as ordinary code, which is why a short honest list beats a long
// half-remembered one.
// ---------------------------------------------------------------------------

const RUST: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "type",
        "unsafe", "use", "where", "while", "macro_rules",
    ],
    types: &[
        "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8",
        "u16", "u32", "u64", "u128", "usize", "String", "Vec", "Option", "Result", "Box",
    ],
    constants: &["true", "false", "None", "Some", "Ok", "Err"],
    capital_types: true,
    char_quote: true,
    ..BASE
};

const C: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern",
        "for", "goto", "if", "inline", "register", "return", "sizeof", "static", "struct",
        "switch", "typedef", "union", "volatile", "while",
    ],
    types: &[
        "char", "double", "float", "int", "long", "short", "signed", "unsigned", "void", "size_t",
        "bool",
    ],
    constants: &["NULL", "true", "false"],
    char_quote: true,
    ..BASE
};

const CPP: Spec = Spec {
    keywords: &[
        "auto", "break", "case", "catch", "class", "const", "constexpr", "continue", "default",
        "delete", "do", "else", "enum", "explicit", "extern", "for", "friend", "if", "inline",
        "namespace", "new", "operator", "private", "protected", "public", "return", "sizeof",
        "static", "struct", "switch", "template", "this", "throw", "try", "typedef", "typename",
        "union", "using", "virtual", "volatile", "while",
    ],
    capital_types: true,
    ..C
};

const GO: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '`', '\''],
    keywords: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough",
        "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range",
        "return", "select", "struct", "switch", "type", "var",
    ],
    types: &[
        "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int", "int8",
        "int16", "int32", "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64",
        "uintptr", "any",
    ],
    constants: &["true", "false", "nil", "iota"],
    char_quote: true,
    ..BASE
};

const JAVA: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "abstract", "assert", "break", "case", "catch", "class", "const", "continue", "data",
        "default", "do", "else", "enum", "extends", "final", "finally", "for", "fun", "if",
        "implements", "import", "instanceof", "interface", "native", "new", "object", "package",
        "private", "protected", "public", "return", "static", "super", "switch", "synchronized",
        "this", "throw", "throws", "try", "val", "var", "when", "while",
    ],
    types: &[
        "boolean", "byte", "char", "double", "float", "int", "long", "short", "void", "String",
    ],
    constants: &["true", "false", "null"],
    capital_types: true,
    char_quote: true,
    ..BASE
};

const JAVASCRIPT: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\'', '`'],
    keywords: &[
        "as", "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
        "default", "delete", "do", "else", "export", "extends", "finally", "for", "from",
        "function", "get", "if", "import", "in", "instanceof", "let", "new", "of", "return", "set",
        "static", "super", "switch", "this", "throw", "try", "typeof", "var", "void", "while",
        "yield",
    ],
    constants: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    capital_types: true,
    ..BASE
};

const TYPESCRIPT: Spec = Spec {
    keywords: &[
        "abstract", "as", "async", "await", "break", "case", "catch", "class", "const",
        "continue", "declare", "default", "delete", "do", "else", "enum", "export", "extends",
        "finally", "for", "from", "function", "get", "if", "implements", "import", "in",
        "instanceof", "interface", "keyof", "let", "namespace", "new", "of", "private",
        "protected", "public", "readonly", "return", "satisfies", "set", "static", "super",
        "switch", "this", "throw", "try", "type", "typeof", "var", "void", "while", "yield",
    ],
    types: &["any", "boolean", "never", "number", "object", "string", "symbol", "unknown"],
    ..JAVASCRIPT
};

const PYTHON: Spec = Spec {
    line_comments: &["#"],
    quotes: &['"', '\''],
    triple: true,
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
        "with", "yield",
    ],
    types: &[
        "bool", "bytes", "dict", "float", "frozenset", "int", "list", "set", "str", "tuple",
    ],
    constants: &["True", "False", "None", "self", "cls"],
    ..BASE
};

const RUBY: Spec = Spec {
    line_comments: &["#"],
    quotes: &['"', '\''],
    keywords: &[
        "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "elsif",
        "else", "end", "ensure", "for", "if", "in", "module", "next", "not", "or", "raise",
        "redo", "require", "require_relative", "rescue", "retry", "return", "self", "super",
        "then", "unless", "until", "when", "while", "yield",
    ],
    constants: &["true", "false", "nil"],
    capital_types: true,
    ..BASE
};

const LUA: Spec = Spec {
    line_comments: &["--"],
    block_comment: Some(("--[[", "]]")),
    quotes: &['"', '\''],
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in",
        "local", "not", "or", "repeat", "return", "then", "until", "while",
    ],
    constants: &["true", "false", "nil"],
    ..BASE
};

const SHELL: Spec = Spec {
    line_comments: &["#"],
    quotes: &['"', '\''],
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
        "in", "local", "readonly", "return", "select", "shift", "source", "then", "unset",
        "until", "while",
    ],
    types: &[
        "cat", "cd", "cp", "curl", "echo", "grep", "ls", "mkdir", "mv", "rm", "sed", "awk", "set",
        "git", "cargo", "make", "sudo",
    ],
    constants: &["true", "false"],
    call_paren: false,
    ..BASE
};

const SQL: Spec = Spec {
    line_comments: &["--"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "ALTER", "AND", "AS", "ASC", "BY", "CREATE", "DELETE", "DESC", "DISTINCT", "DROP",
        "EXISTS", "FROM", "GROUP", "HAVING", "IN", "INDEX", "INNER", "INSERT", "INTO", "JOIN",
        "LEFT", "LIKE", "LIMIT", "NOT", "OFFSET", "ON", "OR", "ORDER", "OUTER", "PRIMARY",
        "REFERENCES", "RIGHT", "SELECT", "SET", "TABLE", "UNION", "UPDATE", "VALUES", "VIEW",
        "WHERE", "WITH",
        "alter", "and", "as", "asc", "by", "create", "delete", "desc", "distinct", "drop",
        "exists", "from", "group", "having", "in", "index", "inner", "insert", "into", "join",
        "left", "like", "limit", "not", "offset", "on", "or", "order", "outer", "primary",
        "references", "right", "select", "set", "table", "union", "update", "values", "view",
        "where", "with",
    ],
    types: &[
        "BOOLEAN", "DATE", "FLOAT", "INT", "INTEGER", "JSONB", "NUMERIC", "SERIAL", "TEXT",
        "TIMESTAMP", "UUID", "VARCHAR",
        "boolean", "date", "float", "int", "integer", "jsonb", "numeric", "serial", "text",
        "timestamp", "uuid", "varchar",
    ],
    constants: &["NULL", "TRUE", "FALSE", "null", "true", "false"],
    ..BASE
};

const JSON: Spec = Spec {
    line_comments: &["//"],
    constants: &["true", "false", "null"],
    call_paren: false,
    key_before: &[':'],
    ..BASE
};

const YAML: Spec = Spec {
    line_comments: &["#"],
    quotes: &['"', '\''],
    constants: &["true", "false", "null", "yes", "no", "on", "off"],
    call_paren: false,
    key_before: &[':'],
    hyphens: true,
    ..BASE
};

const TOML: Spec = Spec {
    line_comments: &["#"],
    quotes: &['"', '\''],
    triple: true,
    constants: &["true", "false"],
    call_paren: false,
    key_before: &['='],
    ..BASE
};

const HTML: Spec = Spec {
    block_comment: Some(("<!--", "-->")),
    quotes: &['"', '\''],
    call_paren: false,
    key_before: &['='],
    tags: true,
    ..BASE
};

const CSS: Spec = Spec {
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    constants: &["inherit", "initial", "none", "unset", "auto"],
    key_before: &[':'],
    hyphens: true,
    ..BASE
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The classes a line produces, as `(text, class)` pairs — reads like the
    /// line itself, which is what makes these tests worth writing.
    fn classes(line: &str, lang: Lang) -> Vec<(String, Class)> {
        let mut out = Vec::new();
        scan(line, lang, Cont::None, &mut out);
        let ch: Vec<char> = line.chars().collect();
        out.into_iter()
            .map(|t| (ch[t.range.clone()].iter().collect(), t.class))
            .collect()
    }

    fn find_class(line: &str, lang: Lang, text: &str) -> Option<Class> {
        classes(line, lang).into_iter().find(|(t, _)| t == text).map(|(_, c)| c)
    }

    #[test]
    fn an_info_string_resolves_through_its_aliases() {
        assert_eq!(Lang::from_info("rust"), Some(Lang::Rust));
        assert_eq!(Lang::from_info("RS"), Some(Lang::Rust));
        // Attributes and comma lists are common and must not defeat the lookup.
        assert_eq!(Lang::from_info("rust,no_run"), Some(Lang::Rust));
        assert_eq!(Lang::from_info("python title=\"x\""), Some(Lang::Python));
        assert_eq!(Lang::from_info(""), None);
        assert_eq!(Lang::from_info("brainfuck"), None);
    }

    #[test]
    fn a_rust_line_separates_keyword_string_and_comment() {
        let got = classes("let s = \"hi\"; // done", Lang::Rust);
        assert_eq!(got[0], ("let".into(), Class::Keyword));
        assert!(got.contains(&("\"hi\"".into(), Class::Str)));
        assert!(got.contains(&("// done".into(), Class::Comment)));
    }

    /// A lifetime is not an unterminated character literal, and the rest of the
    /// line must not turn into a string.
    #[test]
    fn a_rust_lifetime_does_not_open_a_string() {
        let got = classes("fn f<'a>(x: &'a str) -> u8 { 1 }", Lang::Rust);
        assert!(got.iter().all(|(_, c)| *c != Class::Str), "{got:?}");
        assert_eq!(find_class("fn f<'a>(x: &'a str) -> u8 { 1 }", Lang::Rust, "str"), Some(Class::Type));
    }

    #[test]
    fn a_call_is_told_from_a_plain_identifier() {
        assert_eq!(find_class("compute(x)", Lang::Rust, "compute"), Some(Class::Function));
        assert_eq!(find_class("let compute = 1", Lang::Rust, "compute"), None);
    }

    #[test]
    fn numbers_keep_their_suffix_but_not_a_trailing_dot() {
        assert_eq!(find_class("let n = 0xff_u8;", Lang::Rust, "0xff_u8"), Some(Class::Literal));
        assert_eq!(find_class("3.14e-2 + 1", Lang::Rust, "3.14e-2"), Some(Class::Literal));
        // `1.foo()` — the dot is a field access.
        assert_eq!(find_class("1.max(2)", Lang::Rust, "1"), Some(Class::Literal));
    }

    /// A block comment that does not close carries to the next line, and the
    /// line that closes it starts in comment.
    #[test]
    fn a_block_comment_carries_across_lines() {
        let mut out = Vec::new();
        let cont = scan("/* one", Lang::C, Cont::None, &mut out);
        assert_eq!(cont, Cont::Block);
        assert_eq!(out[0].class, Class::Comment);

        let mut out = Vec::new();
        let cont = scan("still */ int x;", Lang::C, Cont::Block, &mut out);
        assert_eq!(cont, Cont::None);
        assert_eq!(out[0].class, Class::Comment);
        assert!(out.iter().any(|t| t.class == Class::Type));
    }

    #[test]
    fn a_python_docstring_carries_across_lines() {
        let mut out = Vec::new();
        assert_eq!(scan("x = \"\"\"open", Lang::Python, Cont::None, &mut out), Cont::Triple('"'));
        let mut out = Vec::new();
        assert_eq!(scan("still open", Lang::Python, Cont::Triple('"'), &mut out), Cont::Triple('"'));
        let mut out = Vec::new();
        assert_eq!(scan("done\"\"\"", Lang::Python, Cont::Triple('"'), &mut out), Cont::None);
    }

    #[test]
    fn a_key_value_language_colors_its_keys() {
        assert_eq!(find_class("  name: shoin", Lang::Yaml, "name"), Some(Class::Type));
        assert_eq!(find_class("\"a\": 1", Lang::Json, "\"a\""), Some(Class::Type));
        assert_eq!(find_class("edition = \"2021\"", Lang::Toml, "edition"), Some(Class::Type));
        // The VALUE is a string, not a key.
        assert_eq!(find_class("edition = \"2021\"", Lang::Toml, "\"2021\""), Some(Class::Str));
    }

    #[test]
    fn a_tag_name_reads_as_the_keyword_of_markup() {
        assert_eq!(find_class("<div class=\"a\">", Lang::Html, "div"), Some(Class::Keyword));
        assert_eq!(find_class("<div class=\"a\">", Lang::Html, "class"), Some(Class::Type));
    }

    /// Every token must lie inside the line and none may overlap — the styler
    /// hands these straight to a span cover.
    #[test]
    fn tokens_are_ordered_and_in_bounds() {
        let lines = [
            ("let x = f(\"a\", 'b', 12) /* c */;", Lang::Rust),
            ("# comment 'quoted' \"x\"", Lang::Shell),
            ("SELECT * FROM t WHERE a = 1;", Lang::Sql),
            ("", Lang::Rust),
            ("héllo = \"wörld\" # ünicode", Lang::Toml),
        ];
        for (line, lang) in lines {
            let mut out = Vec::new();
            scan(line, lang, Cont::None, &mut out);
            let n = line.chars().count();
            let mut end = 0;
            for t in &out {
                assert!(t.range.start >= end, "overlap in {line:?}: {out:?}");
                assert!(t.range.end <= n, "out of bounds in {line:?}: {out:?}");
                assert!(t.range.start < t.range.end, "empty token in {line:?}");
                end = t.range.end;
            }
        }
    }
}

