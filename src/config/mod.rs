//! Config discovery, parsing, and hot reload. SPEC.md §8.
//!
//! A config location is either a single `.conf` file or a DIRECTORY of `.conf`
//! files (conf.d style) — the standard `~/.config/<app>/*.conf` layout. When a
//! directory is used, its `*.conf` files are read in sorted order and deep-merged
//! (later keys win), so settings can be split by category (`theme.conf`,
//! `keys.conf`, …). Missing keys fall back to defaults, so any subset is valid.

pub mod init;
pub mod keys;
pub mod schema;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub use schema::Config;

/// The config files to merge, and the path to watch for hot reload.
struct Located {
    files: Vec<PathBuf>,
    watch: PathBuf,
}

/// The `.conf` file(s) a candidate path resolves to: itself if a file, its
/// sorted `*.conf` children if a directory, nothing otherwise.
fn resolve(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "conf"))
            .collect();
        files.sort();
        return files;
    }
    Vec::new()
}

/// Candidate locations, first that resolves to any file wins:
///   1. `--config <path>`            (file or directory; used alone if given)
///   2. `$SHOIN_CONFIG`       (file or directory)
///   3. `./shoin.conf`        (project-local single file)
///   4. `./shoin/`            (project-local conf.d directory)
///   5. `$XDG_CONFIG_HOME/shoin/`
///   6. `~/.config/shoin/`
fn candidates(explicit: Option<&Path>) -> Vec<PathBuf> {
    if let Some(p) = explicit {
        return vec![p.to_path_buf()];
    }
    let mut c = Vec::new();
    if let Ok(p) = std::env::var("SHOIN_CONFIG") {
        c.push(PathBuf::from(p));
    }
    c.push(PathBuf::from("shoin.conf"));
    c.push(PathBuf::from("shoin"));
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        c.push(PathBuf::from(xdg).join("shoin"));
    }
    if let Ok(home) = std::env::var("HOME") {
        c.push(PathBuf::from(home).join(".config").join("shoin"));
    }
    c
}

fn located(explicit: Option<&Path>) -> Option<Located> {
    for cand in candidates(explicit) {
        let files = resolve(&cand);
        if !files.is_empty() {
            return Some(Located { files, watch: cand });
        }
    }
    None
}

/// The resolved config files (empty = built-in defaults).
pub fn discover(explicit: Option<&Path>) -> Vec<PathBuf> {
    located(explicit).map(|l| l.files).unwrap_or_default()
}

/// Resolve and merge the config, falling back to defaults for anything absent.
pub fn load(explicit: Option<&Path>) -> Result<Config> {
    merge_files(&discover(explicit))
}

/// Parse one TOML string over the defaults. Every real config path merges a
/// directory, so this exists for tests that want a config in one literal.
#[cfg(test)]
pub fn parse(text: &str) -> Result<Config> {
    from_table(toml::from_str(text)?)
}

/// Deserialize the merged table, then run the one post-merge pass — settings
/// whose value depends on ANOTHER setting, which serde cannot resolve while it
/// fills in defaults. Every `Config` in the program comes through here, hot
/// reload included.
///
/// The raw table is what makes that possible: it still knows which keys the
/// user actually wrote, a distinction the filled-in struct has lost.
fn from_table(table: toml::Table) -> Result<Config> {
    let set: std::collections::HashSet<String> = table
        .get("glyphs")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    let mut cfg: Config = toml::Value::Table(table)
        .try_into()
        .context("resolving merged config")?;
    cfg.glyphs.apply_ascii_fallback(|key| set.contains(key));
    Ok(cfg)
}

