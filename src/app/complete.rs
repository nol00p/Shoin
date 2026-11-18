//! Tab completion on the `:` line, vim's way: Tab puts the first match in
//! place of the word being typed, Tab again the next, Shift-Tab the one
//! before, and past either end the line goes back to what was typed. Any other
//! key keeps the line as it stands and ends the cycle.
//!
//! What completes depends on where the word is: a command name first, then a
//! path after the commands that take one (`PATH_COMMANDS`), an option after
//! `:set`, a topic after `:help`.

use super::commands::{canonical, COMMANDS, OPTIONS, PATH_COMMANDS};
use super::*;

/// A completion cycle under way on the `:` line.
#[derive(Clone, Debug)]
pub(crate) struct Completion {
    /// The line as it was typed, which the cycle wraps back to.
    original: String,
    /// The byte in `original` where the completed word starts.
    start: usize,
    /// Each match, whole: what replaces the word.
    matches: Vec<String>,
    /// The match on the line, or `None` while it shows `original`.
    at: Option<usize>,
    /// The matches are paths, which the hint bar shows by their last part.
    paths: bool,
}

impl Completion {
    /// The line as the cycle has it now.
    fn line(&self) -> String {
        match self.at {
            Some(i) => format!("{}{}", &self.original[..self.start], self.matches[i]),
            None => self.original.clone(),
        }
    }

    /// One step along `None, 0, 1, …, n-1, None`, forward or back.
    fn step(&mut self, forward: bool) {
        let last = self.matches.len() - 1;
        self.at = match (self.at, forward) {
            (None, true) => Some(0),
            (Some(i), true) if i < last => Some(i + 1),
            (Some(_), true) => None,
            (None, false) => Some(last),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
    }
}

/// What can complete the word at the end of `line`: where that word starts,
/// and every match for it, sorted. Paths are looked up from `base`.
pub(crate) fn candidates(line: &str, base: &Path) -> (usize, Vec<String>, bool) {
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead..];
    let Some(name_end) = rest.find(char::is_whitespace) else {
        // Still typing the command's name.
        let names = COMMANDS.iter().map(|(name, _)| *name).filter(|n| n.starts_with(rest));
        return (lead, sorted(names.map(String::from).collect()), false);
    };
    let name = &rest[..name_end];
    let after = &rest[name_end..];
    let start = lead + name_end + (after.len() - after.trim_start().len());
    let arg = &line[start..];
    let Some(command) = canonical(COMMANDS, name.strip_suffix('!').unwrap_or(name)) else {
        return (start, Vec::new(), false);
    };
    if PATH_COMMANDS.contains(&command) {
        // A path may hold spaces: the whole argument is the word.
        return (start, paths(arg, base), true);
    }
    // One word, and for `:set` only the key: `measure=` is a value.
    if arg.contains(char::is_whitespace) || arg.contains('=') {
        return (start, Vec::new(), false);
    }
    let words: Vec<String> = match command {
        "set" => OPTIONS.iter().map(|(name, _)| *name).filter(|n| n.starts_with(arg)).map(String::from).collect(),
        "help" => {
            let typed = arg.to_lowercase();
            crate::help::topics().into_iter().filter(|t| t.starts_with(&typed)).map(String::from).collect()
        }
        _ => Vec::new(),
    };
    (start, sorted(words), false)
}

/// The entries that complete a typed path, each as it would be typed: the
/// folder part kept as written (`~` too), a folder ending in `/`. Hidden
/// entries only when the typed name starts with `.`.
fn paths(typed: &str, base: &Path) -> Vec<String> {
    if typed == "~" {
        return match std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            Some(_) => vec!["~/".into()],
            None => Vec::new(),
        };
    }
    let (dir, name) = match typed.rfind('/') {
        Some(i) => typed.split_at(i + 1),
        None => ("", typed),
    };
    let folder = if dir.is_empty() { base.to_path_buf() } else { base.join(expand_home(dir)) };
    let Ok(entries) = std::fs::read_dir(&folder) else { return Vec::new() };
    let hidden = name.starts_with('.');
    let found = entries.flatten().filter_map(|e| {
        let file = e.file_name().into_string().ok()?;
        if !file.starts_with(name) || (file.starts_with('.') && !hidden) {
            return None;
        }
        // Through a link to where it points: a link to a folder is a folder.
        let slash = if e.path().is_dir() { "/" } else { "" };
        Some(format!("{dir}{file}{slash}"))
    });
    sorted(found.collect())
}

