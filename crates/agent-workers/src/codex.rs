use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::process::{Flow, Transcript, apply_worker_env, prepare_git_hooks, run_jsonl};
use crate::types::{digest, looks_rate_limited};
use crate::{
    ExitReason, StageRequest, StageResult, ToolProfile, Usage, Worker, WorkerError, WorkerEvent,
};

/// Runs a stage with the unmodified `codex` CLI (`codex exec --json`), signed in
/// with the user's own ChatGPT plan from a dedicated `CODEX_HOME` (spec §5.5, D27).
///
/// The guard hook lives in `CODEX_HOME/hooks.json` and must already be trusted
/// (see Provefab's `codex_setup`). This worker never passes
/// `--dangerously-bypass-hook-trust`: that flag also runs the target repo's own
/// `.codex/` hooks (spec D28).
pub struct CodexWorker {
    pub program: PathBuf,
    /// `CODEX_HOME`, logged in once with `codex login`.
    pub codex_home: PathBuf,
    /// The `provefab` binary the guard hook calls for `provefab guard`.
    pub provefab_bin: PathBuf,
}

/// Credentials and endpoints that would move Codex off the ChatGPT sign-in.
const API_KEY_ENV: [&str; 3] = ["OPENAI_API_KEY", "CODEX_API_KEY", "OPENAI_BASE_URL"];

/// Inherited variables that could move Codex off Provefab's ChatGPT sign-in
/// (`CODEX_ACCESS_TOKEN` takes precedence over the stored login, for example):
/// every `CODEX_*` except `CODEX_HOME`, and every `OPENAI_*`.
fn inherited_overrides(keys: impl Iterator<Item = String>) -> Vec<String> {
    keys.filter(|k| (k.starts_with("CODEX_") && k != "CODEX_HOME") || k.starts_with("OPENAI_"))
        .collect()
}

impl CodexWorker {
    /// `prompt` is the full text sent to the agent (Codex has no separate
    /// system-prompt flag, so `run` puts the stage instructions first).
    pub fn args(&self, req: &StageRequest, prompt: &str) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["exec".into(), "--json".into(), "-C".into()];
        a.push(req.cwd.clone().into_os_string());
        let mut push = |s: &str| a.push(s.into());
        push("-m");
        push(&req.model);
        // Codex's own sandbox is a second layer behind the guard: read-only for
        // plan and review, writes confined to the worktree (no network) for implement.
        push("--sandbox");
        push(match req.tools {
            ToolProfile::ReadOnly => "read-only",
            ToolProfile::Full => "workspace-write",
        });
        // Execpolicy `.rules` files from the user or the repo must not widen what runs.
        push("--ignore-rules");
        if req.output_schema.is_some() {
            a.push("--output-schema".into());
            a.push(req.session_dir.join("output-schema.json").into_os_string());
        }
        a.push(prompt.into());
        a
    }

    /// The full command, environment included, for one stage.
    pub fn command(&self, req: &StageRequest, prompt: &str) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(self.args(req, prompt)).current_dir(&req.cwd);
        apply_worker_env(&mut cmd, &req.cwd, &req.session_dir.join("git-hooks"));
        for key in API_KEY_ENV {
            cmd.env_remove(key);
        }
        for key in inherited_overrides(std::env::vars().map(|(k, _)| k)) {
            cmd.env_remove(key);
        }
        cmd.env("CODEX_HOME", &self.codex_home);
        cmd.env("PROVEFAB_BIN", &self.provefab_bin);
        cmd
    }
}

