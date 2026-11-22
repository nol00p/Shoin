//! Line addresses, ranges and the line commands. Every expected text,
//! cursor and register below is what `nvim --clean` (with `ts=4 sw=4 et`)
//! gives for the same text, cursor and keys.

use super::*;
use crate::test_support::app_with;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.on_key(KeyEvent::new(code, mods));
    app.sync_after_input();
}

fn feed(app: &mut App, keys: &str) {
    for c in keys.chars() {
        key(app, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

/// Type `:cmd` and Enter, from wherever the editor is.
fn ex(app: &mut App, cmd: &str) {
    feed(app, ":");
    feed(app, cmd);
    key(app, KeyCode::Enter, KeyModifiers::NONE);
}

fn text(app: &App) -> String {
    app.buffer.rope.to_string()
}

fn flash(app: &App) -> String {
    app.flash.as_ref().and_then(|f| f.text.clone()).unwrap_or_default()
}

fn at(app: &App) -> (usize, usize) {
    (app.buffer.cursor.line, app.buffer.cursor.col)
}

/// One step of a case: `:cmd`, or Normal keys (`<C-r>` is Ctrl-r).
fn step(app: &mut App, s: &str) {
    if let Some(cmd) = s.strip_prefix(':') {
        ex(app, cmd);
    } else if s == "<C-r>" {
        key(app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    } else {
        feed(app, s);
    }
}

type Case = (&'static str, (usize, usize), &'static [&'static str], &'static str, (usize, usize), &'static str);

/// Text, cursor before, steps, then Neovim's text, cursor and unnamed
/// register after them ("" where no step fills it).
const CASES: &[Case] = &[
    // jumps
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":+"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (4, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":-"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (2, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":+2"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (5, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":-2"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (1, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":.+2"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (5, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":.-2"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (1, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":$-1"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (8, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":$-3"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (6, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":3+2"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (4, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":3-1"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (1, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[": 3 + 2 "], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (5, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":.  +  2"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (6, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":++"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (5, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":--"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (1, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":+-"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":.5"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (8, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":3 2"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (4, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":+100"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (9, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":100"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (9, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":0"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (0, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":$+1"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (9, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":3,5"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (4, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":%"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (9, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":.,+2"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (5, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":3,+2"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (5, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":-0"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":+0"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":4,"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":,6"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (5, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":$"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (9, 1), ""),
    ("a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (3, 1), &[":7"], "a1\nb2\nc3\nd4\ne5\nf6\ng7\nh8\ni9\nj10\n", (6, 1), ""),
    // delete
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":2,3d"], "one\n    four\nfive\nsix\nseven\n", (1, 4), "  two\nthree\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":d"], "one\n  two\nthree\nfive\nsix\nseven\n", (3, 0), "    four\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":de"], "one\n  two\nthree\nfive\nsix\nseven\n", (3, 0), "    four\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":del"], "one\n  two\nthree\nfive\nsix\nseven\n", (3, 0), "    four\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":delete"], "one\n  two\nthree\nfive\nsix\nseven\n", (3, 0), "    four\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":d 3"], "one\n  two\nthree\nseven\n", (3, 0), "    four\nfive\nsix\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":2,3d 2"], "one\n  two\nfive\nsix\nseven\n", (2, 0), "three\n    four\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":%d"], "", (0, 0), "one\n  two\nthree\n    four\nfive\nsix\nseven\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":6,7d"], "one\n  two\nthree\n    four\nfive\n", (4, 0), "six\nseven\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":$d"], "one\n  two\nthree\n    four\nfive\nsix\n", (5, 0), "seven\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":1d"], "  two\nthree\n    four\nfive\nsix\nseven\n", (0, 2), "one\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":d3"], "one\n  two\nthree\nseven\n", (3, 0), "    four\nfive\nsix\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":2,3 d"], "one\n    four\nfive\nsix\nseven\n", (1, 4), "  two\nthree\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":5,$d"], "one\n  two\nthree\n    four\n", (3, 4), "five\nsix\nseven\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (6, 2), &[":d"], "one\n  two\nthree\n    four\nfive\nsix\n", (5, 0), "seven\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":d", "j"], "alpha one\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 0), "  bravo two two two\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":d", "j"], "alpha one\nc\n    delta four\necho five\nfoxtrot six\n", (2, 0), "  bravo two two two\n"),
    // yank
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":2,3y"], "one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), "  two\nthree\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":y"], "one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), "    four\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":ya"], "one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), "    four\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":yank"], "one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), "    four\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":y 2"], "one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), "    four\nfive\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":5,6y"], "one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), "five\nsix\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":y3"], "one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), "    four\nfive\nsix\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":%y"], "one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), "one\n  two\nthree\n    four\nfive\nsix\nseven\n"),
    ("one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), &[":$y 5"], "one\n  two\nthree\n    four\nfive\nsix\nseven\n", (3, 5), "seven\n"),
    // shift
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":>"], "    one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":4,5>"], "one\n  two\nthree\n        four four\n    five five five\nsix\n        seven seven\n", (4, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":4,5<"], "one\n  two\nthree\nfour four\nfive five five\nsix\n        seven seven\n", (4, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":3,4>"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":6,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":5,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":5,6<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (5, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":1,3<"], "one\ntwo\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (2, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":>>"], "        one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":<<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":2,3>>"], "one\n          two\n        three\n    four four\nfive five five\nsix\n        seven seven\n", (2, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":> 2"], "    one\n      two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (1, 6), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":2,3> 2"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (0, 0), &[":4 >"], "one\n  two\nthree\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":>"], "one\n  two\nthree\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":<"], "one\n  two\nthree\nfour four\nfive five five\nsix\n        seven seven\n", (3, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":4,5>"], "one\n  two\nthree\n        four four\n    five five five\nsix\n        seven seven\n", (4, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":4,5<"], "one\n  two\nthree\nfour four\nfive five five\nsix\n        seven seven\n", (4, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":3,4>"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":6,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":5,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":5,6<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (5, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":1,3<"], "one\ntwo\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (2, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":>>"], "one\n  two\nthree\n            four four\nfive five five\nsix\n        seven seven\n", (3, 12), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":<<"], "one\n  two\nthree\nfour four\nfive five five\nsix\n        seven seven\n", (3, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":2,3>>"], "one\n          two\n        three\n    four four\nfive five five\nsix\n        seven seven\n", (2, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":> 2"], "one\n  two\nthree\n        four four\n    five five five\nsix\n        seven seven\n", (4, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":2,3> 2"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 0), &[":4 >"], "one\n  two\nthree\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":>"], "one\n  two\nthree\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":<"], "one\n  two\nthree\nfour four\nfive five five\nsix\n        seven seven\n", (3, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":4,5>"], "one\n  two\nthree\n        four four\n    five five five\nsix\n        seven seven\n", (4, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":4,5<"], "one\n  two\nthree\nfour four\nfive five five\nsix\n        seven seven\n", (4, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":3,4>"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":6,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":5,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":5,6<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (5, 2), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":1,3<"], "one\ntwo\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (2, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":>>"], "one\n  two\nthree\n            four four\nfive five five\nsix\n        seven seven\n", (3, 12), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":<<"], "one\n  two\nthree\nfour four\nfive five five\nsix\n        seven seven\n", (3, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":2,3>>"], "one\n          two\n        three\n    four four\nfive five five\nsix\n        seven seven\n", (2, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":> 2"], "one\n  two\nthree\n        four four\n    five five five\nsix\n        seven seven\n", (4, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":2,3> 2"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (3, 11), &[":4 >"], "one\n  two\nthree\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":>"], "one\n  two\nthree\n    four four\n    five five five\nsix\n        seven seven\n", (4, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":4,5>"], "one\n  two\nthree\n        four four\n    five five five\nsix\n        seven seven\n", (4, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":4,5<"], "one\n  two\nthree\nfour four\nfive five five\nsix\n        seven seven\n", (4, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":3,4>"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":6,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":5,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":5,6<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (5, 2), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":1,3<"], "one\ntwo\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (2, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":>>"], "one\n  two\nthree\n    four four\n        five five five\nsix\n        seven seven\n", (4, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":<<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":2,3>>"], "one\n          two\n        three\n    four four\nfive five five\nsix\n        seven seven\n", (2, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":> 2"], "one\n  two\nthree\n    four four\n    five five five\n    six\n        seven seven\n", (5, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":2,3> 2"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (4, 8), &[":4 >"], "one\n  two\nthree\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":>"], "one\n  two\nthree\n    four four\nfive five five\nsix\n            seven seven\n", (6, 12), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":4,5>"], "one\n  two\nthree\n        four four\n    five five five\nsix\n        seven seven\n", (4, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":4,5<"], "one\n  two\nthree\nfour four\nfive five five\nsix\n        seven seven\n", (4, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":3,4>"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":6,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":5,7<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n    seven seven\n", (6, 4), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":5,6<"], "one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (5, 2), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":1,3<"], "one\ntwo\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (2, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":>>"], "one\n  two\nthree\n    four four\nfive five five\nsix\n                seven seven\n", (6, 16), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":<<"], "one\n  two\nthree\n    four four\nfive five five\nsix\nseven seven\n", (6, 0), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":2,3>>"], "one\n          two\n        three\n    four four\nfive five five\nsix\n        seven seven\n", (2, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":> 2"], "one\n  two\nthree\n    four four\nfive five five\nsix\n            seven seven\n", (6, 12), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":2,3> 2"], "one\n  two\n    three\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("one\n  two\nthree\n    four four\nfive five five\nsix\n        seven seven\n", (6, 13), &[":4 >"], "one\n  two\nthree\n        four four\nfive five five\nsix\n        seven seven\n", (3, 8), ""),
    ("a\n  bb\n", (0, 0), &[":>", "j"], "    a\n  bb\n", (1, 0), ""),
    ("abcdef\nabcdef\nabcdef\n", (0, 4), &[":>", "j"], "    abcdef\nabcdef\nabcdef\n", (1, 4), ""),
    // move
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":m 0"], "charlie three three\nalpha one\n  bravo two two two\n    delta four\necho five\nfoxtrot six\n", (0, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":m $"], "alpha one\n  bravo two two two\n    delta four\necho five\nfoxtrot six\ncharlie three three\n", (5, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":1,2m 4"], "charlie three three\n    delta four\nalpha one\n  bravo two two two\necho five\nfoxtrot six\n", (3, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":1,2m $"], "charlie three three\n    delta four\necho five\nfoxtrot six\nalpha one\n  bravo two two two\n", (5, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":2,3m 0"], "  bravo two two two\ncharlie three three\nalpha one\n    delta four\necho five\nfoxtrot six\n", (1, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":2,3m 3"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":2,3m 1"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":m4"], "alpha one\n  bravo two two two\n    delta four\ncharlie three three\necho five\nfoxtrot six\n", (3, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":m+1"], "alpha one\n  bravo two two two\n    delta four\ncharlie three three\necho five\nfoxtrot six\n", (3, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":m -2"], "alpha one\ncharlie three three\n  bravo two two two\n    delta four\necho five\nfoxtrot six\n", (1, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":m0"], "charlie three three\nalpha one\n  bravo two two two\n    delta four\necho five\nfoxtrot six\n", (0, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":m ."], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":4m 1"], "alpha one\n    delta four\n  bravo two two two\ncharlie three three\necho five\nfoxtrot six\n", (1, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":co 0"], "charlie three three\nalpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":copy 0"], "charlie three three\nalpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":t 0"], "charlie three three\nalpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":t $"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\ncharlie three three\n", (6, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":t."], "alpha one\n  bravo two two two\ncharlie three three\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (3, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":1,2t 2"], "alpha one\n  bravo two two two\nalpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (3, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":1,2t 1"], "alpha one\nalpha one\n  bravo two two two\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":2,4t 3"], "alpha one\n  bravo two two two\ncharlie three three\n  bravo two two two\ncharlie three three\n    delta four\n    delta four\necho five\nfoxtrot six\n", (5, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":1,2t 4"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\nalpha one\n  bravo two two two\necho five\nfoxtrot six\n", (5, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":t0"], "charlie three three\nalpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":t 1 2"], "alpha one\n  bravo two two two\ncharlie three three\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (3, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":mo 5"], "alpha one\n  bravo two two two\n    delta four\necho five\ncharlie three three\nfoxtrot six\n", (4, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":move 5"], "alpha one\n  bravo two two two\n    delta four\necho five\ncharlie three three\nfoxtrot six\n", (4, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":cop $"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\ncharlie three three\n", (6, 11), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":t 0", "j"], "charlie three three\nalpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 11), &[":1,2m 4", "j"], "charlie three three\n    delta four\nalpha one\n  bravo two two two\necho five\nfoxtrot six\n", (4, 8), ""),
    ("x\ny", (1, 0), &[":m 0"], "y\nx", (0, 0), ""),
    ("x\ny", (0, 0), &[":m $"], "y\nx", (1, 0), ""),
    ("x\ny", (0, 0), &[":t $"], "x\ny\nx", (2, 0), ""),
    ("x\ny", (1, 0), &[":t 0"], "y\nx\ny", (0, 0), ""),
    // undo
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":d", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), "  bravo two two two\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":d", "u", "<C-r>"], "alpha one\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), "  bravo two two two\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":d", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), "  bravo two two two\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":d", "u", "<C-r>"], "alpha one\nc\n    delta four\necho five\nfoxtrot six\n", (1, 0), "  bravo two two two\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5d", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (3, 13), "    delta four\necho five\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5d", "u", "<C-r>"], "alpha one\n  bravo two two two\ncharlie three three\nfoxtrot six\n", (3, 10), "    delta four\necho five\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5d", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (3, 13), "    delta four\necho five\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5d", "u", "<C-r>"], "alpha one\n  bravo two two two\nc\nfoxtrot six\n", (3, 10), "    delta four\necho five\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1,2d", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 8), "alpha one\n  bravo two two two\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1,2d", "u", "<C-r>"], "charlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 8), "alpha one\n  bravo two two two\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1,2d", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (0, 8), "alpha one\n  bravo two two two\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1,2d", "u", "<C-r>"], "c\n    delta four\necho five\nfoxtrot six\n", (0, 0), "alpha one\n  bravo two two two\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":5,6d", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (4, 8), "echo five\nfoxtrot six\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":5,6d", "u", "<C-r>"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\n", (3, 8), "echo five\nfoxtrot six\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":5,6d", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (4, 8), "echo five\nfoxtrot six\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":5,6d", "u", "<C-r>"], "alpha one\n  bravo two two two\nc\n    delta four\n", (3, 8), "echo five\nfoxtrot six\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":3,4d", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 14), "charlie three three\n    delta four\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":3,4d", "u", "<C-r>"], "alpha one\n  bravo two two two\necho five\nfoxtrot six\n", (2, 8), "charlie three three\n    delta four\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":3,4d", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (2, 0), "c\n    delta four\n"),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":3,4d", "u", "<C-r>"], "alpha one\n  bravo two two two\necho five\nfoxtrot six\n", (2, 0), "c\n    delta four\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":m 0", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":m 0", "u", "<C-r>"], "  bravo two two two\nalpha one\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":m 0", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":m 0", "u", "<C-r>"], "  bravo two two two\nalpha one\nc\n    delta four\necho five\nfoxtrot six\n", (1, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":m $", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":m $", "u", "<C-r>"], "alpha one\ncharlie three three\n    delta four\necho five\nfoxtrot six\n  bravo two two two\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":m $", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":m $", "u", "<C-r>"], "alpha one\nc\n    delta four\necho five\nfoxtrot six\n  bravo two two two\n", (1, 0), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5m 0", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (5, 10), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5m 0", "u", "<C-r>"], "    delta four\necho five\nalpha one\n  bravo two two two\ncharlie three three\nfoxtrot six\n", (1, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5m 0", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (5, 10), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5m 0", "u", "<C-r>"], "    delta four\necho five\nalpha one\n  bravo two two two\nc\nfoxtrot six\n", (1, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1m 3", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1m 3", "u", "<C-r>"], "  bravo two two two\ncharlie three three\nalpha one\n    delta four\necho five\nfoxtrot six\n", (3, 13), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1m 3", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1m 3", "u", "<C-r>"], "  bravo two two two\nc\nalpha one\n    delta four\necho five\nfoxtrot six\n", (3, 13), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 4", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (4, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 4", "u", "<C-r>"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\n  bravo two two two\necho five\nfoxtrot six\n", (4, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 4", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (4, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 4", "u", "<C-r>"], "alpha one\n  bravo two two two\nc\n    delta four\n  bravo two two two\necho five\nfoxtrot six\n", (4, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 0", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 0", "u", "<C-r>"], "  bravo two two two\nalpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 0", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 0", "u", "<C-r>"], "  bravo two two two\nalpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5t 0", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5t 0", "u", "<C-r>"], "    delta four\necho five\nalpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5t 0", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":4,5t 0", "u", "<C-r>"], "    delta four\necho five\nalpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t .", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t .", "u", "<C-r>"], "alpha one\n  bravo two two two\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t .", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t .", "u", "<C-r>"], "alpha one\n  bravo two two two\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 1", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 1", "u", "<C-r>"], "alpha one\n  bravo two two two\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 1", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 1", "u", "<C-r>"], "alpha one\n  bravo two two two\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 5", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (5, 10), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 5", "u", "<C-r>"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\n  bravo two two two\nfoxtrot six\n", (5, 10), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 5", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (5, 10), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t 5", "u", "<C-r>"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\n  bravo two two two\nfoxtrot six\n", (5, 10), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":2,3t 0", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":2,3t 0", "u", "<C-r>"], "  bravo two two two\ncharlie three three\nalpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":2,3t 0", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":2,3t 0", "u", "<C-r>"], "  bravo two two two\nc\nalpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 0), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t $", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (5, 10), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t $", "u", "<C-r>"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n  bravo two two two\n", (6, 10), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t $", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (5, 10), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":t $", "u", "<C-r>"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n  bravo two two two\n", (6, 10), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":3,4>", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (2, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":3,4>", "u", "<C-r>"], "alpha one\n  bravo two two two\n    charlie three three\n        delta four\necho five\nfoxtrot six\n", (2, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":3,4>", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (2, 0), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":3,4>", "u", "<C-r>"], "alpha one\n  bravo two two two\n    c\n        delta four\necho five\nfoxtrot six\n", (2, 0), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":>", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":>", "u", "<C-r>"], "alpha one\n      bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":>", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":>", "u", "<C-r>"], "alpha one\n      bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":<", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":<", "u", "<C-r>"], "alpha one\nbravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":<", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":<", "u", "<C-r>"], "alpha one\nbravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1,4<", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1,4<", "u", "<C-r>"], "alpha one\nbravo two two two\ncharlie three three\ndelta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1,4<", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":1,4<", "u", "<C-r>"], "alpha one\nbravo two two two\nc\ndelta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":%>", "u"], "alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":%>", "u", "<C-r>"], "    alpha one\n      bravo two two two\n    charlie three three\n        delta four\n    echo five\n    foxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":%>", "u"], "alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (0, 8), ""),
    ("alpha one\n  bravo two two two\nc\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[":%>", "u", "<C-r>"], "    alpha one\n      bravo two two two\n    c\n        delta four\n    echo five\n    foxtrot six\n", (0, 8), ""),
    // dot
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &[">>", ":4d", "j", "."], "alpha one\n      bravo two two two\ncharlie three three\necho five\n    foxtrot six\n", (4, 0), "    delta four\n"),
    ("alpha one\n  bravo two two two\ncharlie three three\n    delta four\necho five\nfoxtrot six\n", (1, 14), &["dd", ":d", "j", "."], "alpha one\n    delta four\nfoxtrot six\n", (2, 4), "echo five\n"),
];