/// Settings whose value is one word out of a fixed set.
///
/// Every one of these parsers is LENIENT — it has to be, or one stale value in
/// one file would refuse the whole config, which is the leniency `[keys.*]`
/// already gets. The cost of leniency is silence: `cursor.normal = "blok"` is a
/// block cursor and no explanation. So the fallback stays and this makes it
/// visible, through the same startup/reload flash a bad keybinding uses.
///
/// This exists because `editor.final_newline` was documented, shipped, and read
/// by nothing at all — the kind of rot that is only ever found by asking every
/// setting, in one place, whether anyone understood it.
pub fn validate(cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    let mut check = |name: &str, value: &str, ok: bool, legal: &str| {
        if !ok {
            out.push(format!("{name}: {legal}, not {value:?}"));
        }
    };

    check(
        "editor.final_newline",
        &cfg.editor.final_newline,
        crate::fs::save::FinalNewline::parse(&cfg.editor.final_newline).is_some(),
        "preserve · always · never",
    );
    {
        // A number, so the value shown is the number that was written — the
        // point of the message is that 9 is not one of five, not that "9" is
        // an unknown word.
        let n = cfg.editor.autosave_interval;
        check(
            "editor.autosave_interval",
            &n.to_string(),
            crate::fs::save::AutosaveInterval::parse(n).is_some(),
            "1 to 5 minutes",
        );
    }
    check(
        "layout.focus",
        &cfg.layout.focus,
        crate::render::focus::FocusMode::parse(&cfg.layout.focus).is_some(),
        "off · paragraph · sentence",
    );
    check(
        "layout.align",
        &cfg.layout.align,
        matches!(cfg.layout.align.trim(), "left" | "center" | "centre"),
        "left · center",
    );
    check(
        "transclude.embed",
        &cfg.transclude.embed,
        crate::transclude::Mode::parse(&cfg.transclude.embed).is_some(),
        "none · short · rec · full",
    );
    for (name, value) in [
        ("cursor.normal", &cfg.cursor.normal),
        ("cursor.insert", &cfg.cursor.insert),
        ("cursor.visual", &cfg.cursor.visual),
    ] {
        check(
            name,
            value,
            crate::app::CursorShape::parse(value).is_some(),
            "block · bar · underline",
        );
    }
    out
}

/// Deep-merge a set of `.conf` files (in order) into a single `Config`.
fn merge_files(files: &[PathBuf]) -> Result<Config> {
    if files.is_empty() {
        return Ok(Config::default());
    }
    let mut merged = toml::Table::new();
    for f in files {
        let text = std::fs::read_to_string(f)
            .with_context(|| format!("reading config {}", f.display()))?;
        let table: toml::Table =
            toml::from_str(&text).with_context(|| format!("parsing config {}", f.display()))?;
        merge_tables(&mut merged, table);
    }
    from_table(merged)
}

/// Recursively merge `over` into `base`; a nested table merges key-by-key, any
/// other value replaces.
fn merge_tables(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(bt)), toml::Value::Table(ot)) => merge_tables(bt, ot),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Watches the resolved config location with `notify`. Watching a directory
/// catches any `.conf` file being edited, added, or removed.
///
/// On change the caller re-runs [`reload`], which re-discovers and merges; a
/// candidate is swapped in only if it parses. A broken config never takes down a
/// running editor holding unsaved text.
pub struct ConfigWatcher {
    explicit: Option<PathBuf>,
    rx: std::sync::mpsc::Receiver<()>,
    _watcher: notify::RecommendedWatcher,
}

