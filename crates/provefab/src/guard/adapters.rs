//! Maps each worker's tool-call format onto `ToolCall`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::ToolCall;

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

/// A read tool's paths: each key present, or the default directory `.`.
fn read(args: &Value, keys: &[&str], default_dir: bool, pattern: Option<&str>) -> ToolCall {
    let mut paths: Vec<PathBuf> = keys
        .iter()
        .filter_map(|k| str_arg(args, k))
        .map(PathBuf::from)
        .collect();
    if paths.is_empty() && default_dir {
        paths.push(PathBuf::from("."));
    }
    ToolCall::Read {
        paths,
        pattern_is_option: pattern
            .and_then(|k| str_arg(args, k))
            .is_some_and(|p| p.starts_with('-')),
    }
}

/// The directories a glob pattern starts from: its leading components
/// without a wildcard (`/etc/*` reads `/etc`), under `base`.
fn glob_base(base: &str, pattern: &str) -> PathBuf {
    let fixed: PathBuf = Path::new(pattern)
        .components()
        .take_while(|c| {
            !c.as_os_str()
                .to_string_lossy()
                .contains(['*', '?', '[', '{'])
        })
        .collect();
    Path::new(base).join(fixed)
}

fn malformed(tool: &str, key: &str) -> ToolCall {
    ToolCall::Blocked {
        reason: format!("`{tool}` call without a `{key}` string"),
    }
}

/// Pi built-in tools: `bash {command}`, `edit {path}`, `write {path}`; paths may be relative.
pub fn from_pi(tool: &str, args: &Value) -> ToolCall {
    match tool {
        "bash" => str_arg(args, "command")
            .map(|command| ToolCall::Shell { command })
            .unwrap_or_else(|| malformed(tool, "command")),
        "edit" | "write" => str_arg(args, "path")
            .map(|p| ToolCall::Write {
                path: PathBuf::from(p),
            })
            .unwrap_or_else(|| malformed(tool, "path")),
        "read" => read(args, &["path", "file_path"], false, None),
        "grep" => read(args, &["path"], true, Some("pattern")),
        "find" => read(args, &["path"], true, Some("pattern")),
        "ls" => read(args, &["path"], true, None),
        // Pi's Windows shell tool: never expected on macOS, and the shell policy only parses sh.
        "powershell" => ToolCall::Blocked {
            reason: "`powershell` is disabled in provefab workers".into(),
        },
        other => ToolCall::Other {
            name: other.to_string(),
        },
    }
}

/// Claude Code PreToolUse `tool_name` / `tool_input`.
pub fn from_claude_code(tool: &str, input: &Value) -> ToolCall {
    let write = |key: &str| {
        str_arg(input, key)
            .map(|p| ToolCall::Write {
                path: PathBuf::from(p),
            })
            .unwrap_or_else(|| malformed(tool, key))
    };
    match tool {
        "Bash" => str_arg(input, "command")
            .map(|command| ToolCall::Shell { command })
            .unwrap_or_else(|| malformed(tool, "command")),
        "Edit" | "Write" | "MultiEdit" => write("file_path"),
        "NotebookEdit" => write("notebook_path"),
        "Read" => read(input, &["file_path"], false, None),
        "Grep" => read(input, &["path"], true, None),
        "Glob" => {
            let base = str_arg(input, "path").unwrap_or_else(|| ".".into());
            let pattern = str_arg(input, "pattern").unwrap_or_default();
            ToolCall::Read {
                paths: vec![PathBuf::from(&base), glob_base(&base, &pattern)],
                pattern_is_option: false,
            }
        }
        "WebFetch" | "WebSearch" => ToolCall::Blocked {
            reason: format!("`{tool}` is disabled in provefab workers"),
        },
        t if t.starts_with("mcp__") => ToolCall::Blocked {
            reason: "MCP tools are disabled in provefab workers".into(),
        },
        other => ToolCall::Other {
            name: other.to_string(),
        },
    }
}

/// Codex PreToolUse `tool_name` / `tool_input`. Shell calls arrive as `Bash`
/// with `{"command"}`; file edits as `apply_patch` whose `command` is the patch text.
pub fn from_codex(tool: &str, input: &Value) -> ToolCall {
    match tool {
        "Bash" => str_arg(input, "command")
            .map(|command| ToolCall::ShellUnknownCwd { command })
            .unwrap_or_else(|| malformed(tool, "command")),
        "apply_patch" => match str_arg(input, "command").map(|p| parse_patch(&p)) {
            Some(Ok(paths)) => ToolCall::Patch { paths },
            Some(Err(reason)) => ToolCall::Blocked { reason },
            None => malformed(tool, "command"),
        },
        "Edit" | "Write" => str_arg(input, "file_path")
            .or_else(|| str_arg(input, "path"))
            .map(|p| ToolCall::Write {
                path: PathBuf::from(p),
            })
            .unwrap_or_else(|| malformed(tool, "file_path")),
        t if t.starts_with("mcp__") => ToolCall::Blocked {
            reason: "MCP tools are disabled in provefab workers".into(),
        },
        other => ToolCall::Other {
            name: other.to_string(),
        },
    }
}

/// Paths named by a Codex patch (`*** Add File:`, `*** Update File:`,
/// `*** Delete File:`, `*** Move to:`).
pub fn patch_paths(patch: &str) -> Vec<PathBuf> {
    parse_patch(patch).unwrap_or_default()
}