impl Worker for CodexWorker {
    async fn run(
        &self,
        req: &StageRequest,
        events: UnboundedSender<WorkerEvent>,
    ) -> Result<StageResult, WorkerError> {
        let io = |e: std::io::Error| WorkerError::Io(e.to_string());
        std::fs::create_dir_all(&req.session_dir).map_err(io)?;
        if let Some(schema) = &req.output_schema {
            std::fs::write(
                req.session_dir.join("output-schema.json"),
                schema.to_string(),
            )
            .map_err(io)?;
        }
        let prompt = match &req.system_prompt_file {
            Some(f) => format!(
                "{}\n\n{}",
                std::fs::read_to_string(f).map_err(io)?.trim_end(),
                req.prompt
            ),
            None => req.prompt.clone(),
        };
        prepare_git_hooks(&req.session_dir).map_err(io)?;
        let cmd = self.command(req, &prompt);
        let mut transcript = Transcript::open(&req.session_dir)?;
        let mut state = CodexState {
            wants_output: req.output_schema.is_some(),
            ..CodexState::default()
        };
        let finished = run_jsonl(cmd, req.timeout, |v| {
            transcript.write(&v);
            state.on_record(&v, &events);
            Flow::Continue
        })
        .await?;
        let exit = if finished.timed_out {
            ExitReason::Timeout
        } else if state.completed && finished.status.is_some_and(|s| s.success()) {
            // Errors Codex retried before the turn completed do not decide the outcome.
            ExitReason::Completed
        } else if let Some(err) = state.error.clone() {
            if looks_rate_limited(&err) {
                ExitReason::RateLimited(err)
            } else {
                ExitReason::ProviderError(err)
            }
        } else if !state.completed || !finished.status.is_some_and(|s| s.success()) {
            // No `turn.completed`: Codex never finished the turn.
            ExitReason::Crashed {
                code: finished.status.and_then(|s| s.code()),
                stderr_tail: finished.stderr_tail,
            }
        } else {
            ExitReason::Completed
        };
        Ok(StageResult {
            exit,
            structured_output: state.output,
            final_text: state.final_text,
            usage: state.usage,
            turns: state.turns,
            actual_model: None,
        })
    }
}

#[derive(Default)]
struct CodexState {
    wants_output: bool,
    completed: bool,
    error: Option<String>,
    output: Option<Value>,
    final_text: Option<String>,
    usage: Usage,
    turns: u32,
}

