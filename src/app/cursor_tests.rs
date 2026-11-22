//! Where the cursor goes and where its caret is drawn, as Neovim: `$`'s
//! goal at every line's end, the caret on a tab, and Insert's `Tab` with
//! `expand_tab`. Every expected place is what `nvim --clean --headless`
//! (`ts=4 sw=4 et sts=0`, unless a case sets `ts`) gave for the same text
//! and keys.

use super::*;
use crate::test_support::app_with;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};

fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.on_key(KeyEvent::new(code, mods));
    app.sync_after_input();
}

/// Type `keys`, where the control characters stand for `Ctrl-d` `Ctrl-u`
/// `Ctrl-f` `Ctrl-b` `Ctrl-e` `Ctrl-y` and `Ctrl-r`, `\u{1b}` for Esc,
/// `\r` for Enter, `\t` for Tab, `\u{8}` for Backspace and `↓` `↑` for
/// the arrow keys.
fn feed(app: &mut App, keys: &str) {
    for c in keys.chars() {
        let (code, mods) = match c {
            '\u{4}' => (KeyCode::Char('d'), KeyModifiers::CONTROL),
            '\u{15}' => (KeyCode::Char('u'), KeyModifiers::CONTROL),
            '\u{6}' => (KeyCode::Char('f'), KeyModifiers::CONTROL),
            '\u{2}' => (KeyCode::Char('b'), KeyModifiers::CONTROL),
            '\u{5}' => (KeyCode::Char('e'), KeyModifiers::CONTROL),
            '\u{19}' => (KeyCode::Char('y'), KeyModifiers::CONTROL),
            '\u{12}' => (KeyCode::Char('r'), KeyModifiers::CONTROL),
            '\u{1b}' => (KeyCode::Esc, KeyModifiers::NONE),
            '\r' => (KeyCode::Enter, KeyModifiers::NONE),
            '\t' => (KeyCode::Tab, KeyModifiers::NONE),
            '\u{8}' => (KeyCode::Backspace, KeyModifiers::NONE),
            '↓' => (KeyCode::Down, KeyModifiers::NONE),
            '↑' => (KeyCode::Up, KeyModifiers::NONE),
            c => (KeyCode::Char(c), KeyModifiers::NONE),
        };
        key(app, code, mods);
    }
}

/// A fresh app on `text`, with a 30-row window and tab width `tw`.
fn fresh(text: &str, tw: usize) -> App {
    let mut app = app_with(text);
    app.term_size = (80, 30);
    app.config.editor.tab_width = tw;
    app.refresh_blocks();
    app
}

/// Run each `(keys, (line, col))` case on a fresh app and list every one
/// that lands elsewhere, so one run shows them all.
fn misses(text: &str, cases: &[(&str, (usize, usize))]) -> Vec<String> {
    let mut misses = Vec::new();
    for &(keys, want) in cases {
        let mut app = fresh(text, 4);
        feed(&mut app, keys);
        let got = (app.buffer.cursor.line, app.buffer.cursor.col);
        if got != want {
            misses.push(format!("{keys:?}: got {got:?}, Neovim {want:?}"));
        }
    }
    misses
}

/// Lines with a first non-blank past column 0, a short one, a line of
/// blanks, an empty one, a leading tab and wide characters.
const CASE_LINES: &str = "Abc Def\n   Indented Line Here With Words\nab\n   \n\nXYZ long line here with some words\n\tTabbed Line Here\n日本 Wide Line Here\nlast Line here\n";

/// A long line, a short one, an empty one, tabs, wide characters.
const STICKY: &str = "    abcdefghijklmno\nab\n\n\tx\tyz\n日本語テキスト\n  indented line here\nlast line of the file\n";

/// Line 3 is longer than every line before it, so a goal at the end of
/// line 1 tells there whether it was still the end or a column.
const AFTER: &str = "mid\nmedium line\nx\nthe very longest line of the whole text here\nab\nmedium again\n";

/// Eighty lines: long ones, a short `ab` every seventh and an empty one
/// every tenth.
fn long_text() -> String {
    (1..=80)
        .map(|i| match i {
            _ if i % 10 == 0 => "\n".to_string(),
            _ if i % 7 == 0 => "ab\n".to_string(),
            _ => format!("  line {i:02} with some text here\n"),
        })
        .collect()
}