impl ConfigWatcher {
    /// Begin watching the config location for `explicit` (or the discovered
    /// default). `Ok(None)` when there is nothing to watch (running on defaults).
    pub fn new(explicit: Option<PathBuf>) -> Result<Option<Self>> {
        use notify::Watcher;
        let Some(located) = located(explicit.as_deref()) else {
            return Ok(None);
        };
        // Watch the DIRECTORY, not the file. Editors that save by writing a
        // temp file and renaming it over the original leave the watched inode
        // behind, so a file watch goes deaf after the first such save; the
        // directory sees the rename. Events are filtered back down to the
        // config path, so unrelated files in the directory cost nothing.
        let (watch_dir, only) = match located.watch.is_dir() {
            true => (located.watch.clone(), None),
            false => match located.watch.parent() {
                Some(dir) if !dir.as_os_str().is_empty() => {
                    (dir.to_path_buf(), Some(located.watch.clone()))
                }
                _ => (located.watch.clone(), None),
            },
        };

        // Filter by FILE NAME, not the whole path: the watch is one
        // non-recursive directory, and platform watchers report canonicalized
        // paths (on macOS `/var/...` comes back as `/private/var/...`), which
        // no full-path comparison would ever match.
        let (tx, rx) = std::sync::mpsc::channel();
        let filter = only.as_ref().and_then(|p| p.file_name().map(|n| n.to_owned()));
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            let ours = match &filter {
                Some(name) => event.paths.iter().any(|p| p.file_name() == Some(name.as_os_str())),
                None => true,
            };
            if ours {
                let _ = tx.send(());
            }
        })
        .context("starting config watcher")?;
        watcher
            .watch(&watch_dir, notify::RecursiveMode::NonRecursive)
            .with_context(|| format!("watching {}", watch_dir.display()))?;
        Ok(Some(ConfigWatcher {
            explicit,
            rx,
            _watcher: watcher,
        }))
    }

    /// True if anything changed since the last check (drains coalesced events).
    pub fn changed(&self) -> bool {
        let mut changed = false;
        while self.rx.try_recv().is_ok() {
            changed = true;
        }
        changed
    }

    /// Re-discover and merge the config. `Ok` only if every file parses.
    pub fn reload(&self) -> Result<Config> {
        load(self.explicit.as_deref())
    }

    /// The directory the config was read from, so a setting that names a file
    /// (`[splash] art`) can resolve a relative path against it.
    pub fn config_dir(&self) -> Option<PathBuf> {
        discover(self.explicit.as_deref())
            .first()
            .and_then(|f| f.parent().map(|p| p.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmpdir(tag: &str) -> PathBuf {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("shoin-cfg-{tag}-{t}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn merges_conf_files_across_categories() {
        let dir = tmpdir("merge");
        std::fs::write(dir.join("layout.conf"), "[layout]\nmeasure = 100\n").unwrap();
        std::fs::write(
            dir.join("input.conf"),
            "[input]\nmouse = false\n[theme]\ntext = \"#010203\"\n",
        )
        .unwrap();

        let cfg = load(Some(&dir)).unwrap();
        assert_eq!(cfg.layout.measure, 100); // from layout.conf
        assert!(!cfg.input.mouse); // from input.conf
        assert!(cfg.theme.colors.contains_key("text")); // [theme] merged in
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every `.conf` this repo ships must actually parse.
    ///
    /// It once did not: `transclude.embed` changed from a bool to a mode
    /// string and `shoin/input.conf` still said `false`, which refused the
    /// WHOLE config at startup. Nothing tested the shipped files as files.
    #[test]
    fn the_shipped_config_directory_parses() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shoin");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("shoin/ ships with the repo") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("conf") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            parse(&text).unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
            seen += 1;
        }
        assert!(seen >= 5, "expected the shipped conf.d, found {seen} files");

        // …and merged together, which is how it is actually loaded.
        let files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("conf"))
            .collect();
        let merged = merge_files(&files).expect("the shipped directory merges");
        // Parsing is not enough: every enumerated value in the shipped files
        // must also be a word the editor knows, or the demo config warns on
        // startup about itself.
        let warnings = validate(&merged);
        assert!(warnings.is_empty(), "the shipped config warns: {warnings:?}");
    }

    #[test]
    fn later_file_overrides_earlier_within_a_section() {
        let dir = tmpdir("override");
        // Sorted order: a before z, so z wins the shared key.
        std::fs::write(dir.join("a.conf"), "[layout]\nmeasure = 50\nalign = \"left\"\n").unwrap();
        std::fs::write(dir.join("z.conf"), "[layout]\nmeasure = 90\n").unwrap();

        let cfg = load(Some(&dir)).unwrap();
        assert_eq!(cfg.layout.measure, 90); // overridden
        assert_eq!(cfg.layout.align, "left"); // untouched key survives the merge
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `nerd_fonts = false` is a real setting, not decoration: it swaps in the
    /// ASCII glyph set — but never over a glyph the user chose by hand.
    #[test]
    fn nerd_fonts_off_falls_back_to_ascii() {
        let cfg = parse("[glyphs]\nnerd_fonts = false\n").unwrap();
        assert_eq!(cfg.glyphs.task_todo, "[ ]");
        assert_eq!(cfg.glyphs.quote_bar, "|");
        assert_eq!(cfg.glyphs.folder, "", "no Nerd Font, no folder icon");
        assert!(
            !format!("{:?}", cfg.glyphs).chars().any(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)),
            "the ASCII set must contain no Private Use Area glyphs"
        );

        // A hand-set glyph survives the fallback.
        let cfg = parse("[glyphs]\nnerd_fonts = false\nquote_bar = \"▎\"\n").unwrap();
        assert_eq!(cfg.glyphs.quote_bar, "▎");
        assert_eq!(cfg.glyphs.task_todo, "[ ]", "…but the rest still fall back");

        // Left on, nothing changes.
        let cfg = parse("[glyphs]\nnerd_fonts = true\n").unwrap();
        assert_eq!(cfg.glyphs.task_todo, "☐");
    }

    /// The fallback has to survive the conf.d merge and hot reload, not just
    /// `parse` — both paths land in `finish`.
    #[test]
    fn ascii_fallback_survives_the_merge() {
        let dir = tmpdir("ascii");
        std::fs::write(dir.join("glyphs.conf"), "[glyphs]\nnerd_fonts = false\n").unwrap();
        std::fs::write(dir.join("layout.conf"), "[layout]\nmeasure = 70\n").unwrap();

        let cfg = load(Some(&dir)).unwrap();
        assert_eq!(cfg.glyphs.rule, "-");
        assert_eq!(cfg.layout.measure, 70);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Most editors save by writing a temp file and renaming it into place,
    /// which replaces the inode. Watching the directory is what keeps hot
    /// reload alive across that; watching the file itself goes deaf.
    #[test]
    fn watcher_survives_a_save_by_rename() {
        let dir = tmpdir("rename");
        let conf = dir.join("shoin.conf");
        std::fs::write(&conf, "[layout]\nmeasure = 60\n").unwrap();

        let watcher = ConfigWatcher::new(Some(conf.clone()))
            .unwrap()
            .expect("a watcher for an explicit path");
        // Drain whatever the initial write produced.
        std::thread::sleep(Duration::from_millis(200));
        let _ = watcher.changed();

        let tmp = dir.join("shoin.conf.tmp");
        std::fs::write(&tmp, "[layout]\nmeasure = 42\n").unwrap();
        std::fs::rename(&tmp, &conf).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut seen = false;
        while std::time::Instant::now() < deadline && !seen {
            seen = watcher.changed();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(seen, "a rename over the config should wake the watcher");
        assert_eq!(watcher.reload().unwrap().layout.measure, 42);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The shipped defaults must all be words the editor understands — this is
    /// what stops a default and its parser drifting apart.
    #[test]
    fn the_defaults_are_all_legal() {
        let warnings = validate(&Config::default());
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// An out-of-range `autosave_interval` is a NUMBER, so it gets the same
    /// treatment as a misspelt word: the config still loads, the timer still
    /// runs at the default, and the number is named on startup. Refusing the
    /// whole file over one setting is what the leniency rule above forbids.
    #[test]
    fn an_impossible_autosave_interval_is_named_not_fatal() {
        let cfg = parse("[editor]\nautosave = true\nautosave_interval = 9\n")
            .expect("out of range must not refuse the config");
        assert_eq!(cfg.editor.autosave_interval, 9, "kept as written");
        let w = validate(&cfg);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("editor.autosave_interval"), "{w:?}");
        assert!(w[0].contains("1 to 5"), "the message names the range: {w:?}");

        // And the timer runs at the default rather than at 9 minutes.
        assert_eq!(
            crate::fs::save::Autosave::from_config(&cfg.editor).interval(),
            Some(std::time::Duration::from_secs(180))
        );

        // 0 is the other end of the same mistake.
        let cfg = parse("[editor]\nautosave_interval = 0\n").unwrap();
        assert_eq!(validate(&cfg).len(), 1);
    }

    /// A misspelt setting is REPORTED. It still falls back, but silently
    /// falling back is how `editor.final_newline` went a whole release unread.
    #[test]
    fn a_misspelt_setting_is_named() {
        let cfg = parse(
            "[editor]\nfinal_newline = \"alway\"\n\n[cursor]\nnormal = \"blok\"\n\n[layout]\nfocus = \"paragrph\"\nalign = \"centre\"\n",
        )
        .expect("a typo must not refuse the whole config");
        let w = validate(&cfg);
        assert_eq!(w.len(), 3, "align = centre is legal: {w:?}");
        assert!(w.iter().any(|s| s.starts_with("editor.final_newline")), "{w:?}");
        assert!(w.iter().any(|s| s.starts_with("cursor.normal")), "{w:?}");
        assert!(w.iter().any(|s| s.starts_with("layout.focus")), "{w:?}");
        assert!(w[0].contains("preserve"), "the message lists the legal words: {w:?}");
    }

}
