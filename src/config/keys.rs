//! Key string parser. SPEC.md §8.
//!
//! Grammar:
//!   sequence  := key+
//!   key       := "<" modifier* named ">" | CHAR
//!   modifier  := "C-" | "M-" | "S-"
//!   named     := "Esc" | "CR" | "Tab" | "Space" | "BS" | "Del"
//!              | "Up" | "Down" | "Left" | "Right"
//!              | "Home" | "End" | "PageUp" | "PageDown"
//!              | "leader" | CHAR
//!
//! Examples: "gg" → [g, g] · "<C-s>" → [ctrl+s] · "<leader>w" → [leader, w]

use anyhow::{bail, Result};

use crate::input::keymap::KeySeq;
use crate::input::pending::{Key, KeyCode};

/// Parse a config key string. `leader` substitutes for `<leader>`.
pub fn parse_seq(s: &str, leader: &str) -> Result<KeySeq> {
    let mut out = KeySeq::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            let end = (i + 1..chars.len())
                .find(|&j| chars[j] == '>')
                .ok_or_else(|| anyhow::anyhow!("unclosed `<` in key {s:?}"))?;
            let token: String = chars[i + 1..end].iter().collect();
            if token.eq_ignore_ascii_case("leader") {
                out.extend(parse_seq(leader, "")?);
            } else {
                out.push(parse_bracketed(&token)?);
            }
            i = end + 1;
        } else {
            out.push(Key::char(chars[i]));
            i += 1;
        }
    }
    if out.is_empty() {
        bail!("empty key sequence");
    }
    Ok(out)
}

/// Parse the inside of a `<…>` token: modifier prefixes then a named key/char.
fn parse_bracketed(token: &str) -> Result<Key> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut rest = token;
    loop {
        let lower = rest.to_ascii_lowercase();
        if let Some(r) = lower.strip_prefix("c-") {
            ctrl = true;
            rest = &rest[rest.len() - r.len()..];
        } else if let Some(r) = lower.strip_prefix("m-") {
            alt = true;
            rest = &rest[rest.len() - r.len()..];
        } else if let Some(r) = lower.strip_prefix("s-") {
            shift = true;
            rest = &rest[rest.len() - r.len()..];
        } else {
            break;
        }
    }
    let code = named_key(rest)?;
    Ok(Key { code, ctrl, alt, shift })
}

fn named_key(name: &str) -> Result<KeyCode> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        // Single character.
        return Ok(KeyCode::Char(c));
    }
    Ok(match name.to_ascii_lowercase().as_str() {
        "esc" => KeyCode::Esc,
        "cr" | "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "bs" | "backspace" => KeyCode::Backspace,
        "del" | "delete" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        other => bail!("unknown key name {other:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_grammar_examples() {
        assert_eq!(parse_seq("gg", " ").unwrap(), vec![Key::char('g'), Key::char('g')]);

        let cs = parse_seq("<C-s>", " ").unwrap();
        assert_eq!(cs.len(), 1);
        assert!(cs[0].ctrl && cs[0].code == KeyCode::Char('s'));

        let lw = parse_seq("<leader>w", " ").unwrap();
        assert_eq!(lw, vec![Key::char(' '), Key::char('w')]);

        assert_eq!(parse_seq("<CR>", " ").unwrap()[0].code, KeyCode::Enter);
        assert!(parse_seq("<C->", " ").is_err());
        assert!(parse_seq("<Bogus>", " ").is_err());
    }
}
