//! What the renderer really draws on 16- and 256-colour terminals: every
//! glyph's foreground against the background under it, for the overlays and
//! the composed styles where two theme roles meet on one cell.
//!
//! Neovim's cterm defaults give each overlay BOTH colours (`Visual
//! ctermfg=0 ctermbg=15`, `Search` and `CurSearch` `ctermfg=0 ctermbg=11`),
//! which is why its overlays never hide the text under them; these tests
//! hold Shoin to the same.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color as Rt;
use ratatui::Terminal;

use crate::app::{App, Search};
use crate::input::mode::Mode;
use crate::render::theme::{ColorDepth, Theme};
use crate::test_support;
use crate::text::cursor::Cursor;

const DEPTHS: [ColorDepth; 2] = [ColorDepth::Basic16, ColorDepth::Indexed256];

/// `text` open with `[theme]` set from `theme` (TOML lines), adapted to
/// `depth` rather than to the terminal the tests run in.
fn app_at(text: &str, theme: &str, depth: ColorDepth) -> App {
    let cfg = crate::config::parse(&format!("[theme]\n{theme}\n")).unwrap();
    let mut app = test_support::app_with_config(text, cfg);
    let mut t = Theme::authored(&app.config.theme).unwrap();
    t.adapt(depth);
    app.theme = t;
    app
}

