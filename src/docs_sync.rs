//! Keeps `:help`, the docs and the code saying the same thing.
//!
//! The in-app help (`help.rs`) and the reference pages (`docs/reference/`)
//! describe the same commands, flags and keys twice, and the code is a third
//! description. Each test below takes one of those vocabularies and checks the
//! places that list it against each other — and, where the code can answer,
//! against the code itself: a documented `:command` has to run, a documented
//! `--flag` has to parse.
//!
//! The docs are read at test time rather than with `include_str!`, so the
//! tests check the files as they are on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use clap::CommandFactory;

use crate::app::App;
use crate::config::Config;
use crate::help::Help;
use crate::input::pending::{Key, KeyCode};
use crate::values::OPTIONS;

/// A file from the repository, by its path from the root.
fn read(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Every page of `:help`, as one text.
fn help_text() -> String {
    Help::all_text()
}

/// Every Markdown file a reader sees: the README and everything under docs/.
fn all_docs() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = vec![("README.md".to_string(), read("README.md"))];
    let mut dirs = vec![root.join("docs")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                let text = std::fs::read_to_string(&path).unwrap();
                let name = path.strip_prefix(root).unwrap_or(&path).display().to_string();
                out.push((name, text));
            }
        }
    }
    out
}

/// Every `:command` named in `text`: a `:` that does not follow a letter,
/// digit or `:` (so not `https:` or `10:30`), then letters, then an optional
/// `!`.
fn ex_commands(text: &str) -> BTreeSet<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = BTreeSet::new();
    for (i, &c) in chars.iter().enumerate() {
        if c != ':' || i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == ':') {
            continue;
        }
        let mut name: String = chars[i + 1..].iter().take_while(|c| c.is_ascii_alphabetic()).collect();
        if name.is_empty() {
            continue;
        }
        if chars.get(i + 1 + name.len()) == Some(&'!') {
            name.push('!');
        }
        out.insert(name);
    }
    out
}

/// The option names after every `:set`, splitting alternatives written as
/// `conceal|typewriter` (or `\|` inside a Markdown table).
fn set_options(text: &str) -> BTreeSet<String> {
    text.match_indices(":set ")
        .flat_map(|(i, m)| {
            let rest = &text[i + m.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_lowercase() || c == '_' || c == '|' || c == '\\'))
                .unwrap_or(rest.len());
            rest[..end].split(['|', '\\']).filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>()
        })
        .collect()
}

/// What running `:{cmd}` in a fresh editor flashes.
fn run(cmd: &str) -> String {
    let mut app = App::new(Config::default(), None, None).unwrap();
    app.run_command(cmd);
    app.flash.and_then(|f| f.text).unwrap_or_default()
}

const COMMANDS_PAGE: &str = "docs/reference/commands.md";
const CLI_PAGE: &str = "docs/reference/cli.md";
const CONFIG_PAGE: &str = "docs/reference/config.md";
const KEYS_PAGE: &str = "docs/reference/keys.md";

fn assert_same<T: Ord + std::fmt::Debug>(
    expected: &BTreeSet<T>, actual: &BTreeSet<T>, source: &str, destination: &str,
) {
    assert!(!expected.is_empty(), "{source}: found no entries");
    assert!(!actual.is_empty(), "{destination}: found no entries");
    let missing: Vec<_> = expected.difference(actual).collect();
    let extra: Vec<_> = actual.difference(expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{destination} compared with {source}\nmissing: {missing:?}\nunexpected: {extra:?}",
    );
}

type AliasGroups = BTreeSet<(String, BTreeSet<String>)>;

fn command_table() -> AliasGroups {
    crate::app::COMMANDS.iter().map(|(name, aliases)| {
        (name.to_string(), aliases.iter().map(|alias| alias.to_string()).collect())
    }).collect()
}

fn option_table() -> AliasGroups {
    OPTIONS.iter().map(|(name, aliases)| {
        (name.to_string(), aliases.iter().map(|alias| alias.to_string()).collect())
    }).collect()
}

fn code_spans(cell: &str) -> impl Iterator<Item = &str> {
    cell.split('`').skip(1).step_by(2)
}