impl CodexState {
    fn on_record(&mut self, v: &Value, events: &UnboundedSender<WorkerEvent>) {
        let emit = |e| {
            let _ = events.send(e);
        };
        let item = v.get("item").unwrap_or(&Value::Null);
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "item.started" => match item_type {
                "command_execution" => emit(WorkerEvent::ToolStart {
                    name: "Bash".into(),
                    input: item
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }),
                "file_change" => emit(WorkerEvent::ToolStart {
                    name: "apply_patch".into(),
                    input: digest(item.get("changes").unwrap_or(&Value::Null)),
                }),
                _ => {}
            },
            "item.completed" => match item_type {
                "command_execution" => emit(WorkerEvent::ToolEnd {
                    name: "Bash".into(),
                    is_error: item.get("exit_code").and_then(Value::as_i64) != Some(0)
                        || status != "completed",
                }),
                "file_change" => emit(WorkerEvent::ToolEnd {
                    name: "apply_patch".into(),
                    is_error: status != "completed",
                }),
                // A call the guard hook refused arrives as an error item.
                // A call the guard hook refused. Other error items (config
                // warnings, reroute notices) are not tool calls; they stay in the transcript.
                "error"
                    if item
                        .get("message")
                        .and_then(Value::as_str)
                        .is_some_and(|m| m.starts_with("Command blocked by PreToolUse hook")) =>
                {
                    emit(WorkerEvent::ToolEnd {
                        name: "blocked".into(),
                        is_error: true,
                    })
                }
                "agent_message" => {
                    let text = item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if self.wants_output {
                        self.output = serde_json::from_str(&text).ok();
                    }
                    emit(WorkerEvent::Text(text.clone()));
                    self.final_text = Some(text);
                }
                _ => {}
            },
            "turn.completed" => {
                self.completed = true;
                self.turns += 1;
                emit(WorkerEvent::TurnEnd);
                let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
                // Codex's input_tokens include the cached ones (D74).
                let cached = n("/usage/cached_input_tokens");
                self.usage.input_tokens += n("/usage/input_tokens").saturating_sub(cached);
                self.usage.cache_read_tokens += cached;
                self.usage.output_tokens += n("/usage/output_tokens");
            }
            "turn.failed" => {
                self.error = Some(
                    v.pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("turn failed")
                        .to_string(),
                );
            }
            "error" => {
                let message = v
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string();
                // "Reconnecting... n/5" is a stream error Codex is retrying, not a limit.
                if message.starts_with("Reconnecting") {
                    emit(WorkerEvent::Retry {
                        message: message.clone(),
                    });
                } else if looks_rate_limited(&message) {
                    emit(WorkerEvent::RateLimited {
                        message: message.clone(),
                    });
                }
                self.error = Some(message);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn worker() -> CodexWorker {
        CodexWorker {
            program: "codex".into(),
            codex_home: "/home/.provefab/codex".into(),
            provefab_bin: "/x/provefab".into(),
        }
    }

    fn req(tools: ToolProfile, schema: bool) -> StageRequest {
        StageRequest {
            cwd: "/w".into(),
            prompt: "Fix it".into(),
            model: "gpt-5.5".into(),
            provider: None,
            tools,
            system_prompt_file: None,
            output_schema: schema.then(|| json!({"type": "object"})),
            max_turns: 40,
            timeout: Duration::from_secs(60),
            session_dir: "/w/.s".into(),
        }
    }

    fn joined(args: Vec<OsString>) -> String {
        args.into_iter()
            .map(|s| s.into_string().unwrap())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn args_pick_the_sandbox_from_the_tool_profile_and_never_bypass_hook_trust() {
        let full = joined(worker().args(&req(ToolProfile::Full, false), "Fix it"));
        assert!(
            full.starts_with(
                "exec --json -C /w -m gpt-5.5 --sandbox workspace-write --ignore-rules"
            ),
            "{full}"
        );
        assert!(full.ends_with(" Fix it"), "{full}");
        assert!(!full.contains("--output-schema"), "{full}");
        let review = joined(worker().args(&req(ToolProfile::ReadOnly, true), "Review"));
        assert!(review.contains("--sandbox read-only"), "{review}");
        assert!(
            review.contains("--output-schema /w/.s/output-schema.json"),
            "{review}"
        );
        for a in [&full, &review] {
            assert!(
                !a.contains("bypass"),
                "hook trust must never be bypassed: {a}"
            );
            assert!(!a.contains("danger-full-access"), "{a}");
        }
    }

    #[test]
    fn command_uses_the_provefab_codex_home_and_drops_api_keys() {
        let cmd = worker().command(&req(ToolProfile::Full, false), "Fix it");
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
        for key in [
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "OPENAI_BASE_URL",
            "SSH_AUTH_SOCK",
        ] {
            assert!(envs.contains(&(key.into(), None)), "{key} kept: {envs:?}");
        }
        assert!(envs.contains(&("CODEX_HOME".into(), Some("/home/.provefab/codex".into()))));
        assert!(envs.contains(&("PROVEFAB_BIN".into(), Some("/x/provefab".into()))));
        assert!(envs.contains(&("PROVEFAB_WORKTREE".into(), Some("/w".into()))));
    }

    fn feed(s: &mut CodexState, records: &[Value]) -> Vec<WorkerEvent> {
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
    fn parses_commands_patches_refusals_output_and_usage() {
        let mut s = CodexState {
            wants_output: true,
            ..CodexState::default()
        };
        let events = feed(
            &mut s,
            &[
                json!({"type":"thread.started","thread_id":"t"}),
                json!({"type":"turn.started"}),
                json!({"type":"item.started","item":{"id":"1","type":"command_execution","command":"/bin/bash -lc 'ls'","status":"in_progress"}}),
                json!({"type":"item.completed","item":{"id":"1","type":"command_execution","command":"/bin/bash -lc 'ls'","exit_code":0,"status":"completed"}}),
                json!({"type":"item.completed","item":{"id":"2","type":"error","message":"Command blocked by PreToolUse hook: `git tag` is reserved for Provefab"}}),
                json!({"type":"item.started","item":{"id":"3","type":"file_change","changes":[{"path":"/w/a.rs","kind":"add"}],"status":"in_progress"}}),
                json!({"type":"item.completed","item":{"id":"3","type":"file_change","changes":[{"path":"/w/a.rs","kind":"add"}],"status":"completed"}}),
                json!({"type":"item.completed","item":{"id":"4","type":"agent_message","text":"{\"verdict\":\"approve\"}"}}),
                json!({"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":7}}),
            ],
        );
        assert_eq!(
            events[..4],
            [
                WorkerEvent::ToolStart {
                    name: "Bash".into(),
                    input: "/bin/bash -lc 'ls'".into()
                },
                WorkerEvent::ToolEnd {
                    name: "Bash".into(),
                    is_error: false
                },
                WorkerEvent::ToolEnd {
                    name: "blocked".into(),
                    is_error: true
                },
                WorkerEvent::ToolStart {
                    name: "apply_patch".into(),
                    input: r#"[{"kind":"add","path":"/w/a.rs"}]"#.into()
                },
            ]
        );
        assert!(s.completed);
        assert_eq!(s.turns, 1);
        assert_eq!(s.output, Some(json!({"verdict": "approve"})));
        assert_eq!(
            s.usage,
            // Codex's input_tokens include the cached ones: split them.
            Usage {
                input_tokens: 200,
                output_tokens: 7,
                cache_read_tokens: 800,
                cache_write_tokens: 0,
            }
        );
    }

    #[test]
    fn failed_commands_turn_failures_and_usage_limits() {
        let mut s = CodexState::default();
        let events = feed(
            &mut s,
            &[
                json!({"type":"item.completed","item":{"id":"1","type":"command_execution","command":"cargo test","exit_code":101,"status":"completed"}}),
                json!({"type":"error","message":"You've hit your usage limit. Try again later."}),
            ],
        );
        assert_eq!(
            events,
            vec![
                WorkerEvent::ToolEnd {
                    name: "Bash".into(),
                    is_error: true
                },
                WorkerEvent::RateLimited {
                    message: "You've hit your usage limit. Try again later.".into()
                },
            ]
        );
        let mut s = CodexState::default();
        feed(
            &mut s,
            &[json!({"type":"turn.failed","error":{"message":"model not found"}})],
        );
        assert_eq!(s.error.as_deref(), Some("model not found"));
        assert!(!s.completed);
    }

    #[test]
    fn free_text_is_not_structured_output() {
        let mut s = CodexState {
            wants_output: true,
            ..CodexState::default()
        };
        feed(
            &mut s,
            &[
                json!({"type":"item.completed","item":{"id":"1","type":"agent_message","text":"Looks good to me."}}),
            ],
        );
        assert_eq!(s.output, None);
    }

    /// Final review #6: any CODEX_* or OPENAI_* override (an access token, a refresh URL,
    /// a base URL) could move the run off Provefab's ChatGPT sign-in.
    #[test]
    fn review_6_codex_and_openai_overrides_are_removed() {
        let inherited = [
            "PATH",
            "HOME",
            "CODEX_HOME",
            "CODEX_ACCESS_TOKEN",
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
            "OPENAI_API_KEY",
            "OPENAI_ORG_ID",
        ];
        let mut removed = inherited_overrides(inherited.iter().map(|k| k.to_string()));
        removed.sort();
        assert_eq!(
            removed,
            vec![
                "CODEX_ACCESS_TOKEN",
                "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
                "OPENAI_API_KEY",
                "OPENAI_ORG_ID"
            ]
        );
    }

    /// Final review #8: only hook refusals are "blocked" tool events; other error items
    /// (config warnings, reroute notices) are not tool calls.
    #[test]
    fn review_8_only_hook_refusals_are_blocked_events() {
        let mut s = CodexState::default();
        let events = feed(
            &mut s,
            &[
                json!({"type":"item.completed","item":{"id":"1","type":"error","message":"config: unknown key `foo` ignored"}}),
                json!({"type":"item.completed","item":{"id":"2","type":"error","message":"Command blocked by PreToolUse hook: no"}}),
            ],
        );
        assert_eq!(
            events,
            vec![WorkerEvent::ToolEnd {
                name: "blocked".into(),
                is_error: true
            }]
        );
    }
}
