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
        let config = path.join("config/shoin");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("test.conf"), "[markdown]\nmath = true\n").unwrap();
        Self(path)
    }

    fn fixture_path(&self) -> std::ffi::OsString {
        std::env::join_paths([self.0.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shoin"));
        command.current_dir(&self.0).env_remove("SHOIN_CONFIG")
            .env("HOME", &self.0).env("XDG_CONFIG_HOME", self.0.join("config"));
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
/bin/sleep 30 &
engine=$!
printf '%s %s\n' "$$" "$engine" > pids.tmp
mv pids.tmp pids
wait "$engine"
while [ "$1" != --output ]; do shift; done
echo converted > "$2"
"#).unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut shoin = vault.command().env("PATH", vault.fixture_path())
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
    let engine_alive = running(pids[1]);
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

#[test]
fn export_respects_read_only_destination() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let vault = Vault::new("read-only");
    fs::write(vault.0.join("note.md"), "new\n").unwrap();
    let dest = vault.0.join("out.md");
    fs::write(&dest, "old\n").unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o444)).unwrap();

    let out = vault.command().args(["--export", "note.md", "--out", "out.md"]).output().unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("Permission denied"), "{out:?}");
    assert_eq!(fs::read(&dest).unwrap(), b"old\n");
    assert_eq!(fs::metadata(dest).unwrap().permissions().mode() & 0o777, 0o444);
}