fn section<'a>(page: &'a str, heading: &str) -> &'a str {
    let marker = format!("\n## {heading}\n");
    let (_, rest) = page.split_once(&marker).unwrap_or_else(|| panic!("missing ## {heading}"));
    rest.split_once("\n## ").map_or(rest, |(body, _)| body)
}

/// The command cell lists the canonical name and its aliases together.
fn command_groups(page: &str) -> AliasGroups {
    let mut groups = BTreeSet::new();
    for line in page.lines() {
        let Some(cell) = line.strip_prefix('|').and_then(|row| row.split_once(" |").map(|(cell, _)| cell)) else { continue };
        // Force forms are checked by running the documented commands.
        let mut names = code_spans(cell).filter_map(|span| span.strip_prefix(':'))
            .filter_map(|span| span.split_whitespace().next())
            .filter(|name| !name.ends_with('!'));
        if let Some(name) = names.next() {
            groups.insert((name.to_string(), names.map(str::to_string).collect()));
        }
    }
    groups
}

fn option_groups(page: &str) -> AliasGroups {
    section(page, "Settings").lines().filter_map(|line| {
        let mut cells = line.strip_prefix('|')?.split('|');
        let name = code_spans(cells.next()?).next()?;
        let aliases = code_spans(cells.next()?).map(str::to_string).collect();
        Some((name.to_string(), aliases))
    }).collect()
}

#[test]
fn the_commands_page_matches_the_code_tables() {
    let page = read(COMMANDS_PAGE);
    assert_same(&command_table(), &command_groups(&page), "COMMANDS", COMMANDS_PAGE);
    assert_same(&option_table(), &option_groups(&page), "OPTIONS", COMMANDS_PAGE);
}

#[test]
fn help_and_the_commands_page_name_the_same_commands() {
    assert_same(&ex_commands(&help_text()), &ex_commands(&read(COMMANDS_PAGE)), ":help", COMMANDS_PAGE);
}

#[test]
fn help_names_every_set_option_and_alias() {
    let names = OPTIONS.iter().flat_map(|&(name, aliases)| std::iter::once(name).chain(aliases.iter().copied()))
        .map(|name| name.to_string()).collect();
    assert_same(&names, &set_options(&help_text()), "OPTIONS", ":help");
}

#[test]
fn every_documented_command_exists() {
    for (path, body) in all_docs().into_iter().chain([(":help".to_string(), help_text())]) {
        let unknown: Vec<_> = ex_commands(&body).into_iter().filter(|name| run(name).starts_with("not a command")).collect();
        assert!(unknown.is_empty(), "{path} names unknown commands: {unknown:?}");
    }
}

#[test]
fn every_documented_set_option_exists() {
    for (path, body) in all_docs().into_iter().chain([(":help".to_string(), help_text())]) {
        let unknown: Vec<_> = set_options(&body).into_iter()
            .filter(|name| run(&format!("set {name}")).starts_with("unknown option"))
            .collect();
        assert!(unknown.is_empty(), "{path} names unknown :set options: {unknown:?}");
    }
}

fn fenced_block<'a>(page: &'a str, language: &str, path: &str) -> &'a str {
    let fence = format!("\n```{language}\n");
    page.split_once(&fence).and_then(|(_, tail)| tail.split_once("\n```"))
        .map(|(body, _)| body)
        .filter(|body| !body.trim().is_empty())
        .unwrap_or_else(|| panic!("{path}: missing or empty fenced {language} block"))
}

#[test]
fn the_command_line_page_matches_help_output() {
    let output = crate::Cli::command()
        .color(clap::ColorChoice::Never)
        .try_get_matches_from(["shoin", "--help"])
        .expect_err("--help exits before opening the editor");
    assert_eq!(output.kind(), clap::error::ErrorKind::DisplayHelp);
    let help = output.to_string();
    let page = read(CLI_PAGE);
    let expected = help.trim_end_matches('\n');
    let actual = fenced_block(&page, "text", CLI_PAGE);
    // Find the first mismatching line, including an added or deleted line.
    let mut expected = expected.lines();
    let mut actual = actual.lines();
    for line in 1.. {
        let (want, found) = (expected.next(), actual.next());
        assert_eq!(want, found, "{CLI_PAGE}: --help block line {line} (clap vs docs)");
        if want.is_none() {
            break;
        }
    }
}

