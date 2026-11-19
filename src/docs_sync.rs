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
    let missing: Vec<_> = expected.difference(actual).collect();
    let extra: Vec<_> = actual.difference(expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{destination} compared with {source}\nmissing: {missing:?}\nunexpected: {extra:?}",
    );
}

type AliasGroups = BTreeSet<(String, BTreeSet<String>)>;

/// COMMANDS is private to app. Read its literal tuples without widening the
/// module's visibility. OPTIONS is available directly.
fn command_table() -> AliasGroups {
    let source = read("src/app/commands.rs");
    let (_, declaration) = source.split_once("const COMMANDS:").expect("COMMANDS declaration");
    let (_, table) = declaration.split_once("= &[").expect("COMMANDS initializer");
    let (table, _) = table.split_once("\n];").expect("end of COMMANDS");
    table.trim().split_terminator("),").map(|entry| {
        let mut names = entry.split('"').skip(1).step_by(2);
        let name = names.next().unwrap_or_else(|| panic!("unrecognized COMMANDS entry: {entry:?}"));
        (name.to_string(), names.map(str::to_string).collect())
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
        .unwrap_or_else(|| panic!("{path}: missing fenced {language} block"))
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

/// The reference's first-column keys, grouped by the modes of each section.
fn documented_keys(page: &str) -> BTreeSet<(&str, &str)> {
    let mut modes: &[&str] = &[];
    let mut keys = BTreeSet::new();
    for line in page.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            modes = match heading {
                "Normal mode" | "Operators" | "Leader keys" => &["normal"],
                "Insert mode" => &["insert"],
                "Visual mode" => &["visual"],
                "Normal and Visual mode" | "Motions" | "Writer verbs" | "Lists" | "Registers" | "Folds" | "Panes" =>
                    &["normal", "visual"],
                _ => &[],
            };
        }
        if let Some(cell) = line.strip_prefix('|').and_then(|row| row.split('|').next()) {
            for key in code_spans(cell) {
                for &mode in modes {
                    keys.insert((mode, key));
                }
            }
        }
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

fn shipped_bindings(body: &str) -> BTreeSet<(String, String)> {
    let tables = body.parse::<toml::Table>().expect("shoin/keys.conf parses");
    tables["keys"].as_table().unwrap().iter().flat_map(|(mode, bindings)| {
        bindings.as_table().unwrap().keys().map(|key| (mode.clone(), shipped_key_name(key)))
    }).collect()
}

fn missing_bindings(
    page: &str, expected: &BTreeSet<(String, String)>,
) -> Vec<String> {
    let documented = documented_keys(page);
    expected.iter().filter(|(mode, key)| !documented.contains(&(mode.as_str(), key.as_str())))
        .map(|(mode, key)| format!("{mode}: {key}"))
        .collect()
}

#[test]
fn the_keys_page_lists_shipped_keys_in_their_modes() {
    let keys = shipped_bindings(&read("shoin/keys.conf"));
    let missing = missing_bindings(&read(KEYS_PAGE), &keys);
    assert!(missing.is_empty(), "{KEYS_PAGE} missing shoin/keys.conf bindings by mode: {missing:?}");

    let help = help_text();
    for (_, key) in keys {
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

/// Read the character patterns of the window-command dispatcher.
fn window_keys() -> BTreeSet<char> {
    let source = read("src/app/tree.rs");
    let (_, handler) = source.split_once("match key {").expect("window_command dispatch");
    let (arms, _) = handler.split_once("            _ =>").expect("window_command fallback");
    arms.lines().filter_map(|line| line.split_once("=>")).flat_map(|(pattern, _)| {
        pattern.trim().split(" | ").filter_map(|part| {
            let c = part.strip_prefix('\'')?.strip_suffix('\'')?;
            let mut chars = c.chars();
            chars.next().filter(|_| chars.next().is_none())
        })
    }).collect()
}

/// Read the named keys and explicit control chords in the Insert dispatcher.
fn insert_keys() -> BTreeSet<String> {
    let source = read("src/app/insert.rs");
    let (_, body) = source.split_once("fn on_key_insert(").expect("Insert handler");
    let (_, arms) = body.split_once("match key.code {").expect("Insert key dispatch");
    arms.lines().filter_map(|line| line.split_once("=>")).flat_map(|(pattern, _)| {
        pattern.trim().split(" | ").filter_map(|arm| {
            let key = arm.strip_prefix("KeyCode::")?.split_whitespace().next()?;
            match key {
                "BackTab" => Some("Shift-Tab".to_string()),
                _ if key.starts_with("Char(") => {
                    let letter = key.strip_prefix("Char('")?.strip_suffix("')")?;
                    arm.contains("KeyModifiers::CONTROL").then(|| format!("Ctrl-{letter}"))
                }
                _ => Some(key.to_string()),
            }
        })
    }).collect()
}

#[test]
fn the_keys_page_lists_builtin_keys_in_their_modes() {
    let missing = missing_bindings(&read(KEYS_PAGE), &builtin_bindings());
    assert!(missing.is_empty(), "{KEYS_PAGE} missing built-in bindings by mode: {missing:?}");
}

/// The resolved defaults expose the fixed schema fields through Debug, the
/// same representation used by --print-config and the shipped equality test.
fn default_config_fields() -> BTreeSet<String> {
    let config = format!("{:#?}", Config::default());
    let mut fields = BTreeSet::new();
    let mut section = "";
    for line in config.lines() {
        if let Some(field) = debug_field(line, 4) {
            section = field;
        }
        // Key bindings and theme overrides are maps with their own vocabulary.
        if !matches!(section, "keys" | "theme") {
            if let Some(field) = debug_field(line, 8) {
                fields.insert(format!("{section}.{field}"));
            }
        }
    }

    // Theme overrides are a flat map, so its dispatchers name the fields
    // that have defaults. theme.name is optional and has no authored value.
    let theme = read("src/render/theme.rs");
    for (function, section) in [("set_color", "theme"), ("set_style", "theme.styles")] {
        let marker = format!("fn {function}(");
        let (_, body) = theme.split_once(&marker).expect("theme dispatcher");
        let (arms, _) = body.split_once("            _ =>").expect("theme dispatcher fallback");
        for (pattern, _) in arms.lines().filter_map(|line| line.split_once("=>")) {
            if let Some(field) = pattern.trim().strip_prefix('"').and_then(|part| part.strip_suffix('"')) {
                fields.insert(format!("{section}.{field}"));
            }
        }
    }
    fields.insert("theme.indent_colors".to_string());
    fields
}

fn debug_field(line: &str, indent: usize) -> Option<&str> {
    if line.len() - line.trim_start().len() != indent {
        return None;
    }
    line[indent..].split_once(": ").map(|(field, _)| field)
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
    for (field, value) in config_entries(&table) {
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