/// After `$` (and `N$`), `j` `k` `G` `gg` `NG` `:N` and the paging keys
/// go to the end of every line they land on, Neovim's `curswant` of
/// MAXCOL, however long or short the lines between: `6j$k` is at the end
/// of the line above, column 33, not at the tab line's last cell.
#[test]
fn dollar_keeps_every_line_end_as_neovim() {
    let mut out = misses(
        CASE_LINES,
        &[
            ("$j", (1, 31)),
            ("$jj", (2, 1)),
            ("$jjj", (3, 2)),
            ("$jjjj", (4, 0)),
            ("$jjjjj", (5, 33)),
            ("$jjjjjj", (6, 16)),
            ("$G", (8, 13)),
            ("G$gg", (0, 6)),
            ("$3G", (2, 1)),
            ("$4G", (3, 2)),
            ("$5G", (4, 0)),
            ("6j$k", (5, 33)),
            ("6j$kk", (4, 0)),
            ("6j$kh", (5, 32)),
            ("6j$k0j", (6, 0)),
            ("2$j", (2, 1)),
            ("2$jk", (1, 31)),
            ("j$k", (0, 6)),
            ("$jl", (1, 31)),
            ("$jhj", (2, 1)),
            ("$jjjjjjgg", (0, 6)),
            ("$\u{4}", (8, 13)),
            ("$\u{4}\u{15}", (0, 6)),
            ("$:5\r", (4, 0)),
            ("$:5\rj", (5, 33)),
            ("6j$:1\r", (0, 6)),
            ("$:$\r", (8, 13)),
            ("$jx", (1, 30)),
        ],
    );
    out.extend(misses(
        STICKY,
        &[
            ("$j", (1, 1)),
            ("$jj", (2, 0)),
            ("$jjj", (3, 4)),
            ("$jjjj", (4, 6)),
            ("$jjjjj", (5, 19)),
            ("$jjjjjj", (6, 20)),
            ("$G", (6, 20)),
        ],
    ));
    // The paging keys and `H` land on a line Shoin's `scroll_off` (3, where
    // Neovim's is 0) picks; at the end of it, as Neovim's are.
    let text = long_text();
    for keys in ["$\u{4}", "$\u{4}\u{4}", "$\u{4}\u{15}", "$\u{6}", "$\u{6}\u{2}", "$H", "$L", "$Lj", "$M"] {
        let mut app = fresh(&text, 4);
        feed(&mut app, keys);
        let c = app.buffer.cursor;
        let end = app.buffer.line_len(c.line).saturating_sub(1);
        if c.col != end {
            out.push(format!("{keys:?}: col {} on line {}, Neovim the end, {end}", c.col, c.line));
        }
    }
    out.extend(misses(&text, &[("$5jdd", (5, 1)), ("$5jddj", (6, 1))]));
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// The linewise operators land where Neovim's do after `$`: `yk` and
/// `gUk` at the end of the line above, `dd` at the end of the line it
/// leaves the cursor on, `Vk>` at the end of the shifted line.
#[test]
fn linewise_operators_after_dollar_land_as_neovim() {
    let out = misses(
        CASE_LINES,
        &[
            ("6j$yk", (5, 33)),
            ("6j$ykj", (6, 16)),
            ("6j$yy", (6, 16)),
            ("6j$yyk", (5, 19)),
            ("6j$ddk", (5, 18)),
            ("6j$dd", (6, 16)),
            ("6j$ykk", (4, 0)),
            ("6j$y2k", (4, 0)),
            ("6j$ygg", (0, 6)),
            ("6j$yjj", (7, 16)),
            ("$gUj", (0, 6)),
            ("6j$gUk", (5, 33)),
            ("6j$Vk>", (5, 37)),
            ("6j$>k", (5, 37)),
            ("6j$Vkd", (5, 16)),
        ],
    );
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// What forgets `$`'s goal and what keeps it, as Neovim: from the end of
/// line 1 (column 10), each key and then `4G`, which lands at column 43
/// while the goal is still the end, and at the column the goal became
/// otherwise. A horizontal motion, an edit, a yank, Insert, Esc out of
/// Visual and undo forget it; a vertical motion, a scroll, `:N` `:>` `:m`
/// `:y` and a motion that cannot move (`l` at the end) keep it. (`Ctrl-e`
/// is left out: with Shoin's `scroll_off` it moves the cursor here, and
/// Neovim's, at 0, does not.)
#[test]
fn what_forgets_the_line_end_goal_as_neovim() {
    let cases: Vec<(String, (usize, usize))> = [
        ("", 43),
        ("l", 43),
        ("h", 9),
        ("yy", 10),
        ("yk", 2),
        ("yj", 10),
        ("Y", 10),
        ("y$", 10),
        ("yl", 10),
        ("yh", 9),
        (">>", 14),
        ("<<", 10),
        (">j", 14),
        ("gUU", 0),
        ("guu", 0),
        ("gUk", 2),
        ("gUj", 10),
        ("x", 9),
        ("X", 9),
        ("~", 10),
        ("r.", 10),
        ("yip", 0),
        ("Vy", 0),
        ("vy", 10),
        ("vjy", 10),
        ("vky", 2),
        ("V>", 14),
        ("Vj>", 14),
        ("v\u{1b}", 10),
        ("vj\u{1b}", 0),
        ("V\u{1b}", 10),
        ("i\u{1b}", 9),
        ("a\u{1b}", 10),
        ("A\u{1b}", 10),
        ("xu", 10),
        ("xu\u{12}", 9),
        ("ddu", 10),
        (">>u", 10),
        (":2\r", 43),
        (":m0\r", 43),
        (":>\r", 43),
        (":1,2y\r", 43),
        ("zz", 43),
        ("zt", 43),
        ("\u{19}", 43),
        ("w", 0),
        ("b", 7),
        ("0", 0),
        ("^", 0),
        ("cc\u{1b}", 0),
        ("$", 43),
        ("j", 43),
        ("k", 43),
        ("G", 43),
        ("gg", 43),
        ("jk", 43),
    ]
    .into_iter()
    .map(|(keys, col)| (format!("2G${keys}4G"), (3, col)))
    .collect();
    let cases: Vec<(&str, (usize, usize))> = cases.iter().map(|(k, w)| (k.as_str(), *w)).collect();
    let mut out = misses(AFTER, &cases);
    // Insert takes the goal afresh too, from where it starts typing.
    out.extend(misses(AFTER, &[("2G$i↓↓", (3, 10)), ("2G$a↓↓", (3, 11)), ("2G$o↑", (1, 0))]));
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// Visual mode keeps `$`'s goal too: `v` after `$` and then `j` ends each
/// line's selection at its end. Neovim's Visual cursor stands on the line
/// break there (one past the last character); Shoin's stands on the last
/// character, as its Visual `$` already does, so that is the column here.
#[test]
fn visual_keeps_the_line_end_goal() {
    let out = misses(
        AFTER,
        &[
            ("2G$vj", (2, 0)),
            ("2G$vjj", (3, 43)),
            ("2G$vjjj", (4, 1)),
            ("2G$Vjj", (3, 43)),
            ("2G$vjjjk", (3, 43)),
            ("2G$v4G", (3, 43)),
            ("2G$vj4G", (3, 43)),
        ],
    );
    let mut out = out;
    out.extend(misses(CASE_LINES, &[("$vj", (1, 31)), ("$vjj", (2, 1)), ("6j$vk", (5, 33)), ("6j$Vk", (5, 33))]));
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// Lines with tabs mid-line, at the start, and ending at a stop.
const TABS: &str = "ab\tcd\tef\nx\ty\n\tz\nabc\td\nabcd\te\n";

/// The caret's cell in its line after `keys`, counted from the line's
/// first cell (the caret's cell at column 0 of line 0, which starts with
/// `a`).
fn caret_cell(keys: &str, tw: usize) -> (usize, usize, u16) {
    let draw = |app: &App| {
        let mut t = Terminal::new(TestBackend::new(60, 12)).unwrap();
        t.draw(|f| crate::render::frame::render(f, app)).unwrap();
        t.get_cursor_position().unwrap().x
    };
    let x0 = draw(&fresh(TABS, tw));
    let mut app = fresh(TABS, tw);
    feed(&mut app, keys);
    (app.buffer.cursor.line, app.buffer.cursor.col, draw(&app) - x0)
}

/// Normal mode draws the caret on a tab's LAST cell, as Neovim does, for a
/// tab at the line's start, mid-line and one only a cell wide; so does an
/// operator waiting for its motion. Insert draws it on the first, before
/// the tab, where typing goes. Visual draws it on the last cell when the
/// cursor is past the anchor and on the first otherwise, as Neovim's
/// `getvcol` with 'selection' inclusive. Every cell is Neovim's
/// `wincol()` for the same keys (Neovim reached each tab with `f<Tab>`).
#[test]
fn the_caret_on_a_tab_is_drawn_where_neovim_draws_it() {
    #[rustfmt::skip]
    let cases: &[(usize, &str, (usize, usize, u16))] = &[
        (2, "0", (0, 0, 0)), (2, "02l", (0, 2, 3)), (2, "05l", (0, 5, 7)),
        (2, "j0l", (1, 1, 1)), (2, "jj0", (2, 0, 1)), (2, "3j03l", (3, 3, 3)), (2, "4j04l", (4, 4, 5)),
        (2, "02lv", (0, 2, 2)), (2, "02li", (0, 2, 2)), (2, "02la", (0, 3, 4)), (2, "0v2l", (0, 2, 3)),
        (2, "02lvl", (0, 3, 4)), (2, "02lvh", (0, 1, 1)),
        (4, "02l", (0, 2, 3)), (4, "05l", (0, 5, 7)), (4, "j0l", (1, 1, 3)), (4, "jj0", (2, 0, 3)),
        (4, "3j03l", (3, 3, 3)), (4, "4j04l", (4, 4, 7)), (4, "02lv", (0, 2, 2)), (4, "02li", (0, 2, 2)),
        (4, "02la", (0, 3, 4)), (4, "3j03li", (3, 3, 3)), (4, "0v2l", (0, 2, 3)),
        (4, "02lvlh", (0, 2, 2)), (4, "j0lvk", (0, 2, 2)), (4, "02lV", (0, 2, 2)), (4, "0V2l", (0, 2, 3)),
        (4, "j0lVk", (0, 2, 2)), (4, "02ld", (0, 2, 3)), (4, "02lc", (0, 2, 3)), (4, "02ly", (0, 2, 3)),
        (8, "02l", (0, 2, 7)), (8, "05l", (0, 5, 15)), (8, "j0l", (1, 1, 7)), (8, "jj0", (2, 0, 7)),
        (8, "3j03l", (3, 3, 7)), (8, "4j04l", (4, 4, 7)), (8, "02lv", (0, 2, 2)), (8, "02li", (0, 2, 2)),
        (8, "02la", (0, 3, 8)), (8, "3j03li", (3, 3, 3)), (8, "0v2l", (0, 2, 7)), (8, "02lvl", (0, 3, 8)),
        (8, "02lV", (0, 2, 2)), (8, "0V2l", (0, 2, 7)), (8, "02ld", (0, 2, 7)),
    ];
    let mut out = Vec::new();
    for &(tw, keys, want) in cases {
        let got = caret_cell(keys, tw);
        if got != want {
            out.push(format!("{keys:?} at {tw}: got {got:?}, Neovim {want:?}"));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// With `expand_tab`, Insert's `Tab` types spaces to the next tab stop, as
/// Neovim with 'expandtab' and 'softtabstop' 0, counting a tab before it to
/// its stop and a wide character as two cells; Backspace then takes one
/// space, as Neovim's does there. Each case is the line and the column.
#[test]
fn insert_tab_with_expand_tab_reaches_the_next_stop_as_neovim() {
    let text = "abc\nx\tyz\n日本\n";
    #[rustfmt::skip]
    let cases: &[(usize, &str, &str, usize)] = &[
        (2, "i\t", "  abc", 2), (2, "a\t", "a bc", 2), (2, "A\t", "abc ", 4), (2, "la\t", "ab  c", 4),
        (2, "A\tx", "abc x", 5), (2, "jA\t", "x\tyz  ", 6), (2, "jjA\t", "日本  ", 4),
        (2, "A\t\u{8}", "abc", 3), (2, "a\t\u{8}", "abc", 1), (2, "i \t", "  abc", 2), (2, "i\t\t", "    abc", 4),
        (2, "A\ty\t", "abc y ", 6), (2, "oab\tc", "ab  c", 5),
        (4, "i\t", "    abc", 4), (4, "a\t", "a   bc", 4), (4, "A\t", "abc ", 4), (4, "la\t", "ab  c", 4),
        (4, "jA\t", "x\tyz  ", 6), (4, "jjA\t", "日本    ", 6), (4, "A\t\u{8}", "abc", 3),
        (4, "a\t\u{8}", "a  bc", 3), (4, "i \t", "    abc", 4), (4, "i\t\t\u{8}", "    abc", 4),
        (4, "A\ty\t", "abc y   ", 8), (4, "oab\tc", "ab  c", 5),
        (8, "i\t", "        abc", 8), (8, "a\t", "a       bc", 8), (8, "A\t", "abc     ", 8),
        (8, "jA\t", "x\tyz      ", 10), (8, "jjA\t", "日本    ", 6), (8, "A\t\u{8}\u{8}", "abc   ", 6),
        (8, "i \t", "        abc", 8), (8, "i\t\t", "                abc", 16), (8, "A\ty\t", "abc     y       ", 16),
    ];
    let mut out = Vec::new();
    for &(tw, keys, line, col) in cases {
        let mut app = fresh(text, tw);
        feed(&mut app, keys);
        let l = app.buffer.cursor.line;
        let got = (app.buffer.line_text(l), app.buffer.cursor.col);
        if got != (line.to_string(), col) {
            out.push(format!("{keys:?} at {tw}: got {got:?}, Neovim {:?}", (line, col)));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// Backspace in a line's indent (only blanks before the cursor) takes it
/// back to the stop before, as Neovim's default 'smarttab' does: spaces
/// and tabs alike, `expand_tab` or not. Past the indent it takes one
/// character. Each case is the text, the keys, `expand_tab`, then the line
/// and the column Neovim gave.
#[test]
fn backspace_in_the_indent_goes_back_a_stop_as_neovim() {
    #[rustfmt::skip]
    let cases: &[(usize, &str, &str, bool, &str, usize)] = &[
        (4, "      abc", "0wi\u{8}", true, "    abc", 4), (4, "      abc", "0wi\u{8}\u{8}", true, "abc", 0),
        (4, "  x", "0wi\u{8}", true, "x", 0), (4, "\t  abc", "0wi\u{8}", false, "\tabc", 1),
        (4, "  \tabc", "0wi\u{8}", false, "abc", 0), (4, "\t\tabc", "0wi\u{8}", false, "\tabc", 1),
        (4, " \t abc", "0wi\u{8}", false, " \tabc", 2), (4, " \t abc", "0wi\u{8}", true, " \tabc", 2),
        (4, "ab    cd", "0fci\u{8}", true, "ab   cd", 5), (4, "      ", "A\u{8}", true, "    ", 4),
        (4, "     abc", "0wi\u{8}", true, "    abc", 4), (4, "   abc", "02li\u{8}", true, " abc", 0),
        (4, "      abc", "03li\u{8}", true, "   abc", 0), (4, "  \tabc", "0wi\u{8}", true, "abc", 0),
        (4, "x", "o\t \u{8}", true, "    ", 4), (4, "x", "o\t \u{8}\u{8}", true, "", 0),
        (4, "   abc", "0wi\t", true, "    abc", 4), (4, "  abc", "0i\t", true, "      abc", 4),
        (8, "      abc", "0wi\u{8}", true, "abc", 0), (8, "\t  abc", "0wi\u{8}", false, "\tabc", 1),
        (8, " \t abc", "0wi\u{8}", true, " \tabc", 2), (8, "ab    cd", "0fci\u{8}", true, "ab   cd", 5),
        (8, "x", "o\t \u{8}", true, "        ", 8), (8, "   abc", "0wi\t", true, "        abc", 8),
    ];
    let mut out = Vec::new();
    for &(tw, text, keys, expand, line, col) in cases {
        let mut app = fresh(&format!("{text}\n"), tw);
        app.config.editor.expand_tab = expand;
        feed(&mut app, keys);
        let l = app.buffer.cursor.line;
        let got = (app.buffer.line_text(l), app.buffer.cursor.col);
        if got != (line.to_string(), col) {
            out.push(format!("{text:?} {keys:?} at {tw}: got {got:?}, Neovim {:?}", (line, col)));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

