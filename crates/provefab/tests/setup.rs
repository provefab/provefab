//! Agent setup spec sections 3 to 6 through the binary: the commands an
//! agent runs and the exit codes it reads.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use provefab::testkit::git;

fn fake(dir: &Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A directory used as the whole PATH, with the real `git` linked in.
fn bin_dir(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(out.stdout).unwrap().trim().to_string();
    std::os::unix::fs::symlink(real, bin.join("git")).unwrap();
    bin
}

fn provefab(home: &Path, path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_provefab"))
        .args(args)
        .env_clear()
        .env("PROVEFAB_HOME", home)
        .env("HOME", home)
        .env("PATH", path)
        .output()
        .unwrap()
}

/// Spec section 6: the exit code, and an error is one stderr line
/// starting with `provefab:`.
fn refused(out: &Output, code: i32) -> String {
    let err = String::from_utf8(out.stderr.clone()).unwrap();
    assert_eq!(out.status.code(), Some(code), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("provefab: "), "{err}");
    err
}

fn ok(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// A clone of a repository holding `files`, whose origin is
/// github.com/<slug> and whose `origin/HEAD` is `main`.
fn checkout(root: &Path, slug: &str, files: &[(&str, &str)]) -> PathBuf {
    let src = root.join(format!("src-{}", slug.replace('/', "-")));
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    for (name, text) in files {
        std::fs::write(src.join(name), text).unwrap();
    }
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "init"]);
    let dest = root.join(format!("clone-{}", slug.replace('/', "-")));
    git(
        root,
        &["clone", "-q", src.to_str().unwrap(), dest.to_str().unwrap()],
    );
    git(
        &dest,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("https://github.com/{slug}.git"),
        ],
    );
    dest
}

const CLAUDE: &str = r#"if [ "$1" = auth ]; then echo '{"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max"}'; exit 0; fi
echo '2.1.281 (Claude Code)'"#;

const GH_SIGNED_IN: &str = r#"case "$1" in
  auth) echo 'github.com'; exit 0 ;;
  api) echo 'error connecting to api.github.com' >&2; exit 1 ;;
  *) echo 'gh version 2.80' ;;
esac"#;

const GH_SIGNED_OUT: &str = r#"case "$1" in
  auth) echo 'You are not logged into any GitHub hosts. To log in, run: gh auth login' >&2; exit 1 ;;
  *) echo 'gh version 2.80' ;;
esac"#;

#[test]
fn an_agent_configures_a_repository_and_reads_each_exit_code() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let bin = bin_dir(t.path());
    fake(&bin, "gh", GH_SIGNED_IN);
    fake(&bin, "security", "exit 44");
    let config = home.join("provefab.toml");

    // No worker CLI on PATH: 4, nothing written.
    refused(&provefab(&home, &bin, &["init"]), 4);
    assert!(!config.exists());

    fake(&bin, "claude", CLAUDE);
    let said = ok(&provefab(&home, &bin, &["init"]));
    assert!(
        said.contains("models: claude-sonnet, claude-opus"),
        "{said}"
    );
    assert!(config.exists());
    refused(&provefab(&home, &bin, &["init"]), 3);
    refused(&provefab(&home, &bin, &["init", "--dry-run"]), 3);

    // Usage errors: clap's, then ours.
    assert_eq!(
        provefab(&home, &bin, &["repos", "add"]).status.code(),
        Some(2)
    );
    refused(&provefab(&home, &bin, &["repos", "add", "not-a-slug"]), 2);
    let rust = checkout(
        t.path(),
        "o/r",
        &[("Cargo.toml", "[package]\nname = \"r\"\n")],
    );
    let elsewhere = checkout(t.path(), "o/other", &[("go.mod", "module x\n")]);
    refused(
        &provefab(
            &home,
            &bin,
            &["repos", "add", "o/r", "--path", elsewhere.to_str().unwrap()],
        ),
        2,
    );

    // Through gh: gh cannot reach GitHub here, so 5.
    refused(&provefab(&home, &bin, &["repos", "add", "o/r"]), 5);

    let before = std::fs::read_to_string(&config).unwrap();
    let shown = ok(&provefab(
        &home,
        &bin,
        &[
            "repos",
            "add",
            "o/r",
            "--path",
            rust.to_str().unwrap(),
            "--dry-run",
        ],
    ));
    assert!(shown.starts_with("[[repos]]\nslug = \"o/r\""), "{shown}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
    ok(&provefab(
        &home,
        &bin,
        &["repos", "add", "o/r", "--path", rust.to_str().unwrap()],
    ));
    let after = std::fs::read_to_string(&config).unwrap();
    assert!(after.starts_with(&before), "{after}");
    assert!(
        after.contains("gates = [\"cargo fmt -- --check\""),
        "{after}"
    );
    refused(
        &provefab(
            &home,
            &bin,
            &["repos", "add", "O/R", "--path", rust.to_str().unwrap()],
        ),
        3,
    );

    // Not recognised: 4.
    let docs = checkout(t.path(), "o/docs", &[("README.md", "# docs\n")]);
    refused(
        &provefab(
            &home,
            &bin,
            &["repos", "add", "o/docs", "--path", docs.to_str().unwrap()],
        ),
        4,
    );

    // doctor --json: every line is an object with name, ok and detail; all
    // pass but the optional Jev key, so 0.
    let out = provefab(&home, &bin, &["doctor", "--json"]);
    let lines = ok(&out);
    let parsed: Vec<serde_json::Value> = lines
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(!parsed.is_empty());
    for l in &parsed {
        assert!(
            l["name"].is_string() && l["ok"].is_boolean() && l["detail"].is_string(),
            "{l}"
        );
    }
    let jev = parsed.iter().find(|l| l["name"] == "jev key").unwrap();
    assert_eq!(
        (jev["ok"].as_bool(), jev["fix"].as_str()),
        (
            Some(false),
            Some("security add-generic-password -s provefab-typesafe -a provefab -w")
        )
    );

    // A failed sign-in: 1, with its fix.
    fake(&bin, "gh", GH_SIGNED_OUT);
    let out = provefab(&home, &bin, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8(out.stdout).unwrap();
    let gh = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|l| l["name"] == "gh login")
        .unwrap();
    assert_eq!(
        (gh["ok"].as_bool(), gh["fix"].as_str()),
        (Some(false), Some("gh auth login"))
    );
    // Without --json the output is the text one, and the code the same.
    let text = provefab(&home, &bin, &["doctor"]);
    assert_eq!(text.status.code(), Some(1));
    assert!(
        String::from_utf8(text.stdout)
            .unwrap()
            .contains("FAIL gh login")
    );
}

