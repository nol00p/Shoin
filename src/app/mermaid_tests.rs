//! Mermaid previews in the editor, end to end: a stub renderer, real
//! frames, real keys. The renderer is a shell script, so these are Unix
//! only.
//!
//! The stub prints `D[<second line of the body>]` as the diagram, which
//! tells the fences apart, and for `--format png` waits for a `gate` file
//! (when the test made one necessary) and prints a PNG of its own.

#![cfg(unix)]

use super::*;
use crate::input::mode::Mode as InputMode;
use crate::render::layout::RowSource;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

/// A scratch directory holding the stub renderer, and an app with it as
/// `mermaid_renderer`, concealment on, editing `note.md` holding `body`.
struct Fixture {
    dir: PathBuf,
    app: App,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

const STUB: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo "$*" >>"$dir/log"
case "$*" in
  *png*)
    cat >/dev/null
    if [ -e "$dir/gated" ]; then
      while [ ! -e "$dir/gate" ]; do sleep 0.01; done
    fi
    cat "$dir/diagram.png"
    ;;
  *)
    if [ -e "$dir/fail" ]; then cat >/dev/null; echo oops >&2; exit 3; fi
    read first; read second; cat >/dev/null
    printf 'D[%s]\n' "$second"
    ;;
esac
"#;

fn fixture(body: &str, files: &[(&str, &str)], tweak: impl FnOnce(&mut Config)) -> Fixture {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("shoin-mermaid-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = dir.join("renderer");
    std::fs::write(&stub, STUB).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(dir.join("diagram.png"), crate::test_support::sized_png(480, 120)).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let note = dir.join("note.md");
    std::fs::write(&note, body).unwrap();
    let mut config = Config::default();
    config.layout.conceal = true;
    config.layout.typewriter = false;
    config.markdown.mermaid_renderer = Some(stub.to_string_lossy().into_owned());
    tweak(&mut config);
    let mut app = App::new(config, Some(note), None).unwrap();
    app.term_size = (60, 21);
    app.refresh_blocks();
    Fixture { dir, app }
}

fn feed(app: &mut App, keys: &str) {
    for c in keys.chars() {
        key(app, KeyCode::Char(c));
    }
}

fn key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    app.sync_after_input();
}

fn click(app: &mut App, column: u16, row: u16) {
    app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE });
    app.sync_after_input();
}

fn goto(app: &mut App, line: usize) {
    app.buffer.cursor = Cursor::new(line, 0);
    app.sync_after_input();
}

/// Draw a frame at the app's terminal size, as the event loop does.
fn draw(app: &mut App) -> ratatui::buffer::Buffer {
    app.refresh_blocks();
    let (w, h) = app.term_size;
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::render::frame::render(f, app)).unwrap();
    t.backend().buffer().clone()
}

fn rows_of(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    let area = buf.area;
    (0..area.height).map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect()).collect()
}

fn screen(app: &mut App) -> Vec<String> {
    rows_of(&draw(app))
}

/// Draw frames, taking the renderer's results between them, until the
/// screen passes `done`; that screen.
fn settle(app: &mut App, done: impl Fn(&[String]) -> bool) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = screen(app);
        if done(&rows) {
            return rows;
        }
        assert!(Instant::now() < deadline, "the screen never settled:\n{}", rows.join("\n"));
        if !app.poll_mermaid() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Take renderer results, without drawing, until one arrives.
