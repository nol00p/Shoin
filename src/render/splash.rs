//! The empty-start screen: what `shoin` with no file argument shows until you
//! type something.
//!
//! It is NOT a mode and NOT buffer content — no text is ever inserted. It is a
//! frame-time drawing over a genuinely empty, unnamed buffer, and the condition
//! that shows it is recomputed every frame rather than stored (`active`). That
//! is what makes it disappear the instant the first character lands, with no
//! state to get out of step: SPEC §1's "chrome appears when you summon it and
//! leaves when you're done", applied to the one piece of chrome you never asked
//! for.

use std::path::Path;

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::input::mode::Mode;
use crate::render::layout::display_width;
use crate::render::theme::Style as ThemeStyle;

// The bonsai is attribution-free (confirmed by its author, 2026-08-19), so it
// ships as the default. `[splash] art` replaces it with any text file.

/// One drawing, and the row its pot begins on — everything from there down is
/// earthenware rather than foliage, which is all the colouring needs to know.
struct Art {
    rows: &'static [&'static str],
    width: u16,
    pot: usize,
}

/// What the start screen draws, resolved once from `[splash] art` rather than
/// per frame — the render path must not touch the filesystem.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Chosen {
    /// The built-in bonsai, at whichever of its two sizes fits.
    #[default]
    BuiltIn,
    /// `art = "none"` — the hints, and nothing above them.
    Nothing,
    /// A file the reader supplied. One colour, because we know where the
    /// built-in's pot is and cannot know where anyone else's is.
    Custom(Vec<String>),
}

/// Resolve `[splash] art` into the drawing to use, plus a complaint if the
/// reader asked for a file that is not there.
///
/// A bad path falls back to the built-in rather than to nothing: an empty start
/// screen looks like a bug, and the flash says what actually happened.
pub fn choose(cfg: &crate::config::Config, config_dir: Option<&Path>) -> (Chosen, Option<String>) {
    let raw = cfg.splash.art.trim();
    match raw {
        "" => return (Chosen::BuiltIn, None),
        "none" | "off" => return (Chosen::Nothing, None),
        _ => {}
    }
    let path = expand(raw, config_dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let rows: Vec<String> = text
                .lines()
                .map(|l| l.trim_end().to_string())
                .collect();
            // Leading and trailing blank lines are the file's own padding, not
            // part of the picture; the screen does its own centring.
            let first = rows.iter().position(|r| !r.trim().is_empty());
            let last = rows.iter().rposition(|r| !r.trim().is_empty());
            match (first, last) {
                (Some(a), Some(b)) => (Chosen::Custom(rows[a..=b].to_vec()), None),
                _ => (
                    Chosen::BuiltIn,
                    Some(format!("splash: {} is empty", name_of(&path))),
                ),
            }
        }
        Err(_) => (
            Chosen::BuiltIn,
            Some(format!("splash: cannot read {}", name_of(&path))),
        ),
    }
}

