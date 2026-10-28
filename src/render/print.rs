//! `shoin --render`: the editor's page, printed to a terminal and done.
//!
//! The frame is drawn by the SAME `frame::render` the editor uses, into an
//! off-screen buffer as tall as the document, and the cells are written out as
//! ANSI SGR. A separate ANSI writer over the parse would be a second renderer,
//! and the two would drift: every conceal rule, slab and bullet would have to
//! be taught twice.
//!
//! What differs from the editor is only what a printed page has no use for:
//! there is no cursor, so no line is active and every line conceals, and no
//! status line, scroll hint or line numbers. Embeds are expanded in full,
//! whatever `transclude.embed` says: a page you only read has no cursor to
//! open them with, and the plain-text path expands them too.
//!
//! Front matter is left out, as every export leaves it out: it is metadata
//! about the page, not part of it.
//!
//! Pictures are painted the way the editor paints them, outside the cells, at
//! the boxes the frame left blank for them (`App::paint_images`).

use std::collections::HashMap;
use std::fmt::Write as _;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::render::conceal::ActiveSet;
use crate::render::numbers::NumberMode;

/// Draw `app`'s document at `width` columns and return it as ANSI text.
pub fn styled(app: &mut App, width: u16) -> String {
    // Park the active set past the last line, so nothing is revealed raw.
    app.active = ActiveSet { start: usize::MAX, end: usize::MAX };
    app.config.status.enabled = false;
    app.config.layout.scroll_hint = false;
    app.numbers = NumberMode::Off;
    app.embed_mode = crate::transclude::Mode::Full;

    // One draw to fill the render cache, which then knows how many rows the
    // document wraps into; the second draw is exactly that tall.
    draw(app, width, 1);
    let rows = app.docs[app.current()].cache.borrow().total_rows();
    let pad = app.config.layout.padding_top + app.config.layout.padding_bottom;
    let height = (rows + pad as usize).clamp(1, u16::MAX as usize) as u16;
    let buf = draw(app, width, height);
    let top = app.config.layout.padding_top.min(height / 4);
    let hidden = top..top.saturating_add(front_matter_rows(app));
    ansi(&buf, app.theme.background.to_ratatui(), &pictures(app), hidden)
}

/// How many rows the document's front matter takes, and the blank lines after
/// it: the same lines `strip_front_matter` drops from every export.
fn front_matter_rows(app: &App) -> u16 {
    let b = &app.buffer;
    let text: String = (0..b.line_count()).map(|l| b.line_text(l) + "\n").collect();
    let body = crate::transclude::link::strip_front_matter(&text);
    let lines = text[..text.len() - body.len()].matches('\n').count();
    if lines == 0 {
        return 0;
    }
    let cache = app.cache.borrow();
    let rows = if lines < b.line_count() { cache.locate(lines, 0).0 } else { cache.total_rows() };
    rows.min(u16::MAX as usize) as u16
}

/// The escapes that paint each picture, keyed by the LAST row of its box.
///
/// They are written once that row has been printed, not at the picture's first
/// row: by then the terminal has scrolled to make room for the whole box, so
/// stepping the cursor back up into it is a relative move that cannot be
/// thrown off by a scroll. DECSC/DECRC put the cursor back after, whichever
/// way the protocol leaves it.
fn pictures(app: &App) -> HashMap<u16, String> {
    let mut out: HashMap<u16, String> = HashMap::new();
    let places = std::mem::take(&mut *app.images.borrow_mut());
    if app.image_protocol == crate::image::Protocol::None {
        return out;
    }
    let cache = app.cache.borrow();
    for p in places {
        let Some(crate::transclude::preview::Row::Image(pic)) = cache
            .entry(p.line)
            .and_then(|e| e.embed.as_ref())
            .and_then(|x| x.rows.get(p.index))
        else {
            continue;
        };
        if pic.payload.is_empty() || p.rows == 0 {
            continue;
        }
        let up = p.rows - 1;
        let s = out.entry(p.y + up).or_default();
        s.push_str("\x1b7");
        if up > 0 {
            let _ = write!(s, "\x1b[{up}A");
        }
        s.push('\r');
        if p.x > 0 {
            let _ = write!(s, "\x1b[{}C", p.x);
        }
        s.push_str(&crate::image::draw(app.image_protocol, &pic.payload, p.cols, p.rows));
        s.push_str("\x1b8");
    }
    out
}