/// The long flags `shoin` accepts, from clap itself.
fn cli_flags() -> BTreeSet<String> {
    let mut flags: BTreeSet<String> = crate::Cli::command()
        .get_arguments()
        .filter_map(|a| a.get_long())
        .map(|l| format!("--{l}"))
        .collect();
    flags.extend(["--help".to_string(), "--version".to_string()]);
    flags
}

/// The short flags `shoin` accepts, each with its long form — `-R` with
/// `--read-only` — from clap itself.
fn cli_short_flags() -> BTreeSet<(String, String)> {
    let mut flags: BTreeSet<(String, String)> = crate::Cli::command()
        .get_arguments()
        .filter_map(|a| Some((format!("-{}", a.get_short()?), format!("--{}", a.get_long()?))))
        .collect();
    flags.insert(("-h".to_string(), "--help".to_string()));
    flags.insert(("-V".to_string(), "--version".to_string()));
    flags
}

/// The flags written after `shoin` in `text` — `shoin --render x.md`,
/// `cat x | shoin -p --pager` — leaving other tools' flags (`curl --proto`)
/// alone. A lone `-` is the stdin file, not a flag.
fn shoin_flags(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        for (i, _) in line.match_indices("shoin ") {
            let rest = &line[i..];
            let end = rest.find(['`', '|', ')', '"']).unwrap_or(rest.len());
            for word in rest[..end].split_whitespace() {
                let word = word.trim_end_matches([',', '.', ';', ':']);
                let short = word.len() == 2
                    && word.starts_with('-')
                    && word[1..].chars().all(|c| c.is_ascii_alphabetic());
                if short || word.starts_with("--") && word.len() > 2 {
                    out.insert(word.split('=').next().unwrap().to_string());
                }
            }
        }
    }
    out
}

/// Every flag clap knows is in the command-line reference, and the reference
/// lists no flag clap does not.
#[test]
fn the_command_line_page_matches_the_flags() {
    let flags = cli_flags();
    let page = read("docs/reference/cli.md");
    let listed: BTreeSet<String> = page
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| w.starts_with("--") && w.len() > 2)
        .map(str::to_string)
        .collect();
    let missing: Vec<_> = flags.difference(&listed).collect();
    let unknown: Vec<_> = listed.difference(&flags).collect();
    assert!(
        missing.is_empty() && unknown.is_empty(),
        "not in docs/reference/cli.md: {missing:?}\nin cli.md but not a flag: {unknown:?}"
    );
}

/// Every short flag clap knows is in the command-line reference beside its
/// long form, as `-R, --read-only`, and the reference pairs no short flag
/// clap does not — so `-p` cannot quietly come to mean something else.
#[test]
fn the_command_line_page_matches_the_short_flags() {
    let flags = cli_short_flags();
    let listed: BTreeSet<(String, String)> = read("docs/reference/cli.md")
        .lines()
        .filter_map(|l| {
            let (short, rest) = l.trim_start().split_once(", ")?;
            let long = rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).next()?;
            let is_short = short.len() == 2 && short.starts_with('-') && short != "--";
            (is_short && long.starts_with("--")).then(|| (short.to_string(), long.to_string()))
        })
        .collect();
    let missing: Vec<_> = flags.difference(&listed).collect();
    let unknown: Vec<_> = listed.difference(&flags).collect();
    assert!(
        missing.is_empty() && unknown.is_empty(),
        "not in docs/reference/cli.md: {missing:?}\nin cli.md but not a flag: {unknown:?}"
    );
}

/// A `shoin --flag` or `shoin -p` anywhere in the docs, the README or `:help`
/// is one that exists — so a removed flag cannot linger in an install guide.
#[test]
fn every_documented_flag_exists() {
    let mut flags = cli_flags();
    flags.extend(cli_short_flags().into_iter().map(|(short, _)| short));
    let mut stale = Vec::new();
    for (name, text) in all_docs().into_iter().chain([(":help".to_string(), help_text())]) {
        for flag in shoin_flags(&text) {
            if !flags.contains(&flag) {
                stale.push(format!("{name}: {flag}"));
            }
        }
    }
    assert!(stale.is_empty(), "flags shoin does not have: {stale:?}");
}

