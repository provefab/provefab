//! Runs the real binary the way the worker plugins will.

use std::io::Write;
use std::process::{Command, Stdio};

fn native_policy(
    root: &std::path::Path,
    deny_read: &[std::path::PathBuf],
    deny_write: &[std::path::PathBuf],
) -> serde_json::Value {
    serde_json::json!({
        "network": {"allowedDomains": [], "deniedDomains": ["*"], "strictAllowedDomains": true, "allowLocalBinding": false, "allowAllUnixSockets": false},
        "filesystem": {"denyRead": deny_read, "denyWrite": deny_write, "allowWrite": [root]},
        "enableWeakerNestedSandbox": false, "enableWeakerNetworkIsolation": false
    })
}

fn guard(format: &str, root: Option<&std::path::Path>, stdin: &str) -> std::process::Output {
    guard_with(format, root, stdin, &[])
}

fn guard_with(
    format: &str,
    root: Option<&std::path::Path>,
    stdin: &str,
    env: &[(&str, &str)],
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_provefab"));
    cmd.args(["guard", "--format", format])
        .env_remove("PROVEFAB_WORKTREE")
        .env_remove("PROVEFAB_UNTRUSTED_REVIEW")
        .env_remove("PROVEFAB_NO_TOOLS")
        .env_remove("PROVEFAB_SANDBOX_POLICY")
        .envs(env.iter().copied())
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

/// Final review I1: the worker's review marker reaches the guard through
/// the environment and limits the shell to read-only commands.
#[test]
fn the_review_marker_refuses_running_the_pull_requests_code() {
    let root = tempfile::tempdir().unwrap();
    let call = |command: &str| {
        serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}}).to_string()
    };
    let marker = [("PROVEFAB_UNTRUSTED_REVIEW", "1")];
    let out = guard_with(
        "codex",
        Some(root.path()),
        &call("python3 tools/check.py"),
        &marker,
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "{stdout}"
    );
    let out = guard_with(
        "codex",
        Some(root.path()),
        &call(&format!("cd {} && git diff HEAD~1", root.path().display())),
        &marker,
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "");
    let out = guard("codex", Some(root.path()), &call("python3 tools/check.py"));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "",
        "no marker, stage policy"
    );
}

/// Linux final review, finding 1: a stage reads none of the secrets in
/// Provefab's home, whatever the tool; its worktree stays readable.
#[test]
fn a_stage_cannot_read_the_secrets_in_the_provefab_home() {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().canonicalize().unwrap();
    let home = user.join(".provefab");
    let root = home.join("worktrees/3");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        home.join("credentials.toml"),
        "linear = \"lin_api_SENTINEL\"\n",
    )
    .unwrap();
    let env = [
        ("HOME", user.to_str().unwrap()),
        ("PROVEFAB_HOME", home.to_str().unwrap()),
    ];
    let decide = |input: String| {
        let out = guard_with("claude-code", Some(&root), &input, &env);
        String::from_utf8(out.stdout).unwrap()
    };
    let creds = home.join("credentials.toml");
    for input in [
        format!(
            r#"{{"tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
            creds.display()
        ),
        r#"{"tool_name":"Bash","tool_input":{"command":"cat ~/.provefab/credentials.toml"}}"#
            .to_string(),
        r#"{"tool_name":"Bash","tool_input":{"command":"provefab secrets get anthropic"}}"#
            .to_string(),
        r#"{"tool_name":"Bash","tool_input":{"command":"security find-generic-password -s provefab-linear -w"}}"#
            .to_string(),
    ] {
        let stdout = decide(input.clone());
        assert!(stdout.contains(r#""permissionDecision":"deny""#), "{input}: {stdout}");
        assert!(!stdout.contains("SENTINEL"), "{stdout}");
    }
    let own = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        root.join("src").display()
    );
    assert_eq!(decide(own), "");
}