/// Show `text` in `$PAGER`, or `less -R` when that is unset. False when no
/// pager could be started, so the caller prints it instead.
///
/// Through `sh -c`, so a `PAGER` with flags (`less -FR`, `bat -p`) works the
/// way it does for git. `LESS=R` is set when `LESS` is not, or a bare `less`
/// would show the colours as escape codes.
pub fn page(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let pager = std::env::var("PAGER")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "less -R".into());
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&pager).stdin(Stdio::piped());
    if std::env::var_os("LESS").is_none() {
        cmd.env("LESS", "R");
    }
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    // Quitting the pager before the end closes the pipe; that is not an error.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let _ = child.wait();
    true
}

fn draw(app: &App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width.max(1), height)).expect("an in-memory terminal");
    terminal
        .draw(|f| crate::render::frame::render(f, app))
        .expect("drawing into memory");
    terminal.backend().buffer().clone()
}

/// The cells of `buf` as SGR-coloured lines. The theme's own background is
/// left to the terminal, so the page sits on whatever the reader's terminal
/// shows rather than in a painted block; anything else that has a background
/// (a code slab, a highlight) keeps it. Blank rows at either end are dropped.
fn ansi(
    buf: &Buffer,
    page_bg: Color,
    after_row: &HashMap<u16, String>,
    hidden: std::ops::Range<u16>,
) -> String {
    let bg_of = |c: Color| if c == page_bg { Color::Reset } else { c };
    let w = buf.area.width;
    let mut lines: Vec<String> = Vec::with_capacity(buf.area.height as usize);
    for y in (0..buf.area.height).filter(|y| !hidden.contains(y)) {
        // Where the row stops mattering: past its last visible glyph or
        // non-default background.
        let end = (0..w)
            .rev()
            .find(|&x| {
                let c = &buf[(x, y)];
                !c.symbol().trim().is_empty() || bg_of(c.bg) != Color::Reset
            })
            .map_or(0, |x| x + 1);
        let mut out = String::new();
        let mut current: Option<(Color, Color, Modifier)> = None;
        let mut x = 0;
        while x < end {
            let c = &buf[(x, y)];
            let style = (c.fg, bg_of(c.bg), c.modifier);
            if current != Some(style) {
                out.push_str(&sgr(style.0, style.1, style.2));
                current = Some(style);
            }
            out.push_str(c.symbol());
            // A wide glyph owns the cells after it; ratatui leaves them blank.
            x += c.symbol().width().max(1) as u16;
        }
        if current.is_some() {
            out.push_str("\x1b[0m");
        }
        if let Some(paint) = after_row.get(&y) {
            out.push_str(paint);
        }
        lines.push(out);
    }
    let first = lines.iter().position(|l| !l.is_empty()).unwrap_or(lines.len());
    let last = lines.iter().rposition(|l| !l.is_empty()).map_or(first, |i| i + 1);
    let mut text = lines[first..last].join("\n");
    text.push('\n');
    text
}

/// One complete SGR sequence: a reset, then everything this cell asks for.
fn sgr(fg: Color, bg: Color, m: Modifier) -> String {
    let mut codes = vec!["0".to_string()];
    for (bit, code) in [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
        (Modifier::CROSSED_OUT, "9"),
    ] {
        if m.contains(bit) {
            codes.push(code.into());
        }
    }
    if let Some(c) = color(fg, false) {
        codes.push(c);
    }
    if let Some(c) = color(bg, true) {
        codes.push(c);
    }
    let mut s = String::from("\x1b[");
    let _ = write!(s, "{}m", codes.join(";"));
    s
}

