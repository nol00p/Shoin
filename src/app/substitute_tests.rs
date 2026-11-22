//! Expected text, cursor and substitute messages from `nvim --clean --headless`.

use super::*;
use crate::test_support::app_with;

fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.on_key(KeyEvent::new(code, mods));
    app.sync_after_input();
}

fn feed(app: &mut App, keys: &str) {
    for ch in keys.chars() {
        if ch == '\r' { enter(app) } else { key(app, KeyCode::Char(ch), KeyModifiers::NONE) }
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
    ("slash_collection", "a/b /\n", &[
        (":s/[/]/X/g", "aXb X\n", (0, 0), Some("")),
    ]),
    ("bracket_first", "a/b ]/\n", &[
        (":s/[]/]/X/g", "aXb XX\n", (0, 0), Some("")),
    ]),
    ("very_collection", "a/b /\n", &[
        (":s/\\v[/]/X/g", "aXb X\n", (0, 0), Some("")),
    ]),
    ("nomagic_collection", "a/b [/]\n", &[
        (":s/\\M[/]/X/g", "a/b X\n", (0, 0), Some("")),
    ]),
    ("literal_collection", "a[b /\n", &[
        (":s/\\V[/X/g", "aXb /\n", (0, 0), Some("")),
    ]),
    ("unclosed_collection", "a[b/x/\n", &[
        (":s/[b/x/", "a\n", (0, 0), Some("")),
    ]),
    ("dollar_very", "a\nb\n", &[
        (":%s/a\\m$\\v|b/X/g", "X\nX\n", (1, 0), Some("")),
    ]),
    ("stacked", "xx\n", &[
        (":s/x**/y/", "xx\n", (0, 0), Some("pattern: can't have a multi follow a multi")),
    ]),
    ("stacked_count", "xx\n", &[
        (":s/x\\{2}*/y/", "xx\n", (0, 0), Some("pattern: can't have a multi follow a multi")),
    ]),
    ("zs_ze_branch", "abc\n", &[
        (":s/a\\zs\\ze\\|b/X/g", "aXXc\n", (0, 0), Some("")),
    ]),
    ("word_end_eol", "abc\n", &[
        (":s/\\>/X/g", "abcX\n", (0, 0), Some("")),
    ]),
    ("empty_repeat", "abc\nbaaa\n", &[
        (":%s/x\\?/-/g", "-a-b-c\n-b-a-a-a\n", (1, 0), Some("7 substitutions on 2 lines")),
        (":2s/a*/X/g", "-a-b-c\nX-XbX-X-X-X\n", (1, 0), Some("")),
    ]),
    ("char_or_eol", "abc\n", &[
        (":s/c\\|$/X/g", "abX\n", (0, 0), Some("")),
    ]),
    ("zs_multiline_range", "a\nb\nc\n", &[
        (":1s/a\\n\\zsb/X/", "a\nX\nc\n", (1, 0), Some("")),
    ]),
    ("final_break", "a\nb\nz\n", &[
        (":%s/.\\n/X/g", "XXX\n", (0, 0), Some("")),
    ]),
    ("tilde_group", "abbb abab a\nx\n", &[
        (":2s/x/ab/", "abbb abab a\nab\n", (1, 0), Some("")),
        (":1s/~*/X/g", "XbXbX X Xa\nab\n", (0, 0), Some("")),
    ]),
    ("tilde_repeat", "q\nx\ny\nx\n", &[
        (":1s/q/x/", "x\nx\ny\nx\n", (0, 0), Some("")),
        (":2s/~/y/", "x\ny\ny\nx\n", (1, 0), Some("")),
        (":4s", "x\ny\ny\nx\n", (1, 0), Some("pattern not found: ~")),
        (":3s", "x\ny\ny\nx\n", (2, 0), Some("")),
    ]),
    ("star_punct", "foo++\n++\nfoo++\n", &[
        ("$*", "foo++\n++\nfoo++\n", (1, 0), None),
    ]),
    ("upper_simple", "ᾳ ᾀ ᾓ ß ŉ\n", &[
        (":s/.*/\\U&/", "ᾼ ᾈ ᾛ ẞ ŉ\n", (0, 0), Some("")),
    ]),
    ("lower_simple", "İ ẞ Ω\n", &[
        (":s/.*/\\L&/", "i ß ω\n", (0, 0), Some("")),
    ]),
    ("upper_once", "ᾳx\n", &[
        (":s/.*/\\u&/", "ᾼx\n", (0, 0), Some("")),
    ]),
    ("collection_line_end", "caa\nx\n", &[
        (":%s/\\s\\|[a\\n]/X/g", "cXX\nxX\n", (1, 0), Some("3 substitutions on 2 lines")),
    ]),
    ("collection_joins", "b\nb\n", &[
        (":%s/[a\\n]/X/g", "bXbX\n", (0, 0), Some("")),
    ]),
    ("joined_again", "c \nacb\ncaa  \n", &[
        (":%s/\\n/X/", "c XacbXcaa  X\n", (0, 0), Some("")),
    ]),
    ("joined_in_range", "a\nb\nc\nd\n", &[
        (":1,2s/\\n/X/", "aXbXc\nd\n", (0, 0), Some("")),
    ]),
    ("joined_then_line", "a\nb\nc\nd\n", &[
        (":%s/\\n\\|b/X/", "aXX\ncXdX\n", (1, 0), Some("4 substitutions on 2 lines")),
    ]),
    ("joined_past_end", "\n\n", &[
        (":%s/\\a*\\n\\{,2}/X/", "X\n", (0, 0), Some("")),
    ]),
    ("past_last_line", "b A\n", &[
        (":%s/[a\\n]\\zs\\(a\\|b\\)\\{-}.*/X/", "b A\n", (0, 0), Some("")),
    ]),
    ("ze_before_zs", "c ca\na\n", &[
        (":%s/\\ze\\zsc/<&>/g", "<c> <c>a\na\n", (0, 0), Some("")),
    ]),
    ("star_after_newline", " a\nb\nAA\n\n", &[
        (":%s/\\%(ab\\)\\=\\n*[a\\n]\\{2}/X/", " Xb\nAAX\n", (1, 0), Some("")),
    ]),
    ("caret_after_zs", "a ^a\n", &[
        (":s/\\zs^a/X/", "a X\n", (0, 0), Some("")),
    ]),
    // `\zs` puts the first edit on the next line, whose own search then
    // finds edits before it: applied in source order.
    ("zs_next_line_then_caret", "a\nbX\n", &[
        (":%s/a\\nb\\zsX\\|^/Y/", "a\nYbY\n", (1, 0), Some("")),
    ]),
    ("zs_next_line_global", "a\nbX\n", &[
        (":%s/a\\nb\\zsX\\|^/Y/g", "a\nbY\n", (1, 0), Some("")),
    ]),
    ("zs_next_line_overlap", "a\nbX\n", &[
        (":%s/a\\n\\zsbX\\|X/Z/", "a\nZ\n", (1, 0), Some("")),
    ]),
    ("zs_next_line_before", "a\nbX\n", &[
        (":%s/a\\nb\\zsX\\|b/Z/", "a\nZZ\n", (1, 0), Some("")),
    ]),
    ("zs_two_lines_on", "a\n\nbX\n", &[
        (":%s/a\\n\\n\\zsbX\\|^/Z/", "a\nZ\nZZ\n", (2, 0), None),
    ]),
    // A line whose match `\zs` put on the next line still has its own
    // search there: `n` and `N` stop as Neovim's `searchit`.
    ("search_zs_line_keeps_own", "\nAcc\naA \n\n", &[
        ("/\\n\\zs[^a]\\+\\|.\r", "\nAcc\naA \n\n", (1, 0), None),
        ("n", "\nAcc\naA \n\n", (1, 1), None),
        ("n", "\nAcc\naA \n\n", (1, 2), None),
        ("N", "\nAcc\naA \n\n", (1, 1), None),
    ]),
    ("search_zs_first_line", "  \ncb c \nc\n", &[
        ("/\\n\\w\\{,2}\\zs\\a\\|^\r", "  \ncb c \nc\n", (1, 1), None),
        ("n", "  \ncb c \nc\n", (2, 0), None),
        ("n", "  \ncb c \nc\n", (0, 0), None),
        ("N", "  \ncb c \nc\n", (2, 0), None),
    ]),
    ("search_zs_backward", "cc \ncc b \n", &[
        ("/ \\=[a\\n]\\zs\\|[ab]\\(b*\\)b\r", "cc \ncc b \n", (1, 0), None),
        ("n", "cc \ncc b \n", (1, 4), None),
        ("n", "cc \ncc b \n", (1, 0), None),
        ("N", "cc \ncc b \n", (1, 4), None),
    ]),
    ("search_zs_backward_wrap", "\naba\nAbA\n", &[
        ("/\\c\\n[a\\n]\\{-1,2}\\zs\\(b*\\)\\|b\r", "\naba\nAbA\n", (1, 1), None),
        ("n", "\naba\nAbA\n", (2, 1), None),
        ("n", "\naba\nAbA\n", (1, 1), None),
        ("N", "\naba\nAbA\n", (2, 1), None),
    ]),
    ("search_zs_two_lines", "   bc\n\nbAA  \n \n", &[
        ("/\\n\\n\\zs[ab]\\|x.\\| \r", "   bc\n\nbAA  \n \n", (0, 1), None),
        ("n", "   bc\n\nbAA  \n \n", (0, 2), None),
        ("n", "   bc\n\nbAA  \n \n", (2, 0), None),
        ("N", "   bc\n\nbAA  \n \n", (0, 2), None),
    ]),
    // A match at a line's end lands on its last character, and `j` keeps
    // that column, not the one past the end.
    ("search_line_end_goal", "abc\n0123456789\n", &[
        ("/$\r", "abc\n0123456789\n", (0, 2), None),
        ("j", "abc\n0123456789\n", (1, 2), None),
    ]),
    ("search_line_break_goal", "abc\n0123456789\n", &[
        ("/c\\n/e\r", "abc\n0123456789\n", (0, 2), None),
        ("j", "abc\n0123456789\n", (1, 2), None),
        ("gg/c\\n\r", "abc\n0123456789\n", (0, 2), None),
        ("j", "abc\n0123456789\n", (1, 2), None),
    ]),
    ("search_continues_at_end", "aaca\n abAc\n", &[
        ("/a\\w\r", "aaca\n abAc\n", (1, 1), None),
        ("n", "aaca\n abAc\n", (0, 0), None),
        ("N", "aaca\n abAc\n", (1, 1), None),
    ]),
    ("search_empty_lines", "\n\n \nca \n", &[
        ("/\\n\r", "\n\n \nca \n", (1, 0), None),
        ("n", "\n\n \nca \n", (2, 0), None),
        ("n", "\n\n \nca \n", (3, 2), None),
    ]),
    ("search_past_last_line", "b A\n", &[
        ("/[a\\n]\\zs\\(a\\|b\\)\\{-}.*\r", "b A\n", (0, 2), None),
    ]),
    ("search_empty_match", "b Aa \n", &[
        ("/\\(b*\\)\r", "b Aa \n", (0, 1), None),
        ("n", "b Aa \n", (0, 2), None),
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

/// `a` at the first question applies what a plain `:s` would, even when
/// `\zs` put the first match after later ones (Neovim, `c` then `a`).
#[test]
fn confirm_all_applies_edits_in_source_order() {
    for (text, command, expected) in [
        ("a\nbX\n", r"%s/a\nb\zsX\|^/Y/c", "a\nYbY\n"),
        ("a\nbX\n", r"%s/a\n\zsbX\|X/Z/c", "a\nZ\n"),
        ("a\n\nbX\n", r"%s/a\n\n\zsbX\|^/Z/c", "a\nZ\nZZ\n"),
    ] {
        let mut app = app_with(text);
        ex(&mut app, command);
        key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(app.buffer.rope.to_string(), expected, "{command}");
        feed(&mut app, "u");
        assert_eq!(app.buffer.rope.to_string(), text, "{command}");
    }
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
    feed(&mut app, "/x**");
    enter(&mut app);
    assert_eq!(at(&app), before);
    assert_eq!(app.search.as_ref().unwrap().pattern, "old");
    assert_eq!(flash(&app), "pattern: can't have a multi follow a multi");
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
    assert!(elapsed < Duration::from_secs(2), "substitute took {elapsed:?}");
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

/// Neovim: "E35: No previous regular expression".
#[test]
fn a_repeat_without_a_search_is_an_error() {
    let mut app = app_with("x\n");
    feed(&mut app, "n");
    assert_eq!(flash(&app), "no previous regular expression");
}

#[test]
fn multiline_search_repeats_reuse_one_index() {
    let mut app = app_with(&"foo\nbar\n".repeat(50_000));
    feed(&mut app, r"/foo\nbar");
    enter(&mut app);
    assert_eq!(at(&app), (2, 0));
    let start = Instant::now();
    for _ in 0..200 {
        feed(&mut app, "n");
    }
    assert_eq!(at(&app), (402, 0));
    feed(&mut app, "G");
    for _ in 0..200 {
        feed(&mut app, "N");
    }
    let elapsed = start.elapsed();
    assert_eq!(at(&app), (99_600, 0));
    assert!(elapsed < Duration::from_millis(500), "400 repeats took {elapsed:?}");
    feed(&mut app, "ggN");
    assert_eq!(at(&app), (99_998, 0));
}

#[test]
fn a_pathological_visible_line_does_not_stall_highlighting() {
    let mut app = app_with(&format!("{}\nz\n", "a".repeat(1 << 20)));
    assert!(app.set_search(r".*z\|.".into(), false));
    let text = app.buffer.line_text(0);
    let start = Instant::now();
    let first = app.search_highlights(&app, 0, &text);
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(1), "first highlight took {elapsed:?}");
    assert_eq!(first.first(), Some(&(0..1)));
    let start = Instant::now();
    for _ in 0..100 {
        assert_eq!(app.search_highlights(&app, 0, &text).len(), first.len());
    }
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_millis(100), "cached highlights took {elapsed:?}");
    assert_eq!(app.search_highlights(&app, 1, "z"), vec![0..1]);
    // `n` and `N` try that line's matches only until they pass the cursor.
    let start = Instant::now();
    feed(&mut app, "/.*z\\|.\r");
    assert_eq!(at(&app), (0, 1));
    feed(&mut app, "N");
    assert_eq!(at(&app), (0, 0));
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(2), "searching the line took {elapsed:?}");
}

/// One match per character on a 10 MB line (1 MiB in a debug build, whose
/// regex is about ten times slower): the stops are found in one pass,
/// without looking for the line's end again per match.
#[test]
fn stops_on_a_long_line_take_bounded_time() {
    let len = if cfg!(debug_assertions) { 1 << 20 } else { 10_000_000 };
    let text = format!("{}\nb\n", "a".repeat(len));
    // Multi-line: the index of stops is built once, linearly.
    let mut app = app_with(&text);
    let start = Instant::now();
    feed(&mut app, "/\\n\\|.\r");
    assert_eq!(at(&app), (0, 1));
    feed(&mut app, "n");
    assert_eq!(at(&app), (0, 2));
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(10), "multi-line stops took {elapsed:?}");
    // Line-local: `$` then `N` tries the line's matches up to the cursor once.
    let mut app = app_with(&text);
    feed(&mut app, "/.\r");
    let start = Instant::now();
    feed(&mut app, "$N");
    let elapsed = start.elapsed();
    assert_eq!(at(&app), (0, len - 2));
    assert!(elapsed < Duration::from_secs(10), "N on the line took {elapsed:?}");
}

/// The first scan of a large document runs out of its redraw budget; a
/// viewport past what it found still gets its highlights, from its own scan.
#[test]
fn multiline_highlights_reach_past_a_cut_scan() {
    let pairs = 200_000;
    let mut app = app_with(&"foo\nbar\n".repeat(pairs));
    assert!(app.set_search(r"foo\nbar".into(), false));
    let doc: &BufferState = &app;
    assert_eq!(app.search_highlights(doc, 0, "foo"), vec![0..3]);
    assert_eq!(app.search_highlights(doc, 1, "bar"), vec![0..3]);
    let last = 2 * pairs - 1;
    let start = Instant::now();
    assert_eq!(app.search_highlights(doc, last - 1, "foo"), vec![0..3]);
    assert_eq!(app.search_highlights(doc, last, "bar"), vec![0..3]);
    // The lines around it come from the same scan.
    for line in last - 40..last {
        assert_eq!(app.search_highlights(doc, line, "foo"), vec![0..3], "line {line}");
    }
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(1), "the viewport's highlights took {elapsed:?}");
    // And the middle, after the end was scanned.
    assert_eq!(app.search_highlights(doc, pairs, "foo"), vec![0..3]);

    // A line too long to scan in time is served by its own scan, kept.
    let mut app = app_with(&format!("{}\nz\n", "a".repeat(1 << 20)));
    assert!(app.set_search(r"\n\|.".into(), false));
    let doc: &BufferState = &app;
    let text = app.buffer.line_text(0);
    let first = app.search_highlights(doc, 0, &text);
    assert_eq!(first.first(), Some(&(0..1)));
    let start = Instant::now();
    for _ in 0..100 {
        assert_eq!(app.search_highlights(doc, 0, &text).len(), first.len());
    }
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_millis(500), "cached highlights took {elapsed:?}");
}

