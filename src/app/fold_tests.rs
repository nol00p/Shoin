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
    app.folds().closed().iter().map(|f| (f.start, f.end)).collect()
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

fn ctrl(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    app.sync_after_input();
}

fn click(app: &mut App, column: u16, row: u16) {
    app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE });
    app.sync_after_input();
}

fn wheel_down(app: &mut App, column: u16, row: u16) {
    app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column, row, modifiers: KeyModifiers::NONE });
    app.sync_after_input();
}

/// What pane `id` has folded in the current document.
fn folds_of(app: &App, id: PaneId) -> Vec<(usize, usize)> {
    app.docs[app.current()].folds_in(id).closed().iter().map(|f| (f.start, f.end)).collect()
}

/// The screen at the app's own size, one string per row, trailing spaces
/// trimmed: for a split, which needs more room than `screen` has.
fn wide_screen(app: &mut App) -> Vec<String> {
    app.refresh_blocks();
    let (w, h) = app.term_size;
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::render::frame::render(f, app)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

/// Where `needle` is drawn in the left or right half of the screen.
fn find_in(rows: &[String], needle: &str, right: bool, half: usize) -> Option<(u16, u16)> {
    rows.iter().enumerate().find_map(|(y, r)| {
        let chars: Vec<char> = r.chars().collect();
        let (from, to) = if right { (half, chars.len()) } else { (0, half.min(chars.len())) };
        let s: String = chars.get(from..to).unwrap_or(&[]).iter().collect();
        s.find(needle).map(|i| ((from + s[..i].chars().count()) as u16, y as u16))
    })
}

#[test]
fn two_panes_on_one_file_fold_apart() {
    let mut app = app_with(DOC);
    app.term_size = (120, 24);
    let left = app.focus_pane;
    goto(&mut app, 2);
    feed(&mut app, "zc");
    // A split starts with the folds of the pane it came from, as vim's.
    app.run_command("vs");
    app.sync_after_input();
    let right = app.focus_pane;
    assert_ne!(left, right);
    assert_eq!(folds_of(&app, right), [(2, 5)]);
    // ...and from there each pane folds on its own.
    feed(&mut app, "zo");
    goto(&mut app, 8);
    feed(&mut app, "zc");
    assert_eq!(folds_of(&app, right), [(8, 10)]);
    assert_eq!(folds_of(&app, left), [(2, 5)]);

    let rows = wide_screen(&mut app);
    let half = 60;
    // The left pane is the one on the left: `:vs` puts the new pane right.
    assert!(find_in(&rows, "one … 3 lines", false, half).is_some(), "{rows:#?}");
    assert!(find_in(&rows, "one.a", false, half).is_none(), "{rows:#?}");
    assert!(find_in(&rows, "body", false, half).is_some(), "{rows:#?}");
    assert!(find_in(&rows, "one.a", true, half).is_some(), "{rows:#?}");
    assert!(find_in(&rows, "Second … 2 lines", true, half).is_some(), "{rows:#?}");
    assert!(find_in(&rows, "body", true, half).is_none(), "{rows:#?}");

    // A click in each pane goes through that pane's folds.
    let (x, y) = find_in(&rows, "Third", true, half).unwrap();
    click(&mut app, x + 2, y);
    assert_eq!((app.focus_pane, line(&app)), (right, 12));
    let (x, y) = find_in(&rows, "Third", false, half).unwrap();
    click(&mut app, x + 2, y);
    assert_eq!((app.focus_pane, line(&app)), (left, 12));
    let (x, y) = find_in(&rows, "two", false, half).unwrap();
    click(&mut app, x, y);
    assert_eq!((app.focus_pane, line(&app)), (left, 6));
    let (x, y) = find_in(&rows, "two", true, half).unwrap();
    click(&mut app, x, y);
    assert_eq!((app.focus_pane, line(&app)), (right, 6));
    assert_eq!(folds_of(&app, left), [(2, 5)], "clicks never open a fold");
    assert_eq!(folds_of(&app, right), [(8, 10)]);

    // An edit in one pane moves the folds of the other.
    goto(&mut app, 0);
    feed(&mut app, "yyP");
    assert_eq!(folds_of(&app, right), [(9, 11)]);
    assert_eq!(folds_of(&app, left), [(3, 6)]);

    // Closing a pane forgets its folds.
    app.run_command("close");
    app.sync_after_input();
    assert_eq!(app.focus_pane, left);
    assert!(!app.docs[app.current()].folds.contains_key(&right));
    assert_eq!(folds(&app), [(3, 6)]);
}

#[test]
fn the_wheel_scrolls_each_pane_through_its_own_folds() {
    let body: String = (0..40).map(|i| format!("  - child {i}\n")).collect();
    let tail: String = (0..30).map(|i| format!("- after {i}\n")).collect();
    let doc = format!("- parent\n{body}{tail}");
    let mut app = app_with(&doc);
    app.term_size = (120, 24);
    let left = app.focus_pane;
    app.run_command("vs");
    app.sync_after_input();
    let right = app.focus_pane;
    feed(&mut app, "zc");
    assert_eq!(folds_of(&app, right), [(0, 40)]);
    assert!(folds_of(&app, left).is_empty());

    // Scroll the right pane, then the left one, three rows each.
    let rows = wide_screen(&mut app);
    let (x, y) = find_in(&rows, "after 0", true, 60).unwrap();
    click(&mut app, x, y);
    wheel_down(&mut app, x, y);
    let (x, y) = find_in(&rows, "child 0", false, 60).unwrap();
    click(&mut app, x, y);
    assert_eq!(app.focus_pane, left);
    wheel_down(&mut app, x, y);

    let rows = wide_screen(&mut app);
    let first = |right: bool| {
        rows.iter()
            .map(|r| {
                let c: Vec<char> = r.chars().collect();
                let part: String = if right { c.get(60..).unwrap_or(&[]).iter().collect() } else { c.iter().take(60).collect() };
                part.trim().to_string()
            })
            .find(|p| !p.is_empty())
            .unwrap()
    };
    // Row 3 of the right pane is `after 2`: the fold took one row.
    assert!(first(true).contains("after 2"), "{rows:#?}");
    assert!(first(false).contains("child 2"), "{rows:#?}");
    assert_eq!(folds_of(&app, right), [(0, 40)]);
}

#[test]
fn j_on_a_fold_joins_its_first_two_lines_and_keeps_it_closed() {
    let mut app = app_with(DOC);
    goto(&mut app, 2);
    feed(&mut app, "zcJ");
    assert_eq!(app.buffer.line_text(2), "- one - one.a");
    assert_eq!(app.buffer.line_text(3), "    - deep");
    assert_eq!(folds(&app), [(2, 4)], "still closed, one line shorter, as vim's");
    assert_eq!(line(&app), 2);
    feed(&mut app, "u");
    assert_eq!(text(&app), DOC);
}

#[test]
fn visual_j_joins_a_closed_fold_whole() {
    let mut app = app_with(DOC);
    goto(&mut app, 2);
    feed(&mut app, "zcVJ");
    assert_eq!(app.buffer.line_text(2), "- one - one.a - deep - one.b");
    assert_eq!(app.buffer.line_text(3), "- two");
    assert!(folds(&app).is_empty());
}

#[test]
fn o_and_capital_o_open_lines_around_a_closed_fold() {
    let mut app = app_with(DOC);
    goto(&mut app, 2);
    feed(&mut app, "zco");
    assert_eq!(app.mode, Mode::Insert);
    assert_eq!(line(&app), 6, "below the whole fold");
    assert_eq!(folds(&app), [(2, 5)], "which stays closed");
    feed(&mut app, "- new");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.buffer.line_text(6), "- new");
    assert_eq!(app.buffer.line_text(7), "- two");
    assert_eq!(folds(&app), [(2, 5)]);

    goto(&mut app, 2);
    feed(&mut app, "O");
    assert_eq!(line(&app), 2, "above the fold");
    assert_eq!(folds(&app), [(3, 6)], "which moves down and stays closed");
    feed(&mut app, "- top");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.buffer.line_text(2), "- top");
    assert_eq!(folds(&app), [(3, 6)]);
    // A count types the line that many times, below the fold.
    goto(&mut app, 3);
    feed(&mut app, "2o- x");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.buffer.line_text(7), "- x");
    assert_eq!(app.buffer.line_text(8), "- x");
    assert_eq!(folds(&app), [(3, 6)]);
}