fn color(c: Color, bg: bool) -> Option<String> {
    let base = if bg { 40 } else { 30 };
    Some(match c {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
        Color::Indexed(i) => format!("{};5;{i}", base + 8),
        Color::Black => format!("{}", base),
        Color::Red => format!("{}", base + 1),
        Color::Green => format!("{}", base + 2),
        Color::Yellow => format!("{}", base + 3),
        Color::Blue => format!("{}", base + 4),
        Color::Magenta => format!("{}", base + 5),
        Color::Cyan => format!("{}", base + 6),
        Color::Gray => format!("{}", base + 7),
        Color::DarkGray => format!("{}", base + 60),
        Color::LightRed => format!("{}", base + 61),
        Color::LightGreen => format!("{}", base + 62),
        Color::LightYellow => format!("{}", base + 63),
        Color::LightBlue => format!("{}", base + 64),
        Color::LightMagenta => format!("{}", base + 65),
        Color::LightCyan => format!("{}", base + 66),
        Color::White => format!("{}", base + 67),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::render::markdown::block::BlockCache;
    use crate::text::cursor::Cursor;

    fn app_with(text: &str) -> App {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.theme = crate::render::theme::Theme::authored(&app.config.theme).unwrap();
        app.buffer.insert_str(Cursor::new(0, 0), text);
        app.blocks = BlockCache::build(&app.buffer);
        app
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for ch in s.chars() {
            match (esc, ch) {
                (false, '\x1b') => esc = true,
                (true, 'm') => esc = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    /// Every line conceals, the first one included: there is no cursor to
    /// reveal it, and a printed `**` would be a rendering bug.
    #[test]
    fn every_line_is_concealed() {
        let mut app = app_with("# Title\n\nSome **bold** text.\n");
        let plain = strip(&styled(&mut app, 60));
        assert!(!plain.contains("**"), "{plain}");
        assert!(!plain.contains("# "), "{plain}");
        assert!(plain.contains("Title") && plain.contains("bold"), "{plain}");
    }

    /// The whole document is printed, not one screenful.
    #[test]
    fn a_long_document_prints_every_line() {
        let text: String = (0..300).map(|i| format!("line {i}\n\n")).collect();
        let mut app = app_with(&text);
        let plain = strip(&styled(&mut app, 60));
        assert!(plain.contains("line 0") && plain.contains("line 299"));
        assert!(!plain.starts_with('\n') && !plain.ends_with("\n\n"), "trimmed at both ends");
    }

    /// An embed prints its note in full, even with `transclude.embed` at
    /// its default of none, and a picture is painted into the box it left.
    #[test]
    fn embeds_expand_and_pictures_are_painted() {
        let dir = std::env::temp_dir().join(format!("shoin-print-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A real PNG header, claiming 400x200 so the box is several rows tall.
        let mut png = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89,
        ];
        png[16..20].copy_from_slice(&400u32.to_be_bytes());
        png[20..24].copy_from_slice(&200u32.to_be_bytes());
        std::fs::write(dir.join("photo.png"), &png).unwrap();
        std::fs::write(dir.join("other.md"), "The embedded sentence.\n").unwrap();
        let main = dir.join("main.md");
        std::fs::write(&main, "Before.\n\n![[other]]\n\n![[photo.png]]\n\nAfter.\n").unwrap();

        let mut app = App::new(Config::default(), Some(main), None).unwrap();
        app.image_protocol = crate::image::Protocol::Kitty;
        let out = styled(&mut app, 80);
        let plain = strip(&out);
        assert!(plain.contains("The embedded sentence."), "{plain}");
        assert!(!plain.contains("![["), "{plain}");
        assert!(plain.contains("After."), "{plain}");
        assert_eq!(out.matches("\x1b_Ga=T").count(), 1, "one picture, painted once");
        assert!(out.contains("\x1b7") && out.contains("\x1b8"), "the cursor is put back");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Front matter is metadata, so the printed page starts at the body.
    #[test]
    fn front_matter_is_left_out() {
        let mut app = app_with("---\ntitle: Secret\ntags: [a]\n---\n\n# Heading\n\nBody.\n");
        let plain = strip(&styled(&mut app, 60));
        assert!(!plain.contains("title") && !plain.contains("---"), "{plain}");
        assert!(plain.trim_start().starts_with("Heading"), "{plain}");
        assert!(plain.contains("Body."), "{plain}");

        // Without any, nothing is cut.
        let mut app = app_with("Para one.\n\n---\n\nPara two.\n");
        let plain = strip(&styled(&mut app, 60));
        assert!(plain.trim_start().starts_with("Para one.") && plain.contains("Para two."), "{plain}");
    }

    /// Bold is bold and the page background is left to the terminal.
    #[test]
    fn styles_become_sgr_and_the_page_background_does_not() {
        let mut app = app_with("Some **bold** text.\n");
        let out = styled(&mut app, 60);
        assert!(out.contains("\x1b[0;1;"), "bold reaches the output: {out:?}");
        let bg = match app.theme.background.to_ratatui() {
            Color::Rgb(r, g, b) => format!("48;2;{r};{g};{b}"),
            other => panic!("authored theme is RGB, got {other:?}"),
        };
        assert!(!out.contains(&bg), "the page background is not painted");
    }
}
