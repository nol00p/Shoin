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

/// A fresh app on `text`, with a 30-row window and tab width `tw`, and
/// the theme as authored rather than downgraded for the terminal the tests
/// run in: on a 16-color terminal (a CI runner, with no `COLORTERM` and no
/// `TERM`) the selection and the background both fall on slot 0, and a
/// test reading the selection's background would see it everywhere.
fn fresh(text: &str, tw: usize) -> App {
    let mut app = app_with(text);
    app.theme = crate::render::theme::Theme::authored(&app.config.theme).unwrap();
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
/// line's selection at its end. The Visual cursor stands on the line break
/// there, one past the last character, as Neovim's does, in `V` as in `v`.
#[test]
fn visual_keeps_the_line_end_goal() {
    let out = misses(
        AFTER,
        &[
            ("2G$vj", (2, 1)),
            ("2G$vjj", (3, 44)),
            ("2G$vjjj", (4, 2)),
            ("2G$Vjj", (3, 44)),
            ("2G$vjjjk", (3, 44)),
            ("2G$v4G", (3, 44)),
            ("2G$vj4G", (3, 44)),
            ("2G$vjjo", (1, 10)),
        ],
    );
    let mut out = out;
    out.extend(misses(CASE_LINES, &[("$vj", (1, 32)), ("$vjj", (2, 2)), ("6j$vk", (5, 34)), ("6j$Vk", (5, 34))]));
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// Three lines, the middle one a single character: a goal that is still
/// the line end at the third line lands on its last character, column 9.
const SHORT_MID: &str = "abcd\nx\nabcdefghij\n";

/// A motion that cannot fail sets its goal even where the cursor already
/// is, as Neovim's: `$` on a line of one character, `0` and `^` there
/// after `$`. One that fails (`l` at the end, `h` at the start) keeps it.
#[test]
fn a_goal_changes_where_the_cursor_stays_as_neovim() {
    let mut out = misses("x\nabcdefghij\n", &[("$j", (1, 9)), ("$0j", (1, 0)), ("$^j", (1, 0))]);
    out.extend(misses(
        SHORT_MID,
        &[("$j0j", (2, 0)), ("$j^j", (2, 0)), ("$j$jk", (1, 0)), ("$jlj", (2, 9)), ("$jhj", (2, 9))],
    ));
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// A text, keys, the text after, where the cursor is and the anchor.
type VisualCase = (&'static str, &'static str, &'static str, (usize, usize), Option<(usize, usize)>);

/// `$` in Visual, a sticky line end it carries down, and `j` or `k` onto a
/// line shorter than the goal, end the selection on the line break, so `d`
/// takes it, as Neovim's: `v$jd` on
/// `abcd`, `x`, `abcdefghij` leaves `abcdefghij`, not an empty line above
/// it. Each case is the text, the keys, the text after and where the
/// cursor is, with Visual's anchor when it is still on.
#[test]
fn visual_dollar_takes_the_line_break_as_neovim() {
    #[rustfmt::skip]
    let cases: &[VisualCase] = &[
        (SHORT_MID, "v$jd", "abcdefghij\n", (0, 0), None),
        (SHORT_MID, "v$d", "x\nabcdefghij\n", (0, 0), None),
        (SHORT_MID, "v$j", SHORT_MID, (1, 1), Some((0, 0))),
        (SHORT_MID, "v$jj", SHORT_MID, (2, 10), Some((0, 0))),
        (SHORT_MID, "v$jk", SHORT_MID, (0, 4), Some((0, 0))),
        (SHORT_MID, "v$jy", SHORT_MID, (0, 0), None),
        (SHORT_MID, "$vjd", "abcabcdefghij\n", (0, 3), None),
        (SHORT_MID, "$vj", SHORT_MID, (1, 1), Some((0, 3))),
        (SHORT_MID, "jv$d", "abcd\nabcdefghij\n", (1, 0), None),
        (SHORT_MID, "jv$kd", "abcd\nabcdefghij\n", (0, 3), None),
        (SHORT_MID, "v$jx", "abcdefghij\n", (0, 0), None),
        (SHORT_MID, "v$jc\u{1b}", "abcdefghij\n", (0, 0), None),
        (SHORT_MID, "$vjjkd", "abcabcdefghij\n", (0, 3), None),
        (SHORT_MID, "V$jd", "abcdefghij\n", (0, 9), None),
        (SHORT_MID, "v$jo", SHORT_MID, (0, 0), Some((1, 1))),
        (SHORT_MID, "$vjo", SHORT_MID, (0, 3), Some((1, 1))),
        (SHORT_MID, "$vjoj", SHORT_MID, (1, 1), Some((1, 1))),
        (SHORT_MID, "v3lj", SHORT_MID, (1, 1), Some((0, 0))),
        (SHORT_MID, "v3ljd", "abcdefghij\n", (0, 0), None),
        (SHORT_MID, "2jv9lkk", SHORT_MID, (0, 4), Some((2, 0))),
        (SHORT_MID, "lv2lj", SHORT_MID, (1, 1), Some((0, 1))),
        ("ab\ncd\nef\n", "v$d", "cd\nef\n", (0, 0), None),
        ("ab\ncd\nef\n", "v$y", "ab\ncd\nef\n", (0, 0), None),
        ("ab\ncd\nef\n", "jv$d", "ab\nef\n", (1, 0), None),
        ("ab\ncd\nef\n", "v$hd", "\ncd\nef\n", (0, 0), None),
        ("ab\ncd\nef\n", "v$l", "ab\ncd\nef\n", (0, 2), Some((0, 0))),
        ("ab\ncd\nef\n", "v$ld", "cd\nef\n", (0, 0), None),
        ("ab\ncd\nef\n", "$v$d", "acd\nef\n", (0, 1), None),
    ];
    let mut out = Vec::new();
    for &(text, keys, after, at, anchor) in cases {
        let mut app = fresh(text, 4);
        feed(&mut app, keys);
        let c = app.buffer.cursor;
        let got = (app.buffer.rope.to_string(), (c.line, c.col), app.visual_anchor().map(|a| (a.line, a.col)));
        if got != (after.to_string(), at, anchor) {
            out.push(format!("{text:?} {keys:?}: got {got:?}, Neovim {:?}", (after, at, anchor)));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// An operator that does nothing keeps `$`'s goal, as Neovim's: one
/// cancelled with Esc, one given a key that is no motion, one whose motion
/// fails (`dk` on the first line). `Ctrl-r` with nothing to redo still
/// forgets it, as Neovim's, which says so and sets the goal anyway.
#[test]
fn an_operator_that_does_nothing_keeps_the_goal_as_neovim() {
    let out = misses(
        SHORT_MID,
        &[
            ("$d\u{1b}2j", (2, 9)),
            ("$c\u{1b}2j", (2, 9)),
            ("$y\u{1b}2j", (2, 9)),
            ("$g\u{1b}2j", (2, 9)),
            ("$d\u{1b}\u{1b}2j", (2, 9)),
            ("$dx2j", (2, 9)),
            ("$dk2j", (2, 9)),
            ("$yk2j", (2, 9)),
            ("$\u{12}2j", (2, 3)),
        ],
    );
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// `Ctrl-e` that pushes the cursor down a line keeps `$`'s goal, as
/// Neovim's does (both at `scroll_off` 0): the cursor goes to the end of
/// the line it is carried to, and `j` on to the end of the next.
#[test]
fn a_scroll_that_carries_the_cursor_keeps_the_goal_as_neovim() {
    let mut out = Vec::new();
    for (keys, want) in [("$\u{5}", (1, 0)), ("$\u{5}j", (2, 9)), ("$\u{19}j", (1, 0))] {
        let mut app = fresh(SHORT_MID, 4);
        app.config.editor.scroll_off = 0;
        let mut t = Terminal::new(TestBackend::new(80, 30)).unwrap();
        t.draw(|f| crate::render::frame::render(f, &app)).unwrap();
        feed(&mut app, keys);
        let got = (app.buffer.cursor.line, app.buffer.cursor.col);
        if got != want {
            out.push(format!("{keys:?}: got {got:?}, Neovim {want:?}"));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// A bracketed paste after `$`, undone, leaves the goal where `u` puts the
/// cursor, as a typed change does and as Neovim's: `u2j` lands on the
/// column `u` went back to, not at the end.
#[test]
fn undoing_a_paste_takes_the_goal_afresh_as_neovim() {
    let mut app = fresh(SHORT_MID, 4);
    feed(&mut app, "$");
    app.on_paste("Z".to_string());
    app.sync_after_input();
    feed(&mut app, "u2j");
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (2, 3));
}

/// A tab as wide as `tab_width` allows, with the caret on it in Normal
/// mode, draws the caret at the pane's last column, not past the edge of
/// a terminal coordinate.
#[test]
fn a_huge_tab_width_keeps_the_caret_on_screen() {
    let mut app = fresh("\tx\n", 65535);
    let mut t = Terminal::new(TestBackend::new(60, 12)).unwrap();
    t.draw(|f| crate::render::frame::render(f, &app)).unwrap();
    let x = t.get_cursor_position().unwrap().x;
    assert!(x < 60, "caret at {x}");
    for keys in ["0", "l", "0", "a\t\t\u{1b}0"] {
        feed(&mut app, keys);
        t.draw(|f| crate::render::frame::render(f, &app)).unwrap();
        let x = t.get_cursor_position().unwrap().x;
        assert!(x < 60, "{keys:?}: caret at {x}");
    }
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


/// `d$` `y$` `c$` on a line's last character take that character, as
/// Neovim's: `$` cannot fail, under an operator either. Each case is the
/// keys, the text after and where the cursor is.
#[test]
fn an_operator_to_the_end_takes_the_last_character_as_neovim() {
    let mut out = Vec::new();
    for (keys, after, at) in [
        ("$d$", "abc\nx\nabcdefghij\n", (0, 2)),
        ("$y$P", "abcdd\nx\nabcdefghij\n", (0, 3)),
        ("$c$Z\u{1b}", "abcZ\nx\nabcdefghij\n", (0, 3)),
        ("jd$", "abcd\n\nabcdefghij\n", (1, 0)),
        ("j$d$", "abcd\n\nabcdefghij\n", (1, 0)),
    ] {
        let mut app = fresh(SHORT_MID, 4);
        feed(&mut app, keys);
        let c = app.buffer.cursor;
        let got = (app.buffer.rope.to_string(), (c.line, c.col));
        if got != (after.to_string(), at) {
            out.push(format!("{keys:?}: got {got:?}, Neovim {:?}", (after, at)));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// `abcd`, `x`: the selected line break of the first line, and the file's
/// last line, whose break a selection never takes.
const SHORT_LAST: &str = "abcd\nx\n";

/// A text, keys, the text after, where the cursor is and the unnamed
/// register (its text, and whether it is linewise).
type PutCase = (&'static str, &'static str, &'static str, (usize, usize), (&'static str, bool));

/// Visual `p` and `P` over a selection that takes a line break replace the
/// break too, and `p` puts it in the register, as Neovim's: `ylv$p` on
/// `abcd`, `x` leaves `ax` and `abcd\n` in the register. Each case is the
/// text, the keys, the text after, where the cursor is, and the unnamed
/// register (its text, linewise).
#[test]
fn visual_put_replaces_a_selected_line_break_as_neovim() {
    #[rustfmt::skip]
    let cases: &[PutCase] = &[
        (SHORT_LAST, "ylv$p", "ax\n", (0, 0), ("abcd\n", false)),
        (SHORT_LAST, "ylv$P", "ax\n", (0, 0), ("a", false)),
        (SHORT_LAST, "yyv$p", "\nabcd\nx\n", (1, 0), ("abcd\n", false)),
        (SHORT_LAST, "yyv$P", "\nabcd\nx\n", (1, 0), ("abcd\n", true)),
        (SHORT_LAST, "ylv$jp", "a\n", (0, 0), ("abcd\nx", false)),
        (SHORT_LAST, "jylv$p", SHORT_LAST, (1, 0), ("x", false)),
        (SHORT_LAST, "ylv$hp", "a\nx\n", (0, 0), ("abcd", false)),
        (SHORT_LAST, "lylv$p", "abx\n", (0, 1), ("bcd\n", false)),
        (SHORT_LAST, "ylv$2p", "aax\n", (0, 1), ("abcd\n", false)),
        (SHORT_LAST, "yl$v$p", "abcax\n", (0, 3), ("d\n", false)),
        ("abcd\n\nx\n", "yljvp", "abcd\nax\n", (1, 0), ("\n", false)),
        ("abcd\n\nx\n", "yljvP", "abcd\nax\n", (1, 0), ("a", false)),
    ];
    let mut out = Vec::new();
    for &(text, keys, after, at, reg) in cases {
        let mut app = fresh(text, 4);
        feed(&mut app, keys);
        let c = app.buffer.cursor;
        let r = app.registers.get(&'"').map(|r| (r.text.clone(), r.linewise));
        let got = (app.buffer.rope.to_string(), (c.line, c.col), r);
        let want = (after.to_string(), at, Some((reg.0.to_string(), reg.1)));
        if got != want {
            out.push(format!("{text:?} {keys:?}: got {got:?}, Neovim {want:?}"));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// `l` (and the right arrow) in Visual goes on to the line break after the
/// last character, as Neovim's, so `v4ld` on `abcd`, `x` takes the break;
/// Normal's `l` still stops on the last character. `e` and `f` never pass
/// the last character. Each case is the text, the keys, the text after,
/// where the cursor is, and Visual's anchor while it is on.
#[test]
fn visual_right_reaches_the_line_break_as_neovim() {
    #[rustfmt::skip]
    let cases: &[VisualCase] = &[
        (SHORT_LAST, "v4ld", "x\n", (0, 0), None),
        (SHORT_LAST, "v4l", SHORT_LAST, (0, 4), Some((0, 0))),
        (SHORT_LAST, "v9l", SHORT_LAST, (0, 4), Some((0, 0))),
        (SHORT_LAST, "v9ld", "x\n", (0, 0), None),
        (SHORT_LAST, "v3lld", "x\n", (0, 0), None),
        (SHORT_LAST, "4l", SHORT_LAST, (0, 3), None),
        (SHORT_LAST, "4lv", SHORT_LAST, (0, 3), Some((0, 3))),
        (SHORT_LAST, "v4lh", SHORT_LAST, (0, 3), Some((0, 0))),
        (SHORT_LAST, "v$hl", SHORT_LAST, (0, 4), Some((0, 0))),
        (SHORT_LAST, "v4lj", SHORT_LAST, (1, 1), Some((0, 0))),
        (SHORT_LAST, "v4lo", SHORT_LAST, (0, 0), Some((0, 4))),
        (SHORT_LAST, "V4l", SHORT_LAST, (0, 4), Some((0, 0))),
        (SHORT_LAST, "jv4l", SHORT_LAST, (1, 1), Some((1, 0))),
        (SHORT_LAST, "jv4ld", "abcd\n\n", (1, 0), None),
        ("ab cd\nx\n", "ve", "ab cd\nx\n", (0, 1), Some((0, 0))),
        ("ab cd\nx\n", "vee", "ab cd\nx\n", (0, 4), Some((0, 0))),
        ("ab cd\nx\n", "v2e", "ab cd\nx\n", (0, 4), Some((0, 0))),
        ("ab cd\nx\n", "vfdl", "ab cd\nx\n", (0, 5), Some((0, 0))),
        ("ab cd\nx\n", "v4lw", "ab cd\nx\n", (1, 0), Some((0, 0))),
    ];
    let mut out = Vec::new();
    for &(text, keys, after, at, anchor) in cases {
        let mut app = fresh(text, 4);
        feed(&mut app, keys);
        let c = app.buffer.cursor;
        let got = (app.buffer.rope.to_string(), (c.line, c.col), app.visual_anchor().map(|a| (a.line, a.col)));
        if got != (after.to_string(), at, anchor) {
            out.push(format!("{text:?} {keys:?}: got {got:?}, Neovim {:?}", (after, at, anchor)));
        }
    }
    let mut app = fresh(SHORT_LAST, 4);
    feed(&mut app, "v");
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    feed(&mut app, "4");
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    if app.buffer.cursor.col != 4 {
        out.push(format!("v<Right>4<Right>: col {}, Neovim 4", app.buffer.cursor.col));
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// Visual `o` sets the goal afresh at the end it lands on, both ways, as
/// Neovim's: `v$ooj` on `abcd`, `abcdefghij` goes to column 4, under the
/// first line's break, not to the second line's end.
#[test]
fn visual_other_end_takes_a_fresh_goal_as_neovim() {
    const AB: &str = "abcd\nabcdefghij\n";
    #[rustfmt::skip]
    let cases: &[VisualCase] = &[
        (AB, "v$ooj", AB, (1, 4), Some((0, 0))),
        (AB, "v$oj", AB, (1, 0), Some((0, 4))),
        (AB, "v$oojd", "fghij\n", (0, 0), None),
        (AB, "v$oo", AB, (0, 4), Some((0, 0))),
        (AB, "v$ok", AB, (0, 0), Some((0, 4))),
        (AB, "jv$ook", AB, (0, 4), Some((1, 0))),
        (AB, "v4looj", AB, (1, 4), Some((0, 0))),
    ];
    let mut out = Vec::new();
    for &(text, keys, after, at, anchor) in cases {
        let mut app = fresh(text, 4);
        feed(&mut app, keys);
        let c = app.buffer.cursor;
        let got = (app.buffer.rope.to_string(), (c.line, c.col), app.visual_anchor().map(|a| (a.line, a.col)));
        if got != (after.to_string(), at, anchor) {
            out.push(format!("{keys:?}: got {got:?}, Neovim {:?}", (after, at, anchor)));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// The rows of `app` drawn on a `w` by `h` terminal, and the caret's row.
fn screen(app: &App, w: u16, h: u16) -> (Vec<String>, u16) {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::render::frame::render(f, app)).unwrap();
    let buf = t.backend().buffer().clone();
    let rows = (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect();
    (rows, t.get_cursor_position().unwrap().y)
}

/// A scroll that carries the cursor onto a wrapped line keeps it on a row
/// the scroll shows, even when its goal is on a row further down (`$` on a
/// paragraph taller than the pane): `Ctrl-e` scrolls one row, no more, and
/// the cursor takes the nearest column it can, the last on the bottom row.
#[test]
fn a_scroll_keeps_a_carried_cursor_in_the_rows_it_shows() {
    let words: Vec<String> = (0..80).map(|i| format!("w{i:02}")).collect();
    let text = format!("x\n{}\n", words.join(" "));
    let (w, h) = (40, 8);
    let mut app = fresh(&text, 4);
    app.term_size = (w, h);
    app.config.editor.scroll_off = 0;
    screen(&app, w, h);
    feed(&mut app, "$\u{5}");
    let (rows, caret) = screen(&app, w, h);
    let all = rows.join("\n");
    let is_word = |t: &str| t.len() == 3 && t.starts_with('w') && t[1..].bytes().all(|b| b.is_ascii_digit());
    let text_rows: Vec<usize> = (0..rows.len()).filter(|&y| rows[y].split_whitespace().any(is_word)).collect();
    assert!(rows[text_rows[0]].contains("w00"), "the paragraph's first row is on top:\n{all}");
    assert_eq!(app.buffer.cursor.line, 1);
    let bottom = *text_rows.last().unwrap();
    assert_eq!(caret as usize, bottom, "on the bottom row:\n{all}");
    // On the last column of that row, the blank before the next row's
    // first word.
    let last: usize = rows[bottom].split_whitespace().last().unwrap()[1..].parse().unwrap();
    let next_row = text.lines().nth(1).unwrap().find(&format!("w{:02}", last + 1)).unwrap();
    assert_eq!(app.buffer.cursor.col, next_row - 1, "\n{all}");
    // And the goal stays: `j` past the paragraph goes to the line's end.
    assert_eq!(app.buffer.cursor.goal_col, Cursor::LINE_END);
}

/// The wheel in Insert carries the cursor by the Insert caret's goal: the
/// first cell of a tab it stands before, as Insert's Up and Down, not the
/// tab's last cell Normal mode draws it on.
#[test]
fn the_wheel_in_insert_keeps_the_insert_caret_goal() {
    use ratatui::crossterm::event::{MouseEvent, MouseEventKind};
    let mut app = fresh("\tx\na\nb\nabcdef\nc\nd\n", 4);
    app.term_size = (40, 8);
    app.config.editor.scroll_off = 0;
    screen(&app, 40, 8);
    feed(&mut app, "i");
    app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: 10, row: 2, modifiers: KeyModifiers::NONE });
    app.sync_after_input();
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (3, 0));
}

/// Whether the cell just after the last `needle` drawn on screen (on the
/// last row that shows it) has the selection's background.
fn selected_after(app: &App, needle: &str) -> bool {
    let (w, h) = (40u16, 12u16);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::render::frame::render(f, app)).unwrap();
    let buf = t.backend().buffer().clone();
    let sel = app.theme.selection.to_ratatui();
    assert_ne!(sel, app.theme.background.to_ratatui(), "the selection is drawn in the background's color");
    let row = |y: u16| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
    let y = (0..h).rev().find(|&y| row(y).contains(needle)).unwrap_or_else(|| panic!("{needle:?} not drawn"));
    let text = row(y);
    let x = text[..text.rfind(needle).unwrap()].chars().count() + needle.chars().count();
    buf[(x as u16, y)].bg == sel
}

/// A selected line break is drawn as one selected cell after the line's
/// last character, on its last row, as Neovim draws it: in `v` where the
/// selection takes it, on every line in `V`. The file's last line break,
/// which a selection never takes, is not.
#[test]
fn a_selected_line_break_is_drawn_as_neovim() {
    let long = format!("{} end", ["word"; 12].join(" "));
    let wrapped = format!("{long}\nx\n");
    #[rustfmt::skip]
    let cases: &[(&str, &str, &str, bool)] = &[
        (SHORT_LAST, "v$", "abcd", true),
        (SHORT_LAST, "v4l", "abcd", true),
        (SHORT_LAST, "v3l", "abcd", false),
        (SHORT_LAST, "vj", "abcd", true),
        (SHORT_LAST, "vj", "x", false),
        (SHORT_LAST, "jv$", "x", false),
        (SHORT_LAST, "V", "abcd", true),
        (SHORT_LAST, "Vj", "x", true),
        (SHORT_LAST, "jVk", "abcd", true),
        ("abcd\n\nx\n", "vjj", "abcd", true),
        ("# Head\n**ab**\nx\ny\n", "jv$", "**ab**", true),
        ("# Head\n**ab**\nx\ny\n", "jv$h", "**ab**", false),
        (&wrapped, "v$", "end", true),
        (&wrapped, "v$h", "end", false),
    ];
    let mut out = Vec::new();
    for &(text, keys, needle, want) in cases {
        let mut app = fresh(text, 4);
        feed(&mut app, keys);
        if selected_after(&app, needle) != want {
            out.push(format!("{text:?} {keys:?}: the cell after {needle:?} selected: {}, Neovim {want}", !want));
        }
    }
    // An empty line in the selection is one selected cell.
    let mut app = fresh("abcd\n\nx\n", 4);
    feed(&mut app, "vjj");
    let sel = app.theme.selection.to_ratatui();
    let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
    t.draw(|f| crate::render::frame::render(f, &app)).unwrap();
    let buf = t.backend().buffer().clone();
    let row = |y: u16| (0..40).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
    let y = (0..12).find(|&y| row(y).contains("abcd")).unwrap();
    let x = row(y).find("abcd").unwrap() as u16;
    if buf[(x, y + 1)].bg != sel {
        out.push("the empty line's break is not drawn selected".into());
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}