#[test]
fn half_pages_count_a_fold_as_one_line() {
    let body: String = (0..40).map(|i| format!("  - child {i}\n")).collect();
    let tail: String = (0..30).map(|i| format!("- after {i}\n")).collect();
    let doc = format!("intro\n- parent\n{body}{tail}");
    let mut app = app_with(&doc);
    goto(&mut app, 1);
    feed(&mut app, "zc");
    goto(&mut app, 0);
    let half = app.viewport_height() / 2;
    ctrl(&mut app, 'd');
    // intro → parent (the fold) → after 0 … : `half` shown lines.
    assert_eq!(line(&app), 41 + half - 1);
    assert_eq!(folds(&app), [(1, 41)], "the fold stays closed");
    ctrl(&mut app, 'u');
    assert_eq!(line(&app), 0);
    assert_eq!(folds(&app), [(1, 41)]);
    // From just above the fold, Ctrl-d crosses it rather than landing in it.
    goto(&mut app, 1);
    ctrl(&mut app, 'd');
    assert_eq!(line(&app), 41 + half);
    assert_eq!(folds(&app), [(1, 41)]);
}

#[test]
fn paragraph_motions_stop_on_a_fold_and_skip_it() {
    let doc = "# Title\nintro\n\n## Second\nbody\n\nmore\n\n## Third\nend\n";
    let mut app = app_with(doc);
    goto(&mut app, 3);
    feed(&mut app, "zc");
    assert_eq!(folds(&app), [(3, 6)]);
    goto(&mut app, 0);
    feed(&mut app, "}");
    assert_eq!(line(&app), 2);
    // The next blank line is inside the fold: the cursor stays on the
    // folded line, which stays closed.
    feed(&mut app, "}");
    assert_eq!((line(&app), app.buffer.cursor.col), (3, 0));
    assert_eq!(folds(&app), [(3, 6)]);
    // From the folded line, `}` starts after the fold.
    feed(&mut app, "}");
    assert_eq!(line(&app), 7);
    feed(&mut app, "{");
    assert_eq!(line(&app), 3, "back onto the fold");
    feed(&mut app, "{");
    assert_eq!(line(&app), 2);
    assert_eq!(folds(&app), [(3, 6)]);
    // A count takes each step the same way.
    goto(&mut app, 0);
    feed(&mut app, "3}");
    assert_eq!(line(&app), 7);
    feed(&mut app, "2{");
    assert_eq!(line(&app), 2);
    // A search still opens it.
    feed(&mut app, "/more");
    key(&mut app, KeyCode::Enter);
    assert_eq!(line(&app), 6);
    assert!(folds(&app).is_empty());
}

