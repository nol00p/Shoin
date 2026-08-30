//! The built-in grammar, as a TABLE rather than as control flow. SPEC.md §7.2.
//!
//! Every Normal/Visual key resolves to a `Verb` here, and `Pending` is the state
//! machine that assembles verbs into commands (`2d3w`, `"aY`, `ci(`). Splitting
//! it this way is what makes the grammar remappable: a user binding produces the
//! same `Verb`s, so `[keys.normal]` can move an OPERATOR or a MOTION, not just
//! the handful of discrete actions the old overlay could reach.
//!
//! Two things are deliberately NOT here. Anything needing the viewport (`H`/`M`/
//! `L` resolve against the visible range) stays a `Motion` for `app` to finish,
//! and Insert mode is not a grammar at all — it is text entry with a few keys.

use super::action::{Action, Operator};
use super::pending::{Key, KeyCode};
use crate::text::motion::Motion;

/// What a key means before any count, operator, or argument is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    /// Move, or give a pending operator its range.
    Motion(Motion),
    /// `G` / `gg`: a count makes it an absolute line, else this motion.
    Goto(Motion),
    /// Waits for a motion, a text object, or its own key doubled (`dd`).
    Operator(Operator),
    /// `i` / `a` — waits for the object's key (`iw`, `a(`).
    Object { around: bool },
    /// `f` `F` `t` `T` — waits for the character to seek.
    Find { forward: bool, till: bool },
    /// `r` — waits for the replacement character.
    ReplaceChar,
    /// `"` — waits for a register name.
    Register,
    /// Owns the next key (`g`, `z`, `<C-w>`).
    Prefix(char),
    /// Anything with no operand.
    Act(Action),
}

/// Normal mode, first key.
pub fn normal(key: Key) -> Option<Verb> {
    if let Some(m) = motion(key) {
        return Some(Verb::Motion(m));
    }
    let ctrl = key.ctrl;
    Some(match (key.code, ctrl) {
        (KeyCode::Char('G'), false) => Verb::Goto(Motion::BufferEnd),
        (KeyCode::Char('d'), false) => Verb::Operator(Operator::Delete),
        (KeyCode::Char('c'), false) => Verb::Operator(Operator::Change),
        (KeyCode::Char('y'), false) => Verb::Operator(Operator::Yank),
        (KeyCode::Char('>'), false) => Verb::Operator(Operator::Indent),
        (KeyCode::Char('<'), false) => Verb::Operator(Operator::Outdent),
        (KeyCode::Char('"'), false) => Verb::Register,
        (KeyCode::Char('r'), false) => Verb::ReplaceChar,
        (KeyCode::Char('g'), false) => Verb::Prefix('g'),
        (KeyCode::Char('z'), false) => Verb::Prefix('z'),
        (KeyCode::Char('w'), true) => Verb::Prefix('w'),
        (KeyCode::Char('f'), false) => Verb::Find { forward: true, till: false },
        (KeyCode::Char('F'), false) => Verb::Find { forward: false, till: false },
        (KeyCode::Char('t'), false) => Verb::Find { forward: true, till: true },
        (KeyCode::Char('T'), false) => Verb::Find { forward: false, till: true },

        // Objects, which decay to Insert/Append with no operator pending:
        // `diw` takes an object, a bare `i` starts typing.
        (KeyCode::Char('i'), false) => Verb::Object { around: false },
        (KeyCode::Char('a'), false) => Verb::Object { around: true },
        (KeyCode::Char('I'), false) => Verb::Act(Action::InsertLineStart),
        (KeyCode::Char('A'), false) => Verb::Act(Action::AppendLineEnd),
        (KeyCode::Char('o'), false) => Verb::Act(Action::OpenBelow),
        (KeyCode::Char('O'), false) => Verb::Act(Action::OpenAbove),
        (KeyCode::Char('x'), false) => Verb::Act(Action::DeleteChar),
        (KeyCode::Char('X'), false) => Verb::Act(Action::DeleteCharBack),
        (KeyCode::Char('D'), false) => Verb::Act(Action::DeleteToEol),
        (KeyCode::Char('C'), false) => Verb::Act(Action::ChangeToEol),
        (KeyCode::Char('Y'), false) => Verb::Act(Action::YankLine),
        (KeyCode::Char('J'), false) => Verb::Act(Action::JoinLines),
        (KeyCode::Char('~'), false) => Verb::Act(Action::ToggleCase),
        (KeyCode::Char('p'), false) => Verb::Act(Action::PasteAfter),
        (KeyCode::Char('P'), false) => Verb::Act(Action::PasteBefore),
        (KeyCode::Char('v'), false) => Verb::Act(Action::Visual),
        (KeyCode::Char('V'), false) => Verb::Act(Action::VisualLine),
        (KeyCode::Char('u'), false) => Verb::Act(Action::Undo),
        (KeyCode::Char('r'), true) => Verb::Act(Action::Redo),
        (KeyCode::Char('s'), true) => Verb::Act(Action::Save),
        // Both spellings, as vim documents them: terminals disagree about
        // whether the chord arrives as `^` or as the `6` the key is printed
        // with, and a reader should not have to know which one theirs sends.
        (KeyCode::Char('^'), true) => Verb::Act(Action::AlternateBuffer),
        (KeyCode::Char('6'), true) => Verb::Act(Action::AlternateBuffer),
        (KeyCode::Char('/'), false) => Verb::Act(Action::SearchForward),
        (KeyCode::Char('?'), false) => Verb::Act(Action::SearchBackward),
        (KeyCode::Char('n'), false) => Verb::Act(Action::SearchNext { reverse: false }),
        (KeyCode::Char('N'), false) => Verb::Act(Action::SearchNext { reverse: true }),
        (KeyCode::Char(';'), false) => Verb::Act(Action::RepeatFind { reverse: false }),
        (KeyCode::Char(','), false) => Verb::Act(Action::RepeatFind { reverse: true }),
        (KeyCode::Char('*'), false) => Verb::Act(Action::SearchWordUnderCursor),
        (KeyCode::Char(':'), false) => Verb::Act(Action::Command),
        (KeyCode::Char('.'), false) => Verb::Act(Action::Repeat),
        (KeyCode::Esc, _) => Verb::Act(Action::NormalMode),
        _ => return None,
    })
}

