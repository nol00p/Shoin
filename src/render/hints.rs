//! The hint bar: the keys that matter right now, drawn in the status line's
//! row whenever the status line has nothing to say.
//!
//! What "right now" means, most specific first: a prefix waiting for its next
//! key (`Space`, `Space f`, `g`, `Ctrl-w`), an operator waiting for its range,
//! the finder, a prompt or the command line, the file tree, read-only mode,
//! then the mode itself. Leader continuations are read from the live keymap,
//! so a rebound key shows as what it now does.
//!
//! `[hints] enabled = false` turns all of it off, and zen mode hides it with
//! the rest of the chrome. Help, `:diff` and the start screen carry their own
//! keys, so the bar stays out of their way.

use crate::app::App;
use crate::config::keys::parse_seq;
use crate::input::mode::Mode;
use crate::input::pending::{Key, KeyCode};
use crate::render::splash;

/// One hint: the keys, and what they do.
pub type Hint = (String, String);

/// The hints for this moment, or none.
pub fn hints(app: &App) -> Vec<Hint> {
    if !app.config.hints.enabled
        || app.zen_on()
        || app.help.is_some()
        || app.diff.is_some()
        || splash::active(app)
    {
        return Vec::new();
    }
    let leader = leader_label(&app.config.input.leader);
    let pending = app.pending();
    let visual = matches!(app.mode, Mode::Visual | Mode::VisualLine);

    if let Some(prefix) = pending.prefix() {
        return prefix_hints(prefix, visual);
    }
    if !pending.partial.is_empty() {
        let table = if visual { &app.config.keys.visual } else { &app.config.keys.normal };
        return sequence_hints(table, &pending.partial, &app.config.input.leader);
    }
    if pending.operator.is_some() {
        return list(&[
            ("w b e $", "a motion"),
            ("iw ip i(", "an object"),
            ("again", "the whole line"),
            ("Esc", "cancel"),
        ]);
    }
    if app.finder.is_some() {
        return list(&[
            ("type", "narrow"),
            ("Ctrl-n Ctrl-p", "move"),
            ("Enter", "open"),
            ("Esc", "close"),
        ]);
    }
    match &app.mode {
        Mode::Prompt(_) => {
            return list(&[("Enter", "confirm"), ("Ctrl-u", "clear"), ("Esc", "cancel")]);
        }
        Mode::Command(_) => {
            return list(&[("Enter", "run"), ("Esc", "cancel"), (":help", "every command")]);
        }
        Mode::Search { .. } => {
            return list(&[("Enter", "find"), ("Esc", "cancel"), ("n N", "next · previous after")]);
        }
        _ => {}
    }
    if app.tree.as_ref().is_some_and(|t| t.focused) {
        // Most used first: whatever doesn't fit drops off the end.
        return list(&[
            ("l", "open"),
            ("q", "close"),
            ("a", "new"),
            ("r", "rename"),
            ("m", "move"),
            ("d", "delete"),
            ("?", "help"),
            ("- =", "root"),
            ("H", "dotfiles"),
        ]);
    }
    if app.view {
        return vec![
            hint("j k", "scroll"),
            hint("/", "search"),
            hint("i", "edit"),
            hint(&format!("{leader} v"), "edit"),
            hint("q", "quit"),
        ];
    }
    match app.mode {
        Mode::Insert => list(&[("Esc", "Normal"), ("Ctrl-s", "save"), ("Tab", "indent a list item")]),
        Mode::Visual | Mode::VisualLine => list(&[
            ("d c y", "delete · change · yank"),
            ("> <", "indent"),
            ("o", "other end"),
            ("Esc", "cancel"),
        ]),
        _ => vec![
            hint("i", "write"),
            hint("v", "select"),
            hint("/", "search"),
            hint(":", "command"),
            hint(&leader, "more"),
            hint(&format!("{leader} hl"), "help"),
        ],
    }
}

fn hint(keys: &str, does: &str) -> Hint {
    (keys.to_string(), does.to_string())
}

fn list(items: &[(&str, &str)]) -> Vec<Hint> {
    items.iter().map(|(k, d)| hint(k, d)).collect()
}

/// The leader as it is pressed: `Space` for the default.
fn leader_label(leader: &str) -> String {
    match leader {
        " " => "Space".into(),
        "" => "\\".into(),
        other => other.into(),
    }
}