#[test]
fn native_policy_denies_direct_validation_writes_and_secret_searches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("work");
    let session = dir.path().join("session");
    std::fs::create_dir_all(root.join("checks")).unwrap();
    std::fs::create_dir(&session).unwrap();
    let protected = root.join("checks/validate");
    let secret = root.join("hidden-secret");
    std::fs::write(&protected, "validator").unwrap();
    std::fs::write(&secret, "secret").unwrap();
    std::os::unix::fs::symlink(&protected, root.join("alias")).unwrap();
    let policy = session.join("sandbox-policy.json");
    std::fs::write(
        &policy,
        native_policy(&root, &[secret], &[protected, session]).to_string(),
    )
    .unwrap();
    let env = [("PROVEFAB_SANDBOX_POLICY", policy.to_str().unwrap())];
    for (tool, field, path) in [
        ("Write", "file_path", "checks/validate"),
        ("Edit", "file_path", "checks"),
        ("Write", "file_path", "alias"),
        ("Edit", "file_path", "."),
        ("Read", "file_path", "hidden-secret"),
        ("Grep", "path", "."),
        ("Glob", "path", "."),
    ] {
        let input =
            serde_json::json!({"tool_name": tool, "tool_input": {field: path, "pattern": "*"}})
                .to_string();
        let out = guard_with("claude-code", Some(&root), &input, &env);
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(
            stdout.contains("\"permissionDecision\":\"deny\""),
            "{tool} {path}: {stdout}"
        );
    }
    for patch in [
        "*** Begin Patch\n*** Update File: checks/validate\n@@\n-validator\n+changed\n*** End Patch",
        "*** Begin Patch\n*** Delete File: checks\n*** End Patch",
        "*** Begin Patch\n*** Update File: alias\n@@\n-validator\n+changed\n*** End Patch",
    ] {
        let input =
            serde_json::json!({"tool_name": "apply_patch", "tool_input": {"command": patch}})
                .to_string();
        let out = guard_with("codex", Some(&root), &input, &env);
        assert!(
            String::from_utf8(out.stdout)
                .unwrap()
                .contains("\"permissionDecision\":\"deny\"")
        );
    }
    let input = r#"{"tool":"read","args":{"path":"checks/validate"}}"#;
    let out = guard_with("pi", Some(&root), input, &env);
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("\"decision\":\"allow\"")
    );
}

#[test]
fn native_named_policy_errors_deny_even_review_reads() {
    let root = tempfile::tempdir().unwrap();
    let policy = root.path().join("absent.json");
    let env = [
        ("PROVEFAB_SANDBOX_POLICY", policy.to_str().unwrap()),
        ("PROVEFAB_UNTRUSTED_REVIEW", "1"),
    ];
    let out = guard_with(
        "claude-code",
        Some(root.path()),
        r#"{"tool_name":"Read","tool_input":{"file_path":"README.md"}}"#,
        &env,
    );
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("\"permissionDecision\":\"deny\"")
    );
}

#[test]
fn native_parent_search_cannot_follow_a_nested_alias_to_a_denied_secret() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    let secret = dir.path().join("external-secret");
    std::fs::write(&secret, "secret").unwrap();
    std::os::unix::fs::symlink(&secret, root.join("nested/leak")).unwrap();
    let policy = dir.path().join("policy.json");
    std::fs::write(&policy, native_policy(&root, &[secret], &[]).to_string()).unwrap();
    let env = [("PROVEFAB_SANDBOX_POLICY", policy.to_str().unwrap())];
    let input = r#"{"tool_name":"Glob","tool_input":{"path":".","pattern":"**/*"}}"#;
    let out = guard_with("claude-code", Some(&root), input, &env);
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("\"permissionDecision\":\"deny\"")
    );
}

#[test]
fn native_named_policy_rejects_malformed_and_widened_security_fields() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("README.md"), "safe").unwrap();
    let path = dir.path().join("policy.json");
    let fs = serde_json::json!({"denyRead": [], "denyWrite": [], "allowWrite": [root]});
    for policy in [
        serde_json::json!({"filesystem": fs}),
        serde_json::json!({
            "filesystem": fs,
            "network": {"allowedDomains": ["example.com"], "deniedDomains": ["*"], "strictAllowedDomains": true, "allowLocalBinding": false, "allowAllUnixSockets": false},
            "enableWeakerNestedSandbox": false, "enableWeakerNetworkIsolation": false
        }),
    ] {
        std::fs::write(&path, policy.to_string()).unwrap();
        let env = [
            ("PROVEFAB_SANDBOX_POLICY", path.to_str().unwrap()),
            ("PROVEFAB_UNTRUSTED_REVIEW", "1"),
        ];
        let out = guard_with(
            "claude-code",
            Some(&root),
            r#"{"tool_name":"Read","tool_input":{"file_path":"README.md"}}"#,
            &env,
        );
        assert!(
            String::from_utf8(out.stdout)
                .unwrap()
                .contains("\"permissionDecision\":\"deny\"")
        );
    }
}