const LISTS: &str = "- one\n  - one.a\n- two\n  - two.a\n\n## S\nbody\n";

#[test]
fn visual_zc_folds_every_fold_in_the_selection() {
    let mut app = app_with(LISTS);
    feed(&mut app, "VGzc");
    assert_eq!(folds(&app), [(0, 1), (2, 3), (5, 6)]);
    assert_eq!(app.mode, Mode::Normal, "it ends Visual, as vim's");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 0));
    // Only the selected lines.
    feed(&mut app, "zR");
    goto(&mut app, 1);
    feed(&mut app, "vjzc");
    assert_eq!(folds(&app), [(0, 1), (2, 3)]);
    assert_eq!(line(&app), 0, "the start of the selection was folded away");
    // Nothing to fold.
    feed(&mut app, "zR");
    goto(&mut app, 4);
    feed(&mut app, "vzc");
    assert!(folds(&app).is_empty());
}

#[test]
fn visual_zo_opens_every_fold_in_the_selection() {
    let mut app = app_with(LISTS);
    feed(&mut app, "zM");
    assert_eq!(folds(&app), [(0, 1), (2, 3), (5, 6)]);
    goto(&mut app, 0);
    // j steps over the folds: 0, 2, 4, 5.
    feed(&mut app, "Vjjjzo");
    assert!(folds(&app).is_empty());
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(line(&app), 0);
    feed(&mut app, "zMggVjzo");
    assert_eq!(folds(&app), [(5, 6)], "the section was not selected");
}

