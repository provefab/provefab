//! Runs the real binary the way the worker plugins will.

use std::io::Write;
use std::process::{Command, Stdio};

fn guard(format: &str, root: Option<&std::path::Path>, stdin: &str) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_provefab"));
    cmd.args(["guard", "--format", format])
        .env_remove("PROVEFAB_WORKTREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = root {
        cmd.env("PROVEFAB_WORKTREE", root);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn claude_code_hook_denies_git_push_via_env_root() {
    let root = tempfile::tempdir().unwrap();
    let out = guard(
        "claude-code",
        Some(root.path()),
        r#"{"tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#,
    );
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "{stdout}"
    );
}

#[test]
fn claude_code_hook_blocks_with_exit_2_on_garbage() {
    let root = tempfile::tempdir().unwrap();
    let out = guard("claude-code", Some(root.path()), "garbage");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn pi_without_root_denies() {
    let out = guard("pi", None, r#"{"tool":"bash","args":{"command":"ls"}}"#);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains(r#""decision":"deny""#), "{stdout}");
}
