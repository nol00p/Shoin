//! Key sequence -> Action lookup, built from defaults + config overrides.
//! SPEC.md §7.4.
//!
//! RESOLUTION RULE: an exact match wins the moment it is complete — `lookup`
//! answers `Found` before it answers `Prefix`. So there is no ambiguity to
//! resolve on a timer, and no keystroke is ever held back waiting for one.
//! The cost is that a binding which is also the start of a LONGER binding
//! shadows it, and that is reported as a warning at load time rather than
//! left to be discovered by a key that stopped working.
//!
//! Duplicate sequences need no rule of their own: within one table TOML makes
//! them a parse error, and across merged `.conf` files later-wins is the whole
//! point of the conf.d layering.

use std::collections::HashMap;

use super::bindings::Verb;
use super::pending::Key;
use crate::config::keys::parse_seq;
use crate::config::schema::KeysConfig;

pub type KeySeq = Vec<Key>;

/// User-defined key bindings, overlaid on the built-in grammar in `bindings`.
/// Only sequences present here override the defaults; everything else falls
/// through to that table.
///
/// A binding names a `Verb`, not just an `Action`, so config can move an
/// OPERATOR or a MOTION — `"x" = "operator_delete"` keeps its whole grammar,
/// counts, objects and all.
#[derive(Default)]
pub struct Keymap {
    normal: HashMap<KeySeq, Verb>,
    insert: HashMap<KeySeq, Verb>,
    visual: HashMap<KeySeq, Verb>,
}

pub enum Lookup<'a> {
    /// Exact match.
    Found(&'a Verb),
    /// A strict prefix of one or more bindings — keep collecting.
    Prefix,
    /// Nothing matches.
    None,
}

impl Keymap {
    /// Build from the `[keys.*]` tables. A binding that fails to parse or names
    /// an unknown action is SKIPPED, not fatal — one typo must never silently
    /// wipe out every other binding. Collected warnings are returned for display.
    pub fn from_config(cfg: &KeysConfig, leader: &str) -> (Keymap, Vec<String>) {
        let mut warnings = Vec::new();
        let mut table = |src: &HashMap<String, String>, mode: &str| -> HashMap<KeySeq, Verb> {
            let mut map = HashMap::new();
            // Kept alongside so a warning can quote the binding the way the
            // user wrote it, rather than a re-rendering of it.
            let mut written: Vec<(KeySeq, &str)> = Vec::new();
            for (keys, action) in src {
                let seq = match parse_seq(keys, leader) {
                    Ok(s) => s,
                    Err(e) => {
                        warnings.push(format!("key {keys:?}: {e}"));
                        continue;
                    }
                };
                match Verb::from_config_name(action) {
                    Some(verb) => {
                        written.push((seq.clone(), keys));
                        map.insert(seq, verb);
                    }
                    None => warnings.push(format!("key {keys:?}: unknown action {action:?}")),
                }
            }
            // An exact match fires immediately (see the module header), so a
            // binding that is also the START of a longer one makes the longer
            // one unreachable. Report it — silently dead bindings are the
            // hardest kind of config to debug.
            for (seq, raw) in &written {
                if let Some((_, longer)) = written
                    .iter()
                    .find(|(k, _)| k.len() > seq.len() && k[..seq.len()] == seq[..])
                {
                    warnings.push(format!(
                        "keys.{mode}: {raw:?} fires on its own, so {longer:?} can never run"
                    ));
                }
            }
            map
        };
        let keymap = Keymap {
            normal: table(&cfg.normal, "normal"),
            insert: table(&cfg.insert, "insert"),
            visual: table(&cfg.visual, "visual"),
        };
        (keymap, warnings)
    }

    /// The `[keys.insert]` verb bound to one key, if any.
    ///
    /// Insert mode looks up SINGLE keys only: a multi-key sequence would have
    /// to hold characters back from the buffer while it waited, and the one
    /// sequence Insert mode does have — `input.escape_alias` — is deliberately
    /// not a keymap entry for exactly that reason (SPEC.md §7.2).
    pub fn insert_key(&self, key: Key) -> Option<&Verb> {
        self.insert.get(std::slice::from_ref(&key))
    }

    fn table(&self, name: &str) -> &HashMap<KeySeq, Verb> {
        match name {
            "insert" => &self.insert,
            "visual" => &self.visual,
            _ => &self.normal,
        }
    }

    /// Resolve a partial sequence: an exact binding, a strict prefix of one (so
    /// the caller keeps collecting), or nothing.
    pub fn lookup(&self, table: &str, seq: &[Key]) -> Lookup<'_> {
        let map = self.table(table);
        if let Some(action) = map.get(seq) {
            return Lookup::Found(action);
        }
        if map.keys().any(|k| k.len() > seq.len() && &k[..seq.len()] == seq) {
            return Lookup::Prefix;
        }
        Lookup::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(pairs: &[(&str, &str)]) -> KeysConfig {
        KeysConfig {
            normal: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..KeysConfig::default()
        }
    }

    /// A binding that is also the start of a longer one shadows it, because an
    /// exact match fires immediately. That has to be reported, or the longer
    /// binding is silently dead.
    #[test]
    fn a_shadowing_binding_is_reported() {
        let (_, warnings) = Keymap::from_config(
            &keys(&[("<leader>f", "save"), ("<leader>ff", "find_file")]),
            " ",
        );
        assert!(
            warnings.iter().any(|w| w.contains("can never run")),
            "expected a shadowing warning, got {warnings:?}"
        );
    }

    /// Sibling bindings under one prefix are not shadowing — neither is a
    /// prefix of the other.
    #[test]
    fn siblings_under_a_prefix_are_fine() {
        let (_, warnings) = Keymap::from_config(
            &keys(&[("<leader>ff", "find_file"), ("<leader>fb", "find_buffer")]),
            " ",
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    /// The shipped defaults must not warn about themselves.
    #[test]
    fn the_default_bindings_are_clean() {
        let cfg = crate::config::Config::default();
        let (_, warnings) = Keymap::from_config(&cfg.keys, &cfg.input.leader);
        assert!(warnings.is_empty(), "default keys warn: {warnings:?}");
    }
}