/// `[` then many `[:` with no `:]`: each opener looks at its own name only.
#[test]
fn a_pasted_pattern_of_class_openers_is_read_in_linear_time() {
    let pattern = format!("[{}x]]", "[:".repeat(200_000));
    let input = format!("{pattern}/y/");
    let start = Instant::now();
    let (field, rest) = search::pattern_field(&input, '/', false);
    let elapsed = start.elapsed();
    assert_eq!(field, pattern);
    assert_eq!(rest, Some("y/"));
    assert!(elapsed < Duration::from_secs(1), "reading the pattern took {elapsed:?}");
    // Class names are still read through: a `/` inside one does not end it.
    assert_eq!(search::pattern_field("[[:alpha:]/]x/y", '/', false), ("[[:alpha:]/]x".into(), Some("y")));
    assert_eq!(search::pattern_field("[[:al/pha:]]/y", '/', false), ("[[:al/pha:]]".into(), Some("y")));
}

/// A search stays active while files are opened, drawn and closed: what
/// it kept for a closed buffer (highlights, `n`'s stops) goes with it.
#[test]
fn closing_a_buffer_drops_its_search_matches() {
    let dir = std::env::temp_dir().join(format!("shoin-search-cache-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = app_with("x\n");
    for pattern in ["/x\r", "/x\\n\r"] {
        feed(&mut app, pattern);
        for i in 0..20 {
            let path = dir.join(format!("{i}.md"));
            std::fs::write(&path, "x\nx\n").unwrap();
            app.open_file(path);
            let doc: &BufferState = &app;
            assert_eq!(app.search_highlights(doc, 0, "x"), vec![0..1]);
            feed(&mut app, "n");
            ex(&mut app, "bd");
        }
        assert_eq!(app.docs.len(), 1);
        let doc: &BufferState = &app;
        app.search_highlights(doc, 0, "x");
        assert_eq!(app.search.as_ref().unwrap().cached_docs(), 1, "{pattern}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_final_line_break_matches_without_a_final_newline() {
    let mut app = app_with("a\nb\nz");
    ex(&mut app, r"%s/.\n/X/g");
    assert_eq!(app.buffer.rope.to_string(), "XXX");
    assert!(app.buffer.undo());
    assert_eq!(app.buffer.rope.to_string(), "a\nb\nz");
}

/// Replays cases that `target/nvim-random.py` ran in Neovim: the path in
/// `SHOIN_ORACLE` holds one case per line (name, text, then commands, tab
/// separated, `\` escapes; a command starting `!` is a `:s` with `c`
/// answered `a`), and the results go to `<path>.shoin` for it to compare.
/// Nothing to do without the variable.
#[test]
#[ignore]
fn replay_neovim_oracle_cases() {
    let Ok(path) = std::env::var("SHOIN_ORACLE") else { return };
    let unescape = |text: &str| {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            let escaped = if ch == '\\' { chars.next() } else { None };
            out.push(match escaped {
                Some('n') => '\n',
                Some('t') => '\t',
                Some('r') => '\r',
                Some(other) => other,
                None => ch,
            });
        }
        out
    };
    let escape = |text: &str| text.replace('\\', r"\\").replace('\n', r"\n").replace('\t', r"\t").replace('\r', r"\r");
    let mut out = String::new();
    for case in std::fs::read_to_string(&path).unwrap().lines() {
        let fields: Vec<&str> = case.split('\t').collect();
        let mut app = app_with(&unescape(fields[1]));
        out.push_str(fields[0]);
        for command in &fields[2..] {
            let command = unescape(command);
            app.flash = None;
            if let Some(command) = command.strip_prefix(':') {
                ex(&mut app, command);
            } else if let Some(command) = command.strip_prefix('!') {
                // Confirm, then `a` at the first question.
                ex(&mut app, command);
                if app.mode == Mode::SubstituteConfirm {
                    feed(&mut app, "a");
                }
            } else {
                feed(&mut app, &command);
            }
            let error = app.flash.as_ref().is_some_and(|f| matches!(f.kind, FlashKind::Error));
            let (line, col) = at(&app);
            out.push_str(&format!("\t{}\t{line}\t{col}\t{}", escape(&app.buffer.rope.to_string()), if error { "E" } else { "-" }));
        }
        out.push('\n');
    }
    std::fs::write(format!("{path}.shoin"), out).unwrap();
}
