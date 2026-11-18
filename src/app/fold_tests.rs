//! Folding: `za` `zo` `zc` `zR` `zM`, and what the rest of the editor does
//! around a closed fold.

use super::*;
use crate::render::markdown::block::BlockCache;
use ratatui::{backend::TestBackend, Terminal};

const W: u16 = 60;
const H: u16 = 16;

fn app_with(text: &str) -> App {
    let mut app = App::new(Config::default(), None, None).unwrap();
    app.buffer.insert_str(Cursor::new(0, 0), text);
    app.buffer.cursor = Cursor::new(0, 0);
    app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width);
    app.term_size = (W, H);
    app
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

fn text(app: &App) -> String {
    app.buffer.rope.to_string()
}

fn line(app: &App) -> usize {
    app.buffer.cursor.line
}

fn goto(app: &mut App, l: usize) {
    app.buffer.cursor = Cursor::new(l, 0);
    app.sync_after_input();
}

/// The closed folds, as `(first, last)` line pairs.
fn folds(app: &App) -> Vec<(usize, usize)> {
    app.folds.closed().iter().map(|f| (f.start, f.end)).collect()
}

/// The screen, one string per row, trailing spaces trimmed.
fn screen(app: &mut App) -> Vec<String> {
    app.refresh_blocks();
    let mut t = Terminal::new(TestBackend::new(W, H)).unwrap();
    t.draw(|f| crate::render::frame::render(f, app)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..H)
        .map(|y| (0..W).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

/// The screen row showing `needle`.
fn row_of(app: &mut App, needle: &str) -> u16 {
    screen(app).iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("{needle:?} not on screen")) as u16
}

const DOC: &str = "\
# Title
intro
- one
  - one.a
    - deep
  - one.b
- two

## Second
body
more

## Third
end
";

#[test]
fn zc_folds_an_item_and_zo_opens_it() {
    let mut app = app_with(DOC);
    goto(&mut app, 2);
    feed(&mut app, "zc");
    assert_eq!(folds(&app), [(2, 5)]);
    let rows = screen(&mut app);
    assert!(rows.iter().any(|r| r.contains("one … 3 lines")), "{rows:#?}");
    assert!(!rows.iter().any(|r| r.contains("one.a")), "{rows:#?}");
    assert!(rows.iter().any(|r| r.contains("two")));
    feed(&mut app, "zo");
    assert!(folds(&app).is_empty());
    assert!(screen(&mut app).iter().any(|r| r.contains("one.a")));
    assert_eq!(text(&app), DOC, "folding never touches the text");
}

#[test]
fn za_toggles_a_heading_section() {
    let mut app = app_with(DOC);
    goto(&mut app, 9);
    feed(&mut app, "za");
    assert_eq!(folds(&app), [(8, 10)], "the blank before ## Third stays out");
    assert_eq!(line(&app), 8, "the cursor goes to the fold's first line");
    let rows = screen(&mut app);
    assert!(rows.iter().any(|r| r.contains("Second … 2 lines")), "{rows:#?}");
    assert!(!rows.iter().any(|r| r.contains("body")));
    feed(&mut app, "za");
    assert!(folds(&app).is_empty());
}

#[test]
fn nested_folds_close_outward_and_stay_closed_inside() {
    let mut app = app_with(DOC);
    goto(&mut app, 3);
    feed(&mut app, "zc");
    assert_eq!(folds(&app), [(3, 4)], "one.a, the innermost");
    feed(&mut app, "zc");
    assert_eq!(folds(&app), [(2, 5), (3, 4)], "then the item around it");
    assert_eq!(line(&app), 2);
    feed(&mut app, "zc");
    assert_eq!(folds(&app), [(0, 13), (2, 5), (3, 4)], "then the # Title section");
    assert_eq!(line(&app), 0);
    feed(&mut app, "zo");
    assert_eq!(folds(&app), [(2, 5), (3, 4)], "opening the outer one keeps the inner ones");
    feed(&mut app, "jzo");
    assert_eq!(line(&app), 1);
    goto(&mut app, 2);
    feed(&mut app, "zo");
    let rows = screen(&mut app);
    assert!(rows.iter().any(|r| r.contains("one.a … 1 line")), "{rows:#?}");
}

#[test]
fn zm_folds_everything_and_zr_opens_it() {
    let mut app = app_with(DOC);
    goto(&mut app, 4);
    feed(&mut app, "zM");
    assert_eq!(folds(&app), [(0, 13), (2, 5), (3, 4), (8, 10), (12, 13)]);
    assert_eq!(line(&app), 0, "the cursor goes to the line shown for it");
    let rows = screen(&mut app);
    assert!(rows.iter().any(|r| r.contains("Title … 13 lines")), "{rows:#?}");
    feed(&mut app, "zR");
    assert!(folds(&app).is_empty());
}

#[test]
fn j_and_k_step_over_a_fold() {
    let mut app = app_with(DOC);
    goto(&mut app, 2);
    feed(&mut app, "zc");
    feed(&mut app, "j");
    assert_eq!(line(&app), 6, "past the fold");
    feed(&mut app, "k");
    assert_eq!(line(&app), 2, "back onto its first line");
    feed(&mut app, "k2j");
    assert_eq!(line(&app), 6, "a count counts the fold as one line");
    assert_eq!(folds(&app), [(2, 5)], "and never opens it");
    feed(&mut app, "kdj");
    assert_eq!(text(&app), "# Title\nintro\n\n## Second\nbody\nmore\n\n## Third\nend\n", "dj takes the fold and the line");
}

#[test]
fn search_and_jumps_into_a_fold_open_it() {
    let mut app = app_with(DOC);
    goto(&mut app, 2);
    feed(&mut app, "zc");
    goto(&mut app, 0);
    feed(&mut app, "/deep");
    key(&mut app, KeyCode::Enter);
    assert_eq!(line(&app), 4);
    assert!(folds(&app).is_empty(), "the search opened it");

    feed(&mut app, "zMgg");
    feed(&mut app, "zo");
    feed(&mut app, "G");
    assert_eq!(line(&app), 13);
    assert!(!folds(&app).iter().any(|&(s, e)| s < 13 && 13 <= e), "{:?}", folds(&app));
}

#[test]
fn dd_on_a_folded_line_deletes_the_fold() {
    let mut app = app_with(DOC);
    goto(&mut app, 2);
    feed(&mut app, "zcdd");
    assert_eq!(text(&app), "# Title\nintro\n- two\n\n## Second\nbody\nmore\n\n## Third\nend\n");
    assert!(folds(&app).is_empty());
    feed(&mut app, "u");
    assert_eq!(text(&app), DOC);
    feed(&mut app, "zc");
    assert_eq!((line(&app), folds(&app)), (2, vec![(2, 5)]));
    feed(&mut app, "yyGp");
    assert!(text(&app).ends_with("end\n- one\n  - one.a\n    - deep\n  - one.b\n"), "{:?} {:?}", text(&app), line(&app));
}

#[test]
fn an_edit_on_the_folded_line_keeps_the_fold_and_insert_opens_it() {
    let mut app = app_with(DOC);
    goto(&mut app, 2);
    feed(&mut app, "zc$x");
    assert_eq!(app.buffer.line_text(2), "- on");
    app.sync_after_input();
    assert_eq!(folds(&app), [(2, 5)]);
    feed(&mut app, "A");
    assert!(folds(&app).is_empty(), "Insert on the folded line opens it");
}

#[test]
fn an_edit_above_moves_the_fold() {
    let mut app = app_with(DOC);
    goto(&mut app, 8);
    feed(&mut app, "zc");
    goto(&mut app, 1);
    feed(&mut app, "yyp");
    assert_eq!(folds(&app), [(9, 11)]);
    feed(&mut app, "dd");
    feed(&mut app, "dd");
    assert_eq!(folds(&app), [(7, 9)]);
    assert!(app.buffer.line_text(7).starts_with("## Second"));
}

#[test]
fn a_heading_level_change_moves_or_dissolves_the_section() {
    let mut app = app_with(DOC);
    goto(&mut app, 8);
    feed(&mut app, "zcg1");
    assert_eq!(app.buffer.line_text(8), "# Second");
    app.sync_after_input();
    assert_eq!(folds(&app), [(8, 13)], "a level-1 heading reaches past ## Third");
    feed(&mut app, "g0");
    app.sync_after_input();
    assert!(folds(&app).is_empty(), "no heading, no section");
}

#[test]
fn a_click_below_a_fold_lands_on_the_right_line() {
    let mut app = app_with(DOC);
    goto(&mut app, 2);
    feed(&mut app, "zc");
    let row = row_of(&mut app, "Second");
    let col = 20;
    app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE });
    app.sync_after_input();
    assert_eq!(line(&app), 8);
    assert_eq!(folds(&app), [(2, 5)]);
}

