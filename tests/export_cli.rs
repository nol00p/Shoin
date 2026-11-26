#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Vault(PathBuf);

impl Vault {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("shoin-export-{name}-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shoin"));
        command.current_dir(&self.0).env_remove("SHOIN_CONFIG");
        command
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn batch_config_warnings_stay_on_stderr() {
    let vault = Vault::new("warnings");
    fs::write(vault.0.join("note.md"), "hello\n").unwrap();
    fs::create_dir(vault.0.join("shoin")).unwrap();
    fs::write(vault.0.join("shoin/input.conf"), "[markdown]\nmermaid_renderer = \"tools/merman-cli\"\n").unwrap();
    for (args, warning) in [
        (&[][..], "set mermaid_renderer in user config or with --config"),
        (&["--config", "shoin"][..], "mermaid_renderer needs a PATH name"),
    ] {
        let out = vault.command().args(args)
            .args(["--export", "note.md", "--format", "md", "--stdout"]).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(out.stdout, b"hello\n");
        assert!(String::from_utf8_lossy(&out.stderr).contains(warning), "{out:?}");
    }
}

#[test]
fn interrupted_pdf_reaps_pandoc_and_preserves_existing_output() {
    let vault = Vault::new("cancel");
    fs::write(vault.0.join("note.md"), "hello\n").unwrap();
    fs::write(vault.0.join("note.pdf"), "original\n").unwrap();
    for name in ["typst", "pandoc"] {
        let script = vault.0.join(name);
        fs::write(&script, r#"#!/bin/sh
sleep 30 &
engine=$!
printf '%s %s\n' "$$" "$engine" > pids.tmp
mv pids.tmp pids
wait "$engine"
while [ "$1" != --output ]; do shift; done
echo converted > "$2"
"#).unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut paths = vec![vault.0.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut shoin = vault.command().env("PATH", std::env::join_paths(paths).unwrap())
        .args(["--export", "note.md", "--format", "pdf"])
        .stdout(Stdio::null()).spawn().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let pids = loop {
        if let Ok(pids) = fs::read_to_string(vault.0.join("pids")) { break pids }
        assert!(Instant::now() < deadline, "pandoc did not start");
        std::thread::sleep(Duration::from_millis(10));
    };
    let pids: Vec<libc::pid_t> = pids.split_whitespace().map(|pid| pid.parse().unwrap()).collect();
    // SAFETY: signal the Shoin child spawned above.
    assert_eq!(unsafe { libc::kill(shoin.id() as libc::pid_t, libc::SIGINT) }, 0);
    let status = loop {
        if let Some(status) = shoin.try_wait().unwrap() { break status }
        if Instant::now() >= deadline {
            shoin.kill().unwrap();
            shoin.wait().unwrap();
            panic!("Shoin did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let pandoc_alive = alive(pids[0]);
    let engine_alive = if cfg!(target_os = "linux") {
        // An orphan killed with its parent can briefly be a zombie under PID 1.
        fs::read_to_string(Path::new("/proc").join(pids[1].to_string()).join("stat"))
            .is_ok_and(|stat| !stat.rsplit_once(") ").unwrap().1.starts_with('Z'))
    } else {
        alive(pids[1])
    };
    for pid in pids {
        // SAFETY: clean up the two fixture processes if the regression occurs.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert_eq!(status.code(), Some(128 + libc::SIGINT));
    assert!(!pandoc_alive, "pandoc survived or was not reaped before shutdown");
    assert!(!engine_alive, "layout engine survived shutdown");
    assert_eq!(fs::read_to_string(vault.0.join("note.pdf")).unwrap(), "original\n");
}

fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal zero probes the process without changing it.
    unsafe { libc::kill(pid, 0) == 0 }
}