/// Alphabetical, ignoring case, with case breaking ties so the order is the
/// same every time.
fn sorted(mut words: Vec<String>) -> Vec<String> {
    words.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
    words.dedup();
    words
}

impl App {
    /// Tab (`forward`) or Shift-Tab on the `:` line holding `buf`: the line
    /// the next step of the cycle gives, starting one if none is under way.
    /// No match leaves the line as it is.
    pub(super) fn complete(&mut self, buf: String, forward: bool) -> String {
        let mut c = match self.completion.take().filter(|c| c.line() == buf) {
            Some(c) => c,
            None => {
                let (start, matches, paths) = candidates(&buf, &self.cwd());
                if matches.is_empty() {
                    return buf;
                }
                Completion { original: buf, start, matches, at: None, paths }
            }
        };
        c.step(forward);
        let line = c.line();
        // A lone match is done with: the next Tab starts over from it, so a
        // folder's Tab goes on into the folder.
        if c.matches.len() > 1 {
            self.completion = Some(c);
        }
        line
    }

    /// The matches for the hint bar while a cycle is under way, the one on
    /// the line in brackets. From two before it when the list is long, so it
    /// stays in view.
    pub(crate) fn completion_hints(&self) -> Option<Vec<(String, String)>> {
        let Mode::Command(buf) = &self.mode else { return None };
        let c = self.completion.as_ref().filter(|c| c.line() == *buf)?;
        let shown = |m: &str| -> String {
            if !c.paths {
                return m.to_string();
            }
            let trimmed = m.strip_suffix('/').unwrap_or(m);
            let cut = trimmed.rfind('/').map_or(0, |i| i + 1);
            m[cut..].to_string()
        };
        let from = c.at.map_or(0, |i| i.saturating_sub(2));
        let mut hints: Vec<(String, String)> = Vec::new();
        if from > 0 {
            hints.push(("…".into(), String::new()));
        }
        for (i, m) in c.matches.iter().enumerate().skip(from) {
            let label = if c.at == Some(i) { format!("[{}]", shown(m)) } else { shown(m) };
            hints.push((label, String::new()));
        }
        Some(hints)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn app() -> App {
        App::new(Config::default(), None, None).unwrap()
    }

    fn press(app: &mut App, code: KeyCode) {
        let mods = if code == KeyCode::BackTab { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
        app.on_key(KeyEvent::new(code, mods));
    }

    fn typed(app: &mut App, keys: &str) {
        for c in keys.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn line(app: &App) -> &str {
        match &app.mode {
            Mode::Command(s) => s,
            other => panic!("not on the : line: {other:?}"),
        }
    }

    fn words(line: &str) -> Vec<String> {
        candidates(line, Path::new("/nonexistent")).1
    }

    /// A folder of its own under the system temp folder, emptied first.
    fn folder(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("shoin-complete-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn command_names_complete_from_the_table() {
        assert_eq!(words("b"), ["bdelete", "bnext", "bprevious", "buffer", "buffers"]);
        assert_eq!(words("  se"), ["set"], "leading blanks are skipped");
        assert_eq!(candidates("  se", Path::new("/")).0, 2);
        assert_eq!(words("").len(), COMMANDS.len(), "an empty line offers every command");
        assert!(words("nosuch").is_empty());
    }

    #[test]
    fn every_listed_name_runs() {
        for (name, other) in COMMANDS {
            for n in std::iter::once(name).chain(other.iter()) {
                let mut app = app();
                app.run_command(n);
                let said = app.flash.and_then(|f| f.text).unwrap_or_default();
                assert!(!said.starts_with("not a command"), ":{n} says {said:?}");
            }
        }
        for (name, other) in OPTIONS {
            for n in std::iter::once(name).chain(other.iter()) {
                let mut app = app();
                app.run_command(&format!("set {n}"));
                let said = app.flash.and_then(|f| f.text).unwrap_or_default();
                assert!(!said.starts_with("unknown option"), ":set {n} says {said:?}");
            }
        }
        let mut app = app();
        app.run_command("sp!");
        assert_eq!(app.flash.and_then(|f| f.text).as_deref(), Some("not a command: sp!"), "no `!` where none is taken");
    }

    #[test]
    fn options_and_help_topics_complete() {
        assert_eq!(words("set auto"), ["autoreload", "autosave", "autosave_interval"]);
        assert_eq!(words("set  m"), ["measure", "mouse"], "after any run of blanks");
        assert!(words("set measure=").is_empty(), "a value is not an option");
        assert!(words("set measure 4").is_empty());
        assert_eq!(words("help ed"), ["edit", "editing", "edition"]);
        assert_eq!(words("h ED"), ["edit", "editing", "edition"], "topics ignore case");
        assert!(words("undo x").is_empty(), "no arguments to complete");
    }

    #[test]
    fn paths_complete_from_the_folder() {
        let d = folder("paths");
        std::fs::create_dir(d.join("notes")).unwrap();
        std::fs::create_dir(d.join("notes/old")).unwrap();
        std::fs::write(d.join("notes/day one.md"), "").unwrap();
        std::fs::write(d.join("Notebook.md"), "").unwrap();
        std::fs::write(d.join("a.md"), "").unwrap();
        std::fs::write(d.join(".hidden"), "").unwrap();
        let c = |line: &str| candidates(line, &d).1;
        assert_eq!(c("e "), ["a.md", "Notebook.md", "notes/"], "folders and files mixed, case aside");
        assert_eq!(c("e ."), [".hidden"], "hidden only after a typed dot");
        assert_eq!(c("w! no"), ["notes/"], "case counts in a name");
        assert_eq!(c("saveas notes/"), ["notes/day one.md", "notes/old/"]);
        assert_eq!(c("edit notes/day o"), ["notes/day one.md"], "a name with a space");
        assert_eq!(candidates("edit notes/day o", &d).0, 5, "the whole argument is the word");
        assert!(c("e missing/").is_empty());
        let abs = format!("e {}/no", d.display());
        assert_eq!(candidates(&abs, Path::new("/elsewhere")).1, [format!("{}/notes/", d.display())]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tab_cycles_and_wraps_to_what_was_typed() {
        let mut app = app();
        typed(&mut app, ":help ed");
        assert_eq!(app.completion_hints(), None, "no list before Tab");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "help edit");
        let hints = app.completion_hints().unwrap();
        let labels: Vec<&str> = hints.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(labels, ["[edit]", "editing", "edition"]);
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "help editing");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "help edition");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "help ed", "past the last, back to what was typed");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "help edit", "and round again");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(line(&app), "help ed");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(line(&app), "help edition", "back from the start is the last");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(line(&app), "help editing");
    }

    #[test]
    fn shift_tab_first_goes_to_the_last_match() {
        let mut app = app();
        typed(&mut app, ":set auto");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(line(&app), "set autosave_interval");
        // Some terminals send Shift-Tab as Tab with Shift held.
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(line(&app), "set autosave");
    }

    #[test]
    fn another_key_ends_the_cycle_and_keeps_the_line() {
        let mut app = app();
        typed(&mut app, ":help ed");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        typed(&mut app, "x");
        assert_eq!(line(&app), "help editingx");
        assert_eq!(app.completion_hints(), None);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "help editing", "a fresh start from the line as it stands");

        // Esc during a cycle leaves the line and runs nothing.
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.help.is_none());
        assert!(app.completion.is_none());
        typed(&mut app, ":");
        assert_eq!(app.completion_hints(), None, "a new : line starts clean");
    }

    #[test]
    fn no_match_changes_nothing() {
        let mut app = app();
        typed(&mut app, ":nosuch");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "nosuch");
        assert!(app.flash.as_ref().and_then(|f| f.text.as_ref()).is_none(), "and says nothing");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(line(&app), "nosuch");
    }

    #[test]
    fn a_lone_match_ends_the_cycle() {
        let mut app = app();
        typed(&mut app, ":fix");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "fixlist");
        assert_eq!(app.completion_hints(), None);
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "fixlist", "Tab again stays on it");
    }

    #[test]
    fn a_long_list_keeps_the_current_match_in_view() {
        let mut app = app();
        typed(&mut app, ":");
        for _ in 0..6 {
            press(&mut app, KeyCode::Tab);
        }
        let hints = app.completion_hints().unwrap();
        assert_eq!(hints[0].0, "…");
        assert_eq!(hints[3].0, format!("[{}]", line(&app)));
    }
}