/// Plan decision 6: no configuration yet is one `configuration` line, exit 1.
#[test]
fn doctor_json_without_a_configuration_points_to_init() {
    let t = tempfile::tempdir().unwrap();
    let bin = bin_dir(t.path());
    let out = provefab(&t.path().join("home"), &bin, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let line: serde_json::Value =
        serde_json::from_str(String::from_utf8(out.stdout).unwrap().trim()).unwrap();
    assert_eq!(
        (
            line["name"].as_str(),
            line["ok"].as_bool(),
            line["fix"].as_str()
        ),
        (Some("configuration"), Some(false), Some("provefab init"))
    );
}

/// Spec section 6: the codes are in `--help` of the new commands.
#[test]
fn the_new_commands_document_their_exit_codes() {
    let t = tempfile::tempdir().unwrap();
    let bin = bin_dir(t.path());
    for args in [
        vec!["init", "--help"],
        vec!["repos", "add", "--help"],
        vec!["doctor", "--help"],
    ] {
        let help = ok(&provefab(t.path(), &bin, &args));
        // Whitespace-insensitive: clap may wrap the text.
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            help.contains("3 the configuration or the repository already exists"),
            "{args:?}: {help}"
        );
        // Final review: clap prints the usage for its own errors only.
        assert!(
            help.contains(
                "2 usage error (clap prints the usage), or `repos add` before `provefab init`;"
            ),
            "{args:?}: {help}"
        );
    }
}

/// An unparsable configuration is one `provefab:` line in text mode and one
/// `configuration` line with --json; both exit 1.
#[test]
fn doctor_with_an_unparsable_configuration_stays_one_line() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("provefab.toml"), "[jev\nmodel = = 1\n[[repos]\n").unwrap();
    let bin = bin_dir(t.path());
    refused(&provefab(&home, &bin, &["doctor"]), 1);
    let out = provefab(&home, &bin, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(line["name"], "configuration");
    assert_eq!(line["ok"], false);
}

/// Final review, R3: a configuration that does not parse never repeats a
/// secret-looking value on stderr, for `repos add` and text `doctor`.
#[test]
fn a_token_in_a_broken_configuration_never_reaches_stderr() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let token = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    std::fs::write(home.join("provefab.toml"), format!("[jev]\nkey = {token}\n")).unwrap();
    let bin = bin_dir(t.path());
    for args in [vec!["repos", "add", "o/r"], vec!["doctor"]] {
        let err = refused(&provefab(&home, &bin, &args), 1);
        assert!(!err.contains(token), "{args:?}: {err}");
        assert!(err.contains("<redacted>"), "{args:?}: {err}");
    }
}