/// The first column of each table for the three editable modes.
fn documented_keys(page: &str) -> BTreeSet<(String, String)> {
    const SHARED: &[&str] = &["normal", "visual"];
    // These rows explain input grammar rather than standalone bindings.
    const REGISTER_ARGUMENTS: &[&str] = &["\"a{cmd}", "\"A{cmd}", "\"\"", "\"0", "\"1", "\"9", "\"+", "\"*"];
    const OPERATOR_EXAMPLES: &[&str] = &[">al", "<al", ">aL", "<aL"];
    const COUNT_SYNTAX: &str = "`1`–`9`";
    const MOUSE_ROWS: &[(&str, &str)] = &[("Visual mode", "drag")];

    // Text objects and the overlay tables describe their own input contexts.
    let mut keys = BTreeSet::new();
    for (heading, modes) in [
        ("Normal mode", &["normal"][..]), ("Operators", &["normal"]), ("Leader keys", &["normal"]),
        ("Insert mode", &["insert"]), ("Visual mode", &["visual"]),
        ("Normal and Visual mode", SHARED), ("Motions", SHARED), ("Writer verbs", SHARED),
        ("Lists", SHARED), ("Registers", SHARED), ("Folds", SHARED), ("Panes", SHARED),
    ] {
        let mut found_binding = false;
        for line in section(page, heading).lines() {
            let Some((cell, _)) = line.trim().strip_prefix('|').and_then(|row| row.split_once('|')) else { continue };
            let cell = cell.trim();
            if matches!(cell, "Key" | "---") || MOUSE_ROWS.contains(&(heading, cell))
                || (heading, cell) == ("Normal and Visual mode", COUNT_SYNTAX)
            {
                continue;
            }
            let spans: Vec<_> = code_spans(cell).collect();
            assert!(!spans.is_empty(), "{KEYS_PAGE}: unrecognized key row in {heading}: {line}");
            let examples = match heading {
                "Registers" => REGISTER_ARGUMENTS,
                "Operators" => OPERATOR_EXAMPLES,
                _ => &[],
            };
            for key in spans.into_iter().filter(|key| !examples.contains(key)) {
                found_binding = true;
                keys.extend(modes.iter().map(|mode| (mode.to_string(), key.to_string())));
            }
        }
        assert!(found_binding, "{KEYS_PAGE}: no bindings in {heading}");
    }
    keys
}

fn key_name(key: Key) -> String {
    let mut name = String::new();
    for (on, prefix) in [(key.ctrl, "Ctrl-"), (key.alt, "Alt-"), (key.shift, "Shift-")] {
        if on {
            name.push_str(prefix);
        }
    }
    match key.code {
        KeyCode::Char('\0') => name.push_str("<leader>"),
        KeyCode::Char(' ') => name.push_str("Space"),
        KeyCode::Char(c) => name.push(c),
        code => name.push_str(&format!("{code:?}")),
    }
    name
}

fn shipped_key_name(key: &str) -> String {
    // Preserve the leader placeholder through the real config key parser.
    let sequence = crate::config::keys::parse_seq(key, "\0")
        .unwrap_or_else(|err| panic!("shoin/keys.conf: {key}: {err}"));
    let mut name = String::new();
    let mut separate = false;
    for key in sequence {
        if separate {
            name.push(' ');
        }
        let part = key_name(key);
        separate = part.len() > 1 && part != "<leader>";
        name.push_str(&part);
    }
    name
}

fn shipped_bindings() -> BTreeMap<(String, String), bool> {
    use crate::input::{action::Action, bindings::Verb, keymap::Keymap};

    let config = crate::config::parse("").expect("shipped keys.conf parses");
    let (_, warnings) = Keymap::from_config(&config.keys, &config.input.leader);
    assert!(warnings.is_empty(), "shoin/keys.conf: {warnings:?}");
    let mut keys = BTreeMap::new();
    for (mode, bindings) in [
        ("normal", config.keys.normal), ("visual", config.keys.visual), ("insert", config.keys.insert),
    ] {
        for (key, action) in bindings {
            let verb = Verb::from_config_name(&action).expect("validated shipped key action");
            assert!(mode != "insert" || matches!(verb, Verb::Act(_))
                && crate::config::keys::parse_seq(&key, &config.input.leader).unwrap().len() == 1,
                "shoin/keys.conf: Insert binding {key} must be one key bound to an action");
            keys.insert((mode.to_string(), shipped_key_name(&key)), verb != Verb::Act(Action::Nop));
        }
    }
    assert!(keys.values().any(|enabled| *enabled), "shoin/keys.conf: no active bindings");
    keys
}

