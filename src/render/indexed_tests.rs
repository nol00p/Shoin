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
    render_sized(app, 60, 12)
}

fn render_sized(app: &App, w: u16, h: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
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
    for theme in ["", "search_match = \"bright_white\"", "bold = \"white\""] {
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

/// The style of every cell on row `y`, for comparing two renders.
fn styles(buf: &Buffer, y: u16) -> Vec<(Rt, Rt, ratatui::style::Modifier)> {
    (0..buf.area.width).map(|x| { let c = &buf[(x, y)]; (c.fg, c.bg, c.modifier) }).collect()
}

/// `y`'s cells in `a` and `b` have the same foreground and attributes; in
/// `b`, each of `xs` sits on `bg`.
fn same_fg_on(a: &Buffer, b: &Buffer, y: u16, xs: std::ops::Range<u16>, bg: Rt, what: &str) {
    for x in xs {
        let (was, now) = (&a[(x, y)], &b[(x, y)]);
        assert_eq!((now.fg, now.modifier), (was.fg, was.modifier), "{what}: {:?} at {x} keeps its own colour", now.symbol());
        assert_eq!(now.bg, bg, "{what}: {:?} at {x} is lit", now.symbol());
    }
}

/// On a truecolour terminal everything draws as on main: status messages in
/// the terminal's own foreground (dim) and errors in ANSI red; Visual,
/// search, the tree, the finder and help paint only their background and
/// keep each glyph's own colour.
#[test]
fn truecolour_draws_as_before() {
    let depth = ColorDepth::TrueColor;
    // The status bar.
    let mut app = app_at("some text\n", "", depth);
    for (name, kind, fg, dim) in [("info", crate::app::FlashKind::Info, Rt::Reset, true), ("error", crate::app::FlashKind::Error, Rt::Red, false)] {
        app.flash = Some(crate::app::Flash {
            text: Some("a message".into()),
            kind,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(60),
        });
        let buf = render(&app);
        let y = buf.area.height - 1;
        for x in cells(&buf, y, "a message") {
            let c = &buf[(x, y)];
            assert_eq!(c.fg, fg, "{name}: the message's colour, as on main");
            assert_eq!(c.modifier.contains(ratatui::style::Modifier::DIM), dim, "{name}: dim as on main");
        }
    }

    // Visual and search: the background only. On the cursor's line, drawn
    // raw, so the rows compare cell for cell.
    let text = "some **bold** [link](x) `code` #tag end\n\ntop\n";
    let plain = render(&app_at(text, "", depth));
    let y = row_of(&plain, "bold");
    let mut visual = app_at(text, "", depth);
    visual.mode = Mode::VisualLine;
    visual.anchor = Some(Cursor::new(0, 0));
    visual.buffer.cursor = Cursor::new(0, 1);
    let words = cells(&plain, y, "some").start..cells(&plain, y, "end").end;
    same_fg_on(&plain, &render(&visual), y, words, visual.theme.selection.to_ratatui(), "Visual");
    let mut found = app_at(text, "", depth);
    search(&mut found, "bold");
    same_fg_on(&plain, &render(&found), y, cells(&plain, y, "bold"), found.theme.search_match.to_ratatui(), "search");

    // The tree: a selected row differs from itself unselected only by its
    // background.
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("shoin-truecolour-tree-{t}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("alpha.md"), "").unwrap();
    std::fs::write(dir.join("beta.md"), "").unwrap();
    let with_tree = |selected: &str| {
        let mut app = app_at("hi\n", "", depth);
        let mut tree = crate::tree::FileTree::open(dir.clone());
        tree.selected = tree.entries.iter().position(|e| e.name == selected).unwrap();
        app.tree = Some(tree);
        app
    };
    let (other, picked) = (with_tree("beta.md"), with_tree("alpha.md"));
    let (a, b) = (render(&other), render(&picked));
    let y = row_of(&a, "alpha.md");
    let name = cells(&a, y, "alpha.md");
    same_fg_on(&a, &b, y, name, picked.theme.selection.to_ratatui(), "tree row");

    // The finder: as main drew it, the text colour on the selection, the
    // matched characters in the accent.
    let mut app = app_at("hi\n", "", depth);
    let mut finder = crate::finder::Finder::open(dir.clone());
    finder.push_char('b');
    app.finder = Some(finder);
    let buf = render(&app);
    let y = row_of(&buf, "beta.md");
    let xs = cells(&buf, y, "beta.md");
    let sel = app.theme.selection.to_ratatui();
    assert_eq!(buf[(xs.start, y)].bg, sel, "finder: the row is selected");
    assert_ne!(buf[(xs.start, y)].fg, app.theme.text.to_ratatui(), "finder: the match is in the accent");
    for x in xs.start + 1..xs.end {
        assert_eq!((buf[(x, y)].fg, buf[(x, y)].bg), (app.theme.text.to_ratatui(), sel), "finder: the rest in the text colour");
    }
    std::fs::remove_dir_all(&dir).ok();

    // Help: the marked row keeps its spans' colours.
    let mut app = app_at("hi\n", "", depth);
    let mut help = crate::help::Help::open("basics");
    assert!(help.search("register", false));
    app.help = Some(help);
    let marked = render_sized(&app, 100, 60);
    app.help.as_mut().unwrap().mark = None;
    let unmarked = render_sized(&app, 100, 60);
    let y = (0..marked.area.height)
        .find(|&y| (0..marked.area.width).any(|x| marked[(x, y)].bg == sel))
        .expect("a marked help row");
    let xs = (0..marked.area.width).filter(|&x| marked[(x, y)].bg == sel && !marked[(x, y)].symbol().trim().is_empty()).collect::<Vec<_>>();
    assert!(!xs.is_empty());
    for x in xs {
        assert_eq!(marked[(x, y)].fg, unmarked[(x, y)].fg, "help: {:?} keeps its colour", marked[(x, y)].symbol());
    }
    assert_eq!(styles(&marked, 0), styles(&unmarked, 0));
}

/// A colour set in `[theme]` is the user's choice and is never rewritten,
/// even onto the page's slot; colours left to the built-in theme still are.
#[test]
fn explicit_colours_are_kept_and_defaults_are_guarded() {
    let theme = |toml: &str, depth| {
        let mut t = Theme::authored(&crate::config::parse(&format!("[theme]\n{toml}\n")).unwrap().theme).unwrap();
        t.adapt(depth);
        t
    };
    use crate::render::theme::Color as C;
    let t = theme("background = 16\ntext = 0\nselection = 0", ColorDepth::Indexed256);
    assert_eq!((t.background, t.text, t.selection), (C::Indexed(16), C::Indexed(0), C::Indexed(0)));
    // The same slot as the page, set on purpose: kept.
    let t = theme("background = \"black\"\ntext = \"black\"\ncode_bg = \"black\"", ColorDepth::Basic16);
    assert_eq!((t.text, t.code_bg), (C::Indexed(0), C::Indexed(0)));
    // An explicit page, with the text left to the built-in theme (#c0caf5,
    // bright white in 16 colours): the default text moves off the page.
    let t = theme("background = \"bright_white\"", ColorDepth::Basic16);
    assert_eq!(t.background, C::Indexed(15));
    assert_ne!(t.text, C::Indexed(15));
}

/// `==…==` over a heading, bold, a link, code or a tag: the highlight keeps
/// the explicit `bright_yellow`, and every glyph on it stays readable.
#[test]
fn inline_highlights_over_styled_text_stay_readable() {
    let text = "top\n\n### ==Heading==\n\nsome ==**bold** [link](x) `code` #tag plain== end\n";
    for theme in ["", "highlight_bg = \"bright_yellow\""] {
        for depth in DEPTHS {
            let app = app_at(text, theme, depth);
            let buf = render(&app);
            for (row, words) in [("Heading", &["Heading"][..]), ("bold", &["bold", "link", "code", "tag", "plain"][..])] {
                let y = row_of(&buf, row);
                for word in words {
                    readable(&buf, y, cells(&buf, y, word), &format!("{depth:?} {theme:?}: {word} highlighted"));
                }
            }
            if !theme.is_empty() {
                assert_eq!(app.theme.highlight_bg, crate::render::theme::Color::Indexed(11), "{depth:?}: kept as set");
                let y = row_of(&buf, "Heading");
                let x = cells(&buf, y, "Heading").start;
                assert_eq!(buf[(x, y)].bg, Rt::Indexed(11), "{depth:?}: the heading is highlighted");
            }
        }
    }
}