/// What the built-in prefixes take next.
fn prefix_hints(prefix: char, visual: bool) -> Vec<Hint> {
    match prefix {
        'g' => {
            let mut out = list(&[
                ("b i h k", "bold · italic · highlight · code"),
                ("l", "link"),
                ("t", "task"),
                ("a", "table"),
                ("1-6 0", "heading"),
            ]);
            if !visual {
                out.extend(list(&[("g", "top"), ("f x", "follow · open"), ("p", "paragraph"), ("u U", "case")]));
            }
            out
        }
        'w' => list(&[
            ("h j k l", "move"),
            ("v s", "split"),
            ("q", "close"),
            ("o", "only"),
            ("w", "cycle"),
            ("> < + - =", "size"),
        ]),
        _ => list(&[("Esc", "cancel")]),
    }
}

/// What can follow the keys typed so far, read from a `[keys.*]` table: one
/// hint per next key. A next key that only starts longer sequences gets one
/// hint for the lot, `f…`, named after what they share.
fn sequence_hints(table: &std::collections::HashMap<String, String>, typed: &[Key], leader: &str) -> Vec<Hint> {
    let mut finished: Vec<(String, String)> = Vec::new();
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (keys, action) in table {
        if matches!(action.as_str(), "" | "none" | "nop") {
            continue;
        }
        let Ok(seq) = parse_seq(keys, leader) else { continue };
        if seq.len() <= typed.len() || seq[..typed.len()] != *typed {
            continue;
        }
        let next = key_label(&seq[typed.len()]);
        if seq.len() == typed.len() + 1 {
            finished.push((next, label(action)));
        } else if let Some((_, actions)) = groups.iter_mut().find(|(k, _)| *k == next) {
            actions.push(action.clone());
        } else {
            groups.push((next, vec![action.clone()]));
        }
    }
    // A key that both finishes a binding and starts longer ones shows once,
    // as the binding: pressing it runs that.
    groups.retain(|(k, _)| !finished.iter().any(|(f, _)| f == k));
    let mut out: Vec<Hint> = finished;
    out.extend(groups.into_iter().map(|(k, actions)| (format!("{k}…"), group_label(&actions))));
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then(a.0.cmp(&b.0)));
    out
}

/// A key the way the docs write it: `Space`, `Ctrl-w`, `Alt-j`, `Tab`.
fn key_label(key: &Key) -> String {
    let base = match key.code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Backspace => "BS".into(),
        KeyCode::Delete => "Del".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PgUp".into(),
        KeyCode::PageDown => "PgDn".into(),
    };
    let mut out = String::new();
    if key.ctrl {
        out.push_str("Ctrl-");
    }
    if key.alt {
        out.push_str("Alt-");
    }
    if key.shift {
        out.push_str("Shift-");
    }
    out + &base
}

/// A short name for an action, for the bar. Unknown names read as words.
fn label(action: &str) -> String {
    let short = match action {
        "save" | "write" => "save",
        "quit" => "quit",
        "help" | "shortcuts" => "help",
        "cycle_focus" | "focus" => "focus",
        "toggle_typewriter" | "typewriter" => "typewriter",
        "toggle_zen" | "zen" => "zen",
        "toggle_conceal" | "conceal" => "conceal",
        "toggle_numbers" | "numbers" | "number" => "numbers",
        "cycle_align" | "align" => "align",
        "toggle_view" | "view" | "read_only" => "read-only",
        "file_tree" | "file_explorer" | "tree" => "tree",
        "file_tree_home" | "file_explorer_home" => "tree in ~",
        "file_tree_cwd" | "file_explorer_cwd" => "tree in launch folder",
        "find_file" | "fuzzy_find" | "files" => "find",
        "find_file_home" | "fuzzy_find_home" => "find in ~",
        "find_file_cwd" | "fuzzy_find_cwd" => "find in launch folder",
        "find_buffer" | "buffers" | "switch_buffer" => "buffers",
        "split_vertical" | "vsplit" => "split beside",
        "split_horizontal" | "split" => "split below",
        "only_pane" | "only" => "only this pane",
        "close_pane" => "close pane",
        "toggle_bold" | "bold" => "bold",
        "toggle_italic" | "italic" => "italic",
        other => return other.replace('_', " "),
    };
    short.to_string()
}

