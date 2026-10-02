//! `provefab guard`: reads one tool call as JSON on stdin and answers in the
//! calling worker's format. Every failure path denies (fail closed).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::guard::{self, Decision, adapters};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GuardFormat {
    /// `{"tool", "args", "cwd"}` in, `{"decision", "reason"?}` out. Used by the pi-provefab extension.
    Pi,
    /// Claude Code PreToolUse hook input and output.
    ClaudeCode,
    /// Codex PreToolUse hook: same contract as Claude Code, Codex tool names.
    Codex,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GuardOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn run_guard(format: GuardFormat, root: Option<&Path>, stdin: &str) -> GuardOutput {
    guard_call(format, root, stdin, false)
}

/// `run_guard` for a review of a person's pull request
/// (`PROVEFAB_UNTRUSTED_REVIEW`): the shell runs read-only commands only.
pub fn run_guard_review(format: GuardFormat, root: Option<&Path>, stdin: &str) -> GuardOutput {
    guard_call(format, root, stdin, true)
}

fn guard_call(format: GuardFormat, root: Option<&Path>, stdin: &str, review: bool) -> GuardOutput {
    let Ok(input) = serde_json::from_str::<Value>(stdin) else {
        return unreadable(format);
    };
    let (tool_key, args_key) = match format {
        GuardFormat::Pi => ("tool", "args"),
        GuardFormat::ClaudeCode | GuardFormat::Codex => ("tool_name", "tool_input"),
    };
    let (Some(tool), Some(args)) = (
        input.get(tool_key).and_then(Value::as_str),
        input.get(args_key),
    ) else {
        return unreadable(format);
    };
    let decision = match root {
        None => {
            Decision::Deny("PROVEFAB_WORKTREE is not set, so every tool call is refused".into())
        }
        Some(root) if !root.is_dir() => Decision::Deny(format!(
            "worktree root {} does not exist, so every tool call is refused",
            root.display()
        )),
        Some(root) => {
            let cwd = input
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| root.to_path_buf());
            let call = match format {
                GuardFormat::Pi => adapters::from_pi(tool, args),
                GuardFormat::ClaudeCode => adapters::from_claude_code(tool, args),
                GuardFormat::Codex => adapters::from_codex(tool, args),
            };
            if review {
                guard::check_review(&call, &cwd, root)
            } else {
                guard::check(&call, &cwd, root)
            }
        }
    };
    render(format, &decision)
}

/// `run_guard` for a stage without tools (`PROVEFAB_NO_TOOLS`, repository
/// rules pre-flight S2): every call is refused, reads included, except the
/// tool that carries the structured answer (Pi's `submit_result`, Claude
/// Code's `StructuredOutput`). Codex answers without a tool call.
pub fn run_guard_no_tools(format: GuardFormat, stdin: &str) -> GuardOutput {
    let Ok(input) = serde_json::from_str::<Value>(stdin) else {
        return unreadable(format);
    };
    let (key, answer) = match format {
        GuardFormat::Pi => ("tool", Some(agent_workers::SUBMIT_TOOL)),
        GuardFormat::ClaudeCode => ("tool_name", Some("StructuredOutput")),
        GuardFormat::Codex => ("tool_name", None),
    };
    let Some(tool) = input.get(key).and_then(Value::as_str) else {
        return unreadable(format);
    };
    let decision = if Some(tool) == answer {
        Decision::Allow
    } else {
        Decision::Deny("this stage has no tools, so every tool call is refused".into())
    };
    render(format, &decision)
}

fn render(format: GuardFormat, decision: &Decision) -> GuardOutput {
    let stdout = match (format, decision) {
        (GuardFormat::Pi, Decision::Allow) => json!({"decision": "allow"}).to_string(),
        (GuardFormat::Pi, Decision::Deny(reason)) => {
            json!({"decision": "deny", "reason": reason}).to_string()
        }
        // Empty output leaves Claude Code's normal permission rules in charge.
        // Returning "allow" would skip them, which the guard must never do.
        (GuardFormat::ClaudeCode | GuardFormat::Codex, Decision::Allow) => String::new(),
        (GuardFormat::ClaudeCode | GuardFormat::Codex, Decision::Deny(reason)) => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        })
        .to_string(),
    };
    GuardOutput {
        stdout,
        stderr: String::new(),
        exit_code: 0,
    }
}