#[test]
fn line_commands_land_as_neovim() {
    let mut misses = Vec::new();
    for &(before, (l, c), steps, want, cursor, reg) in CASES {
        let mut app = app_with(before);
        app.buffer.cursor = Cursor::new(l, c);
        for s in steps {
            step(&mut app, s);
        }
        let got_reg = app.registers.get(&'"').map(|r| (r.text.clone(), r.linewise));
        let reg_ok = reg.is_empty() || got_reg == Some((reg.to_string(), true));
        if text(&app) != want || at(&app) != cursor || !reg_ok {
            misses.push(format!(
                "{before:?} at {:?} {steps:?}: got {:?} {:?} {got_reg:?}, want {want:?} {cursor:?} {reg:?} ({})",
                (l, c),
                text(&app),
                at(&app),
                flash(&app),
            ));
        }
    }
    assert!(misses.is_empty(), "{} misses:\n{}", misses.len(), misses.join("\n"));
}

#[test]
fn the_parser_reads_vims_addresses() {
    use range::{address, parse, Address, Base, Range};
    let n = |n, offset| Address { base: Base::Number(n), offset };
    let cur = |offset| Address { base: Base::Current, offset };
    assert_eq!(address("3 + 2"), (Some(n(3, 3)), ""));
    assert_eq!(address("+"), (Some(cur(1)), ""));
    assert_eq!(address("--3d"), (Some(cur(-4)), "d"));
    assert_eq!(address("$-1 x"), (Some(Address { base: Base::Last, offset: -1 }), " x"));
    assert_eq!(address("'<+1"), (Some(Address { base: Base::SelStart, offset: 1 }), ""));
    assert_eq!(address("d"), (None, "d"));
    assert_eq!(parse(" %  d"), (Some(Range::Whole), "d"));
    assert_eq!(parse("'<,'>d"), (Some(Range::Two(Address { base: Base::SelStart, offset: 0 }, Address { base: Base::SelEnd, offset: 0 })), "d"));
    assert_eq!(parse(",5 y"), (Some(Range::Two(cur(0), n(5, 0))), "y"));
    assert_eq!(parse("w x"), (None, "w x"));
    assert_eq!(range::prefix_len(" 1,$ w"), 5);
    assert_eq!(range::prefix_len("w"), 0);
}

