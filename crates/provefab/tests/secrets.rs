//! Linux design spec §3 through the binary: `secrets get`, and a login and
//! the file's mode on Linux.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn fake(dir: &Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn provefab(home: &Path, path: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_provefab"))
        .args(args)
        .env_clear()
        .env("PROVEFAB_HOME", home)
        .env("HOME", home)
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// Stores the Anthropic key the way this system does: a fake `security` on
/// macOS, the 0600 file elsewhere.
fn store_key(home: &Path, bin: &Path, key: Option<&str>) {
    std::fs::create_dir_all(home).unwrap();
    if cfg!(target_os = "macos") {
        let answer = match key {
            Some(k) => format!("echo {k}"),
            None => "exit 44".into(),
        };
        fake(
            bin,
            "security",
            &format!(
                "case \"$*\" in *\"-s provefab-anthropic -a provefab -w\") {answer} ;; *) exit 44 ;; esac"
            ),
        );
    } else if let Some(k) = key {
        use std::os::unix::fs::PermissionsExt;
        let f = home.join("credentials.toml");
        std::fs::write(&f, format!("anthropic = \"{k}\"\n")).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn secrets_get_prints_the_key_and_nothing_else() {
    let t = tempfile::tempdir().unwrap();
    let (home, bin) = (t.path().join("home"), t.path().join("bin"));
    std::fs::create_dir_all(&bin).unwrap();
    store_key(&home, &bin, Some("sk-ant-SENTINEL-1"));
    let out = provefab(&home, &bin, &["secrets", "get", "anthropic"], "");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "sk-ant-SENTINEL-1\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn secrets_get_without_a_key_or_with_another_name_prints_nothing() {
    let t = tempfile::tempdir().unwrap();
    let (home, bin) = (t.path().join("home"), t.path().join("bin"));
    std::fs::create_dir_all(&bin).unwrap();
    store_key(&home, &bin, None);
    let missing = provefab(&home, &bin, &["secrets", "get", "anthropic"], "");
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    let err = String::from_utf8(missing.stderr).unwrap();
    assert!(
        err.starts_with("provefab: ") && err.contains("provefab login claude --api-key"),
        "{err}"
    );
    for name in ["linear", "typesafe", "jira"] {
        let other = provefab(&home, &bin, &["secrets", "get", name], "");
        assert_eq!(other.status.code(), Some(2), "{name}");
        assert!(other.stdout.is_empty(), "{name}");
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_linux_login_writes_an_owner_only_file_and_never_echoes_the_key() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let (home, bin) = (t.path().join("home"), t.path().join("bin"));
    std::fs::create_dir_all(&bin).unwrap();
    let out = provefab(&home, &bin, &["login", "linear"], "lin_api_SENTINEL\n");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!shown.contains("lin_api_SENTINEL"), "{shown}");
    let f = home.join("credentials.toml");
    assert_eq!(
        std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let out = provefab(&home, &bin, &["login", "jev"], "ts_SENTINEL\n");
    assert_eq!(out.status.code(), Some(0));
    let text = std::fs::read_to_string(&f).unwrap();
    assert!(
        text.contains("linear = \"lin_api_SENTINEL\"")
            && text.contains("typesafe = \"ts_SENTINEL\""),
        "{text}"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn run_and_doctor_refuse_a_file_open_to_others() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let (home, bin) = (t.path().join("home"), t.path().join("bin"));
    std::fs::create_dir_all(&bin).unwrap();
    store_key(&home, &bin, Some("sk-ant-SENTINEL-2"));
    let f = home.join("credentials.toml");
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(
        home.join("provefab.toml"),
        "[jev]\nmodel = \"jev-1.13.0\"\n[[models]]\nid = \"c\"\nworker = \"claude-code\"\nmodel = \"sonnet\"\ntier = \"standard\"\n",
    )
    .unwrap();
    let fix = format!("chmod 600 {}", f.display());
    let get = provefab(&home, &bin, &["secrets", "get", "anthropic"], "");
    assert_eq!(get.status.code(), Some(1));
    assert!(get.stdout.is_empty());
    assert!(String::from_utf8_lossy(&get.stderr).contains(&fix));
    let run = provefab(&home, &bin, &["run", "--once"], "");
    assert_eq!(run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&run.stderr).contains(&fix));
    let doctor = provefab(&home, &bin, &["doctor", "--json"], "");
    let text = String::from_utf8_lossy(&doctor.stdout);
    let line = text
        .lines()
        .find(|l| l.contains("\"credentials\""))
        .unwrap_or_default()
        .to_string();
    assert!(
        line.contains("\"ok\":false") && line.contains(&fix),
        "{text}"
    );
    assert!(!text.contains("SENTINEL"), "{text}");
    // The `jev key` line shows why the store was refused, and no login fix:
    // `provefab login jev` would be refused the same way.
    let jev = text
        .lines()
        .find(|l| l.contains("\"jev key\""))
        .unwrap_or_default();
    assert!(
        jev.contains("\"ok\":false") && jev.contains("chmod 600"),
        "{text}"
    );
    assert!(!jev.contains("\"fix\""), "{text}");
}

/// Linux final review (C1): a command that creates Provefab's home creates
/// it for its owner only, whatever the umask.
#[test]
fn the_home_a_command_creates_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let (home, bin) = (t.path().join("home"), t.path().join("bin"));
    std::fs::create_dir_all(&bin).unwrap();
    let _ = provefab(&home, &bin, &["status"], "");
    let mode = std::fs::metadata(&home).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

/// Linux final review: the Jev login does not imply a service is installed.
#[test]
fn the_jev_login_speaks_of_the_service_only_if_you_run_it() {
    let t = tempfile::tempdir().unwrap();
    let (home, bin) = (t.path().join("home"), t.path().join("bin"));
    std::fs::create_dir_all(&bin).unwrap();
    // macOS stores at `security`'s prompt: a fake that accepts.
    fake(&bin, "security", "exit 0");
    let out = provefab(&home, &bin, &["login", "jev"], "ts_SENTINEL\n");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("stored; Provefab reads it at its next start (if you run the service, `provefab service install` restarts it)"),
        "{text}"
    );
}