fn unreadable(format: GuardFormat) -> GuardOutput {
    let reason = "provefab guard could not read the tool call";
    match format {
        GuardFormat::Pi => render(format, &Decision::Deny(reason.into())),
        // Exit code 2 blocks the tool call in Claude Code; stderr becomes the reason.
        GuardFormat::ClaudeCode | GuardFormat::Codex => GuardOutput {
            stdout: String::new(),
            stderr: reason.into(),
            exit_code: 2,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn pi_allow_and_deny() {
        let r = root();
        let out = run_guard(
            GuardFormat::Pi,
            Some(r.path()),
            r#"{"tool":"bash","args":{"command":"cargo test"}}"#,
        );
        assert_eq!(out.stdout, r#"{"decision":"allow"}"#);
        let out = run_guard(
            GuardFormat::Pi,
            Some(r.path()),
            r#"{"tool":"bash","args":{"command":"git push"}}"#,
        );
        let v: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(v["decision"], "deny");
        assert_eq!(out.exit_code, 0);
    }

    #[test]
    fn claude_code_allow_is_silent_and_deny_uses_hook_output() {
        let r = root();
        let allow = run_guard(
            GuardFormat::ClaudeCode,
            Some(r.path()),
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
        );
        assert_eq!(
            allow,
            GuardOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0
            }
        );

        let deny = run_guard(
            GuardFormat::ClaudeCode,
            Some(r.path()),
            r#"{"tool_name":"Write","tool_input":{"file_path":"/etc/hosts","content":""}}"#,
        );
        let v: Value = serde_json::from_str(&deny.stdout).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    }

    #[test]
    fn missing_root_denies_even_harmless_calls() {
        let out = run_guard(
            GuardFormat::Pi,
            None,
            r#"{"tool":"read","args":{"path":"a"}}"#,
        );
        assert!(out.stdout.contains("PROVEFAB_WORKTREE"), "{}", out.stdout);
    }

    /// Final review M1 (re-graded): a root that does not exist denies shell calls too.
    #[test]
    fn review_m1_nonexistent_root_denies_everything() {
        let out = run_guard(
            GuardFormat::Pi,
            Some(Path::new("/nope/not/a/worktree")),
            r#"{"tool":"bash","args":{"command":"ls"}}"#,
        );
        assert!(
            out.stdout.contains(r#""decision":"deny""#),
            "{}",
            out.stdout
        );
    }

    /// Repository rules pre-flight S2: a stage without tools has every call
    /// refused, harmless reads of absolute paths included; only the answer
    /// itself goes through.
    #[test]
    fn without_tools_every_call_but_the_answer_is_refused() {
        let denied = |format, input: Value| {
            let out = run_guard_no_tools(format, &input.to_string());
            let v: Value = serde_json::from_str(&out.stdout).unwrap();
            let decision = match format {
                GuardFormat::Pi => v["decision"].clone(),
                _ => v["hookSpecificOutput"]["permissionDecision"].clone(),
            };
            decision == "deny"
        };
        let pi = |tool: &str, args: Value| json!({"tool": tool, "args": args});
        let cc = |tool: &str, input: Value| json!({"tool_name": tool, "tool_input": input});
        for (format, input) in [
            (
                GuardFormat::Pi,
                pi("read", json!({"path": "/home/u/.provefab/license.key"})),
            ),
            (GuardFormat::Pi, pi("ls", json!({"path": "/"}))),
            (GuardFormat::Pi, pi("bash", json!({"command": "ls"}))),
            (
                GuardFormat::ClaudeCode,
                cc("Read", json!({"file_path": "/etc/hosts"})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Glob", json!({"pattern": "**"})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Bash", json!({"command": "ls"})),
            ),
            (
                GuardFormat::Codex,
                cc("Bash", json!({"command": "cat /etc/hosts"})),
            ),
            (GuardFormat::Codex, cc("submit_result", json!({}))),
            (GuardFormat::Pi, pi("StructuredOutput", json!({}))),
        ] {
            assert!(denied(format, input.clone()), "{format:?} {input}");
        }
        assert_eq!(
            run_guard_no_tools(GuardFormat::Pi, &pi("submit_result", json!({})).to_string()).stdout,
            r#"{"decision":"allow"}"#
        );
        let answer = run_guard_no_tools(
            GuardFormat::ClaudeCode,
            &cc("StructuredOutput", json!({"changes": []})).to_string(),
        );
        assert_eq!((answer.stdout.as_str(), answer.exit_code), ("", 0));
        let cc_bad = run_guard_no_tools(GuardFormat::ClaudeCode, "not json");
        assert_eq!(cc_bad.exit_code, 2);
    }

    /// Final review I1: the review marker turns the shell read-only; the
    /// same calls without it keep the stage policy.
    #[test]
    fn a_pull_request_review_runs_no_code_from_it() {
        let r = root();
        let decision = |review: bool, format, input: Value| {
            let out = if review {
                run_guard_review(format, Some(r.path()), &input.to_string())
            } else {
                run_guard(format, Some(r.path()), &input.to_string())
            };
            match format {
                GuardFormat::Pi => serde_json::from_str::<Value>(&out.stdout).unwrap()["decision"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                _ if out.stdout.is_empty() => "allow".to_string(),
                _ => serde_json::from_str::<Value>(&out.stdout).unwrap()["hookSpecificOutput"]
                    ["permissionDecision"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            }
        };
        let codex =
            |command: &str| json!({"tool_name": "Bash", "tool_input": {"command": command}});
        let pi = |command: &str| json!({"tool": "bash", "args": {"command": command}});
        for command in [
            "python3 tools/check.py",
            "bash scripts/test.sh",
            "cargo test",
        ] {
            assert_eq!(
                decision(true, GuardFormat::Codex, codex(command)),
                "deny",
                "{command}"
            );
            assert_eq!(
                decision(true, GuardFormat::Pi, pi(command)),
                "deny",
                "{command}"
            );
            assert_eq!(
                decision(false, GuardFormat::Codex, codex(command)),
                "allow",
                "{command}"
            );
        }
        for command in ["git diff HEAD~1", "cat src/a.rs | head -20"] {
            let command = format!("cd {} && {command}", r.path().display());
            assert_eq!(
                decision(true, GuardFormat::Codex, codex(&command)),
                "allow",
                "{command}"
            );
        }
        let read = json!({"tool_name": "Read", "tool_input": {"file_path": "src/a.rs"}});
        assert_eq!(decision(true, GuardFormat::ClaudeCode, read), "allow");
        let write = json!({"tool_name": "Write", "tool_input": {"file_path": "src/a.rs"}});
        assert_eq!(
            decision(true, GuardFormat::ClaudeCode, write.clone()),
            "deny"
        );
        assert_eq!(decision(false, GuardFormat::ClaudeCode, write), "allow");
        let missing = run_guard_review(GuardFormat::Pi, None, &pi("ls").to_string());
        assert!(
            missing.stdout.contains("PROVEFAB_WORKTREE"),
            "{}",
            missing.stdout
        );
    }

    /// The decision `provefab guard` gives one call, as "allow" or "deny".
    fn decide(review: bool, format: GuardFormat, root: &Path, input: &Value) -> String {
        let out = if review {
            run_guard_review(format, Some(root), &input.to_string())
        } else {
            run_guard(format, Some(root), &input.to_string())
        };
        match format {
            GuardFormat::Pi => serde_json::from_str::<Value>(&out.stdout).unwrap()["decision"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            _ if out.stdout.is_empty() && out.exit_code == 0 => "allow".to_string(),
            _ => "deny".to_string(),
        }
    }

    /// Controller ruling after the final fix wave: under the review marker
    /// every read stays inside the worktree, whatever the tool; without it,
    /// reads keep the stage policy.
    #[test]
    fn a_pull_request_review_reads_only_its_worktree() {
        let r = root();
        let outside = root();
        std::fs::write(outside.path().join("secret"), "token").unwrap();
        std::fs::create_dir(r.path().join("src")).unwrap();
        std::fs::write(r.path().join("src/a.rs"), "fn a() {}").unwrap();
        std::os::unix::fs::symlink(outside.path(), r.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), r.path().join("link")).unwrap();
        let root = r.path();
        let wt = root.display().to_string();
        let out = outside.path().display().to_string();
        let codex = |command: &str| json!({"tool_name": "Bash", "tool_input": {"command": format!("cd {wt} && {command}")}});
        let denied: Vec<String> = [
            "cat ~/.codex/auth.json".to_string(),
            "cat /etc/hosts".into(),
            "cat ../../outside".into(),
            "grep -r token $HOME".into(),
            "grep -r token \"$HOME\"".into(),
            "git -C / log".into(),
            "cat escape/secret".into(),
            "cat link".into(),
            format!("cat {out}/secret"),
            "cat < /etc/hosts".into(),
            "cat {/etc/hosts,src/a.rs}".into(),
            "cat /e*/hosts".into(),
            "grep -R token .".into(),
            "grep -rS token .".into(),
            "rg -L token".into(),
            "find -L . -name secret".into(),
            "git diff --no-index /etc/hosts src/a.rs".into(),
            "git blame --contents=/etc/hosts src/a.rs".into(),
            "grep -f/etc/hosts src/a.rs".into(),
            "grep -if /etc/hosts src".into(),
            "grep -ief /etc/hosts src".into(),
            "rg -if /etc/hosts".into(),
            "git -C src log -1".into(),
            "git -C evil status".into(),
            "cat x=~/.ssh/id_rsa".into(),
            "cat src/a.rs; cd / && cat etc/hosts".into(),
        ]
        .into_iter()
        .filter(|c| decide(true, GuardFormat::Codex, root, &codex(c)) == "allow")
        .collect();
        assert!(denied.is_empty(), "allowed but must be denied: {denied:#?}");
        // Codex runs a command in a directory its hook does not show: a
        // review command starts in the worktree, or names nothing relative.
        let bare = |command: &str| json!({"tool_name": "Bash", "tool_input": {"command": command}});
        for c in [
            "grep -rn foo src".to_string(),
            "git log -1".into(),
            "ls".into(),
            format!("cd {wt}; cat src/a.rs"),
            format!("cd {wt} || cat etc/hosts"),
            format!("cd {wt} && cat src/a.rs || cat etc/hosts"),
            format!("cd {wt} && cat src/a.rs & cat etc/hosts"),
            format!("cd {wt}/missing && cat src/a.rs"),
            format!("cd {wt}/escape && cat secret"),
            format!("cd {wt}/src && cat a.rs"),
            format!("cd {wt}/evil && git status"),
        ] {
            assert_eq!(
                decide(true, GuardFormat::Codex, root, &bare(&c)),
                "deny",
                "{c}"
            );
        }
        let allowed: Vec<String> = [
            "cat src/a.rs".to_string(),
            format!("cat {wt}/src/a.rs"),
            "cat ./src/../src/a.rs".into(),
            "grep -rn 'foo$' src".into(),
            "grep -rn foo".into(),
            "git diff HEAD~1".into(),
            "git status".into(),
            "grep -in foo src".into(),
            "rg -in foo src".into(),
            "head -n 20 src/a.rs | wc -l".into(),
            "find src -name '*.rs'".into(),
            "ls".into(),
        ]
        .into_iter()
        .filter(|c| decide(true, GuardFormat::Codex, root, &codex(c)) != "allow")
        .collect();
        assert!(
            allowed.is_empty(),
            "denied but must be allowed: {allowed:#?}"
        );
        // Even an absolute path needs the leading `cd`: the guard does not
        // tell commands that read their directory from those that do not.
        let abs = format!("{wt}/src/a.rs");
        let refused = run_guard_review(
            GuardFormat::Codex,
            Some(root),
            &bare(&format!("cat {abs} | head -5")).to_string(),
        );
        assert!(
            refused
                .stdout
                .contains(&format!("start it with `cd {wt} && `")),
            "{}",
            refused.stdout
        );
        // Claude Code's and Pi's read tools.
        let cc =
            |tool: &str, input: Value| json!({"tool_name": tool, "tool_input": input, "cwd": wt});
        let pi = |tool: &str, args: Value| json!({"tool": tool, "args": args, "cwd": wt});
        for (format, input) in [
            (
                GuardFormat::ClaudeCode,
                cc("Read", json!({"file_path": "/etc/hosts"})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Read", json!({"file_path": format!("{out}/secret")})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Read", json!({"file_path": "link"})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Read", json!({"file_path": "../x"})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Grep", json!({"pattern": "token", "path": "/"})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Glob", json!({"pattern": "/etc/*"})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Glob", json!({"pattern": "*", "path": "escape"})),
            ),
            (
                GuardFormat::Pi,
                pi("read", json!({"path": "~/.ssh/id_rsa"})),
            ),
            (GuardFormat::Pi, pi("read", json!({"path": "@/etc/hosts"}))),
            (
                GuardFormat::Pi,
                pi("read", json!({"file_path": "/etc/hosts"})),
            ),
            (GuardFormat::Pi, pi("ls", json!({"path": "/"}))),
            (
                GuardFormat::Pi,
                pi("find", json!({"pattern": "*", "path": "escape"})),
            ),
            (
                GuardFormat::Pi,
                pi("grep", json!({"pattern": "--pre=./x", "path": "src"})),
            ),
            (
                GuardFormat::Pi,
                pi("find", json!({"pattern": "--exec=./x"})),
            ),
            (GuardFormat::ClaudeCode, cc("Read", json!({}))),
            (GuardFormat::Pi, pi("read", json!({}))),
        ] {
            assert_eq!(decide(true, format, root, &input), "deny", "{input}");
        }
        for (format, input) in [
            (
                GuardFormat::ClaudeCode,
                cc("Read", json!({"file_path": "src/a.rs"})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Read", json!({"file_path": abs.clone()})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Grep", json!({"pattern": "fn"})),
            ),
            (
                GuardFormat::ClaudeCode,
                cc("Glob", json!({"pattern": "src/**/*.rs"})),
            ),
            (GuardFormat::Pi, pi("read", json!({"path": "src/a.rs"}))),
            (
                GuardFormat::Pi,
                pi("grep", json!({"pattern": "fn", "path": "src"})),
            ),
            (GuardFormat::Pi, pi("find", json!({"pattern": "*.rs"}))),
            (GuardFormat::Pi, pi("ls", json!({}))),
        ] {
            assert_eq!(decide(true, format, root, &input), "allow", "{input}");
        }
        // Without the marker (issue tasks' read-only stages) reads are as before.
        for (format, input) in [
            (
                GuardFormat::ClaudeCode,
                cc("Read", json!({"file_path": "/etc/hosts"})),
            ),
            (
                GuardFormat::Pi,
                pi("read", json!({"path": "~/.ssh/id_rsa"})),
            ),
            (GuardFormat::Codex, bare("cat /etc/hosts")),
        ] {
            assert_eq!(decide(false, format, root, &input), "allow", "{input}");
        }
    }

    #[test]
    fn garbage_input_fails_closed() {
        let r = root();
        for bad in ["", "not json", "{}", r#"{"tool":"bash"}"#] {
            let pi = run_guard(GuardFormat::Pi, Some(r.path()), bad);
            assert!(pi.stdout.contains("deny"), "pi {bad:?}: {pi:?}");
        }
        let cc = run_guard(GuardFormat::ClaudeCode, Some(r.path()), "not json");
        assert_eq!(cc.exit_code, 2);
        assert!(!cc.stderr.is_empty());
    }

    #[test]
    fn relative_pi_paths_resolve_against_cwd() {
        let r = root();
        std::fs::create_dir(r.path().join("sub")).unwrap();
        let input = json!({"tool": "write", "args": {"path": "x.rs"}, "cwd": r.path().join("sub")})
            .to_string();
        assert_eq!(
            run_guard(GuardFormat::Pi, Some(r.path()), &input).stdout,
            r#"{"decision":"allow"}"#
        );
    }

    #[test]
    fn codex_patch_outside_the_worktree_is_denied() {
        let r = root();
        let patch = "*** Begin Patch\n*** Add File: /etc/evil\n+x\n*** End Patch";
        let input = json!({"hook_event_name": "PreToolUse", "tool_name": "apply_patch", "tool_input": {"command": patch}}).to_string();
        let out = run_guard(GuardFormat::Codex, Some(r.path()), &input);
        let v: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        let ok = json!({"tool_name": "apply_patch", "tool_input": {"command": "*** Begin Patch\n*** Add File: notes.txt\n+x\n*** End Patch"}}).to_string();
        assert_eq!(
            run_guard(GuardFormat::Codex, Some(r.path()), &ok).stdout,
            ""
        );
        assert_eq!(
            run_guard(GuardFormat::Codex, Some(r.path()), "nope").exit_code,
            2
        );
    }
}