/// Visual mode, first key. Only the differences from Normal are listed; motions
/// and the shared prefixes fall through to it.
///
/// The differences are all about the SELECTION already existing: an operator
/// fires at once instead of waiting for a range, `i`/`a` open an object with no
/// operator pending, and `u`/`U` are the case operators rather than undo.
pub fn visual(key: Key) -> Option<Verb> {
    if key.ctrl {
        return normal(key);
    }
    Some(match key.code {
        KeyCode::Char('d') | KeyCode::Char('x') => Verb::Operator(Operator::Delete),
        KeyCode::Char('c') | KeyCode::Char('s') => Verb::Operator(Operator::Change),
        KeyCode::Char('y') => Verb::Operator(Operator::Yank),
        KeyCode::Char('>') => Verb::Operator(Operator::Indent),
        KeyCode::Char('<') => Verb::Operator(Operator::Outdent),
        KeyCode::Char('u') => Verb::Operator(Operator::Lowercase),
        KeyCode::Char('U') => Verb::Operator(Operator::Uppercase),
        KeyCode::Char('i') => Verb::Object { around: false },
        KeyCode::Char('a') => Verb::Object { around: true },
        KeyCode::Char('o') => Verb::Act(Action::SwapSelectionEnds),
        KeyCode::Char('v') => Verb::Act(Action::Visual),
        KeyCode::Char('V') => Verb::Act(Action::VisualLine),
        KeyCode::Esc => Verb::Act(Action::NormalMode),
        _ => return normal(key),
    })
}

/// The second key of a prefix. `g` carries the writer verbs (SPEC §7.3) and is
/// shared by Normal and Visual; `<C-w>` moves between panes.
pub fn after_prefix(prefix: char, key: Key, visual_mode: bool) -> Option<Verb> {
    // `<C-w>` takes a DIRECTION, and an arrow key is one. Folded to the vim
    // letter here so the window commands stay a single `char` vocabulary —
    // everything after this point only ever sees `h`/`j`/`k`/`l`.
    let c = match (prefix, key.code) {
        ('w', KeyCode::Left) => 'h',
        ('w', KeyCode::Down) => 'j',
        ('w', KeyCode::Up) => 'k',
        ('w', KeyCode::Right) => 'l',
        (_, KeyCode::Char(c)) => c,
        _ => return None,
    };
    Some(match (prefix, c) {
        ('g', 'g') => Verb::Goto(Motion::BufferStart),
        // `gu`/`gU` take a motion like any operator; in Visual the selection is
        // already the range, so `visual()` handles the bare `u`/`U` instead.
        ('g', 'u') if !visual_mode => Verb::Operator(Operator::Lowercase),
        ('g', 'U') if !visual_mode => Verb::Operator(Operator::Uppercase),
        ('g', 'b') => Verb::Act(Action::ToggleBold),
        ('g', 'i') => Verb::Act(Action::ToggleItalic),
        ('g', 'h') => Verb::Act(Action::ToggleHighlight),
        ('g', 'k') => Verb::Act(Action::ToggleCode),
        ('g', 'l') => Verb::Act(Action::InsertLink),
        ('g', 't') => Verb::Act(Action::ToggleTask),
        ('g', 'f') => Verb::Act(Action::FollowLink),
        ('g', 'x') => Verb::Act(Action::OpenExternal),
        ('g', 'p') => Verb::Act(Action::AppendParagraph),
        ('g', c @ '0'..='6') => Verb::Act(Action::SetHeading(c as u8 - b'0')),
        // `<C-w>` hands its second key straight to the window commands, so a
        // pane verb can be added without touching the grammar.
        ('w', c) => Verb::Act(Action::Window(c)),
        _ => return None,
    })
}

