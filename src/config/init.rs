//! Seeding a user config. SPEC.md §8.
//!
//! `cargo install` has no post-install hook, and a build script may only write
//! to `OUT_DIR` — it also runs in sandboxes, in CI and on cross-compiles, often
//! as another user. So there is no honest way for INSTALLING to create a config
//! directory, and this is the explicit step that does it instead.
//!
//! Explicit on purpose. `fs/save.rs` states the rule this module could easily
//! have broken: a writing tool should not surprise the user with writes they
//! did not ask for. Seeding on first launch would have been exactly such a
//! write, and into their home directory rather than their document. So the
//! editor never writes a config on its own — it says the command exists (see
//! `render::splash`) and waits to be asked.
//!
//! The files are EMBEDDED rather than read from `shoin/` at run time, because
//! after `cargo install` the repository this was built from is not there any
//! more — and may never have been on this machine at all.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The shipped `shoin/*.conf`, compiled into the binary.
///
/// `the_embedded_set_is_the_whole_shipped_directory` holds this list to the
/// directory it came from: adding an eighth `.conf` without adding it here
/// would otherwise seed a config quietly missing a section.
pub const SHIPPED: &[(&str, &str)] = &[
    ("editor.conf", include_str!("../../shoin/editor.conf")),
    ("glyphs.conf", include_str!("../../shoin/glyphs.conf")),
    ("input.conf", include_str!("../../shoin/input.conf")),
    ("keys.conf", include_str!("../../shoin/keys.conf")),
    ("layout.conf", include_str!("../../shoin/layout.conf")),
    ("theme.conf", include_str!("../../shoin/theme.conf")),
    ("tree.conf", include_str!("../../shoin/tree.conf")),
];

/// Where a seeded config goes: the user's own config directory.
///
/// Deliberately NOT the cwd-relative candidates `discover` also looks at.
/// Writing `./shoin/` into whatever directory you happened to be standing in
/// would be a surprise, and a lasting one — every later run from that
/// directory would silently prefer it over the real config.
pub fn target_dir() -> Option<PathBuf> {
    // Mirrors the user-level half of `config::candidates`, in the same order,
    // so what this writes is what discovery finds.
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("shoin"));
        }
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".config").join("shoin"))
}

/// What a seed did, so the caller can report it without re-deriving it.
#[derive(Debug)]
pub struct Seeded {
    pub dir: PathBuf,
    pub written: Vec<&'static str>,
    /// Files that were already there and were left alone. Only ever non-empty
    /// with `force`, which overwrites — so this is what `force` REPLACED.
    pub replaced: Vec<&'static str>,
}

/// Write the shipped config into `dir`.
///
/// Refuses a directory that already holds a `.conf` unless `force`: someone's
/// edited config is the last thing an editor should overwrite for them.
pub fn seed(dir: &Path, force: bool) -> Result<Seeded> {
    let existing: Vec<&'static str> = SHIPPED
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| dir.join(name).exists())
        .collect();

    // Any `.conf` at all, not just ours: a config split up under names of the
    // reader's own choosing is still a config, and still theirs.
    let occupied = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("conf"))
        })
        .unwrap_or(false);

    if occupied && !force {
        bail!(
            "{} already has a config — nothing written (use --force to replace it)",
            dir.display()
        );
    }

    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating {}", dir.display()))?;

    let mut written = Vec::new();
    for (name, body) in SHIPPED {
        let path = dir.join(name);
        std::fs::write(&path, body)
            .with_context(|| format!("writing {}", path.display()))?;
        written.push(*name);
    }

    Ok(Seeded {
        dir: dir.to_path_buf(),
        written,
        replaced: if force { existing } else { Vec::new() },
    })
}

/// `--init-config`: seed the user's config directory and say what happened.
pub fn run(force: bool) -> Result<()> {
    let dir = target_dir()
        .context("no HOME or XDG_CONFIG_HOME to put a config in — use --config <path>")?;
    let done = seed(&dir, force)?;

    println!("wrote {} files to {}", done.written.len(), done.dir.display());
    for name in &done.written {
        println!("  {name}");
    }
    if !done.replaced.is_empty() {
        println!("replaced: {}", done.replaced.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("shoin-init-{tag}-{t}-{n}"))
    }

    /// The embedded set must BE the shipped directory. Adding a `.conf` to
    /// `shoin/` without adding it here would seed a config missing a section,
    /// and nothing else would notice.
    #[test]
    fn the_embedded_set_is_the_whole_shipped_directory() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("shoin");
        let mut on_disk: Vec<String> = std::fs::read_dir(&src)
            .expect("shoin/ ships with the repo")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".conf"))
            .collect();
        on_disk.sort();

        let mut embedded: Vec<String> =
            SHIPPED.iter().map(|(n, _)| (*n).to_string()).collect();
        embedded.sort();

        assert_eq!(embedded, on_disk, "SHIPPED has drifted from shoin/");

        // …and byte-identical, not merely present.
        for (name, body) in SHIPPED {
            let disk = std::fs::read_to_string(src.join(name)).unwrap();
            assert_eq!(*body, disk, "{name} differs from the shipped file");
        }
    }

    /// What is seeded has to be a config the editor accepts — parse AND
    /// validate, the same two bars `the_shipped_config_directory_parses` holds
    /// the source directory to.
    #[test]
    fn a_seeded_directory_loads_cleanly() {
        let d = dir("load");
        let done = seed(&d, false).unwrap();
        assert_eq!(done.written.len(), SHIPPED.len());
        assert!(done.replaced.is_empty());

        let cfg = super::super::load(Some(&d)).expect("the seeded config loads");
        let warnings = super::super::validate(&cfg);
        assert!(warnings.is_empty(), "seeded config warns: {warnings:?}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// An existing config is never overwritten by accident — the whole point of
    /// this being a command rather than something startup does.
    #[test]
    fn an_existing_config_is_refused_without_force() {
        let d = dir("refuse");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("mine.conf"), "[layout]\nmeasure = 55\n").unwrap();

        let err = seed(&d, false).unwrap_err().to_string();
        assert!(err.contains("already has a config"), "{err}");
        assert!(err.contains("--force"), "the message says how to proceed: {err}");
        // Untouched.
        assert_eq!(
            std::fs::read_to_string(d.join("mine.conf")).unwrap(),
            "[layout]\nmeasure = 55\n"
        );
        assert!(!d.join("editor.conf").exists(), "nothing was written");

        // `--force` replaces ours and reports it, but leaves a file of theirs
        // that we do not ship.
        let done = seed(&d, true).unwrap();
        assert!(done.replaced.is_empty(), "none of OURS were there before");
        assert!(d.join("editor.conf").exists());
        assert!(d.join("mine.conf").exists(), "a file we do not ship is left alone");
        std::fs::remove_dir_all(&d).ok();
    }

    /// Seeding twice with `--force` reports what it replaced.
    #[test]
    fn force_reports_what_it_replaced() {
        let d = dir("force");
        seed(&d, false).unwrap();
        let again = seed(&d, true).unwrap();
        assert_eq!(again.replaced.len(), SHIPPED.len());
        std::fs::remove_dir_all(&d).ok();
    }

    /// The target follows `XDG_CONFIG_HOME` when it is set, because discovery
    /// looks there first — seeding somewhere discovery checks later would write
    /// a config that never gets read.
    #[test]
    fn the_target_is_where_discovery_will_look() {
        let dir = target_dir().expect("a home on this machine");
        assert!(dir.ends_with("shoin"), "{}", dir.display());
        assert!(dir.is_absolute(), "{}", dir.display());
    }
}
