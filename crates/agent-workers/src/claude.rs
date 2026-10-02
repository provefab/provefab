use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::process::{Flow, Transcript, apply_worker_env, prepare_git_hooks, run_jsonl};
use crate::types::digest;
use crate::{
    ExitReason, StageRequest, StageResult, ToolProfile, Usage, Worker, WorkerError, WorkerEvent,
};

/// Runs a stage with the unmodified `claude` binary in print mode, signed in
/// with the user's own subscription from a dedicated config dir (spec §5.3, D14).
pub struct ClaudeCodeWorker {
    pub program: PathBuf,
    /// The installed cc-provefab plugin directory.
    pub plugin_dir: PathBuf,
    /// `CLAUDE_CONFIG_DIR`, logged in once with `claude auth login`.
    pub config_dir: PathBuf,
    /// The `provefab` binary the plugin's hook calls for `provefab guard`.
    pub provefab_bin: PathBuf,
}

/// Credentials and provider switches that would move Claude Code off the subscription (D14).
const API_KEY_ENV: [&str; 5] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
];

impl ClaudeCodeWorker {
    pub fn args(&self, req: &StageRequest) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["-p".into(), req.prompt.clone().into()];
        let mut push = |s: &str| a.push(s.into());
        push("--output-format");
        push("stream-json");
        push("--verbose");
        push("--model");
        push(&req.model);
        // Skip the repo's .claude/settings.json and .mcp.json (spec D20).
        push("--setting-sources");
        push("user");
        push("--strict-mcp-config");
        push("--tools");
        push(match req.tools {
            // No shell: nothing the worktree holds can run (final review I1).
            ToolProfile::ReadOnly | ToolProfile::UntrustedReadOnly => "Read,Grep,Glob",
            ToolProfile::Full => "Bash,Read,Edit,Write,Glob,Grep",
            // `""` disables every built-in tool; `--json-schema` still answers.
            ToolProfile::NoTools => "",
        });
        if req.tools == ToolProfile::Full {
            // With --permission-prompts none, shell commands outside Claude Code's
            // read-only set need a pre-approval. The cc-provefab PreToolUse hook
            // (provefab guard) still runs first and can deny any call.
            push("--allowedTools");
            push("Bash");
        }
        push("--permission-mode");
        push("acceptEdits");
        push("--permission-prompts");
        push("none");
        push("--max-turns");
        push(&req.max_turns.to_string());
        a.push("--plugin-dir".into());
        a.push(self.plugin_dir.clone().into_os_string());
        if let Some(f) = &req.system_prompt_file {
            a.push("--append-system-prompt-file".into());
            a.push(f.clone().into_os_string());
        }
        if let Some(schema) = &req.output_schema {
            a.push("--json-schema".into());
            a.push(schema.to_string().into());
        }
        a
    }
}

impl ClaudeCodeWorker {
    /// The full command, environment included, for one stage.
    pub fn command(&self, req: &StageRequest) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(self.args(req)).current_dir(&req.cwd);
        apply_worker_env(
            &mut cmd,
            &req.cwd,
            &req.session_dir.join("git-hooks"),
            req.tools,
        );
        for key in API_KEY_ENV {
            cmd.env_remove(key);
        }
        cmd.env("CLAUDE_CONFIG_DIR", &self.config_dir);
        cmd.env("PROVEFAB_BIN", &self.provefab_bin);
        cmd
    }
}

impl Worker for ClaudeCodeWorker {
    async fn run(
        &self,
        req: &StageRequest,
        events: UnboundedSender<WorkerEvent>,
    ) -> Result<StageResult, WorkerError> {
        let cmd = self.command(req);
        prepare_git_hooks(&req.session_dir).map_err(|e| WorkerError::Io(e.to_string()))?;
        let mut transcript = Transcript::open(&req.session_dir)?;
        let mut state = ClaudeState::default();
        let finished = run_jsonl(cmd, req.timeout, |v| {
            transcript.write(&v);
            state.on_record(&v, &events);
            Flow::Continue
        })
        .await?;
        let exit = if finished.timed_out {
            ExitReason::Timeout
        } else if let Some(exit) = state.exit.take() {
            exit
        } else {
            ExitReason::Crashed {
                code: finished.status.and_then(|s| s.code()),
                stderr_tail: finished.stderr_tail,
            }
        };
        Ok(StageResult {
            exit,
            structured_output: state.output,
            final_text: state.final_text,
            usage: state.usage,
            turns: state.turns,
            actual_model: state.actual_model,
        })
    }
}