#[test]
fn help_lists_the_shipped_bindings() {
    let keys = shipped_bindings();
    let help = help_text();
    for ((_, key), enabled) in keys {
        if !enabled {
            continue;
        }
        let found = help.match_indices(&key).any(|(start, word)| {
            help[start + word.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric())
        });
        assert!(found, ":help is missing shoin/keys.conf binding {key}");
    }
}

fn builtin_bindings() -> BTreeSet<(String, String)> {
    use crate::input::action::Action;
    use crate::input::bindings::{after_prefix, normal, visual, Verb};

    let chars = (' '..='~').map(KeyCode::Char);
    let codes: Vec<_> = chars.chain([
        KeyCode::Esc, KeyCode::Left, KeyCode::Down, KeyCode::Up, KeyCode::Right,
        KeyCode::Enter, KeyCode::Backspace, KeyCode::Delete, KeyCode::Tab,
        KeyCode::Home, KeyCode::End, KeyCode::PageUp, KeyCode::PageDown,
    ]).collect();
    let mut out = BTreeSet::new();
    let window_keys = window_keys();
    for (mode, resolve) in [("normal", normal as fn(Key) -> Option<Verb>), ("visual", visual)] {
        for code in codes.iter().copied() {
            for (ctrl, alt) in [(false, false), (true, false), (false, true)] {
                let key = Key { code, ctrl, alt, shift: false };
                let Some(verb) = resolve(key) else { continue };
                // Modified arrows/Esc and most Alt chords fall through to the
                // unmodified binding. Only explicit chords are documented.
                if (ctrl || alt) && !matches!(code, KeyCode::Char(_))
                    || alt && Some(verb.clone()) == resolve(Key { alt: false, ..key })
                {
                    continue;
                }
                // The Visual table inherits these Normal actions, which
                // app::visual_key consumes without acting on the selection.
                if mode == "visual" && matches!(verb, Verb::Act(
                    Action::OpenAbove | Action::OpenBelow | Action::DeleteChar | Action::Redo | Action::Repeat
                )) {
                    continue;
                }
                let first = key_name(key);
                if let Verb::Prefix(prefix) = verb {
                    for next in codes.iter().copied() {
                        let key = Key { code: next, ctrl: false, alt: false, shift: false };
                        // after_prefix passes every Ctrl-w suffix through to
                        // window_command; only that handler assigns bindings.
                        if after_prefix(prefix, key, mode == "visual").is_some_and(|v| {
                            !matches!(v, Verb::Act(Action::Window(c)) if !window_keys.contains(&c))
                        }) {
                            let separator = if ctrl { " " } else { "" };
                            out.insert((mode.to_string(), format!("{first}{separator}{}", key_name(key))));
                        }
                    }
                } else {
                    out.insert((mode.to_string(), first));
                }
            }
        }
    }
    out.extend(insert_keys().into_iter().map(|key| ("insert".to_string(), key)));
    out
}

/// Collect literal dispatch patterns, allowing the arm and arrow to span
/// lines. These handlers have no enumerable binding table.
fn dispatch_patterns(path: &str, function: &str, matched: &str, starts_pattern: fn(&str) -> bool) -> Vec<String> {
    let source = read(path);
    let (_, body) = source.split_once(&format!("fn {function}")).expect("dispatch function");
    let (_, body) = body.split_once(matched).expect("dispatch match");
    let (_, arms) = body.split_once('{').expect("dispatch match body");
    let mut patterns = Vec::new();
    let mut pending = String::new();
    for line in arms.lines() {
        let line = line.split("//").next().unwrap().trim();
        if pending.is_empty() && !starts_pattern(line) && !line.strip_prefix('_').is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '=', '\t'])) {
            continue;
        }
        pending.push(' ');
        pending.push_str(line);
        if let Some((pattern, _)) = pending.split_once("=>") {
            if pattern.trim() == "_" {
                assert!(!patterns.is_empty(), "{path}: no patterns in {function}");
                return patterns;
            }
            patterns.push(pattern.split_whitespace().collect::<Vec<_>>().join(" "));
            pending.clear();
        }
    }
    panic!("{path}: missing fallback arm in {function}");
}