#[test]
fn visual_za_toggles_at_the_cursor_and_keeps_the_selection() {
    let mut app = app_with(LISTS);
    feed(&mut app, "Vjjza");
    assert_eq!(folds(&app), [(2, 3)], "only the fold at the cursor, as vim's");
    assert_eq!(app.mode, Mode::VisualLine);
    feed(&mut app, "za");
    assert!(folds(&app).is_empty());
    assert_eq!(app.mode, Mode::VisualLine);
}

#[test]
fn zt_zz_zb_put_the_cursor_line_at_the_top_middle_bottom() {
    let doc: String = (0..80).map(|i| format!("line {i}\n")).collect();
    let mut app = app_with(&doc);
    app.config.editor.scroll_off = 0;
    goto(&mut app, 40);
    // The `line N` on each row, without the margin or the scrollbar.
    let shown = |app: &mut App| -> Vec<String> {
        screen(app)
            .into_iter()
            .filter_map(|r| {
                let at = r.find("line ")?;
                Some(r[at..].split_whitespace().take(2).collect::<Vec<_>>().join(" "))
            })
            .collect()
    };
    feed(&mut app, "zt");
    let rows = shown(&mut app);
    assert_eq!(rows[0], "line 40", "{rows:#?}");
    assert_eq!(line(&app), 40, "the cursor stays");
    feed(&mut app, "zb");
    let rows = shown(&mut app);
    assert_eq!(rows.last().unwrap(), "line 40", "{rows:#?}");
    feed(&mut app, "zz");
    let rows = shown(&mut app);
    let at = rows.iter().position(|r| r == "line 40").unwrap();
    assert!(at.abs_diff(rows.len() - 1 - at) <= 1, "{rows:#?}");
    // The view stays where `zt` put it while the cursor moves inside it.
    feed(&mut app, "ztjj");
    assert_eq!(shown(&mut app)[0], "line 40");

    // With scroll_off, the line keeps that much room from the edge.
    app.config.editor.scroll_off = 2;
    goto(&mut app, 40);
    feed(&mut app, "zt");
    assert_eq!(shown(&mut app)[0], "line 38");
    feed(&mut app, "zb");
    let rows = shown(&mut app);
    assert_eq!(rows[rows.len() - 3], "line 40", "{rows:#?}");

    // Near the top there is nothing above the first line to show.
    goto(&mut app, 1);
    feed(&mut app, "zb");
    assert_eq!(shown(&mut app)[0], "line 0");
}

#[test]
fn zz_counts_a_fold_as_one_row() {
    let head: String = (0..10).map(|i| format!("line {i}\n")).collect();
    let body: String = (0..30).map(|i| format!("  - child {i}\n")).collect();
    let tail: String = (0..30).map(|i| format!("- after {i}\n")).collect();
    let doc = format!("{head}- parent\n{body}{tail}");
    let mut app = app_with(&doc);
    app.config.editor.scroll_off = 0;
    goto(&mut app, 10);
    feed(&mut app, "zc");
    goto(&mut app, 43);
    feed(&mut app, "zz");
    let rows: Vec<String> = screen(&mut app).into_iter().filter(|r| r.contains("line") || r.contains("after") || r.contains("parent")).collect();
    let at = rows.iter().position(|r| r.contains("after 2")).unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(at.abs_diff(rows.len() - 1 - at) <= 1, "after 2 in the middle: {rows:#?}");
    assert!(at >= 3 && rows[at - 3].contains("parent … 30 lines"), "the fold is one row: {rows:#?}");
    assert!(rows[at - 4].contains("line 9"), "{rows:#?}");
    assert_eq!(folds(&app), [(10, 40)]);
}
