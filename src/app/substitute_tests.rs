//! Expected text, cursor and substitute messages from `nvim --clean --headless`.

use super::*;
use crate::test_support::app_with;

fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.on_key(KeyEvent::new(code, mods));
    app.sync_after_input();
}

fn feed(app: &mut App, keys: &str) {
    for ch in keys.chars() {
        key(app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
}

fn enter(app: &mut App) {
    key(app, KeyCode::Enter, KeyModifiers::NONE);
}

fn ex(app: &mut App, command: &str) {
    feed(app, ":");
    feed(app, command);
    enter(app);
}

fn flash(app: &App) -> &str {
    app.flash.as_ref().and_then(|f| f.text.as_deref()).unwrap_or("")
}

fn at(app: &App) -> (usize, usize) {
    (app.buffer.cursor.line, app.buffer.cursor.col)
}

type Step = (&'static str, &'static str, (usize, usize), Option<&'static str>);
type Case = (&'static str, &'static str, &'static [Step]);
const CASES: &[Case] = &[
    ("global", "x x\nx\nz\n", &[
        (":%s/x/y/g", "y y\ny\nz\n", (1, 0), Some("3 substitutions on 2 lines")),
    ]),
    ("range", "x\nx\nx\n", &[
        (":2,3s/x/y/", "x\ny\ny\n", (2, 0), Some("")),
    ]),
    ("count", "x x\nx\nx\n", &[
        (":1,2s/x/y/g 2", "x x\ny\ny\n", (2, 0), Some("")),
    ]),
    ("count_only", "x x\nx\nz\n", &[
        (":%s/x/y/gn", "x x\nx\nz\n", (0, 0), Some("3 matches on 2 lines")),
    ]),
    ("case_flags", "X x\nX x\n", &[
        (":1s/x/y/ig", "y y\nX x\n", (0, 0), Some("")),
        (":2s/\\cx/y/Ig", "y y\ny y\n", (1, 0), Some("")),
    ]),
    ("no_match", "x\n", &[
        (":s/z/y/", "x\n", (0, 0), Some("pattern not found: z")),
    ]),
    ("quiet", "x\n", &[
        (":s/z/y/e", "x\n", (0, 0), Some("")),
    ]),
    ("separator", "a/b\nx\n", &[
        (":s#a/b#c#", "c\nx\n", (0, 0), Some("")),
    ]),
    ("escapes", "ab\n", &[
        (":s/\\(a\\)b/\\u\\1:&:\\0:\\&:\\~:\\\\:\\r:\\n/", "A:ab:ab:&:~:\\:\n:\0\n", (1, 0), Some("")),
    ]),
    ("unicode", "ä É\n", &[
        (":s/.*/\\U&\\E/", "Ä É\n", (0, 0), Some("")),
    ]),
    ("repeat", "x x\nx x\nx x\n", &[
        (":s/x/y/g", "y y\nx x\nx x\n", (0, 0), Some("")),
        (":2s", "y y\ny x\nx x\n", (1, 0), Some("")),
        (":3&&", "y y\ny x\ny x\n", (2, 0), Some("")),
    ]),
    ("cursor", "  x\n z x\nend\n", &[
        (":%s/x/y/g", "  y\n z y\nend\n", (1, 1), Some("")),
        (":undo", "  x\n z x\nend\n", (0, 0), None),
        (":redo", "  y\n z y\nend\n", (0, 0), None),
    ]),
    ("empty", "\nx\n\n", &[
        (":%s/^$/empty/g", "empty\nx\nempty\n", (2, 0), Some("")),
    ]),
    ("multi", "a\nb\na\nb\n", &[
        (":%s/a\\nb/z/g", "z\nz\n", (1, 0), Some("")),
    ]),
    ("dot", "old\nold\n", &[
        ("A!", "old!\nold\n", (0, 3), Some("")),
        (":s/old/new/", "new!\nold\n", (0, 0), Some("")),
        ("j.", "new!\nold!\n", (1, 3), Some("")),
    ]),
    ("normal_amp", "x x\nx x\n", &[
        (":s/x/y/g", "y y\nx x\n", (0, 0), Some("")),
        ("j&", "y y\ny x\n", (1, 0), Some("")),
    ]),
    ("normal_gamp", "x x\nx x\n", &[
        (":s/x/y/g", "y y\nx x\n", (0, 0), Some("")),
        ("g&", "y y\ny y\n", (1, 0), Some("")),
    ]),
];

#[test]
fn substitutions_match_neovim() {
    for (name, text, steps) in CASES {
        let mut app = app_with(text);
        for (command, expected, cursor, message) in *steps {
            if let Some(command) = command.strip_prefix(':') {
                ex(&mut app, command);
            } else {
                feed(&mut app, command);
                if *command == "A!" {
                    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                }
            }
            assert_eq!(app.buffer.rope.to_string(), *expected, "{name}: {command}");
            assert_eq!(at(&app), *cursor, "{name}: {command}");
            if let Some(message) = message {
                assert_eq!(flash(&app), *message, "{name}: {command}");
            }
        }
    }
}

#[test]
fn confirm_updates_text_and_keeps_one_undo_step() {
    let original = "old old\n  old\n  old\n";
    for (keys, expected, cursor) in [
        ("ynq", "new old\n  old\n  old\n", (1, 2)),
        ("ynna", "new old\n  old\n  new\n", (2, 2)),
        ("na", "old new\n  new\n  new\n", (2, 2)),
        ("l", "new old\n  old\n  old\n", (0, 0)),
    ] {
        let mut app = app_with(original);
        ex(&mut app, "%s/old/new/gc");
        assert_eq!(app.substitute_prompt().as_deref(), Some("replace with new (y/n/a/q/l/^E/^Y)?"));
        assert_eq!(app.substitute_highlight(0), Some(0..3));
        for (i, ch) in keys.chars().enumerate() {
            key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
            if i == 0 && ch == 'y' {
                assert_eq!(app.buffer.line_text(0), "new old");
                assert_eq!(app.substitute_highlight(0), Some(4..7));
            }
        }
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.buffer.rope.to_string(), expected);
        assert_eq!(at(&app), cursor);
        feed(&mut app, "u");
        assert_eq!(app.buffer.rope.to_string(), original);
        key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(app.buffer.rope.to_string(), expected);
    }
    let mut app = app_with(original);
    ex(&mut app, "%s/old/new/gc");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.buffer.rope.to_string(), original);
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn replacements_share_the_search_pattern_and_skip_dot_recording() {
    let mut app = app_with("old\nother old\nold\n");
    feed(&mut app, "/old");
    enter(&mut app);
    assert_eq!(at(&app), (1, 6));
    ex(&mut app, "s//new/");
    assert_eq!(app.buffer.line_text(1), "other new");
    feed(&mut app, "n");
    assert_eq!(at(&app), (2, 0));
    ex(&mut app, "s/not here/new/e");
    assert_eq!(flash(&app), "");

    let mut app = app_with("old\nold\n");
    feed(&mut app, "A!");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    ex(&mut app, "s/old/new/");
    feed(&mut app, "j.");
    assert_eq!(app.buffer.rope.to_string(), "new!\nold!\n");
}

#[test]
fn substitutes_keep_the_file_ending_and_cover_a_closed_fold() {
    let mut app = app_with("# old\n\nold\n\n# outside\nold");
    feed(&mut app, "zc");
    ex(&mut app, "s/old/new/");
    assert_eq!(app.buffer.rope.to_string(), "# new\n\nnew\n\n# outside\nold");

    let mut app = app_with("");
    app.buffer = Buffer::unnamed(b"old\r\nold".to_vec()).unwrap();
    ex(&mut app, "%su/old/new/");
    assert_eq!(app.buffer.rope.to_string(), "new\nnew");
    assert_eq!(app.buffer.line_ending, crate::text::buffer::LineEnding::Crlf);
    assert!(!app.buffer.final_newline);
    ex(&mut app, "$s/new//");
    assert_eq!(app.buffer.last_line(), 1);
    assert_eq!(app.buffer.line_text(1), "");
    assert_eq!(at(&app), (1, 0));
}

#[test]
fn invalid_patterns_do_not_move_or_replace_and_view_allows_counting() {
    let mut app = app_with("old old\nold\n");
    feed(&mut app, "/old");
    enter(&mut app);
    let before = at(&app);
    feed(&mut app, r"/\(a\)\1");
    enter(&mut app);
    assert_eq!(at(&app), before);
    assert_eq!(app.search.as_ref().unwrap().pattern, "old");
    assert_eq!(flash(&app), r"pattern: \1 not supported");
    ex(&mut app, r"%s/old/\=0/");
    assert_eq!(flash(&app), r"replacement: \= not supported");
    app.set_view(true);
    ex(&mut app, "%s/old/new/g");
    assert!(flash(&app).contains("read-only"));
    ex(&mut app, "%s/old/new/gn");
    assert_eq!(flash(&app), "3 matches on 2 lines");
    assert_eq!(app.buffer.rope.to_string(), "old old\nold\n");
}

#[test]
fn substitutes_a_large_document_in_one_step() {
    let input = "x x\n".repeat(100_000);
    let mut app = app_with(&input);
    let start = Instant::now();
    app.run_command("%s/x/y/g");
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(8), "substitute took {elapsed:?}");
    assert_eq!(app.buffer.rope.to_string(), "y y\n".repeat(100_000));
    assert!(app.buffer.undo());
    assert_eq!(app.buffer.rope.to_string(), input);
}

#[test]
fn search_offsets_and_the_punctuation_word_follow_neovim() {
    let mut app = app_with("abc\n界 abc abc\nabcd\n");
    feed(&mut app, "/abc/e");
    enter(&mut app);
    assert_eq!(at(&app), (0, 2));
    feed(&mut app, "n");
    assert_eq!(at(&app), (1, 4));
    feed(&mut app, "N");
    assert_eq!(at(&app), (0, 2));

    let mut app = app_with("head\nfoo\nfoo\nfoo\nend\n");
    feed(&mut app, "/foo/+1");
    enter(&mut app);
    assert_eq!(at(&app), (2, 0));
    feed(&mut app, "n");
    assert_eq!(at(&app), (4, 0));
    feed(&mut app, "n");
    assert_eq!(at(&app), (2, 0));

    let mut app = app_with("++\n..\n++\n");
    feed(&mut app, "*");
    assert_eq!(at(&app), (2, 0));
    feed(&mut app, "#");
    assert_eq!(at(&app), (0, 0));
}

#[test]
fn substitute_honors_reported_match_end_and_escaped_separators() {
    let mut app = app_with("abcd\na.b axb\na+a aa\nabc\n");
    ex(&mut app, r"1s/.\ze./X/g");
    ex(&mut app, r"2s.a\.b.zz.g");
    ex(&mut app, r"3s+a\+a+X+g");
    ex(&mut app, r"4s/x\?/X/g");
    assert_eq!(app.buffer.rope.to_string(), "XXXd\nzz axb\na+a X\nXaXbXc\n");
}

#[test]
fn undo_and_redo_go_to_the_first_changed_line() {
    let mut app = app_with(" abcd\n unchanged\n  old at\n other\nold tail\nlast\n");
    feed(&mut app, "jjjj$");
    ex(&mut app, "3,5s/old/y/g");
    assert_eq!(at(&app), (4, 0));
    feed(&mut app, "u");
    assert_eq!(at(&app), (2, 0));
    key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(at(&app), (2, 0));
}

#[test]
fn substitute_changes_the_pattern_and_preserves_search_direction_and_offset() {
    let mut app = app_with("old\nold\nold\nold\n");
    feed(&mut app, "3G?old");
    enter(&mut app);
    ex(&mut app, "s/old/new/");
    feed(&mut app, "n");
    assert_eq!(at(&app), (0, 0));

    let mut app = app_with("abc\nxx bar xx\nbar t\nbar t\n");
    feed(&mut app, "/abc/e");
    enter(&mut app);
    ex(&mut app, "2s/bar/x/");
    feed(&mut app, "n");
    assert_eq!(at(&app), (2, 2));
}

#[test]
fn replacement_case_stops_with_lowercase_e() {
    let mut app = app_with("xx\n");
    ex(&mut app, r"s/x/\l&\LTEST\eEND/g");
    assert_eq!(app.buffer.rope.to_string(), "xtestENDxtestEND\n");
}

#[test]
fn multiline_highlights_use_each_panes_document_and_revision() {
    let mut app = app_with("short\n");
    assert!(app.set_search(r"old\nnew".into(), false));
    let mut doc = BufferState::new(
        Buffer::unnamed(b"a\nb\nold\nnew start\n".to_vec()).unwrap(),
        app.config.markdown.math,
        app.config.editor.tab_width,
    );
    let columns = |doc: &BufferState, line| app.search_highlights(doc, line, &doc.buffer.line_text(line));
    assert_eq!(columns(&app, 0).first(), None);
    assert_eq!(columns(&doc, 2).first(), Some(&(0..3)));
    assert_eq!(columns(&doc, 3).first(), Some(&(0..3)));
    let start = doc.buffer.rope.line_to_char(2);
    doc.buffer.delete_chars(start, start + 3);
    assert_eq!(columns(&doc, 3).first(), None);
}