#[test]
fn scrolling_counts_a_fold_as_one_row() {
    let body: String = (0..40).map(|i| format!("  - child {i}\n")).collect();
    let doc = format!("- parent\n{body}- after\n");
    let mut app = app_with(&doc);
    feed(&mut app, "zc");
    let rows = screen(&mut app);
    assert!(rows.iter().any(|r| r.contains("parent … 40 lines")), "{rows:#?}");
    let after = rows.iter().position(|r| r.contains("after")).expect("after is on the first screen");
    let parent = rows.iter().position(|r| r.contains("parent")).unwrap();
    assert_eq!(after, parent + 1);
    // Ctrl-e scrolls a row; the cursor stays on a shown line.
    app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    app.sync_after_input();
    assert!(line(&app) == 0 || line(&app) == 41, "{}", line(&app));
    assert_eq!(folds(&app), [(0, 40)]);
}

#[test]
fn line_spacing_and_relative_numbers_treat_a_fold_as_one_line() {
    let mut app = app_with("- a\n  - b\n  - c\n- d\n- e\n");
    app.config.layout.line_spacing = 1;
    feed(&mut app, "zc");
    let rows: Vec<String> = screen(&mut app).into_iter().filter(|r| !r.trim().is_empty()).collect();
    let rows: Vec<String> = rows.iter().map(|r| r.trim().to_string()).collect();
    let a = rows.iter().position(|r| r.contains("a … 2 lines")).unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(rows[a + 1].contains('d'), "{rows:#?}");
    let all = screen(&mut app);
    let ra = all.iter().position(|r| r.contains("a … 2")).unwrap();
    assert!(all[ra + 1].trim().is_empty(), "one spacer row after the fold: {all:#?}");
    assert!(all[ra + 2].contains('d'), "{all:#?}");

    app.config.layout.line_spacing = 0;
    app.run_command("number relative");
    app.sync_after_input();
    let all = screen(&mut app);
    let e = all.iter().find(|r| r.ends_with('e')).unwrap();
    assert!(e.trim_start().starts_with('2'), "e is two shown lines below: {all:#?}");
}