/// A backward range is turned round; out of range addresses clamp, where
/// Neovim refuses `:-100` and `:t 99`.
#[test]
fn backward_and_out_of_range_lines_clamp() {
    let t = "a\nb\nc\nd\ne\n";
    let mut app = app_with(t);
    ex(&mut app, "4,2d");
    assert_eq!(text(&app), "a\ne\n");
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(3, 0);
    ex(&mut app, "-100");
    assert_eq!(at(&app), (0, 0));
    ex(&mut app, "5,2");
    assert_eq!(at(&app), (4, 0), "a backward range goes to its larger line");
    ex(&mut app, "2,99999999999999999999y");
    assert_eq!(app.registers.get(&'"').map(|r| r.text.as_str()), Some("b\nc\nd\ne\n"), "a number too big is the end");
    ex(&mut app, "1,9d");
    assert_eq!(text(&app), "");
    let mut app = app_with(t);
    ex(&mut app, "t 99");
    assert_eq!(text(&app), "a\nb\nc\nd\ne\na\n");
    ex(&mut app, "m -9");
    assert_eq!(text(&app), "a\na\nb\nc\nd\ne\n");
}

#[test]
fn moving_lines_below_one_of_themselves_is_refused() {
    let t = "a\nb\nc\nd\ne\n";
    for cmd in ["2,3m 2", "2,4m 3"] {
        let mut app = app_with(t);
        app.buffer.cursor = Cursor::new(3, 0);
        ex(&mut app, cmd);
        assert_eq!(text(&app), t, ":{cmd}");
        assert_eq!(at(&app), (3, 0), ":{cmd}");
        assert_eq!(flash(&app), "can't move lines below one of themselves");
    }
}

#[test]
fn a_bad_target_or_count_says_so_and_changes_nothing() {
    let t = "a\nb\nc\n";
    for (cmd, said) in [
        ("m", ":m needs a line to go below: :m 0 is the top, :m $ the end"),
        ("t", ":t needs a line to go below: :t 0 is the top, :t $ the end"),
        ("m x", ":m: not a line: x"),
        ("t 1 x", ":t: not a line: 1 x"),
        ("d 0", "a count must be 1 or more"),
        ("> 0", "a count must be 1 or more"),
        ("'<,'>d", "'< and '> are the lines of a Visual selection: select, then :"),
    ] {
        let mut app = app_with(t);
        ex(&mut app, cmd);
        assert_eq!(text(&app), t, ":{cmd}");
        assert_eq!(flash(&app), said, ":{cmd}");
    }
    // Not parsed at all: still not a command.
    for cmd in ["3x", "$x", "1,5q", "d x y", "'a", "2,3e foo", "%fixlist!"] {
        let mut app = app_with(t);
        app.buffer.cursor = Cursor::new(1, 0);
        ex(&mut app, cmd);
        assert_eq!(text(&app), t, ":{cmd}");
        assert_eq!(at(&app), (1, 0), ":{cmd}");
        assert!(flash(&app).starts_with("not a command"), ":{cmd}: {}", flash(&app));
    }
}

#[test]
fn a_register_and_a_count_after_d_and_y() {
    let mut app = app_with("a\nb\nc\nd\n");
    ex(&mut app, "2d a 2");
    assert_eq!(text(&app), "a\nd\n");
    assert_eq!(app.registers.get(&'a').map(|r| r.text.as_str()), Some("b\nc\n"));
    ex(&mut app, "%y A");
    assert_eq!(app.registers.get(&'a').map(|r| r.text.as_str()), Some("b\nc\na\nd\n"));
    feed(&mut app, "G\"ap");
    assert_eq!(text(&app), "a\nd\nb\nc\na\nd\n");
}