fn window_keys() -> BTreeSet<char> {
    dispatch_patterns("src/app/tree.rs", "window_command", "match key", |line| line.starts_with('\''))
        .iter().flat_map(|pattern| pattern.split('|')).map(|literal| {
            let literal = literal.trim().strip_prefix('\'').and_then(|s| s.strip_suffix('\''))
                .unwrap_or_else(|| panic!("unrecognized window key: {literal}"));
            let mut chars = literal.chars();
            let key = chars.next().expect("empty window key");
            assert!(chars.next().is_none(), "unrecognized window key: {literal}");
            key
        }).collect()
}

fn insert_keys() -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for pattern in dispatch_patterns("src/app/insert.rs", "on_key_insert", "match key.code", |line| {
        line.starts_with("KeyCode::")
    }) {
        let (alternatives, guard) = pattern.split_once(" if ").unwrap_or((&pattern, ""));
        for pattern in alternatives.split('|').map(str::trim) {
            let key = pattern.strip_prefix("KeyCode::")
                .unwrap_or_else(|| panic!("unrecognized Insert key: {pattern}"));
            let name = match key {
                "BackTab" => "Shift-Tab".to_string(),
                "Char(c)" | "Char(_)" => continue, // ordinary typing and ignored chords
                _ if key.starts_with("Char(") => {
                    let letter = key.strip_prefix("Char('").and_then(|s| s.strip_suffix("')"))
                        .unwrap_or_else(|| panic!("unrecognized Insert character: {pattern}"));
                    let modifier = match guard {
                        "key.modifiers.contains(KeyModifiers::CONTROL)" => "Ctrl",
                        "key.modifiers.contains(KeyModifiers::ALT)" => "Alt",
                        "" => "",
                        _ => panic!("unrecognized Insert modifier: {guard}"),
                    };
                    if modifier.is_empty() { letter.to_string() } else { format!("{modifier}-{letter}") }
                }
                _ => key.to_string(),
            };
            keys.insert(name);
        }
    }
    assert!(!keys.is_empty(), "Insert dispatcher: no standalone bindings");
    let alias = Config::default().input.escape_alias;
    if !alias.is_empty() {
        keys.insert(alias);
    }
    keys
}

#[test]
fn the_keys_page_matches_bindings_in_each_mode() {
    let mut expected = builtin_bindings();
    for (binding, enabled) in shipped_bindings() {
        if enabled {
            expected.insert(binding);
        } else {
            expected.remove(&binding);
        }
    }
    let documented = documented_keys(&read(KEYS_PAGE));
    for mode in ["normal", "visual", "insert"] {
        let for_mode = |bindings: &BTreeSet<(String, String)>| {
            bindings.iter().filter(|(name, _)| name == mode).map(|(_, key)| key.clone()).collect()
        };
        assert_same(&for_mode(&expected), &for_mode(&documented), mode, KEYS_PAGE);
    }
}

/// Inspect the field names passed by the derived Deserialize implementation.
/// Config only derives Deserialize, so capture its schema without changing
/// the production types or relying on their Debug representation.
fn schema_fields<T: serde::de::DeserializeOwned>() -> &'static [&'static str] {
    struct Fields(Option<&'static [&'static str]>);

    impl<'de> serde::Deserializer<'de> for &mut Fields {
        type Error = serde::de::value::Error;

        fn deserialize_struct<V: serde::de::Visitor<'de>>(
            self, _name: &'static str, fields: &'static [&'static str], _visitor: V,
        ) -> Result<V::Value, Self::Error> {
            self.0 = Some(fields);
            Err(serde::de::Error::custom("schema inspection complete"))
        }

        fn deserialize_any<V: serde::de::Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
            Err(serde::de::Error::custom("expected a struct schema"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes byte_buf
            option unit unit_struct newtype_struct seq tuple tuple_struct map enum
            identifier ignored_any
        }
    }

    let mut fields = Fields(None);
    let _ = T::deserialize(&mut fields);
    let fields = fields.0.expect("expected a derived Deserialize struct schema");
    assert!(!fields.is_empty(), "empty schema for {}", std::any::type_name::<T>());
    fields
}