/// Like `patch_paths`, but an unknown `*** ` header is an error: Codex trims
/// each line before matching headers, so the guard must never skip one it
/// does not understand.
pub fn parse_patch(patch: &str) -> Result<Vec<PathBuf>, String> {
    const MARKERS: [&str; 4] = [
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];
    const FRAMING: [&str; 3] = ["*** Begin Patch", "*** End Patch", "*** End of File"];
    let mut paths = Vec::new();
    for line in patch.lines() {
        let line = line.trim();
        if let Some(p) = MARKERS.iter().find_map(|m| line.strip_prefix(m)) {
            paths.push(PathBuf::from(p.trim()));
        } else if line.starts_with("*** ") && !FRAMING.contains(&line) {
            return Err(format!(
                "patch header `{line}` is not one the guard understands"
            ));
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pi_powershell_is_blocked() {
        assert!(matches!(
            from_pi("powershell", &json!({"command": "git push"})),
            ToolCall::Blocked { .. }
        ));
    }

    #[test]
    fn pi_mapping() {
        assert_eq!(
            from_pi("bash", &json!({"command": "ls"})),
            ToolCall::Shell {
                command: "ls".into()
            }
        );
        assert_eq!(
            from_pi("write", &json!({"path": "a.rs", "content": ""})),
            ToolCall::Write {
                path: "a.rs".into()
            }
        );
        assert_eq!(
            from_pi("read", &json!({"path": "a.rs"})),
            ToolCall::Read {
                paths: vec!["a.rs".into()],
                pattern_is_option: false
            }
        );
        assert_eq!(
            from_pi("grep", &json!({"pattern": "-x"})),
            ToolCall::Read {
                paths: vec![".".into()],
                pattern_is_option: true
            }
        );
        assert!(matches!(
            from_pi("bash", &json!({})),
            ToolCall::Blocked { .. }
        ));
    }

    #[test]
    fn claude_code_mapping() {
        assert_eq!(
            from_claude_code("Edit", &json!({"file_path": "/w/a.rs"})),
            ToolCall::Write {
                path: "/w/a.rs".into()
            }
        );
        assert_eq!(
            from_claude_code("NotebookEdit", &json!({"notebook_path": "/w/n.ipynb"})),
            ToolCall::Write {
                path: "/w/n.ipynb".into()
            }
        );
        assert!(matches!(
            from_claude_code("WebFetch", &json!({})),
            ToolCall::Blocked { .. }
        ));
        assert!(matches!(
            from_claude_code("mcp__x__y", &json!({})),
            ToolCall::Blocked { .. }
        ));
        assert!(matches!(
            from_claude_code("Bash", &json!({"command": 3})),
            ToolCall::Blocked { .. }
        ));
        assert_eq!(
            from_claude_code("Grep", &json!({})),
            ToolCall::Read {
                paths: vec![".".into()],
                pattern_is_option: false
            }
        );
        assert_eq!(
            from_claude_code("Glob", &json!({"pattern": "/etc/**/*.conf", "path": "src"})),
            ToolCall::Read {
                paths: vec!["src".into(), "/etc".into()],
                pattern_is_option: false
            }
        );
        assert_eq!(
            from_claude_code("TodoWrite", &json!({})),
            ToolCall::Other {
                name: "TodoWrite".into()
            }
        );
    }

    #[test]
    fn codex_mapping() {
        assert_eq!(
            from_codex("Bash", &json!({"command": "ls"})),
            ToolCall::ShellUnknownCwd {
                command: "ls".into()
            }
        );
        let patch = "*** Begin Patch\n*** Add File: /w/a.rs\n+x\n*** Update File: src/b.rs\n*** Move to: src/c.rs\n@@\n-y\n+z\n*** Delete File: old.rs\n*** End Patch";
        assert_eq!(
            from_codex("apply_patch", &json!({"command": patch})),
            ToolCall::Patch {
                paths: vec![
                    "/w/a.rs".into(),
                    "src/b.rs".into(),
                    "src/c.rs".into(),
                    "old.rs".into()
                ]
            }
        );
        assert!(matches!(
            from_codex("apply_patch", &json!({})),
            ToolCall::Blocked { .. }
        ));
        assert!(matches!(
            from_codex("mcp__x__y", &json!({})),
            ToolCall::Blocked { .. }
        ));
        assert_eq!(
            from_codex("view_image", &json!({})),
            ToolCall::Other {
                name: "view_image".into()
            }
        );
    }

    #[test]
    fn patch_lines_that_only_look_like_markers_are_ignored() {
        assert!(patch_paths("+*** Add File: fake\n context *** Update File: x").is_empty());
    }

    /// Final review #1: Codex trims header lines, so indented headers must be seen too.
    #[test]
    fn review_1_indented_patch_headers_are_checked() {
        let patch = "*** Begin Patch\n*** Add File: ok.txt\n+x\n *** Add File: .github/workflows/evil.yml\n+on: push\n\t*** Delete File: .github/workflows/ci.yml\n*** End Patch";
        let ToolCall::Patch { paths } = from_codex("apply_patch", &json!({"command": patch}))
        else {
            panic!("expected a patch")
        };
        assert!(
            paths.contains(&PathBuf::from(".github/workflows/evil.yml")),
            "{paths:?}"
        );
        assert!(
            paths.contains(&PathBuf::from(".github/workflows/ci.yml")),
            "{paths:?}"
        );
    }

    /// Final review #1: a header the guard does not know is refused, not skipped.
    #[test]
    fn review_1_unknown_patch_header_is_blocked() {
        let patch = "*** Begin Patch\n*** Frobnicate File: x\n*** End Patch";
        assert!(matches!(
            from_codex("apply_patch", &json!({"command": patch})),
            ToolCall::Blocked { .. }
        ));
    }

    /// Final review #2: Codex runs shell calls in a `workdir` the hook never sees.
    #[test]
    fn review_2_codex_shell_cwd_is_unknown() {
        assert_eq!(
            from_codex("Bash", &json!({"command": "touch x"})),
            ToolCall::ShellUnknownCwd {
                command: "touch x".into()
            }
        );
    }
}