/// `~` expanded, and a relative path taken as relative to the config directory
/// so an `art.txt` beside the `.conf` files just works.
fn expand(raw: &str, config_dir: Option<&Path>) -> std::path::PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    let p = Path::new(raw);
    match config_dir {
        Some(dir) if p.is_relative() => dir.join(p),
        _ => p.to_path_buf(),
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// The bonsai at a size that fits an 80x24 terminal — the one most readers will
/// actually see.
const SMALL: Art = Art {
    rows: &[
    "             ,.,             ",
    "          ,MMMMM,      ,.,   ",
    "    ,.,   \"  \" \"MM,  ,MMMMM, ",
    "  ,MMMMM,   ,.,  \"\"  \"  \"  \" ",
    "   \"  \"MM,,MMMMM,   ,.,      ",
    "      \"    \\_.  \" ,MMMMM,    ",
    "    ,MMMM,   \\_.  \"  \"       ",
    "     \"  \"._    \\_.           ",
    "           \"._ ( )           ",
    "    .________.--'--.________.",
    "     \\                     / ",
    "      \\___________________/  ",
    "      (_)               (_)  ",
    ],
    width: 29,
    pot: 9,
};

/// The same tree with every needle in it. Needs about 44x35 to sit above the
/// hints, so it is the exception rather than the rule.
const BIG: Art = Art {
    rows: &[
    "                   .s.s.                ",
    "                , `'`Y8bso.             ",
    "              ,d88bso y'd8l             ",
    "              \"`,8K j8P?*?b.            ",
    "             ,bonsai_`o.o               ",
    "        ,r.osbJ--','  e8b?Y..           ",
    "       j*Y888P*{ `._.-'\" 888b           ",
    "         `\"'``,.`'-. `\"*?*P\"            ",
    "          db8sld-'., ,):5ls.            ",
    "     <sd88P,-d888P'd888d8888Rdbc        ",
    "     `\"*J*CJ8*d8888l:'  ``88?bl.o       ",
    "     .o.sl.rsdP^*8bdbs.. *\"?**l888s.    ",
    "   ,`JYsd88P88ls?\\**\"`*`-. `  ` `\"`     ",
    "  dPJ88*J?P;Pd888D;=-.  -.l.s.          ",
    ".'`\"*Y,.sbsdkC l.    ?(     ^.          ",
    "     .Y8*?8P*\"`       `)` .' :          ",
    "       `\"`         _.-'. ,   k.         ",
    "                  (    : '  ('          ",
    "         _______ ,'`-  )`.` `.l  ___    ",
    "     r========-==-==-=-=-=------------=7",
    "     `Y - --  ---- -- -   .          ,' ",
    "       :                        '   :   ",
    "        \\-..  .. .. . . . . .     ,/    ",
    "     .-<=:`._____________________,'.:>-.",
    "     L______                        ___J",
    "            ````````````````````````    ",
    ],
    width: 40,
    pot: 18,
};

/// Rows a hint block of `n` lines needs, plus a little air above it. Derived
/// rather than a constant: the list is not always the same length, and a fixed
/// 7 would have let the art crowd a sixth line off the bottom.
fn hint_rows(n: usize) -> u16 {
    n as u16 + 2
}

/// Whether this frame should show the start screen.
///
/// Every clause is about the DOCUMENT, not about how the editor was launched,
/// so nothing has to be remembered or invalidated:
///   * one document, never named and never edited, with nothing in it;
///   * one pane, because a split means the reader is working;
///   * no overlay, which would be drawing over the top of it anyway.
///
/// `modified` is what makes it vanish on the first keystroke — and come back if
/// you undo all the way to a blank page again, which is the honest answer since
/// that IS a blank page.
pub fn active(app: &App) -> bool {
    app.docs.len() == 1
        && app.layout.count() == 1
        && app.finder.is_none()
        && app.help.is_none()
        && matches!(app.mode, Mode::Normal)
        && {
            let b = &app.docs[0].buffer;
            b.path.is_none() && !b.modified && b.line_count() <= 1 && b.line_len(0) == 0
        }
}

/// One hint: the keys, and what they do.
fn hints(app: &App) -> Vec<(String, &'static str)> {
    // Show the leader the way the reader would have to press it, so a rebound
    // one is not quietly wrong.
    let leader = match app.config.input.leader.as_str() {
        " " => "Space".to_string(),
        "" => "\\".to_string(),
        other => other.to_string(),
    };
    let mut out = vec![
        (format!("{leader} f e"), "browse files"),
        ("i".into(), "start writing"),
        (":w notes.md".into(), "save it under a name"),
        (":help".into(), "bindings, commands, config"),
        (":q".into(), "leave"),
    ];
    // The editor never seeds a config on its own — `config::init` records why.
    // What it can do is say the command exists, and only to the reader who has
    // no config yet, on the one screen that has room to say it.
    if !app.configured {
        out.push((
            "shoin --init-config".into(),
            "write a config you can edit",
        ));
    }
    out
}

/// Draw the start screen into a pane's rect.
///
/// The tree sits in the upper two thirds and the hints below it, both centered
/// on the pane rather than on the text measure — there is no text yet, so the
/// measure has nothing to align to.
pub fn render(frame: &mut Frame, app: &App, rect: Rect) {
    let theme = &app.theme;
    let hints = hints(app);

    // Pick the largest drawing the pane can hold above the hints, and drop the
    // art entirely rather than crop it — the hints are the part that does a job.
    let fits = |w: u16, h: usize| {
        rect.width >= w + 4 && rect.height >= h as u16 + hint_rows(hints.len()) + 2
    };
    // `rows` borrows either a built-in constant or the reader's file; `pot` is
    // `None` for a custom drawing, which is what turns the two-band colouring
    // into one.
    let (rows, width, pot): (Vec<&str>, u16, Option<usize>) = match &app.splash_art {
        Chosen::Nothing => (Vec::new(), 0, None),
        Chosen::Custom(lines) => {
            let w = lines.iter().map(|l| display_width(l)).max().unwrap_or(0);
            if fits(w, lines.len()) {
                (lines.iter().map(String::as_str).collect(), w, None)
            } else {
                // A reader who supplied art does not want ours instead; drop to
                // the hints alone.
                (Vec::new(), 0, None)
            }
        }
        Chosen::BuiltIn => {
            let pick = if fits(BIG.width, BIG.rows.len()) {
                Some(&BIG)
            } else if fits(SMALL.width, SMALL.rows.len()) {
                Some(&SMALL)
            } else {
                None
            };
            match pick {
                Some(a) => (a.rows.to_vec(), a.width, Some(a.pot)),
                None => (Vec::new(), 0, None),
            }
        }
    };
    let art_fits = !rows.is_empty();
    let art_h = rows.len() as u16;

    // Where the art ends, so the hints can be placed clear of it.
    let mut art_bottom = rect.y;
    if art_fits {
        // Centered in the top two thirds, so the hints below sit on the lower
        // third's optical line rather than immediately under the tree.
        let band = (rect.height * 2) / 3;
        let top = rect.y + band.saturating_sub(art_h) / 2;
        art_bottom = top + art_h;
        let x = rect.x + (rect.width.saturating_sub(width)) / 2;
        // Two colours, split at the rim: needles in the green the editor
        // already uses for code, earthenware in its one warm accent. Borrowed
        // from existing roles rather than given `[theme]` keys of their own —
        // a start screen is not worth two settings, and this way it follows
        // whatever theme is loaded.
        for (i, row) in rows.iter().enumerate() {
            let ink = match pot {
                Some(p) if i >= p => theme.bold,
                Some(_) => theme.code,
                // A drawing we did not compose gets one soft ink: we cannot
                // know which of its rows is earthenware.
                None => theme.quote,
            };
            let line = Line::from(Span::styled(
                (*row).to_string(),
                ThemeStyle::fg(ink).to_ratatui(),
            ));
            frame.render_widget(
                Paragraph::new(line),
                Rect { x, y: top + i as u16, width, height: 1 },
            );
        }
    }

    // Hints: keys and descriptions in two columns, aligned on the widest key so
    // the descriptions form a single edge.
    let key_w = hints.iter().map(|(k, _)| display_width(k)).max().unwrap_or(0);
    let gap = 4u16;
    let full_w = hints
        .iter()
        .map(|(_, d)| key_w + gap + display_width(d))
        .max()
        .unwrap_or(0);
    // Too narrow for both columns: drop the descriptions rather than let them
    // be clipped mid-word, which reads as a bug rather than as a small screen.
    let with_desc = full_w <= rect.width;
    let block_w = if with_desc { full_w } else { key_w }.min(rect.width);
    let x = rect.x + (rect.width.saturating_sub(block_w)) / 2;

    // The two-thirds line, unless the drawing runs past it — the full-detail
    // tree is taller than the band on all but an enormous terminal, and hints
    // printed over its canopy would be unreadable.
    let start = if art_fits {
        (rect.y + (rect.height * 2) / 3 + 1).max(art_bottom + 1)
    } else {
        rect.y + rect.height.saturating_sub(hints.len() as u16) / 2
    };

    for (i, (key, desc)) in hints.iter().enumerate() {
        let y = start + i as u16;
        if y >= rect.bottom() {
            break;
        }
        let pad = " ".repeat((key_w - display_width(key)) as usize);
        let mut spans = vec![
            Span::styled(pad, ThemeStyle::fg(theme.text_dim).to_ratatui()),
            Span::styled(key.clone(), ThemeStyle::fg(theme.list_bullet).to_ratatui()),
        ];
        if with_desc {
            spans.push(Span::styled(
                " ".repeat(gap as usize),
                ThemeStyle::fg(theme.text_dim).to_ratatui(),
            ));
            spans.push(Span::styled(
                (*desc).to_string(),
                ThemeStyle::fg(theme.text_dim).to_ratatui(),
            ));
        }
        let line = Line::from(spans);
        frame.render_widget(
            Paragraph::new(line),
            Rect { x, y, width: block_w, height: 1 },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::text::cursor::Cursor;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn bare() -> App {
        App::new(Config::default(), None, None).unwrap()
    }

    fn screen(app: &App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| crate::render::frame::render(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// `shoin` with no argument shows the start screen.
    #[test]
    fn a_bare_start_shows_the_screen() {
        let app = bare();
        assert!(active(&app));
        let rows = screen(&app, 90, 28);
        let all = rows.join("\n");
        assert!(all.contains("(_)"), "the tree should be drawn");
        assert!(all.contains("Space f e"), "the file-tree hint");
        assert!(all.contains("browse files"));
        assert!(all.contains(":help"));
        assert!(all.contains(":q"));
        assert!(all.contains("start writing"));
    }

    /// It is drawing, not content: the buffer stays genuinely empty, so the
    /// first thing typed is the first thing in the file.
    #[test]
    fn the_screen_is_not_buffer_content() {
        let app = bare();
        assert_eq!(app.docs[0].buffer.line_count(), 1);
        assert_eq!(app.docs[0].buffer.line_len(0), 0);
        assert!(!app.docs[0].buffer.modified);
    }

    /// One keystroke retires it, and the same frame renders the document.
    #[test]
    fn typing_retires_the_screen() {
        let mut app = bare();
        app.buffer.insert_str(Cursor::new(0, 0), "H");
        assert!(!active(&app), "a modified buffer is a document, not a start");

        let rows = screen(&app, 90, 28);
        let all = rows.join("\n");
        assert!(all.contains("H"));
        assert!(!all.contains("browse files"), "the hints are gone");
        assert!(!all.contains("(_)"), "and so is the tree");
    }

    /// Every clause of the condition earns its place.
    #[test]
    fn the_screen_stays_out_of_the_way() {
        let mut app = bare();

        // Naming it makes it a document even before it is written.
        app.buffer.path = Some(std::path::PathBuf::from("notes.md"));
        assert!(!active(&app), "a named buffer is not a blank start");
        app.buffer.path = None;
        assert!(active(&app));

        // Declaring intent to write clears the room.
        app.mode = Mode::Insert;
        assert!(!active(&app), "Insert means the reader is writing");
        app.mode = Mode::Normal;
        assert!(active(&app));
    }

    /// The hint shows the leader the reader would actually press.
    #[test]
    fn the_leader_hint_follows_the_config() {
        let mut cfg = Config::default();
        cfg.input.leader = ",".into();
        let app = App::new(cfg, None, None).unwrap();
        let all = screen(&app, 90, 28).join("\n");
        assert!(all.contains(", f e"), "a rebound leader must not be shown as Space");
        assert!(!all.contains("Space f e"));
    }

    /// A terminal big enough gets the full-detail drawing; the common one gets
    /// the compact tree. Neither may ever overlap the hints.
    #[test]
    fn the_biggest_drawing_that_fits_is_the_one_drawn() {
        let app = bare();

        // A tall terminal earns the detailed tree, which signs itself.
        let rows = screen(&app, 90, 40);
        assert!(rows.iter().any(|r| r.contains("bonsai")), "the detailed tree");
        let last_art = rows.iter().rposition(|r| r.contains("___")).unwrap();
        let first_hint = rows.iter().position(|r| r.contains("browse files")).unwrap();
        assert!(first_hint > last_art, "hints must not land on the tree");

        // The default terminal gets the compact one, and every hint still fits.
        let rows = screen(&app, 80, 24);
        assert!(!rows.iter().any(|r| r.contains("bonsai")), "no room for the big one");
        assert!(rows.iter().any(|r| r.contains("(_)")), "but a tree all the same");
        let last_art = rows.iter().rposition(|r| r.contains("(_)")).unwrap();
        let first_hint = rows.iter().position(|r| r.contains("browse files")).unwrap();
        assert!(first_hint > last_art);
        assert!(rows.iter().any(|r| r.contains("leave")), "…and every hint shows");
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shoin-sp-{tag}-{t}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// `[splash] art` swaps the drawing for the reader's own.
    #[test]
    fn a_reader_can_supply_their_own_art() {
        let d = scratch("custom");
        std::fs::write(d.join("art.txt"), "\n\n  /\\_/\\\n ( o.o )\n  > ^ <\n\n").unwrap();
        let mut cfg = Config::default();
        // Relative, to prove it resolves against the config directory.
        cfg.splash.art = "art.txt".into();

        let (chosen, warn) = choose(&cfg, Some(&d));
        assert!(warn.is_none(), "{warn:?}");
        match &chosen {
            Chosen::Custom(rows) => {
                assert_eq!(rows.len(), 3, "blank padding trimmed, picture kept");
                assert!(rows[1].contains("o.o"));
            }
            other => panic!("expected Custom, got {other:?}"),
        }

        let mut app = bare();
        app.splash_art = chosen;
        let all = screen(&app, 90, 28).join("\n");
        assert!(all.contains("o.o"), "the reader's art is drawn");
        assert!(!all.contains("(_)"), "and the built-in bonsai is not");
        assert!(all.contains("browse files"), "hints still there");
        std::fs::remove_dir_all(&d).ok();
    }

    /// `art = "none"` leaves the hints alone on the screen.
    #[test]
    fn art_can_be_turned_off_entirely() {
        let mut cfg = Config::default();
        cfg.splash.art = "none".into();
        let (chosen, warn) = choose(&cfg, None);
        assert_eq!(chosen, Chosen::Nothing);
        assert!(warn.is_none());

        let mut app = bare();
        app.splash_art = chosen;
        let all = screen(&app, 90, 28).join("\n");
        assert!(!all.contains("(_)"), "no drawing");
        assert!(all.contains("browse files"), "but the hints remain");
    }

    /// A path that is not there falls back to the built-in AND says so. An
    /// empty start screen would look like a bug rather than a typo.
    #[test]
    fn a_missing_art_file_complains_and_falls_back() {
        let mut cfg = Config::default();
        cfg.splash.art = "/nowhere/at/all/art.txt".into();
        let (chosen, warn) = choose(&cfg, None);
        assert_eq!(chosen, Chosen::BuiltIn);
        let w = warn.expect("a warning");
        assert!(w.contains("art.txt"), "names the file: {w}");
        assert!(w.len() < 64, "fits the status line: {w}");
    }

    /// The default is unchanged: no setting means the bonsai.
    #[test]
    fn the_default_is_still_the_built_in_bonsai() {
        let (chosen, warn) = choose(&Config::default(), None);
        assert_eq!(chosen, Chosen::BuiltIn);
        assert!(warn.is_none());
    }

    /// Custom art too big for the pane drops to the hints — never to OUR
    /// drawing, which the reader has said they do not want.
    #[test]
    fn oversized_custom_art_does_not_fall_back_to_the_bonsai() {
        let mut app = bare();
        app.splash_art = Chosen::Custom(
            (0..80).map(|i| format!("row {i} of a very tall picture")).collect(),
        );
        let all = screen(&app, 60, 20).join("\n");
        assert!(!all.contains("(_)"), "not our bonsai");
        assert!(!all.contains("row 0"), "and not a cropped version of theirs");
        assert!(all.contains("browse files"));
    }

    /// Too narrow for two columns: the descriptions go rather than get clipped
    /// mid-word, and the art goes before either.
    #[test]
    fn a_small_terminal_sheds_the_art_then_the_descriptions() {
        let app = bare();

        let all = screen(&app, 46, 14).join("\n");
        assert!(!all.contains("(_)"), "no room for the tree");
        assert!(all.contains("browse files"), "but the hints still read");

        let all = screen(&app, 38, 12).join("\n");
        assert!(all.contains("Space f e"), "the keys survive");
        assert!(!all.contains("browse files"), "the descriptions do not");
    }

    /// A reader with no config is told how to get one — and a reader who has
    /// one is not nagged about it.
    #[test]
    fn the_init_hint_appears_only_when_there_is_no_config() {
        let mut app = bare();

        app.configured = false;
        let text = screen(&app, 90, 28).join("\n");
        assert!(text.contains("--init-config"), "offered when there is none:\n{text}");
        assert!(text.contains("write a config you can edit"));
        // The extra line must not have pushed anything off the screen.
        assert!(text.contains(":q"), "the last hint still fits:\n{text}");

        app.configured = true;
        let text = screen(&app, 90, 28).join("\n");
        assert!(
            !text.contains("--init-config"),
            "not offered to someone who already has one:\n{text}"
        );
    }

}
