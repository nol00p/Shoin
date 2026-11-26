//! Expected byte ranges from `nvim --clean --headless` using searchpos and a
//! substitute that wraps the reported match. Unsupported atoms match in
//! Neovim and return a translation error here.

use super::*;

type Case = (&'static str, &'static str, &'static str, Result<Range<usize>, &'static str>);
const CASES: &[Case] = &[
    ("dot", "a界b", "a.b", Ok(0..5)),
    ("leading_star", "*abc", "^*abc", Ok(0..4)),
    ("star", "abbbc", "ab*c", Ok(0..5)),
    ("collection", "ac", "[a-c]", Ok(0..1)),
    ("negated", "界a", "[^a]", Ok(0..3)),
    ("anchors", "abc", "^abc$", Ok(0..3)),
    ("plus", "abbbc", "ab\\+c", Ok(0..5)),
    ("optional", "ac abc", "ab\\=c", Ok(0..2)),
    ("question", "abc", "ab\\?c", Ok(0..3)),
    ("count", "abbbc abbc", "ab\\{2,3}c", Ok(0..5)),
    ("lazy", "bbbb", "b\\{-1,3}", Ok(0..1)),
    ("capturing", "ab", "\\(ab\\)", Ok(0..2)),
    ("noncapturing", "ab", "\\%(ab\\)", Ok(0..2)),
    ("alt", "cb", "a\\|b", Ok(1..2)),
    ("bounds", "other the!", "\\<the\\>", Ok(6..9)),
    ("space", "x \ty", "\\s\\+", Ok(1..3)),
    ("not_space", " \té", "\\S", Ok(2..4)),
    ("digit", "１1", "\\d", Ok(3..4)),
    ("not_digit", "1３", "\\D", Ok(1..4)),
    ("word", "界_", "\\w", Ok(3..4)),
    ("not_word", "a界", "\\W", Ok(1..4)),
    ("alpha", "１Z", "\\a", Ok(3..4)),
    ("lower", "Äx", "\\l", Ok(2..3)),
    ("upper", "éA", "\\u", Ok(2..3)),
    ("hex", "zf", "\\x", Ok(1..2)),
    ("head", "1_", "\\h", Ok(1..2)),
    ("octal", "87", "\\o", Ok(1..2)),
    ("tab", "a\tb", "\\t", Ok(1..2)),
    ("esc", "a\x1bb", "\\e", Ok(1..2)),
    ("cr", "a\rb", "\\r", Ok(1..2)),
    ("very", "abbbc", "\\va(b+)c", Ok(0..5)),
    ("very_literal", "a.b", "\\Va.b", Ok(0..3)),
    ("nomagic", "axb", "\\Ma\\.b", Ok(0..3)),
    ("mode_switch", "a+bXX", "\\Va+b\\m.\\+", Ok(0..5)),
    ("case", "aABC", "Ab\\cc", Ok(1..4)),
    ("case_override", "AB ab", "\\cAb\\C", Ok(0..2)),
    ("markers", "abc", "a\\zsb\\zec", Ok(1..2)),
    ("multiline", "a\nb", "a\\nb", Ok(0..3)),
    ("backref", "aa", "\\(a\\)\\1", Err("pattern: \\1 not supported")),
    ("ahead", "ab", "a\\@=a", Err("pattern: \\@= not supported")),
    ("negative_ahead", "ab", "b\\@!a", Err("pattern: \\@! not supported")),
    ("behind", "ab", "a\\@<=b", Err("pattern: \\@<= not supported")),
    ("negative_behind", "ab", "b\\@<!a", Err("pattern: \\@<! not supported")),
    ("atomic", "ab", "a\\@>", Err("pattern: \\@> not supported")),
    ("very_nomagic_dot", "axb a.b", "\\Va\\.b", Ok(0..3)),
    ("nonmagic_anchors", "abc", "\\M^abc$", Ok(0..3)),
    ("reclose", "abb", "ab\\{2\\}", Ok(0..3)),
    ("bracket_n", "a\nb", "a[\\n]b", Ok(0..3)),
];

#[test]
fn vim_atoms_match_neovim() {
    for (name, text, syntax, expected) in CASES {
        let actual = Pattern::compile(syntax, false, None)
            .map(|pattern| pattern.matches(text, false).next().map(|found| found.span));
        match expected {
            Ok(range) => assert_eq!(actual, Ok(Some(range.clone())), "{name}: {syntax}"),
            Err(error) => assert_eq!(actual, Err(error.to_string()), "{name}: {syntax}"),
        }
    }
}

/// A `[` in a collection that does not start a `[:name:]` is literal, as
/// Vim's `get_char_class`: `[[:x]]` is `[`, `:` or `x`, then `]`.
#[test]
fn a_bracket_without_a_class_name_is_literal_in_a_collection() {
    // Neovim's `matchstrpos`.
    for (syntax, text, expected) in [
        ("[[:x]]", "q:]", Some(1..3)),
        ("[[:x]]", "[x", None),
        ("[[:al]", "q:]", Some(1..2)),
        ("[[:al]", "[x", Some(0..1)),
        ("[[:[:x]]", "q:]", Some(1..3)),
        ("[[:alpha:]:]", "[x", Some(1..2)),
        ("[[:alpha:]:]", "q:]", Some(0..1)),
    ] {
        let pattern = Pattern::compile(syntax, false, None).unwrap_or_else(|error| panic!("{syntax}: {error}"));
        assert_eq!(pattern.find_at(text, 0).map(|found| found.span), expected, "{syntax} on {text}");
    }
    let long = format!("[{}x]]", "[:".repeat(200_000));
    assert!(Pattern::compile(&long, false, None).is_ok());
}

#[test]
fn tilde_is_literal_last_replacement() {
    let pattern = Pattern::compile("~", false, Some("a.b")).unwrap();
    assert_eq!(pattern.char_ranges("axb a.b", None), vec![4..7]);
}

#[test]
fn a_multi_after_a_multi_is_an_error() {
    // Neovim says "E866: Misplaced +" for the last: an error too.
    assert!(Pattern::compile(r"x*\v+", false, None).is_err());
    for syntax in ["x**", r"x\{2}*", r"x\+\=", r"\vx+*"] {
        assert_eq!(Pattern::compile(syntax, false, None).err().as_deref(),
            Some("pattern: can't have a multi follow a multi"), "{syntax}");
    }
    for syntax in ["^**", r"\(*\)*", "x*y*", r"\v(x+)+"] {
        assert!(Pattern::compile(syntax, false, None).is_ok(), "{syntax}");
    }
}

#[test]
fn star_after_a_line_break_repeats_it_and_caret_after_zs_is_literal() {
    let pattern = Pattern::compile(r"a\n*b", false, None).unwrap();
    assert_eq!(pattern.find_at("a\n\nb", 0).map(|found| found.span), Some(0..4));
    // Neovim: `^` after `\zs` or `\ze` is a literal, after `\c` an anchor.
    let pattern = Pattern::compile(r"\zs^a", false, None).unwrap();
    assert_eq!(pattern.find_at("a ^a", 0).map(|found| found.span), Some(2..4));
    let pattern = Pattern::compile(r"\c^a", false, None).unwrap();
    assert_eq!(pattern.find_at("^a", 0), None);
}