/// A name for a group of bindings under one key, from what they do.
fn group_label(actions: &[String]) -> String {
    let all = |words: &[&str]| actions.iter().all(|a| words.iter().any(|w| a.contains(w)));
    if all(&["file", "find", "tree", "buffer", "fuzzy"]) {
        "files".into()
    } else if all(&["split", "pane", "only"]) {
        "splits".into()
    } else if all(&["help", "shortcuts"]) {
        "help".into()
    } else if actions.len() == 1 {
        label(&actions[0])
    } else {
        format!("{} more", actions.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::text::cursor::Cursor;
    use ratatui::crossterm::event::{KeyCode as Ct, KeyEvent, KeyModifiers};

    /// An app with the shipped bindings, past the start screen.
    fn app() -> App {
        let cfg = crate::config::parse("").unwrap();
        let mut app = App::new(cfg, None, None).unwrap();
        app.buffer.insert_str(Cursor::new(0, 0), "some text\n");
        app.buffer.cursor = Cursor::new(0, 0);
        app
    }

    fn press(app: &mut App, c: char) {
        app.on_key(KeyEvent::new(Ct::Char(c), KeyModifiers::NONE));
    }

    fn keys(h: &[Hint]) -> Vec<&str> {
        h.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn normal_mode_names_the_way_in() {
        let app = app();
        let h = hints(&app);
        assert!(keys(&h).contains(&"i"), "{h:?}");
        assert!(keys(&h).contains(&"Space hl"), "{h:?}");
    }

    /// After `Space` the bar lists what can follow, from the live keymap:
    /// single keys by what they do, longer ones grouped under their first key.
    #[test]
    fn after_the_leader_it_lists_what_follows() {
        let mut app = app();
        press(&mut app, ' ');
        let h = hints(&app);
        assert!(h.contains(&("w".into(), "save".into())), "{h:?}");
        assert!(h.contains(&("f…".into(), "files".into())), "{h:?}");
        assert!(h.contains(&("s…".into(), "splits".into())), "{h:?}");
        press(&mut app, 'f');
        let h = hints(&app);
        assert!(h.contains(&("e".into(), "tree".into())), "{h:?}");
        assert!(h.contains(&("F".into(), "find in ~".into())), "{h:?}");
        press(&mut app, 'e');
        assert!(app.tree.is_some(), "the binding still runs");
    }

    #[test]
    fn a_builtin_prefix_lists_its_second_keys() {
        let mut app = app();
        press(&mut app, 'g');
        assert!(keys(&hints(&app)).contains(&"b i h k"));
        app.on_key(KeyEvent::new(Ct::Char('w'), KeyModifiers::CONTROL));
        // `g` then Ctrl-w is nothing; start again from a clean slate.
        app.on_key(KeyEvent::new(Ct::Esc, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(Ct::Char('w'), KeyModifiers::CONTROL));
        assert!(keys(&hints(&app)).contains(&"v s"), "{:?}", hints(&app));
    }

    #[test]
    fn modes_and_panels_have_their_own() {
        let mut app = app();
        press(&mut app, 'i');
        assert!(keys(&hints(&app)).contains(&"Esc"));
        app.on_key(KeyEvent::new(Ct::Esc, KeyModifiers::NONE));
        press(&mut app, 'd');
        assert!(keys(&hints(&app)).contains(&"again"), "operator pending");
        app.on_key(KeyEvent::new(Ct::Esc, KeyModifiers::NONE));
        app.set_view(true);
        assert!(keys(&hints(&app)).contains(&"Space v"));
    }

    #[test]
    fn off_and_zen_show_nothing() {
        let mut app = app();
        app.config.hints.enabled = false;
        assert!(hints(&app).is_empty());
        app.config.hints.enabled = true;
        app.set_zen(true);
        assert!(hints(&app).is_empty());
    }

    #[test]
    fn rebound_leader_keys_show_as_rebound() {
        let mut cfg = Config::default();
        cfg.keys.normal.insert("<leader>x".into(), "toggle_zen".into());
        let mut app = App::new(cfg, None, None).unwrap();
        app.buffer.insert_str(Cursor::new(0, 0), "t\n");
        press(&mut app, ' ');
        assert!(hints(&app).contains(&("x".into(), "zen".into())));
    }
}