#[test]
fn export_creates_the_target_of_a_dangling_symlink() {
    let vault = Vault::new("dangling");
    fs::write(vault.0.join("note.md"), "new\n").unwrap();
    fs::create_dir(vault.0.join("sub")).unwrap();
    let link = vault.0.join("out.md");
    std::os::unix::fs::symlink("sub/actual.md", &link).unwrap();

    let out = vault.command().args(["--export", "note.md", "--out", "out.md"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read_link(link).unwrap(), Path::new("sub/actual.md"));
    assert_eq!(fs::read(vault.0.join("sub/actual.md")).unwrap(), b"new\n");
}

#[test]
fn export_refuses_a_destination_changed_during_conversion() {
    for (name, original, change, expected) in [
        ("edit", Some("old\n"), "printf 'edit\\n' >> note.pdf", Some("old\nedit\n")),
        ("mtime", Some("old\n"), "touch -t 200001010000 note.pdf", Some("old\n")),
        ("replace", Some("old\n"), "cp -p note.pdf replacement; mv replacement note.pdf", Some("old\n")),
        ("create", None, "echo newer > note.pdf", Some("newer\n")),
        ("retarget", None, "ln -sf second.pdf target.pdf", None),
        ("remove", Some("old\n"), "rm note.pdf", None),
    ] {
        let vault = Vault::new(name);
        fs::write(vault.0.join("note.md"), "hello\n").unwrap();
        let dest = vault.0.join("note.pdf");
        if let Some(original) = original {
            fs::write(&dest, original).unwrap();
        }
        if name == "retarget" {
            std::os::unix::fs::symlink("target.pdf", &dest).unwrap();
            std::os::unix::fs::symlink("first.pdf", vault.0.join("target.pdf")).unwrap();
        }
        let script = vault.0.join("pandoc");
        fs::write(&script, format!(r#"#!/bin/sh
set -e
{change}
while [ "$1" != --output ]; do shift; done
echo converted > "$2"
"#)).unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(vault.0.join("typst"), "").unwrap();
        fs::set_permissions(vault.0.join("typst"), fs::Permissions::from_mode(0o700)).unwrap();
        let out = vault.command().env("PATH", vault.fixture_path())
            .args(["--export", "note.md", "--format", "pdf"]).output().unwrap();
        assert!(!out.status.success(), "{name}: {out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("changed while exporting"), "{name}: {out:?}");
        if name == "retarget" {
            assert_eq!(fs::read_link(&dest).unwrap(), Path::new("target.pdf"));
            assert_eq!(fs::read_link(vault.0.join("target.pdf")).unwrap(), Path::new("second.pdf"));
            for target in ["first.pdf", "second.pdf"] {
                assert!(!vault.0.join(target).exists(), "{target} was created");
            }
        }
        match expected {
            Some(content) => assert_eq!(fs::read_to_string(&dest).unwrap(), content, "{name}"),
            None => assert_eq!(fs::metadata(&dest).unwrap_err().kind(), std::io::ErrorKind::NotFound),
        }
    }
}

#[test]
fn export_preserves_destination_acl() {
    let vault = Vault::new("acl");
    fs::write(vault.0.join("note.md"), "new\n").unwrap();
    let dest = vault.0.join("out.md");
    fs::write(&dest, "old\n").unwrap();
    // Use a mapped UID even when the tests run in a user namespace.
    let uid = unsafe { libc::geteuid() };
    let entries = format!("g::---,o::---,u:{uid}:r--");
    if !Command::new("setfacl").args(["-m", &entries]).arg(&dest).output()
        .is_ok_and(|out| out.status.success())
    {
        return;
    }
    let acl = || Command::new("getfacl").args(["-cn"]).arg(&dest).output();
    let Ok(before) = acl() else { return };
    if !before.status.success() || !String::from_utf8_lossy(&before.stdout).contains(&format!("user:{uid}:r--")) {
        return;
    }

    let out = vault.command().args(["--export", "note.md", "--out", "out.md"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read(&dest).unwrap(), b"new\n");
    let after = acl().unwrap();
    assert!(after.status.success(), "{after:?}");
    assert_eq!(after.stdout, before.stdout);
}

#[test]
fn interrupted_stdout_html_reaps_pandoc() {
    let vault = Vault::new("stdout-signal");
    fs::write(vault.0.join("note.md"), "Formula $x$.\n").unwrap();
    let script = vault.0.join("pandoc");
    fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$$\" > pid.tmp\nmv pid.tmp pid\nexec sleep 30\n").unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
    let mut shoin = vault.command().env("PATH", vault.fixture_path())
        .args(["--export", "note.md", "--format", "html", "--stdout"])
        .stdout(Stdio::null()).spawn().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Ok(pid) = fs::read_to_string(vault.0.join("pid")) { break pid.trim().parse().unwrap() }
        assert!(Instant::now() < deadline, "pandoc did not start");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(unsafe { libc::kill(shoin.id() as libc::pid_t, libc::SIGTERM) }, 0);
    let status = loop {
        if let Some(status) = shoin.try_wait().unwrap() { break status }
        if Instant::now() >= deadline {
            shoin.kill().unwrap();
            shoin.wait().unwrap();
            panic!("Shoin did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let survived = alive(pid);
    if survived {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert_eq!(status.code(), Some(128 + libc::SIGTERM));
    assert!(!survived, "pandoc survived or was not reaped before shutdown");
}

#[test]
fn early_pdf_exit_stops_the_layout_engine() {
    for (name, code) in [("early-success", 0), ("early-failure", 1)] {
        let vault = Vault::new(name);
        fs::write(vault.0.join("note.md"), "hello\n").unwrap();
        let dest = vault.0.join("note.pdf");
        fs::write(&dest, "original\n").unwrap();
        let tmp = vault.0.join("tmp");
        fs::create_dir(&tmp).unwrap();
        let script = vault.0.join("pandoc");
        fs::write(&script, format!(r#"#!/bin/sh
/bin/sleep 30 &
echo "$!" > engine
while [ "$1" != --output ]; do shift; done
echo converted > "$2"
echo 'fixture diagnostic' >&2
exit {code}
"#)).unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(vault.0.join("typst"), "").unwrap();
        fs::set_permissions(vault.0.join("typst"), fs::Permissions::from_mode(0o700)).unwrap();
        let out = vault.command().env("TMPDIR", &tmp).env("PATH", &vault.0)
            .args(["--export", "note.md", "--format", "pdf"]).output().unwrap();
        let pid = fs::read_to_string(vault.0.join("engine")).unwrap().trim().parse().unwrap();
        let survived = running(pid);
        if survived {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(!survived, "layout engine survived Pandoc exit {code}");
        assert_eq!(out.status.success(), code == 0, "{out:?}");
        if code == 0 {
            assert_eq!(fs::read(&dest).unwrap(), b"converted\n");
        } else {
            assert_eq!(fs::read(&dest).unwrap(), b"original\n");
            assert!(String::from_utf8_lossy(&out.stderr).contains("fixture diagnostic"));
        }
        assert!(fs::read_dir(&tmp).unwrap().next().is_none());
    }
}

fn running(pid: libc::pid_t) -> bool {
    if cfg!(target_os = "linux") {
        // An orphan killed with its parent can briefly be a zombie under PID 1.
        fs::read_to_string(Path::new("/proc").join(pid.to_string()).join("stat"))
            .is_ok_and(|stat| !stat.rsplit_once(") ").unwrap().1.starts_with('Z'))
    } else {
        alive(pid)
    }
}

fn is_char_device(path: &str) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::metadata(path).is_ok_and(|meta| meta.file_type().is_char_device())
}

#[test]
fn export_writes_to_dev_null() {
    if !is_char_device("/dev/null") {
        return;
    }
    let vault = Vault::new("dev-null");
    fs::write(vault.0.join("note.md"), "hello\n").unwrap();
    let out = vault.command().args(["--export", "note.md", "--format", "txt", "--out", "/dev/null"])
        .output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "/dev/null\n");
}

#[test]
fn pdf_export_writes_to_dev_stdout() {
    // Shoin recognises these names and writes to its fd 1 without opening
    // the path, so this does not depend on what the platform's /dev/fd does.
    let vault = Vault::new("dev-stdout");
    fs::write(vault.0.join("note.md"), "hello\n").unwrap();
    let script = vault.0.join("pandoc");
    fs::write(&script, "#!/bin/sh\nwhile [ \"$1\" != --output ]; do shift; done\necho converted > \"$2\"\n").unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(vault.0.join("typst"), "").unwrap();
    fs::set_permissions(vault.0.join("typst"), fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink("/dev/stdout", vault.0.join("link")).unwrap();
    // `output()` gives the child a pipe for stdout, as `| lpr` would.
    for (format, dest) in [("pdf", "/dev/stdout"), ("txt", "/dev/stdout"), ("txt", "/dev/fd/1"), ("txt", "link")] {
        let out = vault.command().env("PATH", vault.fixture_path())
            .args(["--export", "note.md", "--format", format, "--out", dest])
            .output().unwrap();
        assert!(out.status.success(), "{format} {dest}: {out:?}");
        let expected: &[u8] = if format == "pdf" { b"converted\n" } else { b"hello\n" };
        assert_eq!(out.stdout, expected, "{format} {dest}: {out:?}");
    }
}

#[test]
fn export_refuses_named_pipes_and_directories() {
    let vault = Vault::new("fifo");
    fs::write(vault.0.join("note.md"), "hello\n").unwrap();
    let fifo = vault.0.join("pipe");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a NUL-terminated path inside the test's own directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    fs::create_dir(vault.0.join("dir")).unwrap();
    for (dest, message) in [("./pipe", "./pipe is a named pipe"), ("dir", "dir is a directory")] {
        let start = Instant::now();
        let out = vault.command().args(["--export", "note.md", "--format", "txt", "--out", dest])
            .output().unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "{dest}: export blocked");
        assert!(!out.status.success(), "{dest}: {out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(message), "{dest}: {out:?}");
    }
    assert!(fs::metadata(&fifo).is_ok());
}