/// `:d` fills the delete ring and `:y` the yank register, as `dd` and `yy`,
/// and `p` puts either back as lines.
#[test]
fn d_and_y_fill_the_registers_dd_and_yy_fill() {
    let mut app = app_with("a\nb\nc\n");
    ex(&mut app, "1y");
    ex(&mut app, "2d");
    assert_eq!(app.registers.get(&'0').map(|r| r.text.as_str()), Some("a\n"));
    assert_eq!(app.registers.get(&'1').map(|r| r.text.as_str()), Some("b\n"));
    feed(&mut app, "p");
    assert_eq!(text(&app), "a\nc\nb\n");
    feed(&mut app, "\"0P");
    assert_eq!(text(&app), "a\nc\na\nb\n");
}

/// A closed fold at either end of the range is taken whole, as Neovim
/// takes it; a `:m` or `:t` target inside one is not.
#[test]
fn a_closed_fold_is_taken_whole() {
    let t = "- a\n  - a1\n  - a2\n- b\n- c\n";
    let mut app = app_with(t);
    app.term_size = (60, 16);
    feed(&mut app, "zc");
    ex(&mut app, "d");
    assert_eq!(text(&app), "- b\n- c\n");
    feed(&mut app, "u");
    feed(&mut app, "gg");
    feed(&mut app, "zc");
    ex(&mut app, "2y");
    assert_eq!(app.registers.get(&'"').map(|r| r.text.as_str()), Some("- a\n  - a1\n  - a2\n"));
    ex(&mut app, "m $");
    assert_eq!(text(&app), "- b\n- c\n- a\n  - a1\n  - a2\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), t);
    ex(&mut app, "5t 2");
    assert_eq!(text(&app), "- a\n  - a1\n- c\n  - a2\n- b\n- c\n");
}

/// `:` from Visual starts the line with `'<,'>`: the lines of the
/// selection, whatever its kind. Running it ends Visual.
#[test]
fn colon_from_visual_names_the_selected_lines() {
    let t = "a\nbb\ncc\ndd\ne\n";
    for v in ["Vj", "vj", "lvjh"] {
        let mut app = app_with(t);
        feed(&mut app, "j");
        feed(&mut app, v);
        feed(&mut app, ":");
        assert_eq!(app.mode, Mode::Command("'<,'>".into()), "{v}");
        feed(&mut app, "d");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(text(&app), "a\ndd\ne\n", "{v}");
        assert_eq!(app.mode, Mode::Normal, "{v}");
        assert_eq!(app.anchor, None, "{v}");
    }
    // Backwards, and with the lines gone round a target.
    let mut app = app_with(t);
    feed(&mut app, "3GVk");
    ex(&mut app, "m $");
    assert_eq!(text(&app), "a\ndd\ne\nbb\ncc\n");
    assert_eq!(at(&app), (4, 0));
    // Esc leaves it, and `:` from Normal starts empty again.
    feed(&mut app, "Vj:");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    feed(&mut app, ":");
    assert_eq!(app.mode, Mode::Command(String::new()));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    // The prefilled range taken back out: the cursor's line, as vim.
    let mut app = app_with(t);
    feed(&mut app, "Vj:");
    for _ in 0..5 {
        key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    }
    feed(&mut app, "d");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(text(&app), "a\ncc\ndd\ne\n");
}

/// `.` does not repeat a `:` command, in Neovim: it repeats the change
/// before it.
#[test]
fn dot_keeps_the_change_before_a_line_command() {
    let mut app = app_with("a\nb\nc\nd\ne\n");
    feed(&mut app, "x");
    ex(&mut app, "3d");
    ex(&mut app, "2t 0");
    ex(&mut app, ">");
    feed(&mut app, "jj.");
    assert_eq!(text(&app), "    b\n\n\nd\ne\n");
    assert_eq!(at(&app), (2, 0));
}

#[test]
fn read_only_jumps_and_yanks_but_refuses_edits() {
    let t = "a\nb\nc\nd\n";
    for cmd in ["d", "2,3d", ">", "<", "m 0", "t 0", "fixlist", "%fixlist"] {
        let mut app = app_with(t);
        app.set_view(true);
        ex(&mut app, cmd);
        assert_eq!(text(&app), t, ":{cmd}");
        assert!(flash(&app).starts_with("read-only"), ":{cmd}: {}", flash(&app));
    }
    let mut app = app_with(t);
    app.set_view(true);
    ex(&mut app, "2,3y");
    assert_eq!(app.registers.get(&'"').map(|r| r.text.as_str()), Some("b\nc\n"));
    ex(&mut app, "3");
    assert_eq!(app.buffer.cursor.line, 2);
    // The refusal offers `i`, which then edits.
    ex(&mut app, "d");
    feed(&mut app, "i");
    assert!(!app.view);
}

#[test]
fn w_with_a_range_writes_only_those_lines() {
    let dir = std::env::temp_dir().join(format!("shoin-range-w-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let own = dir.join("own.md");
    std::fs::write(&own, "a\nb\nc\nd").unwrap();
    let mut app = App::new(Default::default(), Some(own.clone()), None).unwrap();
    let part = dir.join("part.md");
    let p = part.to_string_lossy().into_owned();
    ex(&mut app, &format!("2,3w {p}"));
    assert_eq!(std::fs::read_to_string(&part).unwrap(), "b\nc\n");
    assert!(flash(&app).contains("written · 2 lines"), "{}", flash(&app));
    // Taken: refused without `!`.
    ex(&mut app, &format!("1w {p}"));
    assert!(flash(&app).contains("exists"), "{}", flash(&app));
    assert_eq!(std::fs::read_to_string(&part).unwrap(), "b\nc\n");
    ex(&mut app, &format!("1w! {p}"));
    assert_eq!(std::fs::read_to_string(&part).unwrap(), "a\n");
    // The last line, as the file has it: no final newline here.
    ex(&mut app, &format!("$w! {p}"));
    assert_eq!(std::fs::read_to_string(&part).unwrap(), "d");
    // No path, or this buffer's own file: an error, nothing written.
    for cmd in ["%w".to_string(), "2,3w!".into(), format!("%w! {}", own.display())] {
        ex(&mut app, &cmd);
        assert!(matches!(app.flash.as_ref().map(|f| &f.kind), Some(FlashKind::Error)), ":{cmd}: {}", flash(&app));
    }
    assert_eq!(std::fs::read_to_string(&own).unwrap(), "a\nb\nc\nd");
    assert!(!app.buffer.modified);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fixlist_with_a_range_re_indents_each_list_it_touches() {
    let t = "10. j\n   - y\n\npara\n\n10. k\n   - z\n";
    let mut app = app_with(t);
    ex(&mut app, "%fixlist");
    assert_eq!(text(&app), t.replace("   - ", "    - "));
    feed(&mut app, "u");
    assert_eq!(text(&app), t, "one undo step");
    ex(&mut app, "6,7fixlist");
    assert_eq!(text(&app), "10. j\n   - y\n\npara\n\n10. k\n    - z\n");
}

/// Tab completes after a range as it does without one.
#[test]
fn tab_completes_after_a_range() {
    let mut app = app_with("a\n");
    feed(&mut app, "Vj:fi");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Command("'<,'>fixlist".into()));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    feed(&mut app, ":3,5de");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Command("3,5delete".into()));
}

/// `:d`, `:m` and `:t` at both ends of a file, with and without a final
/// newline, LF and CRLF: the file `:w` then writes and the cursor, as
/// Neovim's (`nofixeol`, as Shoin's `final_newline = "preserve"`); `u`
/// gives the text back and `Ctrl-r` the change. A blank line left last, or
/// copied there, stays a line. Deleting every line writes an empty file.
const EOF_CASES: &[(&str, &str, &str, (usize, usize))] = &[
    ("a\n\nb", "1d", "\nb", (0, 0)),
    ("a\n\nb", "$d", "a\n", (1, 0)),
    ("a\n\nb", "1,2d", "b", (0, 0)),
    ("a\n\nb", "2,$d", "a", (0, 0)),
    ("a\n\nb", "%d", "", (0, 0)),
    ("a\n\nb", "1m $", "\nb\na", (2, 0)),
    ("a\n\nb", "$m 0", "b\na\n", (0, 0)),
    ("a\n\nb", "2m $", "a\nb\n", (2, 0)),
    ("a\n\nb", "2m 0", "\na\nb", (0, 0)),
    ("a\n\nb", "1,2m $", "b\na\n", (2, 0)),
    ("a\n\nb", "2,$m 0", "\nb\na", (1, 0)),
    ("a\n\nb", "1t $", "a\n\nb\na", (3, 0)),
    ("a\n\nb", "$t 0", "b\na\n\nb", (0, 0)),
    ("a\n\nb", "2t $", "a\n\nb\n", (3, 0)),
    ("a\n\nb", "$t $", "a\n\nb\nb", (3, 0)),
    ("a\n\nb", "1t 0", "a\na\n\nb", (0, 0)),
    ("a\n\nb", "1,2t $", "a\n\nb\na\n", (4, 0)),
    ("a\n\nb\n", "1d", "\nb\n", (0, 0)),
    ("a\n\nb\n", "$d", "a\n\n", (1, 0)),
    ("a\n\nb\n", "1,2d", "b\n", (0, 0)),
    ("a\n\nb\n", "2,$d", "a\n", (0, 0)),
    ("a\n\nb\n", "%d", "", (0, 0)),
    ("a\n\nb\n", "1m $", "\nb\na\n", (2, 0)),
    ("a\n\nb\n", "$m 0", "b\na\n\n", (0, 0)),
    ("a\n\nb\n", "2m $", "a\nb\n\n", (2, 0)),
    ("a\n\nb\n", "2m 0", "\na\nb\n", (0, 0)),
    ("a\n\nb\n", "1,2m $", "b\na\n\n", (2, 0)),
    ("a\n\nb\n", "2,$m 0", "\nb\na\n", (1, 0)),
    ("a\n\nb\n", "1t $", "a\n\nb\na\n", (3, 0)),
    ("a\n\nb\n", "$t 0", "b\na\n\nb\n", (0, 0)),
    ("a\n\nb\n", "2t $", "a\n\nb\n\n", (3, 0)),
    ("a\n\nb\n", "$t $", "a\n\nb\nb\n", (3, 0)),
    ("a\n\nb\n", "1t 0", "a\na\n\nb\n", (0, 0)),
    ("a\n\nb\n", "1,2t $", "a\n\nb\na\n\n", (4, 0)),
    ("\na\nb", "1d", "a\nb", (0, 0)),
    ("\na\nb", "$d", "\na", (1, 0)),
    ("\na\nb", "1,2d", "b", (0, 0)),
    ("\na\nb", "2,$d", "", (0, 0)),
    ("\na\nb", "%d", "", (0, 0)),
    ("\na\nb", "1m $", "a\nb\n", (2, 0)),
    ("\na\nb", "$m 0", "b\n\na", (0, 0)),
    ("\na\nb", "2m $", "\nb\na", (2, 0)),
    ("\na\nb", "2m 0", "a\n\nb", (0, 0)),
    ("\na\nb", "1,2m $", "b\n\na", (2, 0)),
    ("\na\nb", "2,$m 0", "a\nb\n", (1, 0)),
    ("\na\nb", "1t $", "\na\nb\n", (3, 0)),
    ("\na\nb", "$t 0", "b\n\na\nb", (0, 0)),
    ("\na\nb", "2t $", "\na\nb\na", (3, 0)),
    ("\na\nb", "$t $", "\na\nb\nb", (3, 0)),
    ("\na\nb", "1t 0", "\n\na\nb", (0, 0)),
    ("\na\nb", "1,2t $", "\na\nb\n\na", (4, 0)),
    ("\na\nb\n", "1d", "a\nb\n", (0, 0)),
    ("\na\nb\n", "$d", "\na\n", (1, 0)),
    ("\na\nb\n", "1,2d", "b\n", (0, 0)),
    ("\na\nb\n", "2,$d", "\n", (0, 0)),
    ("\na\nb\n", "1m $", "a\nb\n\n", (2, 0)),
    ("\na\nb\n", "$m 0", "b\n\na\n", (0, 0)),
    ("\na\nb\n", "2m $", "\nb\na\n", (2, 0)),
    ("\na\nb\n", "2m 0", "a\n\nb\n", (0, 0)),
    ("\na\nb\n", "1,2m $", "b\n\na\n", (2, 0)),
    ("\na\nb\n", "2,$m 0", "a\nb\n\n", (1, 0)),
    ("\na\nb\n", "1t $", "\na\nb\n\n", (3, 0)),
    ("\na\nb\n", "$t 0", "b\n\na\nb\n", (0, 0)),
    ("\na\nb\n", "2t $", "\na\nb\na\n", (3, 0)),
    ("\na\nb\n", "$t $", "\na\nb\nb\n", (3, 0)),
    ("\na\nb\n", "1t 0", "\n\na\nb\n", (0, 0)),
    ("\na\nb\n", "1,2t $", "\na\nb\n\na\n", (4, 0)),
    ("a\nb\n\n", "1d", "b\n\n", (0, 0)),
    ("a\nb\n\n", "$d", "a\nb\n", (1, 0)),
    ("a\nb\n\n", "1,2d", "\n", (0, 0)),
    ("a\nb\n\n", "2,$d", "a\n", (0, 0)),
    ("a\nb\n\n", "1m $", "b\n\na\n", (2, 0)),
    ("a\nb\n\n", "$m 0", "\na\nb\n", (0, 0)),
    ("a\nb\n\n", "2m $", "a\n\nb\n", (2, 0)),
    ("a\nb\n\n", "2m 0", "b\na\n\n", (0, 0)),
    ("a\nb\n\n", "1,2m $", "\na\nb\n", (2, 0)),
    ("a\nb\n\n", "2,$m 0", "b\n\na\n", (1, 0)),
    ("a\nb\n\n", "1t $", "a\nb\n\na\n", (3, 0)),
    ("a\nb\n\n", "$t 0", "\na\nb\n\n", (0, 0)),
    ("a\nb\n\n", "2t $", "a\nb\n\nb\n", (3, 0)),
    ("a\nb\n\n", "$t $", "a\nb\n\n\n", (3, 0)),
    ("a\nb\n\n", "1t 0", "a\na\nb\n\n", (0, 0)),
    ("a\nb\n\n", "1,2t $", "a\nb\n\na\nb\n", (4, 0)),
    ("a\n\n\nb", "1d", "\n\nb", (0, 0)),
    ("a\n\n\nb", "$d", "a\n\n", (2, 0)),
    ("a\n\n\nb", "1,2d", "\nb", (0, 0)),
    ("a\n\n\nb", "2,$d", "a", (0, 0)),
    ("a\n\n\nb", "%d", "", (0, 0)),
    ("a\n\n\nb", "1m $", "\n\nb\na", (3, 0)),
    ("a\n\n\nb", "$m 0", "b\na\n\n", (0, 0)),
    ("a\n\n\nb", "2m $", "a\n\nb\n", (3, 0)),
    ("a\n\n\nb", "2m 0", "\na\n\nb", (0, 0)),
    ("a\n\n\nb", "1,2m $", "\nb\na\n", (3, 0)),
    ("a\n\n\nb", "2,$m 0", "\n\nb\na", (2, 0)),
    ("a\n\n\nb", "1t $", "a\n\n\nb\na", (4, 0)),
    ("a\n\n\nb", "$t 0", "b\na\n\n\nb", (0, 0)),
    ("a\n\n\nb", "2t $", "a\n\n\nb\n", (4, 0)),
    ("a\n\n\nb", "$t $", "a\n\n\nb\nb", (4, 0)),
    ("a\n\n\nb", "1t 0", "a\na\n\n\nb", (0, 0)),
    ("a\n\n\nb", "1,2t $", "a\n\n\nb\na\n", (5, 0)),
    ("a\nb", "1d", "b", (0, 0)),
    ("a\nb", "$d", "a", (0, 0)),
    ("a\nb", "1,2d", "", (0, 0)),
    ("a\nb", "2,$d", "a", (0, 0)),
    ("a\nb", "%d", "", (0, 0)),
    ("a\nb", "1m $", "b\na", (1, 0)),
    ("a\nb", "$m 0", "b\na", (0, 0)),
    ("a\nb", "2m $", "a\nb", (1, 0)),
    ("a\nb", "2m 0", "b\na", (0, 0)),
    ("a\nb", "1,2m $", "a\nb", (1, 0)),
    ("a\nb", "2,$m 0", "b\na", (0, 0)),
    ("a\nb", "1t $", "a\nb\na", (2, 0)),
    ("a\nb", "$t 0", "b\na\nb", (0, 0)),
    ("a\nb", "2t $", "a\nb\nb", (2, 0)),
    ("a\nb", "$t $", "a\nb\nb", (2, 0)),
    ("a\nb", "1t 0", "a\na\nb", (0, 0)),
    ("a\nb", "1,2t $", "a\nb\na\nb", (3, 0)),
    ("a\nb\n", "1d", "b\n", (0, 0)),
    ("a\nb\n", "$d", "a\n", (0, 0)),
    ("a\nb\n", "2,$d", "a\n", (0, 0)),
    ("a\nb\n", "1m $", "b\na\n", (1, 0)),
    ("a\nb\n", "$m 0", "b\na\n", (0, 0)),
    ("a\nb\n", "2m $", "a\nb\n", (1, 0)),
    ("a\nb\n", "2m 0", "b\na\n", (0, 0)),
    ("a\nb\n", "1,2m $", "a\nb\n", (1, 0)),
    ("a\nb\n", "2,$m 0", "b\na\n", (0, 0)),
    ("a\nb\n", "1t $", "a\nb\na\n", (2, 0)),
    ("a\nb\n", "$t 0", "b\na\nb\n", (0, 0)),
    ("a\nb\n", "2t $", "a\nb\nb\n", (2, 0)),
    ("a\nb\n", "$t $", "a\nb\nb\n", (2, 0)),
    ("a\nb\n", "1t 0", "a\na\nb\n", (0, 0)),
    ("a\nb\n", "1,2t $", "a\nb\na\nb\n", (3, 0)),
    ("\n\na", "1d", "\na", (0, 0)),
    ("\n\na", "$d", "\n", (1, 0)),
    ("\n\na", "1,2d", "a", (0, 0)),
    ("\n\na", "2,$d", "", (0, 0)),
    ("\n\na", "%d", "", (0, 0)),
    ("\n\na", "1m $", "\na\n", (2, 0)),
    ("\n\na", "$m 0", "a\n\n", (0, 0)),
    ("\n\na", "2m $", "\na\n", (2, 0)),
    ("\n\na", "2m 0", "\n\na", (0, 0)),
    ("\n\na", "1,2m $", "a\n\n", (2, 0)),
    ("\n\na", "2,$m 0", "\na\n", (1, 0)),
    ("\n\na", "1t $", "\n\na\n", (3, 0)),
    ("\n\na", "$t 0", "a\n\n\na", (0, 0)),
    ("\n\na", "2t $", "\n\na\n", (3, 0)),
    ("\n\na", "$t $", "\n\na\na", (3, 0)),
    ("\n\na", "1t 0", "\n\n\na", (0, 0)),
    ("\n\na", "1,2t $", "\n\na\n\n", (4, 0)),
    ("a\n\n", "1d", "\n", (0, 0)),
    ("a\n\n", "$d", "a\n", (0, 0)),
    ("a\n\n", "2,$d", "a\n", (0, 0)),
    ("a\n\n", "1m $", "\na\n", (1, 0)),
    ("a\n\n", "$m 0", "\na\n", (0, 0)),
    ("a\n\n", "2m $", "a\n\n", (1, 0)),
    ("a\n\n", "2m 0", "\na\n", (0, 0)),
    ("a\n\n", "1,2m $", "a\n\n", (1, 0)),
    ("a\n\n", "2,$m 0", "\na\n", (0, 0)),
    ("a\n\n", "1t $", "a\n\na\n", (2, 0)),
    ("a\n\n", "$t 0", "\na\n\n", (0, 0)),
    ("a\n\n", "2t $", "a\n\n\n", (2, 0)),
    ("a\n\n", "$t $", "a\n\n\n", (2, 0)),
    ("a\n\n", "1t 0", "a\na\n\n", (0, 0)),
    ("a\n\n", "1,2t $", "a\n\na\n\n", (3, 0)),
    ("a\r\n\r\nb", "1d", "\r\nb", (0, 0)),
    ("a\r\n\r\nb", "$d", "a\r\n", (1, 0)),
    ("a\r\n\r\nb", "1,2d", "b", (0, 0)),
    ("a\r\n\r\nb", "2,$d", "a", (0, 0)),
    ("a\r\n\r\nb", "%d", "", (0, 0)),
    ("a\r\n\r\nb", "1m $", "\r\nb\r\na", (2, 0)),
    ("a\r\n\r\nb", "$m 0", "b\r\na\r\n", (0, 0)),
    ("a\r\n\r\nb", "2m $", "a\r\nb\r\n", (2, 0)),
    ("a\r\n\r\nb", "2m 0", "\r\na\r\nb", (0, 0)),
    ("a\r\n\r\nb", "1,2m $", "b\r\na\r\n", (2, 0)),
    ("a\r\n\r\nb", "2,$m 0", "\r\nb\r\na", (1, 0)),
    ("a\r\n\r\nb", "1t $", "a\r\n\r\nb\r\na", (3, 0)),
    ("a\r\n\r\nb", "$t 0", "b\r\na\r\n\r\nb", (0, 0)),
    ("a\r\n\r\nb", "2t $", "a\r\n\r\nb\r\n", (3, 0)),
    ("a\r\n\r\nb", "$t $", "a\r\n\r\nb\r\nb", (3, 0)),
    ("a\r\n\r\nb", "1t 0", "a\r\na\r\n\r\nb", (0, 0)),
    ("a\r\n\r\nb", "1,2t $", "a\r\n\r\nb\r\na\r\n", (4, 0)),
    ("a\r\n\r\nb\r\n", "1d", "\r\nb\r\n", (0, 0)),
    ("a\r\n\r\nb\r\n", "$d", "a\r\n\r\n", (1, 0)),
    ("a\r\n\r\nb\r\n", "1,2d", "b\r\n", (0, 0)),
    ("a\r\n\r\nb\r\n", "2,$d", "a\r\n", (0, 0)),
    ("a\r\n\r\nb\r\n", "1m $", "\r\nb\r\na\r\n", (2, 0)),
    ("a\r\n\r\nb\r\n", "$m 0", "b\r\na\r\n\r\n", (0, 0)),
    ("a\r\n\r\nb\r\n", "2m $", "a\r\nb\r\n\r\n", (2, 0)),
    ("a\r\n\r\nb\r\n", "2m 0", "\r\na\r\nb\r\n", (0, 0)),
    ("a\r\n\r\nb\r\n", "1,2m $", "b\r\na\r\n\r\n", (2, 0)),
    ("a\r\n\r\nb\r\n", "2,$m 0", "\r\nb\r\na\r\n", (1, 0)),
    ("a\r\n\r\nb\r\n", "1t $", "a\r\n\r\nb\r\na\r\n", (3, 0)),
    ("a\r\n\r\nb\r\n", "$t 0", "b\r\na\r\n\r\nb\r\n", (0, 0)),
    ("a\r\n\r\nb\r\n", "2t $", "a\r\n\r\nb\r\n\r\n", (3, 0)),
    ("a\r\n\r\nb\r\n", "$t $", "a\r\n\r\nb\r\nb\r\n", (3, 0)),
    ("a\r\n\r\nb\r\n", "1t 0", "a\r\na\r\n\r\nb\r\n", (0, 0)),
    ("a\r\n\r\nb\r\n", "1,2t $", "a\r\n\r\nb\r\na\r\n\r\n", (4, 0)),
    ("\r\na\r\nb", "1d", "a\r\nb", (0, 0)),
    ("\r\na\r\nb", "$d", "\r\na", (1, 0)),
    ("\r\na\r\nb", "1,2d", "b", (0, 0)),
    ("\r\na\r\nb", "2,$d", "", (0, 0)),
    ("\r\na\r\nb", "%d", "", (0, 0)),
    ("\r\na\r\nb", "1m $", "a\r\nb\r\n", (2, 0)),
    ("\r\na\r\nb", "$m 0", "b\r\n\r\na", (0, 0)),
    ("\r\na\r\nb", "2m $", "\r\nb\r\na", (2, 0)),
    ("\r\na\r\nb", "2m 0", "a\r\n\r\nb", (0, 0)),
    ("\r\na\r\nb", "1,2m $", "b\r\n\r\na", (2, 0)),
    ("\r\na\r\nb", "2,$m 0", "a\r\nb\r\n", (1, 0)),
    ("\r\na\r\nb", "1t $", "\r\na\r\nb\r\n", (3, 0)),
    ("\r\na\r\nb", "$t 0", "b\r\n\r\na\r\nb", (0, 0)),
    ("\r\na\r\nb", "2t $", "\r\na\r\nb\r\na", (3, 0)),
    ("\r\na\r\nb", "$t $", "\r\na\r\nb\r\nb", (3, 0)),
    ("\r\na\r\nb", "1t 0", "\r\n\r\na\r\nb", (0, 0)),
    ("\r\na\r\nb", "1,2t $", "\r\na\r\nb\r\n\r\na", (4, 0)),
    ("\r\na\r\nb\r\n", "1d", "a\r\nb\r\n", (0, 0)),
    ("\r\na\r\nb\r\n", "$d", "\r\na\r\n", (1, 0)),
    ("\r\na\r\nb\r\n", "1,2d", "b\r\n", (0, 0)),
    ("\r\na\r\nb\r\n", "2,$d", "\r\n", (0, 0)),
    ("\r\na\r\nb\r\n", "1m $", "a\r\nb\r\n\r\n", (2, 0)),
    ("\r\na\r\nb\r\n", "$m 0", "b\r\n\r\na\r\n", (0, 0)),
    ("\r\na\r\nb\r\n", "2m $", "\r\nb\r\na\r\n", (2, 0)),
    ("\r\na\r\nb\r\n", "2m 0", "a\r\n\r\nb\r\n", (0, 0)),
    ("\r\na\r\nb\r\n", "1,2m $", "b\r\n\r\na\r\n", (2, 0)),
    ("\r\na\r\nb\r\n", "2,$m 0", "a\r\nb\r\n\r\n", (1, 0)),
    ("\r\na\r\nb\r\n", "1t $", "\r\na\r\nb\r\n\r\n", (3, 0)),
    ("\r\na\r\nb\r\n", "$t 0", "b\r\n\r\na\r\nb\r\n", (0, 0)),
    ("\r\na\r\nb\r\n", "2t $", "\r\na\r\nb\r\na\r\n", (3, 0)),
    ("\r\na\r\nb\r\n", "$t $", "\r\na\r\nb\r\nb\r\n", (3, 0)),
    ("\r\na\r\nb\r\n", "1t 0", "\r\n\r\na\r\nb\r\n", (0, 0)),
    ("\r\na\r\nb\r\n", "1,2t $", "\r\na\r\nb\r\n\r\na\r\n", (4, 0)),
    ("a\r\nb\r\n\r\n", "1d", "b\r\n\r\n", (0, 0)),
    ("a\r\nb\r\n\r\n", "$d", "a\r\nb\r\n", (1, 0)),
    ("a\r\nb\r\n\r\n", "1,2d", "\r\n", (0, 0)),
    ("a\r\nb\r\n\r\n", "2,$d", "a\r\n", (0, 0)),
    ("a\r\nb\r\n\r\n", "1m $", "b\r\n\r\na\r\n", (2, 0)),
    ("a\r\nb\r\n\r\n", "$m 0", "\r\na\r\nb\r\n", (0, 0)),
    ("a\r\nb\r\n\r\n", "2m $", "a\r\n\r\nb\r\n", (2, 0)),
    ("a\r\nb\r\n\r\n", "2m 0", "b\r\na\r\n\r\n", (0, 0)),
    ("a\r\nb\r\n\r\n", "1,2m $", "\r\na\r\nb\r\n", (2, 0)),
    ("a\r\nb\r\n\r\n", "2,$m 0", "b\r\n\r\na\r\n", (1, 0)),
    ("a\r\nb\r\n\r\n", "1t $", "a\r\nb\r\n\r\na\r\n", (3, 0)),
    ("a\r\nb\r\n\r\n", "$t 0", "\r\na\r\nb\r\n\r\n", (0, 0)),
    ("a\r\nb\r\n\r\n", "2t $", "a\r\nb\r\n\r\nb\r\n", (3, 0)),
    ("a\r\nb\r\n\r\n", "$t $", "a\r\nb\r\n\r\n\r\n", (3, 0)),
    ("a\r\nb\r\n\r\n", "1t 0", "a\r\na\r\nb\r\n\r\n", (0, 0)),
    ("a\r\nb\r\n\r\n", "1,2t $", "a\r\nb\r\n\r\na\r\nb\r\n", (4, 0)),
    ("a\r\n\r\n\r\nb", "1d", "\r\n\r\nb", (0, 0)),
    ("a\r\n\r\n\r\nb", "$d", "a\r\n\r\n", (2, 0)),
    ("a\r\n\r\n\r\nb", "1,2d", "\r\nb", (0, 0)),
    ("a\r\n\r\n\r\nb", "2,$d", "a", (0, 0)),
    ("a\r\n\r\n\r\nb", "%d", "", (0, 0)),
    ("a\r\n\r\n\r\nb", "1m $", "\r\n\r\nb\r\na", (3, 0)),
    ("a\r\n\r\n\r\nb", "$m 0", "b\r\na\r\n\r\n", (0, 0)),
    ("a\r\n\r\n\r\nb", "2m $", "a\r\n\r\nb\r\n", (3, 0)),
    ("a\r\n\r\n\r\nb", "2m 0", "\r\na\r\n\r\nb", (0, 0)),
    ("a\r\n\r\n\r\nb", "1,2m $", "\r\nb\r\na\r\n", (3, 0)),
    ("a\r\n\r\n\r\nb", "2,$m 0", "\r\n\r\nb\r\na", (2, 0)),
    ("a\r\n\r\n\r\nb", "1t $", "a\r\n\r\n\r\nb\r\na", (4, 0)),
    ("a\r\n\r\n\r\nb", "$t 0", "b\r\na\r\n\r\n\r\nb", (0, 0)),
    ("a\r\n\r\n\r\nb", "2t $", "a\r\n\r\n\r\nb\r\n", (4, 0)),
    ("a\r\n\r\n\r\nb", "$t $", "a\r\n\r\n\r\nb\r\nb", (4, 0)),
    ("a\r\n\r\n\r\nb", "1t 0", "a\r\na\r\n\r\n\r\nb", (0, 0)),
    ("a\r\n\r\n\r\nb", "1,2t $", "a\r\n\r\n\r\nb\r\na\r\n", (5, 0)),
    ("a\r\nb", "1d", "b", (0, 0)),
    ("a\r\nb", "$d", "a", (0, 0)),
    ("a\r\nb", "1,2d", "", (0, 0)),
    ("a\r\nb", "2,$d", "a", (0, 0)),
    ("a\r\nb", "%d", "", (0, 0)),
    ("a\r\nb", "1m $", "b\r\na", (1, 0)),
    ("a\r\nb", "$m 0", "b\r\na", (0, 0)),
    ("a\r\nb", "2m $", "a\r\nb", (1, 0)),
    ("a\r\nb", "2m 0", "b\r\na", (0, 0)),
    ("a\r\nb", "1,2m $", "a\r\nb", (1, 0)),
    ("a\r\nb", "2,$m 0", "b\r\na", (0, 0)),
    ("a\r\nb", "1t $", "a\r\nb\r\na", (2, 0)),
    ("a\r\nb", "$t 0", "b\r\na\r\nb", (0, 0)),
    ("a\r\nb", "2t $", "a\r\nb\r\nb", (2, 0)),
    ("a\r\nb", "$t $", "a\r\nb\r\nb", (2, 0)),
    ("a\r\nb", "1t 0", "a\r\na\r\nb", (0, 0)),
    ("a\r\nb", "1,2t $", "a\r\nb\r\na\r\nb", (3, 0)),
    ("a\r\nb\r\n", "1d", "b\r\n", (0, 0)),
    ("a\r\nb\r\n", "$d", "a\r\n", (0, 0)),
    ("a\r\nb\r\n", "2,$d", "a\r\n", (0, 0)),
    ("a\r\nb\r\n", "1m $", "b\r\na\r\n", (1, 0)),
    ("a\r\nb\r\n", "$m 0", "b\r\na\r\n", (0, 0)),
    ("a\r\nb\r\n", "2m $", "a\r\nb\r\n", (1, 0)),
    ("a\r\nb\r\n", "2m 0", "b\r\na\r\n", (0, 0)),
    ("a\r\nb\r\n", "1,2m $", "a\r\nb\r\n", (1, 0)),
    ("a\r\nb\r\n", "2,$m 0", "b\r\na\r\n", (0, 0)),
    ("a\r\nb\r\n", "1t $", "a\r\nb\r\na\r\n", (2, 0)),
    ("a\r\nb\r\n", "$t 0", "b\r\na\r\nb\r\n", (0, 0)),
    ("a\r\nb\r\n", "2t $", "a\r\nb\r\nb\r\n", (2, 0)),
    ("a\r\nb\r\n", "$t $", "a\r\nb\r\nb\r\n", (2, 0)),
    ("a\r\nb\r\n", "1t 0", "a\r\na\r\nb\r\n", (0, 0)),
    ("a\r\nb\r\n", "1,2t $", "a\r\nb\r\na\r\nb\r\n", (3, 0)),
    ("\r\n\r\na", "1d", "\r\na", (0, 0)),
    ("\r\n\r\na", "$d", "\r\n", (1, 0)),
    ("\r\n\r\na", "1,2d", "a", (0, 0)),
    ("\r\n\r\na", "2,$d", "", (0, 0)),
    ("\r\n\r\na", "%d", "", (0, 0)),
    ("\r\n\r\na", "1m $", "\r\na\r\n", (2, 0)),
    ("\r\n\r\na", "$m 0", "a\r\n\r\n", (0, 0)),
    ("\r\n\r\na", "2m $", "\r\na\r\n", (2, 0)),
    ("\r\n\r\na", "2m 0", "\r\n\r\na", (0, 0)),
    ("\r\n\r\na", "1,2m $", "a\r\n\r\n", (2, 0)),
    ("\r\n\r\na", "2,$m 0", "\r\na\r\n", (1, 0)),
    ("\r\n\r\na", "1t $", "\r\n\r\na\r\n", (3, 0)),
    ("\r\n\r\na", "$t 0", "a\r\n\r\n\r\na", (0, 0)),
    ("\r\n\r\na", "2t $", "\r\n\r\na\r\n", (3, 0)),
    ("\r\n\r\na", "$t $", "\r\n\r\na\r\na", (3, 0)),
    ("\r\n\r\na", "1t 0", "\r\n\r\n\r\na", (0, 0)),
    ("\r\n\r\na", "1,2t $", "\r\n\r\na\r\n\r\n", (4, 0)),
    ("a\r\n\r\n", "1d", "\r\n", (0, 0)),
    ("a\r\n\r\n", "$d", "a\r\n", (0, 0)),
    ("a\r\n\r\n", "2,$d", "a\r\n", (0, 0)),
    ("a\r\n\r\n", "1m $", "\r\na\r\n", (1, 0)),
    ("a\r\n\r\n", "$m 0", "\r\na\r\n", (0, 0)),
    ("a\r\n\r\n", "2m $", "a\r\n\r\n", (1, 0)),
    ("a\r\n\r\n", "2m 0", "\r\na\r\n", (0, 0)),
    ("a\r\n\r\n", "1,2m $", "a\r\n\r\n", (1, 0)),
    ("a\r\n\r\n", "2,$m 0", "\r\na\r\n", (0, 0)),
    ("a\r\n\r\n", "1t $", "a\r\n\r\na\r\n", (2, 0)),
    ("a\r\n\r\n", "$t 0", "\r\na\r\n\r\n", (0, 0)),
    ("a\r\n\r\n", "2t $", "a\r\n\r\n\r\n", (2, 0)),
    ("a\r\n\r\n", "$t $", "a\r\n\r\n\r\n", (2, 0)),
    ("a\r\n\r\n", "1t 0", "a\r\na\r\n\r\n", (0, 0)),
    ("a\r\n\r\n", "1,2t $", "a\r\n\r\na\r\n\r\n", (3, 0)),
];

#[test]
fn line_commands_at_either_end_of_a_file_as_neovim() {
    let dir = std::env::temp_dir().join(format!("shoin-range-eof-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut misses = Vec::new();
    for (i, &(before, cmd, want, cursor)) in EOF_CASES.iter().enumerate() {
        let path = dir.join(format!("{i}.md"));
        std::fs::write(&path, before).unwrap();
        let mut app = App::new(Default::default(), Some(path.clone()), None).unwrap();
        let loaded = text(&app);
        ex(&mut app, cmd);
        let after = at(&app);
        let changed = text(&app);
        feed(&mut app, "u");
        if text(&app) != loaded {
            misses.push(format!("{before:?} :{cmd} u: got {:?}, want {loaded:?}", text(&app)));
        }
        step(&mut app, "<C-r>");
        if text(&app) != changed {
            misses.push(format!("{before:?} :{cmd} u <C-r>: got {:?}, want {changed:?}", text(&app)));
        }
        ex(&mut app, "w");
        let got = std::fs::read_to_string(&path).unwrap();
        if got != want || after != cursor {
            misses.push(format!("{before:?} :{cmd}: got {got:?} {after:?}, want {want:?} {cursor:?} ({})", flash(&app)));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(misses.is_empty(), "{} misses:\n{}", misses.len(), misses.join("\n"));
}

/// A count after a range over a closed fold: the range is widened to the
/// fold first, then exactly `count` lines are taken from its end, not
/// widened again, as Neovim (with lines 2-4 folded by `2,4fold`).
#[test]
fn a_count_starts_after_the_fold_the_range_ends_in() {
    /// Text, command, then Neovim's text, cursor and unnamed register.
    type FoldCase = (&'static str, &'static str, &'static str, (usize, usize), &'static str);
    let t = "a\n# b\nc\nd\n# e\nf\n";
    let u = "  a\n  # b\n  c\n  d\n  # e\n  f\n";
    let cases: &[FoldCase] = &[
        (t, "2d 2", "a\n# b\nc\nf\n", (3, 0), "d\n# e\n"),
        (t, "1d 2", "c\nd\n# e\nf\n", (0, 0), "a\n# b\n"),
        (t, "3d 2", "a\n# b\nc\nf\n", (3, 0), "d\n# e\n"),
        (t, "1,3d 2", "a\n# b\nc\nf\n", (3, 0), "d\n# e\n"),
        (t, "2y 2", t, (0, 0), "d\n# e\n"),
        (t, "1y 2", t, (0, 0), "a\n# b\n"),
        (t, "1,2y", t, (0, 0), "a\n# b\nc\nd\n"),
        (t, "2> 2", "a\n# b\nc\n    d\n    # e\nf\n", (4, 4), ""),
        (t, "1> 2", "    a\n    # b\nc\nd\n# e\nf\n", (1, 4), ""),
        (u, "2< 2", "  a\n  # b\n  c\nd\n# e\n  f\n", (4, 0), ""),
        (u, "1< 2", "a\n# b\n  c\n  d\n  # e\n  f\n", (1, 0), ""),
    ];
    let mut misses = Vec::new();
    for &(before, cmd, want, cursor, reg) in cases {
        let mut app = app_with(before);
        app.term_size = (60, 16);
        feed(&mut app, "jzcgg");
        ex(&mut app, cmd);
        let got_reg = app.registers.get(&'"').map(|r| r.text.clone()).unwrap_or_default();
        if text(&app) != want || at(&app) != cursor || (!reg.is_empty() && got_reg != reg) {
            misses.push(format!(":{cmd}: got {:?} {:?} {got_reg:?}, want {want:?} {cursor:?} {reg:?}", text(&app), at(&app)));
        }
    }
    assert!(misses.is_empty(), "{} misses:\n{}", misses.len(), misses.join("\n"));
}

/// `:{range}w! path` never writes the buffer's own file, whatever names it:
/// a hard link (which a save writes in place) or a symlink.
#[cfg(unix)]
#[test]
fn w_with_a_range_refuses_the_own_file_by_any_link() {
    let dir = std::env::temp_dir().join(format!("shoin-range-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let own = dir.join("notes.md");
    std::fs::write(&own, "a\nb\nc\n").unwrap();
    std::fs::hard_link(&own, dir.join("alias.md")).unwrap();
    std::os::unix::fs::symlink(&own, dir.join("sym.md")).unwrap();
    let mut app = App::new(Default::default(), Some(own.clone()), None).unwrap();
    for name in ["alias.md", "sym.md"] {
        let p = dir.join(name);
        ex(&mut app, &format!("1w! {}", p.display()));
        assert!(matches!(app.flash.as_ref().map(|f| &f.kind), Some(FlashKind::Error)), "{name}: {}", flash(&app));
        assert_eq!(std::fs::read_to_string(&own).unwrap(), "a\nb\nc\n", "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `'<` or `'>` with no selection is an error at either end of a range.
#[test]
fn a_visual_mark_without_a_selection_is_an_error_at_either_end() {
    for cmd in ["'<,3", "3,'<", "'>,3", "3,'>"] {
        let mut app = app_with("a\nb\nc\nd\n");
        ex(&mut app, cmd);
        assert_eq!(at(&app), (0, 0), ":{cmd}");
        assert!(matches!(app.flash.as_ref().map(|f| &f.kind), Some(FlashKind::Error)), ":{cmd}: {}", flash(&app));
    }
}