fn default_config_fields() -> BTreeSet<String> {
    use crate::config::schema::*;

    let mut fields: BTreeSet<_> = schema_fields::<Config>().iter().map(|field| field.to_string()).collect();
    for (section, members) in [
        ("layout", schema_fields::<LayoutConfig>()), ("status", schema_fields::<StatusConfig>()),
        ("cursor", schema_fields::<CursorConfig>()), ("editor", schema_fields::<EditorConfig>()),
        ("input", schema_fields::<InputConfig>()), ("markdown", schema_fields::<MarkdownConfig>()),
        ("glyphs", schema_fields::<GlyphConfig>()), ("splash", schema_fields::<SplashConfig>()),
        ("tree", schema_fields::<TreeConfig>()), ("transclude", schema_fields::<TranscludeConfig>()),
        ("hints", schema_fields::<HintsConfig>()),
    ] {
        assert!(fields.remove(section), "missing config section {section}");
        for member in members {
            // The schema includes the legacy alias of transclude.embed.
            if (section, *member) != ("transclude", "preview") {
                fields.insert(format!("{section}.{member}"));
            }
        }
    }

    // The variable keymap and the theme override maps have their own schema.
    assert!(fields.remove("keys"));
    assert!(fields.remove("theme"));
    for (function, section) in [("set_color", "theme"), ("set_style", "theme.styles")] {
        for pattern in dispatch_patterns("src/render/theme.rs", function, "match key", |line| line.starts_with('"')) {
            let field = pattern.strip_prefix('"').and_then(|part| part.strip_suffix('"'))
                .unwrap_or_else(|| panic!("unrecognized theme key: {pattern}"));
            fields.insert(format!("{section}.{field}"));
        }
    }
    fields.insert("theme.indent_colors".to_string());
    fields
}

/// Compare authored TOML fields before serde supplies any missing values.
fn config_entries(table: &toml::Table) -> BTreeMap<String, toml::Value> {
    fn visit(
        table: &toml::Table, prefix: &str, entries: &mut BTreeMap<String, toml::Value>,
    ) {
        for (name, value) in table {
            let path = format!("{prefix}{name}");
            if let Some(table) = value.as_table() {
                visit(table, &format!("{path}."), entries);
            } else {
                entries.insert(path, value.clone());
            }
        }
    }

    let mut entries = BTreeMap::new();
    visit(table, "", &mut entries);
    entries
}

#[test]
fn shipped_and_documented_config_spell_out_the_defaults() {
    let expected = default_config_fields();
    let mut shipped = BTreeMap::new();
    for (name, _) in crate::config::init::SHIPPED {
        if *name == "keys.conf" {
            continue;
        }
        let body = read(&format!("shoin/{name}"));
        let table = body.parse::<toml::Table>().unwrap_or_else(|err| panic!("shoin/{name}: {err}"));
        shipped.extend(config_entries(&table));
    }
    let page = read(CONFIG_PAGE);
    let docs = fenced_block(&page, "toml", CONFIG_PAGE)
        .parse::<toml::Table>().unwrap_or_else(|err| panic!("{CONFIG_PAGE}: {err}"));
    // The existing shipped-versus-default test covers the other values.
    let config: Config = toml::Value::Table(docs.clone()).try_into().unwrap();
    assert_eq!(
        config.hints.enabled, Config::default().hints.enabled,
        "{CONFIG_PAGE}: hints.enabled differs from the built-in default",
    );
    let docs = config_entries(&docs);

    assert_same(&expected, &shipped.keys().cloned().collect(), "built-in defaults", "shoin/*.conf");
    assert_same(&expected, &docs.keys().cloned().collect(), "built-in defaults", CONFIG_PAGE);
    for (field, value) in shipped {
        assert_eq!(docs[&field], value, "{CONFIG_PAGE}: {field} differs from shoin/*.conf");
    }

    // The configuration guide gives a subset of the defaults.
    let guide_path = "docs/configuration.md";
    let guide = read(guide_path);
    let table = fenced_block(&guide, "toml", guide_path).parse::<toml::Table>().unwrap();
    let guide_entries = config_entries(&table);
    assert!(!guide_entries.is_empty(), "{guide_path}: no config entries");
    for (field, value) in guide_entries {
        if !field.starts_with("keys.") {
            assert_eq!(docs.get(&field), Some(&value), "{guide_path}: {field} differs from {CONFIG_PAGE}");
        }
    }
}

