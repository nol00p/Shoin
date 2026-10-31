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

use std::collections::BTreeSet;
use std::path::Path;

use clap::CommandFactory;

use crate::app::App;
use crate::config::Config;
use crate::help::Help;

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

/// `:help` and the commands reference list the same commands.
#[test]
fn help_and_the_commands_page_name_the_same_commands() {
    let help = ex_commands(&help_text());
    let docs = ex_commands(&read("docs/reference/commands.md"));
    let only_help: Vec<_> = help.difference(&docs).collect();
    let only_docs: Vec<_> = docs.difference(&help).collect();
    assert!(
        only_help.is_empty() && only_docs.is_empty(),
        "only in :help: {only_help:?}\nonly in docs/reference/commands.md: {only_docs:?}"
    );
}

/// Every `:command` the help or any doc mentions is one the editor runs.
#[test]
fn every_documented_command_exists() {
    let mut named = ex_commands(&help_text());
    for (_, text) in all_docs() {
        named.extend(ex_commands(&text));
    }
    let unknown: Vec<_> = named.iter().filter(|c| run(c).starts_with("not a command")).collect();
    assert!(unknown.is_empty(), "documented but not commands: {unknown:?}");
}

/// Every `:set` option the help or any doc mentions is one `:set` knows.
#[test]
fn every_documented_set_option_exists() {
    let mut named = set_options(&help_text());
    for (_, text) in all_docs() {
        named.extend(set_options(&text));
    }
    assert!(!named.is_empty(), "the :set parser found nothing");
    let unknown: Vec<_> =
        named.iter().filter(|o| run(&format!("set {o}")).starts_with("unknown option")).collect();
    assert!(unknown.is_empty(), "documented but not :set options: {unknown:?}");
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

/// The keys after `<leader>` in the shipped `keys.conf`, from its live
/// (uncommented) bindings.
fn leader_keys() -> BTreeSet<String> {
    read("shoin/keys.conf")
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("\"<leader>"))
        .filter_map(|l| l.split_once('"').map(|(key, _)| key.to_string()))
        .collect()
}

/// Every shipped leader binding is on the shortcuts page and in the keys
/// reference.
#[test]
fn every_leader_binding_is_documented() {
    let keys = leader_keys();
    assert!(!keys.is_empty(), "no <leader> bindings found in shoin/keys.conf");
    let help = help_text();
    let reference = read("docs/reference/keys.md");
    let mut missing = Vec::new();
    for key in &keys {
        // Followed by a space or the end, so `f` is not found inside `fe`.
        let in_help = help.match_indices(&format!("Space {key}")).any(|(i, m)| {
            help[i + m.len()..].chars().next().is_none_or(char::is_whitespace)
        });
        if !in_help {
            missing.push(format!(":help: Space {key}"));
        }
        if !reference.contains(&format!("`Space {key}`")) {
            missing.push(format!("docs/reference/keys.md: Space {key}"));
        }
    }
    assert!(missing.is_empty(), "leader bindings not documented: {missing:?}");
}

/// The other live bindings in the shipped `keys.conf` — `<C-s>`, `<M-j>` —
/// written the way the docs write keys: `Ctrl-s`, `Alt-j`.
fn other_shipped_keys() -> BTreeSet<String> {
    read("shoin/keys.conf")
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.starts_with("\"<leader>"))
        .filter_map(|l| l.strip_prefix('"')?.split_once('"').map(|(key, _)| key.to_string()))
        .map(|key| {
            let inner = key.trim_start_matches('<').trim_end_matches('>');
            match inner.split_once('-') {
                Some(("C", k)) => format!("Ctrl-{k}"),
                Some(("M", k)) => format!("Alt-{k}"),
                Some(("S", k)) => format!("Shift-{k}"),
                _ => inner.to_string(),
            }
        })
        .collect()
}

/// Every other shipped binding is in the keys reference and in `:help` too.
/// `Ctrl-s` shipped for a long time with neither saying so, because the
/// check above only looks at `<leader>` keys.
#[test]
fn every_shipped_binding_is_documented() {
    let keys = other_shipped_keys();
    assert!(keys.contains("Ctrl-s"), "parsed shoin/keys.conf: {keys:?}");
    let help = help_text();
    let reference = read("docs/reference/keys.md");
    let mut missing = Vec::new();
    for key in &keys {
        if !reference.contains(&format!("`{key}`")) {
            missing.push(format!("docs/reference/keys.md: {key}"));
        }
        if !help.contains(key.as_str()) {
            missing.push(format!(":help: {key}"));
        }
    }
    assert!(missing.is_empty(), "shipped bindings not documented: {missing:?}");
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
        .split('`')
        .skip(3) // the command itself, then the text before the first topic
        .step_by(2)
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