fn wait_for_result(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.poll_mermaid() {
        assert!(Instant::now() < deadline, "the renderer did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn has(rows: &[String], needle: &str) -> bool {
    rows.iter().any(|row| row.contains(needle))
}

fn top(app: &App) -> usize {
    app.focused_pane().unwrap().scroll.get()
}

fn top_line(app: &App) -> usize {
    app.cache.borrow().row(top(app)).unwrap().line()
}

fn fence(name: &str) -> String {
    format!("```mermaid\ngraph TD\n{name}\n```\n")
}

/// How many times the stub renderer has run.
fn runs(f: &Fixture) -> usize {
    std::fs::read_to_string(f.dir.join("log")).map_or(0, |log| log.lines().count())
}

/// More diagrams on screen than the renderer's cache holds: a stable set
/// that fits is shown, the rest stay as source, and an idle editor stops
/// starting renders. Each result once evicted a diagram still on screen,
/// whose next frame asked for it again, without end.
#[test]
fn more_diagrams_on_screen_than_the_cache_holds_do_not_thrash() {
    let max = crate::render::mermaid::MAX_CACHE_ENTRIES;
    let body: String = (0..max + 3).map(|i| fence(&format!("F{i}"))).collect();
    let mut f = fixture(&format!("top\n{body}"), &[], |_| {});
    f.app.term_size = (60, 4 * (max as u16 + 3) + 10);
    let shown = |rows: &[String]| rows.iter().filter(|row| row.contains("D[F")).count();
    let rows = settle(&mut f.app, |rows| shown(rows) == max);
    assert!(has(&rows, "D[F0]") && !has(&rows, &format!("D[F{}]", max + 2)), "the first ones, in order");
    let idle = |f: &mut Fixture| {
        let until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < until {
            draw(&mut f.app);
            f.app.poll_mermaid();
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    idle(&mut f);
    let before = runs(&f);
    idle(&mut f);
    assert_eq!(runs(&f), before, "an idle editor started renders");
    assert_eq!(before, max, "one render per diagram shown");
    assert_eq!(shown(&screen(&mut f.app)), max);
}

/// A note embed whose header is above the screen still has the fences in
/// the part of it on screen rendered.
#[test]
fn an_embed_fence_under_a_header_scrolled_off_is_rendered() {
    let frag = format!("{}\n{}", "prose\n".repeat(40), fence("X1-->Y1"));
    let mut f = fixture("top\n![[frag]]\nafter\n", &[("frag.md", &frag)], |_| {});
    f.app.embed_mode = crate::transclude::Mode::Short;
    goto(&mut f.app, 2);
    let rows = settle(&mut f.app, |rows| has(rows, "D[X1-->Y1]"));
    assert!(!has(&rows, "frag"), "the embed's header is above the screen");
    assert!(top(&f.app) > 0);
}

/// An embed with more fences than the renderer keeps results for: every
/// one renders once it is on screen, the 33rd and 34th included.
#[test]
fn every_fence_of_a_long_embed_renders_when_reached() {
    let frag: String = (0..34).map(|i| format!("{}\n", fence(&format!("X{i}-->Y{i}")))).collect();
    let mut f = fixture("top\n![[frag]]\nafter\n", &[("frag.md", &frag)], |_| {});
    f.app.embed_mode = crate::transclude::Mode::Short;
    goto(&mut f.app, 2);
    settle(&mut f.app, |rows| has(rows, "D[X33-->Y33]") && has(rows, "D[X32-->Y32]"));
    // And back at the top, the first ones.
    goto(&mut f.app, 0);
    settle(&mut f.app, |rows| has(rows, "D[X0-->Y0]") && has(rows, "D[X1-->Y1]"));
}

/// Lines 0–29 prose, a fence on 30–62 (a 31-line body), prose from 63.
fn tall_fence_note() -> String {
    let body: String = (0..30).map(|i| format!("A{i}-->B{i}\n")).collect();
    let after: String = (0..40).map(|i| format!("q{i}\n")).collect();
    format!("{}```mermaid\ngraph TD\n{body}```\n{after}", (0..30).map(|i| format!("p{i}\n")).collect::<String>())
}

/// The top of the screen is inside a fence's source when its diagram
/// arrives: the screen then starts at the diagram, where that source went,
/// not at the cursor, which did not move.
#[test]
fn a_diagram_arriving_under_the_top_keeps_the_top_on_its_source() {
    let mut f = fixture(&tall_fence_note(), &[], |c| c.editor.scroll_off = 0);
    goto(&mut f.app, 68);
    f.app.focused_pane().unwrap().scroll.set(55);
    screen(&mut f.app);
    assert_eq!(top(&f.app), 55, "the cursor is on screen, so the top stays");
    wait_for_result(&mut f.app);
    let rows = screen(&mut f.app);
    assert_eq!(f.app.buffer.cursor.line, 68);
    assert_eq!(top_line(&f.app), 30, "the top is the diagram, where line 55 went:\n{}", rows.join("\n"));
    assert!(has(&rows, "D[A0-->B0]"));
}

/// A diagram arriving above the screen leaves the same text on top.
#[test]
fn a_diagram_arriving_above_the_top_leaves_the_text_on_top_in_place() {
    let mut f = fixture(&tall_fence_note(), &[], |c| c.editor.scroll_off = 0);
    goto(&mut f.app, 29);
    f.app.focused_pane().unwrap().scroll.set(20);
    let rows = screen(&mut f.app);
    assert!(has(&rows, "A0-->B0"), "the fence is on screen, so asked for:\n{}", rows.join("\n"));
    goto(&mut f.app, 90);
    let before = screen(&mut f.app);
    let line = top_line(&f.app);
    assert!(line > 62, "the fence is above the screen");
    wait_for_result(&mut f.app);
    let after = screen(&mut f.app);
    assert_eq!(top_line(&f.app), line);
    // The scrollbar's thumb moves: the document is shorter. The text does not.
    let text = |rows: &[String]| rows.iter().map(|row| row.trim_end_matches(['\u{2590}', ' ']).to_string()).collect::<Vec<_>>();
    assert_eq!(text(&before), text(&after), "the screen did not move");
}

/// `-R`: a search landing on a hit inside a diagram shows the fence's
/// source with the hit highlighted; moving on shows the diagram again.
#[test]
fn read_only_search_into_a_diagram_reveals_and_highlights_the_hit() {
    // Long enough for read-only mode's cursor, the screen's top, to move.
    let body = format!("top\nneedle out\n```mermaid\ngraph TD\nA-->needle\n```\n{}", "bottom\n".repeat(40));
    let body = body.as_str();
    let mut f = fixture(body, &[], |_| {});
    f.app.set_view(true);
    f.app.sync_after_input();
    settle(&mut f.app, |rows| has(rows, "D[A-->needle]"));

    feed(&mut f.app, "/needle");
    key(&mut f.app, KeyCode::Enter);
    assert_eq!(f.app.buffer.cursor.line, 1);
    assert!(has(&screen(&mut f.app), "D[A-->needle]"), "a hit outside leaves the diagram");

    let hit = |f: &mut Fixture| {
        let buf = draw(&mut f.app);
        let rows = rows_of(&buf);
        let y = rows.iter().position(|row| row.contains("A-->needle") && !row.contains("D[")).expect("source shown");
        let x = rows[y].find("needle").unwrap() as u16;
        assert_eq!(buf[(x, y as u16)].bg, f.app.theme.search_match.to_ratatui(), "the hit is highlighted");
        assert!(!has(&rows, "D[A-->needle]"));
    };
    feed(&mut f.app, "n");
    assert_eq!(f.app.buffer.cursor.line, 4);
    hit(&mut f);

    feed(&mut f.app, "N");
    assert_eq!(f.app.buffer.cursor.line, 1);
    assert!(has(&screen(&mut f.app), "D[A-->needle]"));

    // `*` on the word outside lands inside too.
    f.app.buffer.cursor = Cursor::new(1, 0);
    f.app.sync_after_input();
    feed(&mut f.app, "*");
    assert_eq!(f.app.buffer.cursor.line, 4);
    hit(&mut f);

    // Any other motion lets it go.
    feed(&mut f.app, "j");
    settle(&mut f.app, |rows| has(rows, "D[A-->needle]"));
}

/// `-R` with the whole note on screen: the hit is in the last screenful,
/// so the view cannot start at it and the cursor is parked on the top of
/// the screen. The fence still shows its source with the hit highlighted;
/// the clamp used to measure the text with the diagram in place and park
/// the cursor off the landing, which then revealed nothing.
#[test]
fn read_only_search_reveals_a_hit_in_the_last_screenful() {
    let mut f = fixture("top\nintro\n```mermaid\ngraph TD\nA-->needle\n```\nafter\n", &[], |_| {});
    f.app.set_view(true);
    f.app.sync_after_input();
    settle(&mut f.app, |rows| has(rows, "D[A-->needle]"));
    feed(&mut f.app, "/needle");
    key(&mut f.app, KeyCode::Enter);
    assert_eq!(top(&f.app), 0, "the whole note fits");
    let rows = screen(&mut f.app);
    assert!(has(&rows, "A-->needle") && !has(&rows, "D[A-->needle]"), "the source shows:\n{}", rows.join("\n"));
    // The next frame keeps it, and so does `j`, which cannot move the
    // view; Esc lets it go.
    assert!(!has(&screen(&mut f.app), "D[A-->needle]"));
    feed(&mut f.app, "j");
    assert!(!has(&screen(&mut f.app), "D[A-->needle]"));
    key(&mut f.app, KeyCode::Esc);
    settle(&mut f.app, |rows| has(rows, "D[A-->needle]"));
}

/// A landing left is gone: coming back to the same place by other motions
/// shows the diagram, and so does another pane on the same place.
#[test]
fn a_read_only_search_landing_once_left_reveals_nothing() {
    let body = format!("top\n```mermaid\ngraph TD\nA-->B\n```\n{}", "bottom\n".repeat(40));
    let mut f = fixture(&body, &[], |_| {});
    f.app.set_view(true);
    f.app.sync_after_input();
    settle(&mut f.app, |rows| has(rows, "D[A-->B]"));
    feed(&mut f.app, "/^graph");
    key(&mut f.app, KeyCode::Enter);
    assert_eq!(f.app.buffer.cursor.line, 2);
    assert!(!has(&screen(&mut f.app), "D[A-->B]"), "revealed");
    feed(&mut f.app, "gg");
    settle(&mut f.app, |rows| has(rows, "D[A-->B]"));
    feed(&mut f.app, "3G");
    assert_eq!((f.app.buffer.cursor.line, f.app.buffer.cursor.col), (2, 0), "back where the search landed");
    assert!(has(&screen(&mut f.app), "D[A-->B]"), "not revealed again");

    // Landed again, then a new pane on the same document and position.
    feed(&mut f.app, "gg");
    feed(&mut f.app, "/^graph");
    key(&mut f.app, KeyCode::Enter);
    assert!(!has(&screen(&mut f.app), "D[A-->B]"));
    f.app.split_pane_for_test(true);
    f.app.sync_after_input();
    assert_eq!(f.app.buffer.cursor.line, 2);
    assert!(f.app.search_landing.is_none(), "the landing was the other pane's");
    assert!(!f.app.active.contains(2), "nothing revealed in the new pane");
}

/// A failed render shows a status row under the opener and keeps the
/// source; `j` onto the fence and a click on the status row both land on
/// the opener.
#[test]
fn a_failed_render_shows_a_status_row_that_motions_and_clicks_land_past() {
    let mut f = fixture("intro\n```mermaid\ngraph TD\nA-->B\n```\nafter\n", &[("fail", "")], |_| {});
    let rows = settle(&mut f.app, |rows| has(rows, "Mermaid render failed"));
    let status = rows.iter().position(|row| row.contains("Mermaid render failed")).unwrap();
    let body = rows.iter().position(|row| row.contains("graph TD")).expect("the source stays");
    assert_eq!(body, status + 1, "the status row is between the opener and the body");
    {
        let cache = f.app.cache.borrow();
        let (first, last) = cache.line_rows(1);
        assert_eq!(last, first + 1, "the opener's rows hold its status row");
        let row = cache.row(last).unwrap();
        assert!(matches!(row.source, RowSource::Embedded { line: 1, .. }) && !row.editable);
    }

    feed(&mut f.app, "j");
    assert_eq!(f.app.buffer.cursor.line, 1);
    let rows = screen(&mut f.app);
    assert!(!has(&rows, "Mermaid render failed"), "the fence is being edited: no status");
    feed(&mut f.app, "j");
    assert_eq!(f.app.buffer.cursor.line, 2, "j steps into the body");

    // Back on `intro`, a click on the status row lands on the opener.
    goto(&mut f.app, 0);
    let rows = screen(&mut f.app);
    let status = rows.iter().position(|row| row.contains("Mermaid render failed")).unwrap();
    let x = rows[status].find('M').unwrap() as u16;
    click(&mut f.app, x, status as u16);
    assert_eq!(f.app.buffer.cursor.line, 1);
    assert_eq!(f.app.mode, InputMode::Normal);
}

/// A click on a diagram's row lands on its opener, which shows the source.
#[test]
fn a_click_on_a_diagram_lands_on_its_opener() {
    let mut f = fixture("intro\n\n```mermaid\ngraph TD\nA-->B\n```\n\nafter\n", &[], |_| {});
    let rows = settle(&mut f.app, |rows| has(rows, "D[A-->B]"));
    let y = rows.iter().position(|row| row.contains("D[A-->B]")).unwrap();
    let x = rows[y].find('D').unwrap() as u16;
    click(&mut f.app, x + 2, y as u16);
    assert_eq!(f.app.buffer.cursor.line, 2);
    let rows = screen(&mut f.app);
    assert!(has(&rows, "graph TD") && !has(&rows, "D[A-->B]"));
}

/// A closed fold over a fence wins: the fold's line shows, no diagram and
/// no request for one. Opened, the diagram shows.
#[test]
fn a_fold_over_a_fence_wins() {
    let mut f = fixture("# A\n\n```mermaid\ngraph TD\nA-->B\n```\n\n# B\n", &[], |_| {});
    feed(&mut f.app, "zc");
    let rows = screen(&mut f.app);
    assert!(!has(&rows, "graph TD") && !has(&rows, "D[A-->B]"), "{}", rows.join("\n"));
    std::thread::sleep(Duration::from_millis(100));
    assert!(!f.app.poll_mermaid(), "nothing was asked for under the fold");
    assert!(f.app.cache.borrow().mermaid_picture(2).is_none());
    feed(&mut f.app, "zo");
    settle(&mut f.app, |rows| has(rows, "D[A-->B]"));
    goto(&mut f.app, 7);
    feed(&mut f.app, "gg");
    feed(&mut f.app, "zc");
    let rows = screen(&mut f.app);
    assert!(!has(&rows, "D[A-->B]"), "closed again over a rendered diagram:\n{}", rows.join("\n"));
}

/// Read-only mode shows diagrams with the cursor inside the fence; back to
/// editing, the cursor's fence shows its source.
#[test]
fn view_shows_diagrams_with_the_cursor_inside_and_editing_reveals_them() {
    let mut f = fixture("intro\n```mermaid\ngraph TD\nA-->B\n```\nafter\n", &[], |_| {});
    goto(&mut f.app, 3);
    assert!(has(&screen(&mut f.app), "A-->B"));
    f.app.run_command("view");
    f.app.sync_after_input();
    assert!(f.app.view);
    settle(&mut f.app, |rows| has(rows, "D[A-->B]") && !has(rows, "graph TD"));
    f.app.run_command("view");
    f.app.buffer.cursor = Cursor::new(3, 0);
    f.app.sync_after_input();
    assert!(!f.app.view);
    let rows = screen(&mut f.app);
    assert!(has(&rows, "graph TD") && !has(&rows, "D[A-->B]"), "{}", rows.join("\n"));
}

/// With pictures on: the text diagram shows while the picture is pending,
/// a resize meanwhile keeps the text, and the picture replaces it when it
/// is ready, fitted to the size the pane has then.
#[test]
fn a_pending_picture_shows_the_text_and_swaps_in_when_ready() {
    let mut f = fixture("intro\n\n```mermaid\ngraph TD\nA-->B\n```\n\nafter\n", &[("gated", "")], |c| {
        c.markdown.mermaid_graphics = true;
    });
    f.app.image_protocol = crate::image::Protocol::Kitty;
    f.app.term_size = (120, 21);
    settle(&mut f.app, |rows| has(rows, "D[A-->B]"));
    assert!(f.app.images.borrow().is_empty(), "no picture yet");

    f.app.term_size = (50, 21);
    let rows = screen(&mut f.app);
    assert!(has(&rows, "D[A-->B]"), "the resize keeps the text");
    assert!(f.app.images.borrow().is_empty());

    std::fs::write(f.dir.join("gate"), "").unwrap();
    settle(&mut f.app, |rows| !has(rows, "D[A-->B]"));
    let places = f.app.images.borrow().clone();
    assert_eq!(places.len(), 1, "the picture is placed");
    let narrow = places[0].cols;
    assert!(narrow < 60, "fitted to the narrow pane: {narrow}");
    assert!(f.app.image_payloads.borrow().contains_key(&places[0].id));

    f.app.term_size = (120, 21);
    screen(&mut f.app);
    let places = f.app.images.borrow().clone();
    assert_eq!(places.len(), 1);
    assert_eq!(places[0].cols, 60, "the picture's own width, at a wide pane");

    // Into the fence: the source, and no picture.
    goto(&mut f.app, 3);
    let rows = screen(&mut f.app);
    assert!(has(&rows, "graph TD"));
    assert!(f.app.images.borrow().is_empty());
}