/// Every name a `[keys.*]` binding can use, read from the three parsers'
/// match arms (`Action::from_config_name`, `Verb::from_config_name`,
/// `motion_by_name`): every `"name" =>` arm in those two files that yields an
/// `Action`, `Verb` or `Motion`. The empty name is left out; `heading_N`,
/// matched by prefix, is checked apart.
fn action_names() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for file in ["src/input/action.rs", "src/input/bindings.rs"] {
        for line in read(file).lines() {
            let Some((arm, result)) = line.trim().split_once("=>") else { continue };
            let arm = arm.trim();
            // Only arms that make something bindable: `Root::parse` matches
            // words too, but they are `tree.root` values, not actions.
            let bindable = ["Action::", "Verb::", "Motion::"].iter().any(|t| result.contains(t));
            if !arm.starts_with('"') || !bindable {
                continue;
            }
            for name in arm.split('|').map(|n| n.trim().trim_matches('"')) {
                if !name.is_empty() {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

/// The "Action names" section of the keys reference: its own text, and the
/// names in the first two columns of its tables.
fn documented_action_names() -> (String, BTreeSet<String>) {
    let page = read("docs/reference/keys.md");
    let start = page.find("\n## Action names").expect("keys.md has an Action names section");
    let rest = &page[start + 1..];
    let section = match rest[3..].find("\n## ") {
        Some(end) => &rest[..end + 3],
        None => rest,
    };
    let mut names = BTreeSet::new();
    for row in section.lines().filter(|l| l.starts_with('|')) {
        for cell in row.split('|').skip(1).take(2) {
            for (i, tok) in cell.split('`').enumerate() {
                let is_name = !tok.is_empty()
                    && tok.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '!');
                if i % 2 == 1 && is_name {
                    names.insert(tok.to_string());
                }
            }
        }
    }
    (section.to_string(), names)
}

/// Every bindable action name is in the keys reference, and every name it
/// lists binds. Before this the names were written down only in the code,
/// and a key can't be rebound to something nobody can name.
#[test]
fn every_action_name_is_documented() {
    let (section, documented) = documented_action_names();
    assert!(!documented.is_empty(), "{KEYS_PAGE}: no documented action names");
    let names = action_names();
    assert!(names.contains("save") && names.contains("operator_delete") && names.contains("move_down"));

    let missing: Vec<_> = names
        .iter()
        .filter(|n| !section.contains(&format!("`{n}`")))
        .collect();
    assert!(missing.is_empty(), "action names not in docs/reference/keys.md: {missing:?}");
    assert!(section.contains("`heading_1`"), "the heading_N actions");

    let bogus: Vec<_> = documented
        .iter()
        .filter(|n| crate::input::bindings::Verb::from_config_name(n).is_none())
        .collect();
    assert!(bogus.is_empty(), "documented action names that don't bind: {bogus:?}");
}

/// The help topics the commands page names are real pages, not the
/// "no such topic" fallback.
#[test]
fn every_documented_help_topic_exists() {
    let page = read("docs/reference/commands.md");
    let line = page.lines().find(|l| l.contains("`:help [topic]`")).expect("the :help row");
    let topics: Vec<&str> = line
        .splitn(3, '|')
        .nth(2)
        .into_iter()
        .flat_map(code_spans)
        .collect();
    assert!(!topics.is_empty(), "no topics found in: {line}");
    for topic in topics {
        assert!(Help::knows(topic), ":help {topic} opens nothing");
    }
}

/// Every `:help <topic>` the help or any doc points a reader to opens a real
/// page, not the "no such topic" fallback.
#[test]
fn every_help_topic_mentioned_exists() {
    let mut texts = all_docs();
    texts.push((":help".to_string(), help_text()));
    let mut unknown = Vec::new();
    for (name, text) in texts {
        for (i, m) in text.match_indices(":help ") {
            let topic: String =
                text[i + m.len()..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if topic.is_empty() {
                continue;
            }
            if !Help::knows(&topic) {
                unknown.push(format!("{name}: :help {topic}"));
            }
        }
    }
    assert!(unknown.is_empty(), "not :help topics: {unknown:?}");
}