fn render(app: &App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    terminal.draw(|f| super::frame::render(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn search(app: &mut App, pattern: &str) {
    let compiled = crate::text::pattern::Pattern::compile(pattern, false, None).unwrap();
    app.search = Some(Search::new(pattern.into(), false, compiled));
}

fn row_of(buf: &Buffer, needle: &str) -> u16 {
    (0..buf.area.height)
        .find(|&y| row_text(buf, y).contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not drawn: {:#?}", (0..buf.area.height).map(|y| row_text(buf, y)).collect::<Vec<_>>()))
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()
}

/// The cells of `word`'s first appearance on row `y`.
fn cells(buf: &Buffer, y: u16, word: &str) -> std::ops::Range<u16> {
    let row = row_text(buf, y);
    let at = row.find(word).unwrap_or_else(|| panic!("{word:?} not on {row:?}"));
    let x = row[..at].chars().count() as u16;
    x..x + word.chars().count() as u16
}

/// Every glyph in `xs` on row `y` is drawn in a colour other than the
/// background under it.
fn readable(buf: &Buffer, y: u16, xs: std::ops::Range<u16>, what: &str) {
    for x in xs {
        let c = &buf[(x, y)];
        if c.symbol().trim().is_empty() {
            continue;
        }
        assert_ne!(c.fg, c.bg, "{what}: {:?} at {x} is drawn on its own colour", c.symbol());
    }
}

/// Searching inside `**bold**`: the built-in theme's bold is bright yellow
/// in 16 colours, the colour of the search match, so the matched letters
/// vanished. The match carries its own foreground now, as Neovim's
/// `Search` does: black on bright yellow.
#[test]
fn a_search_match_in_bold_text_stays_readable() {
    for depth in DEPTHS {
        let mut app = app_at("top\n\nsome **needle** here\n", "", depth);
        search(&mut app, "needle");
        let buf = render(&app);
        let y = row_of(&buf, "needle");
        let xs = cells(&buf, y, "needle");
        let cell = &buf[(xs.start, y)];
        assert_eq!(cell.bg, app.theme.search_match.to_ratatui(), "{depth:?}: the match is lit");
        readable(&buf, y, xs, &format!("{depth:?} search in bold"));
    }
    let mut app = app_at("top\n\nsome **needle** here\n", "", ColorDepth::Basic16);
    search(&mut app, "needle");
    let buf = render(&app);
    let y = row_of(&buf, "needle");
    let x = cells(&buf, y, "needle").start;
    assert_eq!((buf[(x, y)].fg, buf[(x, y)].bg), (Rt::Indexed(0), Rt::Indexed(11)), "Neovim's Search pair");
}

/// A Visual selection over bold, a link and inline code, with a theme that
/// puts the selection's fallback on the bold colour: the selection draws
/// its own foreground over every one of them.
#[test]
fn a_selection_over_bold_link_and_code_stays_readable() {
    // On the cursor's line, which is drawn raw: a selected line is active.
    let text = "some **bold** [link](x) `code` dim\n\ntop\n";
    for theme in ["", "selection = \"black\"\nbold = \"white\"\nlink = \"bright_white\"\ncode = \"white\""] {
        for depth in DEPTHS {
            let mut app = app_at(text, theme, depth);
            app.mode = Mode::VisualLine;
            app.anchor = Some(Cursor::new(0, 0));
            app.buffer.cursor = Cursor::new(0, 3);
            let buf = render(&app);
            let y = row_of(&buf, "bold");
            for word in ["some", "bold", "link", "code", "dim"] {
                let xs = cells(&buf, y, word);
                assert_eq!(buf[(xs.start, y)].bg, app.theme.selection.to_ratatui(), "{word} selected");
                readable(&buf, y, xs, &format!("{depth:?} {theme:?}: selected {word}"));
            }
        }
    }
    // The built-in theme in 16 colours gets Neovim's Visual pair.
    let mut app = app_at(text, "", ColorDepth::Basic16);
    app.mode = Mode::Visual;
    app.anchor = Some(Cursor::new(0, 0));
    app.buffer.cursor = Cursor::new(0, 2);
    let buf = render(&app);
    let y = row_of(&buf, "some");
    let x = cells(&buf, y, "some").start;
    assert_eq!((buf[(x, y)].fg, buf[(x, y)].bg), (Rt::Indexed(0), Rt::Indexed(15)), "Neovim's Visual pair");
}

/// The `:s///c` match is told apart from the other search matches around
/// it, and is readable, even when the theme's search colour is the one the
/// selection would fall back to.
#[test]
fn the_substitute_confirm_match_is_distinct_and_readable() {
    for theme in ["", "search_match = \"bright_white\"", "selection = \"black\"\nbold = \"white\""] {
        for depth in DEPTHS {
            let mut app = app_at("ab **ab** ab\n\ntop\n", theme, depth);
            for ch in ":%s/ab/x/gc".chars() {
                app.on_key(ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::Char(ch)));
            }
            app.on_key(ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::Enter));
            assert!(app.substitute_prompt().is_some(), "confirming");
            let buf = render(&app);
            let y = row_of(&buf, "ab ");
            let a: Vec<u16> = (0..buf.area.width).filter(|&x| buf[(x, y)].symbol() == "a").collect();
            let (current, other) = (a[0], a[1]);
            assert_eq!(buf[(other, y)].bg, app.theme.search_match.to_ratatui(), "{depth:?} {theme:?}: the next match is lit");
            assert_ne!(buf[(current, y)].bg, buf[(other, y)].bg, "{depth:?} {theme:?}: confirm vs search");
            assert_ne!(buf[(current, y)].bg, app.theme.background.to_ratatui());
            readable(&buf, y, 0..buf.area.width, &format!("{depth:?} {theme:?}: confirm row"));
        }
    }
}

/// A selected line break is one blank selected cell; on a fenced code row it
/// has to differ from the slab it sits on, or nothing shows.
#[test]
fn a_selected_line_break_in_code_differs_from_the_slab() {
    let theme = "background = \"black\"\ntext = \"green\"\nselection = \"black\"\ncode_bg = \"bright_white\"";
    for depth in DEPTHS {
        let mut app = app_at("top\n\n```\ncode\n```\nend\n", theme, depth);
        app.mode = Mode::VisualLine;
        app.anchor = Some(Cursor::new(3, 0));
        app.buffer.cursor = Cursor::new(3, 0);
        let buf = render(&app);
        let y = row_of(&buf, "code");
        let after = cells(&buf, y, "code").end;
        assert_eq!(buf[(after, y)].bg, app.theme.selection.to_ratatui(), "{depth:?}: the break is selected");
        assert_ne!(app.theme.selection, app.theme.code_bg, "{depth:?}: selection vs slab");
    }
}

/// A link label holding inline code or a tag draws the link colour on the
/// code or tag background: the fallback for those backgrounds has to avoid
/// the link colour as well.
#[test]
fn link_text_on_code_and_tag_backgrounds_stays_readable() {
    for depth in DEPTHS {
        let app = app_at("top\n\n[a `code` and #tag](x)\n", "link = \"bright_black\"", depth);
        let buf = render(&app);
        let y = row_of(&buf, "code");
        readable(&buf, y, cells(&buf, y, "code"), &format!("{depth:?}: code in a link"));
        readable(&buf, y, cells(&buf, y, "tag"), &format!("{depth:?}: tag in a link"));
    }
}

/// The status bar: hints are drawn in `list_bullet` and `text_dim`, a message
/// in the text colour, an error in the error colour, all on `status_bg`.
#[test]
fn the_status_bar_is_readable_on_its_background() {
    for depth in DEPTHS {
        let mut app = app_at("some text here\n", "text_dim = \"bright_black\"", depth);
        let bottom = |app: &App| {
            let buf = render(app);
            let y = buf.area.height - 1;
            (buf.clone(), y)
        };
        let (buf, y) = bottom(&app);
        assert!(!row_text(&buf, y).trim().is_empty(), "{depth:?}: hints are drawn");
        readable(&buf, y, 0..buf.area.width, &format!("{depth:?}: hints"));
        for kind in [crate::app::FlashKind::Info, crate::app::FlashKind::Error] {
            app.flash = Some(crate::app::Flash {
                text: Some("a message".into()),
                kind,
                expires_at: std::time::Instant::now() + std::time::Duration::from_secs(60),
            });
            let (buf, y) = bottom(&app);
            let xs = cells(&buf, y, "message");
            readable(&buf, y, xs.clone(), &format!("{depth:?}: status message"));
            assert_ne!(buf[(xs.start, y)].fg, Rt::Reset, "{depth:?}: a message has a known colour on the bar");
        }
    }
}

/// Colours are compared by palette slot: a terminal can set slot 0 and slot
/// 16 apart, so an explicit `selection = 0` over `background = 16` is kept.
#[test]
fn explicit_indexed_colours_are_compared_by_slot() {
    let mut t = Theme::authored(
        &crate::config::parse("[theme]\nbackground = 16\nselection = 0\n").unwrap().theme,
    ).unwrap();
    t.adapt(ColorDepth::Indexed256);
    assert_eq!(t.background, crate::render::theme::Color::Indexed(16));
    assert_eq!(t.selection, crate::render::theme::Color::Indexed(0));
}