/// The immediate motion a key denotes. `ctrl` selects the paging motions.
fn motion(key: Key) -> Option<Motion> {
    use KeyCode::*;
    Some(match (key.code, key.ctrl) {
        (Char('h'), false) | (Left, _) => Motion::Left,
        (Char('l'), false) | (Right, _) => Motion::Right,
        (Char('k'), false) | (Up, _) => Motion::Up,
        (Char('j'), false) | (Down, _) => Motion::Down,
        (Char('w'), false) => Motion::WordForward { big: false },
        (Char('W'), false) => Motion::WordForward { big: true },
        (Char('b'), false) => Motion::WordBack { big: false },
        (Char('B'), false) => Motion::WordBack { big: true },
        (Char('e'), false) => Motion::WordEnd { big: false },
        (Char('E'), false) => Motion::WordEnd { big: true },
        (Char('0'), false) => Motion::LineStart,
        (Char('^'), false) => Motion::LineFirstNonBlank,
        (Char('$'), false) => Motion::LineEnd,
        (Char('{'), false) => Motion::ParagraphBack,
        (Char('}'), false) => Motion::ParagraphForward,
        (Char('H'), false) => Motion::ScreenTop,
        (Char('M'), false) => Motion::ScreenMiddle,
        (Char('L'), false) => Motion::ScreenBottom,
        (Char('d'), true) => Motion::HalfPageDown,
        (Char('u'), true) => Motion::HalfPageUp,
        (Char('f'), true) => Motion::PageDown,
        (Char('b'), true) => Motion::PageUp,
        _ => return None,
    })
}

impl Verb {
    /// Parse the name used in `shoin.conf` `[keys.*]` tables. Every `Action`
    /// name still works; these add the pieces of the GRAMMAR, which the old
    /// action-only overlay could not name at all.
    pub fn from_config_name(name: &str) -> Option<Verb> {
        let verb = match name {
            "delete" | "operator_delete" => Verb::Operator(Operator::Delete),
            "change" | "operator_change" => Verb::Operator(Operator::Change),
            "yank" | "operator_yank" => Verb::Operator(Operator::Yank),
            "indent" | "operator_indent" => Verb::Operator(Operator::Indent),
            "outdent" | "operator_outdent" => Verb::Operator(Operator::Outdent),
            "lowercase" | "operator_lowercase" => Verb::Operator(Operator::Lowercase),
            "uppercase" | "operator_uppercase" => Verb::Operator(Operator::Uppercase),
            "inner_object" => Verb::Object { around: false },
            "around_object" => Verb::Object { around: true },
            "register" => Verb::Register,
            "replace_char" => Verb::ReplaceChar,
            "find_char" => Verb::Find { forward: true, till: false },
            "find_char_back" => Verb::Find { forward: false, till: false },
            "till_char" => Verb::Find { forward: true, till: true },
            "till_char_back" => Verb::Find { forward: false, till: true },
            "goto_line" => Verb::Goto(Motion::BufferEnd),
            "goto_line_first" => Verb::Goto(Motion::BufferStart),
            _ => {
                if let Some(m) = motion_by_name(name) {
                    return Some(Verb::Motion(m));
                }
                return Action::from_config_name(name).map(Verb::Act);
            }
        };
        Some(verb)
    }
}

fn motion_by_name(name: &str) -> Option<Motion> {
    Some(match name {
        "move_left" => Motion::Left,
        "move_right" => Motion::Right,
        "move_up" => Motion::Up,
        "move_down" => Motion::Down,
        "word_forward" => Motion::WordForward { big: false },
        "word_back" => Motion::WordBack { big: false },
        "word_end" => Motion::WordEnd { big: false },
        "line_start" => Motion::LineStart,
        "line_first_non_blank" => Motion::LineFirstNonBlank,
        "line_end" => Motion::LineEnd,
        "paragraph_forward" => Motion::ParagraphForward,
        "paragraph_back" => Motion::ParagraphBack,
        "half_page_down" => Motion::HalfPageDown,
        "half_page_up" => Motion::HalfPageUp,
        "page_down" => Motion::PageDown,
        "page_up" => Motion::PageUp,
        "screen_top" => Motion::ScreenTop,
        "screen_middle" => Motion::ScreenMiddle,
        "screen_bottom" => Motion::ScreenBottom,
        _ => return None,
    })
}

/// A resolved command: the action, plus the count and register the grammar
/// gathered on its way to it.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub action: Action,
    pub count: usize,
    pub register: Option<char>,
}

impl Command {
    pub fn new(action: Action, count: usize, register: Option<char>) -> Command {
        Command { action, count, register }
    }

    /// A command with no count or register — what a bare config binding fires.
    pub fn bare(action: Action) -> Command {
        Command { action, count: 1, register: None }
    }
}
