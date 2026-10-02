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
                guard::check_review(&call)
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
            assert_eq!(
                decision(true, GuardFormat::Codex, codex(command)),
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