#[derive(Default)]
struct ClaudeState {
    tool_names: HashMap<String, String>,
    rate_limit: Option<String>,
    exit: Option<ExitReason>,
    output: Option<Value>,
    final_text: Option<String>,
    usage: Usage,
    turns: u32,
    actual_model: Option<String>,
}

impl ClaudeState {
    fn on_record(&mut self, v: &Value, events: &UnboundedSender<WorkerEvent>) {
        let emit = |e| {
            let _ = events.send(e);
        };
        let blocks = |v: &Value| -> Vec<Value> {
            v.pointer("/message/content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "assistant" => {
                for b in blocks(v) {
                    match b.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
                            let name = b
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            if let Some(id) = b.get("id").and_then(Value::as_str) {
                                self.tool_names.insert(id.to_string(), name.clone());
                            }
                            emit(WorkerEvent::ToolStart {
                                name,
                                input: digest(b.get("input").unwrap_or(&Value::Null)),
                            });
                        }
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(Value::as_str) {
                                emit(WorkerEvent::Text(t.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                for b in blocks(v) {
                    if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let id = b
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let name = self.tool_names.get(id).cloned().unwrap_or_default();
                        let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                        emit(WorkerEvent::ToolEnd { name, is_error });
                    }
                }
            }
            "system" if v.get("subtype").and_then(Value::as_str) == Some("init") => {
                self.actual_model = v.get("model").and_then(Value::as_str).map(str::to_string);
            }
            "system" if v.get("subtype").and_then(Value::as_str) == Some("api_retry") => {
                let error = v
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                if error == "rate_limit" {
                    emit(WorkerEvent::RateLimited {
                        message: error.clone(),
                    });
                    self.rate_limit = Some(error);
                } else {
                    emit(WorkerEvent::Retry { message: error });
                }
            }
            "result" => {
                self.turns = v.get("num_turns").and_then(Value::as_u64).unwrap_or(0) as u32;
                let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
                // input_tokens excludes cache tokens; they are priced apart (D74).
                self.usage = Usage {
                    input_tokens: n("/usage/input_tokens"),
                    output_tokens: n("/usage/output_tokens"),
                    cache_read_tokens: n("/usage/cache_read_input_tokens"),
                    cache_write_tokens: n("/usage/cache_creation_input_tokens"),
                };
                self.output = v.get("structured_output").filter(|o| !o.is_null()).cloned();
                self.final_text = v.get("result").and_then(Value::as_str).map(str::to_string);
                let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or_default();
                let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                self.exit = Some(if subtype == "error_max_turns" && self.output.is_none() {
                    ExitReason::MaxTurns
                } else if subtype == "error_max_turns" {
                    // An answer delivered at the limit still completes the stage.
                    ExitReason::Completed
                } else if !is_error {
                    ExitReason::Completed
                } else if let Some(r) = self.rate_limit.clone() {
                    ExitReason::RateLimited(r)
                } else {
                    ExitReason::ProviderError(
                        self.final_text
                            .clone()
                            .unwrap_or_else(|| subtype.to_string()),
                    )
                });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::NO_TOOLS_ENV;
    use serde_json::json;
    use std::time::Duration;

    fn worker() -> ClaudeCodeWorker {
        ClaudeCodeWorker {
            program: "claude".into(),
            plugin_dir: "/x/cc-provefab".into(),
            config_dir: "/home/.provefab/claude".into(),
            provefab_bin: "/x/provefab".into(),
        }
    }

    fn req() -> StageRequest {
        StageRequest {
            cwd: "/w".into(),
            prompt: "Review the diff".into(),
            model: "opus".into(),
            provider: None,
            tools: ToolProfile::ReadOnly,
            system_prompt_file: None,
            output_schema: Some(json!({"type": "object"})),
            max_turns: 40,
            timeout: Duration::from_secs(60),
            session_dir: "/w/.s".into(),
        }
    }

    #[test]
    fn args_isolate_the_repo_and_pass_the_schema() {
        let args: Vec<String> = worker()
            .args(&req())
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        let joined = args.join(" ");
        assert!(
            joined.starts_with(
                "-p Review the diff --output-format stream-json --verbose --model opus"
            ),
            "{joined}"
        );
        for part in [
            "--setting-sources user",
            "--strict-mcp-config",
            "--tools Read,Grep,Glob",
            "--permission-prompts none",
            "--max-turns 40",
            "--plugin-dir /x/cc-provefab",
            r#"--json-schema {"type":"object"}"#,
        ] {
            assert!(joined.contains(part), "missing {part}: {joined}");
        }
        assert!(
            !joined.contains("--bare"),
            "bare mode ignores the subscription login"
        );
    }

    /// Final review I4: under `--permission-prompts none`, shell commands need a
    /// pre-approval; the guard's PreToolUse deny still runs first.
    #[test]
    fn review_i4_full_profile_pre_approves_bash_only() {
        let mut full = req();
        full.tools = ToolProfile::Full;
        let joined = |r: &StageRequest| {
            worker()
                .args(r)
                .into_iter()
                .map(|s| s.into_string().unwrap())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(
            joined(&full).contains("--allowedTools Bash"),
            "{}",
            joined(&full)
        );
        assert!(
            !joined(&req()).contains("--allowedTools"),
            "read-only must not pre-approve Bash"
        );
    }

    /// Repository rules pre-flight S2: no built-in tool, and the guard is told
    /// to refuse every call; the answer still comes through `--json-schema`.
    #[test]
    fn no_tools_lists_no_tool_and_tells_the_guard() {
        let mut none = req();
        none.tools = ToolProfile::NoTools;
        let args: Vec<String> = worker()
            .args(&none)
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        let at = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[at + 1], "", "{args:?}");
        assert!(!args.iter().any(|a| a == "--allowedTools"), "{args:?}");
        assert!(args.iter().any(|a| a == "--json-schema"), "{args:?}");
        let no_tools = |r: &StageRequest| {
            worker()
                .command(r)
                .as_std()
                .get_envs()
                .find(|(k, _)| *k == NO_TOOLS_ENV)
                .map(|(_, v)| v.map(|v| v.to_string_lossy().to_string()))
        };
        assert_eq!(no_tools(&none), Some(Some("1".into())));
        assert_eq!(no_tools(&req()), Some(None), "removed for other stages");
    }

    /// Final review I1 (pull request reviews): Claude Code reviews a person's
    /// pull request with Read, Grep and Glob only, no shell, and tells the guard.
    #[test]
    fn an_untrusted_review_has_no_shell_and_tells_the_guard() {
        let mut review = req();
        review.tools = ToolProfile::UntrustedReadOnly;
        let args: Vec<String> = worker()
            .args(&review)
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        let at = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[at + 1], "Read,Grep,Glob", "{args:?}");
        assert!(!args.iter().any(|a| a == "--allowedTools"), "{args:?}");
        let marker = worker()
            .command(&review)
            .as_std()
            .get_envs()
            .find(|(k, _)| *k == crate::UNTRUSTED_REVIEW_ENV)
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().to_string()));
        assert_eq!(marker.as_deref(), Some("1"));
    }

    /// Final review M6: other switches that move Claude Code off the subscription are removed too.
    #[test]
    fn review_m6_provider_switches_are_removed() {
        let cmd = worker().command(&req());
        let removed: Vec<String> = cmd
            .as_std()
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into())
            .collect();
        for key in [
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
        ] {
            assert!(removed.iter().any(|k| k == key), "{key} kept: {removed:?}");
        }
    }

    /// Final review M1: a structured answer delivered at the turn limit completes the stage.
    #[test]
    fn review_m1_output_at_max_turns_is_completed() {
        let mut s = ClaudeState::default();
        feed(
            &mut s,
            &[
                json!({"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":40,"structured_output":{"verdict":"approve"}}),
            ],
        );
        assert_eq!(s.exit, Some(ExitReason::Completed));
    }

    #[test]
    fn command_removes_api_keys_and_sets_the_provefab_config_dir() {
        let cmd = worker().command(&req());
        let envs: Vec<(String, Option<String>)> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into(),
                    v.map(|v| v.to_string_lossy().into()),
                )
            })
            .collect();
        assert!(
            envs.contains(&("ANTHROPIC_API_KEY".into(), None)),
            "{envs:?}"
        );
        assert!(
            envs.contains(&("ANTHROPIC_AUTH_TOKEN".into(), None)),
            "{envs:?}"
        );
        assert!(envs.contains(&("SSH_AUTH_SOCK".into(), None)), "{envs:?}");
        assert!(
            envs.contains(&(
                "CLAUDE_CONFIG_DIR".into(),
                Some("/home/.provefab/claude".into())
            )),
            "{envs:?}"
        );
        assert!(
            envs.contains(&("PROVEFAB_WORKTREE".into(), Some("/w".into()))),
            "{envs:?}"
        );
    }

    fn feed(s: &mut ClaudeState, records: &[Value]) -> Vec<WorkerEvent> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for r in records {
            s.on_record(r, &tx);
        }
        drop(tx);
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    #[test]
    fn parses_tool_calls_results_and_structured_output() {
        let mut s = ClaudeState::default();
        let events = feed(
            &mut s,
            &[
                json!({"type":"system","subtype":"init","model":"claude-sonnet-5-5"}),
                json!({"type":"assistant","message":{"content":[{"type":"text","text":"Checking"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"git push"}}]}}),
                json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"denied"}]}}),
                json!({"type":"result","subtype":"success","is_error":false,"num_turns":3,"result":"done","structured_output":{"verdict":"approve"},"usage":{"input_tokens":50,"cache_creation_input_tokens":1000,"cache_read_input_tokens":20000,"output_tokens":7}}),
            ],
        );
        assert_eq!(
            events,
            vec![
                WorkerEvent::Text("Checking".into()),
                WorkerEvent::ToolStart {
                    name: "Bash".into(),
                    input: r#"{"command":"git push"}"#.into()
                },
                WorkerEvent::ToolEnd {
                    name: "Bash".into(),
                    is_error: true
                },
            ]
        );
        assert_eq!(s.exit, Some(ExitReason::Completed));
        assert_eq!(s.output, Some(json!({"verdict":"approve"})));
        assert_eq!(s.turns, 3);
        assert_eq!(
            s.usage,
            Usage {
                input_tokens: 50,
                output_tokens: 7,
                cache_read_tokens: 20000,
                cache_write_tokens: 1000,
            }
        );
        assert_eq!(s.actual_model.as_deref(), Some("claude-sonnet-5-5"));
    }

    #[test]
    fn maps_max_turns_and_rate_limits() {
        let mut s = ClaudeState::default();
        feed(
            &mut s,
            &[json!({"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":40})],
        );
        assert_eq!(s.exit, Some(ExitReason::MaxTurns));

        let mut s = ClaudeState::default();
        let events = feed(
            &mut s,
            &[
                json!({"type":"system","subtype":"api_retry","attempt":1,"error":"rate_limit","error_status":429}),
                json!({"type":"result","subtype":"error_during_execution","is_error":true,"result":"limit"}),
            ],
        );
        assert_eq!(
            events,
            vec![WorkerEvent::RateLimited {
                message: "rate_limit".into()
            }]
        );
        assert_eq!(s.exit, Some(ExitReason::RateLimited("rate_limit".into())));
    }
}
