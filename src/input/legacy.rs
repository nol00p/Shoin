//! Keys as the rest of the editor expects them, whichever encoding the
//! terminal used.
//!
//! `main` asks for the kitty keyboard protocol, so a terminal that supports
//! it reports Ctrl/Alt chords unambiguously: `Alt-h` no longer splits into
//! `Esc` + `h`, and `Ctrl-h` is not Backspace. The same protocol also tells
//! apart keys that were ONE byte before, and people rely on some of those
//! being the same key. `normalize` folds them back, so every terminal behaves
//! the way a legacy one does, minus the ambiguity.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Fold a key event into its legacy meaning.
///
/// - `Ctrl-[`, `Ctrl-i` and `Ctrl-m` are the bytes for Esc, Tab and Enter on a
///   legacy terminal, and vim users leave Insert with `Ctrl-[`.
/// - A Shift chord on a letter arrives as the lowercase letter plus `SHIFT`
///   when the terminal does not report the shifted key; the editor binds the
///   letter's case, so `Alt-J` must not become `Alt-j`.
pub fn normalize(mut ev: KeyEvent) -> KeyEvent {
    let ctrl_only = ev.modifiers == KeyModifiers::CONTROL;
    match ev.code {
        KeyCode::Char('[') if ctrl_only => {
            ev.code = KeyCode::Esc;
            ev.modifiers = KeyModifiers::NONE;
        }
        KeyCode::Char('i') if ctrl_only => {
            ev.code = KeyCode::Tab;
            ev.modifiers = KeyModifiers::NONE;
        }
        KeyCode::Char('m') if ctrl_only => {
            ev.code = KeyCode::Enter;
            ev.modifiers = KeyModifiers::NONE;
        }
        KeyCode::Char(c) if ev.modifiers.contains(KeyModifiers::SHIFT) && c.is_lowercase() => {
            let mut upper = c.to_uppercase();
            if let (Some(u), None) = (upper.next(), upper.next()) {
                ev.code = KeyCode::Char(u);
            }
        }
        _ => {}
    }
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_bracket_is_esc() {
        let ev = normalize(key(KeyCode::Char('['), KeyModifiers::CONTROL));
        assert_eq!((ev.code, ev.modifiers), (KeyCode::Esc, KeyModifiers::NONE));
    }

    #[test]
    fn ctrl_i_and_ctrl_m_are_tab_and_enter() {
        let tab = normalize(key(KeyCode::Char('i'), KeyModifiers::CONTROL));
        let enter = normalize(key(KeyCode::Char('m'), KeyModifiers::CONTROL));
        assert_eq!(
            (tab.code, tab.modifiers),
            (KeyCode::Tab, KeyModifiers::NONE)
        );
        assert_eq!(
            (enter.code, enter.modifiers),
            (KeyCode::Enter, KeyModifiers::NONE)
        );
    }

    #[test]
    fn ctrl_h_stays_ctrl_h() {
        let ev = normalize(key(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(
            (ev.code, ev.modifiers),
            (KeyCode::Char('h'), KeyModifiers::CONTROL)
        );
    }

    #[test]
    fn shift_keeps_a_letters_case() {
        let ev = normalize(key(
            KeyCode::Char('j'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(ev.code, KeyCode::Char('J'));
        assert_eq!(ev.modifiers, KeyModifiers::ALT | KeyModifiers::SHIFT);
    }

    #[test]
    fn a_chord_with_more_than_ctrl_is_left_alone() {
        let ev = normalize(key(
            KeyCode::Char('['),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(ev.code, KeyCode::Char('['));
    }

    #[test]
    fn plain_keys_pass_through() {
        for code in [
            KeyCode::Char('x'),
            KeyCode::Char('X'),
            KeyCode::Esc,
            KeyCode::Backspace,
        ] {
            assert_eq!(normalize(key(code, KeyModifiers::NONE)).code, code);
        }
    }
}
