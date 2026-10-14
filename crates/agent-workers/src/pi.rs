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

/// Name of the tool the pi-provefab extension registers when a stage has an output schema.
pub const SUBMIT_TOOL: &str = "submit_result";

/// Runs a stage with `pi --mode json` (one prompt, JSONL events, then exit).
pub struct PiWorker {
    pub program: PathBuf,
    /// The installed pi-provefab package directory (loaded with `-e`, extension and skills).
    pub package: PathBuf,
    /// The `provefab` binary the extension calls for `provefab guard`.
    pub provefab_bin: PathBuf,
}

impl PiWorker {
    pub fn args(&self, req: &StageRequest) -> Vec<OsString> {
        let mut a: Vec<OsString> = Vec::new();
        let mut push = |s: &str| a.push(s.into());
        push("--mode");
        push("json");
        // Never load the target repo's own .pi/ extensions (spec D20), nor the
        // user's global extensions, skills and prompt templates: another
        // extension's tool_call handler could rewrite a call after the guard
        // has checked it. Explicit -e and --skill paths still load.
        push("--no-approve");
        push("--no-extensions");
        push("--no-skills");
        push("--no-prompt-templates");
        // Disable host context discovery while retaining the explicit trusted stage file.
        push("--no-context-files");
        push("--system-prompt");
        push("");
        push("--append-system-prompt");
        push("");
        if let Some(p) = &req.provider {
            push("--provider");
            push(p);
        }
        push("--model");
        push(&req.model);
        match tools_arg(req.tools, req.output_schema.is_some()) {
            // Say "no tools" outright rather than pass an empty list.
            t if t.is_empty() => push("--no-tools"),
            t => {
                push("--tools");
                push(&t);
            }
        }
        a.push("--session-dir".into());
        a.push(req.session_dir.clone().into_os_string());
        a.push("-e".into());
        a.push(self.package.join("extensions/provefab.ts").into_os_string());
        a.push("--skill".into());
        a.push(self.package.join("skills").into_os_string());
        if let Some(f) = &req.system_prompt_file {
            a.push("--append-system-prompt".into());
            a.push(f.clone().into_os_string());
        }
        a.push(req.prompt.clone().into());
        a
    }
}

impl PiWorker {
    pub fn prepare_command(&self, req: &StageRequest) -> std::io::Result<Command> {
        let mut req = crate::sandbox::prepare_request(
            req,
            &[self.package.clone(), self.provefab_bin.clone()],
            &[],
        )?;
        let program = crate::sandbox::executable(&self.program, &req.cwd, &req.session_dir)?;
        req.sandbox.deny_write.push(program.clone());
        let runtime = crate::sandbox::executable(&req.sandbox.runtime, &req.cwd, &req.session_dir)?;
        crate::sandbox::check_version(&runtime, [0, 0, 79], &req)?;
        req.sandbox.deny_write.push(runtime.clone());
        let policy = crate::sandbox::write_policy(&req, false)?;
        let mut cmd = Command::new(program);
        cmd.args(self.args(&req)).current_dir(&req.cwd);
        crate::sanitize_environment(&mut cmd);
        apply_worker_env(
            &mut cmd,
            &req.cwd,
            &req.session_dir.join("git-hooks"),
            req.tools,
        );
        crate::sandbox::apply_temporary_env(&mut cmd, &req);
        cmd.env("PROVEFAB_BIN", &self.provefab_bin)
            .env(crate::SANDBOX_POLICY_ENV, policy)
            .env(crate::SANDBOX_RUNTIME_ENV, runtime);
        Ok(cmd)
    }
}

fn tools_arg(profile: ToolProfile, submit: bool) -> String {
    let base = match profile {
        // File operations must execute inside SRT, not an unsandboxed native factory.
        ToolProfile::ReadOnly | ToolProfile::UntrustedReadOnly | ToolProfile::Full => {
            "provefab_bash"
        }
        ToolProfile::NoTools => "",
    };
    if submit && base.is_empty() {
        SUBMIT_TOOL.to_string()
    } else if submit {
        format!("{base},{SUBMIT_TOOL}")
    } else {
        base.to_string()
    }
}

impl Worker for PiWorker {
    async fn run(
        &self,
        req: &StageRequest,
        events: UnboundedSender<WorkerEvent>,
    ) -> Result<StageResult, WorkerError> {
        let normalized = crate::sandbox::prepare_request(
            req,
            &[self.package.clone(), self.provefab_bin.clone()],
            &[],
        )
        .map_err(|e| WorkerError::Io(e.to_string()))?;
        let req = &normalized;
        let mut cmd = self
            .prepare_command(req)
            .map_err(|e| WorkerError::Io(e.to_string()))?;
        cmd.env_remove("PROVEFAB_OUTPUT_SCHEMA");
        if let Some(schema) = &req.output_schema {
            let path = req.session_dir.join("output-schema.json");
            std::fs::write(&path, schema.to_string())
                .map_err(|e| WorkerError::Io(e.to_string()))?;
            cmd.env("PROVEFAB_OUTPUT_SCHEMA", path);
        }
        prepare_git_hooks(&req.session_dir).map_err(|e| WorkerError::Io(e.to_string()))?;
        let mut transcript = Transcript::open(&req.session_dir)?;
        let mut state = PiState::new(req.max_turns);
        let finished = run_jsonl(cmd, req.timeout, |v| {
            transcript.write(&v);
            state.on_record(&v, &events)
        })
        .await?;
        let exit = if finished.timed_out {
            ExitReason::Timeout
        } else if finished.stopped {
            // An answer submitted on the last allowed turn still completes the stage.
            if state.output.is_some() {
                ExitReason::Completed
            } else {
                ExitReason::MaxTurns
            }
        } else if let Some(err) = state.error.clone() {
            if looks_rate_limited(&err) {
                ExitReason::RateLimited(err)
            } else {
                ExitReason::ProviderError(err)
            }
        } else if !state.settled || !finished.status.is_some_and(|s| s.success()) {
            // Exiting without `agent_settled` means Pi never finished the run.
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

struct PiState {
    max_turns: u32,
    turns: u32,
    output: Option<Value>,
    final_text: Option<String>,
    usage: Usage,
    error: Option<String>,
    settled: bool,
    last_attempt_failed: bool,
}

impl PiState {
    fn new(max_turns: u32) -> Self {
        Self {
            max_turns,
            turns: 0,
            output: None,
            final_text: None,
            usage: Usage::default(),
            error: None,
            settled: false,
            last_attempt_failed: false,
        }
    }

    fn on_record(&mut self, v: &Value, events: &UnboundedSender<WorkerEvent>) -> Flow {
        let emit = |e| {
            let _ = events.send(e);
        };
        let str_at = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "tool_execution_start" => emit(WorkerEvent::ToolStart {
                name: str_at("toolName").trim_start_matches("provefab_").into(),
                input: digest(v.get("args").unwrap_or(&Value::Null)),
            }),
            "tool_execution_end" => {
                let name = str_at("toolName")
                    .trim_start_matches("provefab_")
                    .to_string();
                let is_error = v.get("isError").and_then(Value::as_bool).unwrap_or(false);
                if name == SUBMIT_TOOL && !is_error {
                    self.output = v.pointer("/result/details").cloned();
                }
                emit(WorkerEvent::ToolEnd { name, is_error });
            }
            "message_end"
                if v.pointer("/message/role").and_then(Value::as_str) == Some("assistant") =>
            {
                let msg = &v["message"];
                let n = |p: &str| msg.pointer(p).and_then(Value::as_u64).unwrap_or(0);
                // Field names from pi-ai's `Usage` type (D74).
                self.usage.input_tokens += n("/usage/input");
                self.usage.output_tokens += n("/usage/output");
                self.usage.cache_read_tokens += n("/usage/cacheRead");
                self.usage.cache_write_tokens += n("/usage/cacheWrite");
                let text: Vec<&str> = msg
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|c| c.get("text").and_then(Value::as_str))
                    .collect();
                if !text.is_empty() {
                    let joined = text.join("\n");
                    emit(WorkerEvent::Text(joined.clone()));
                    self.final_text = Some(joined);
                }
                self.last_attempt_failed =
                    msg.get("stopReason").and_then(Value::as_str) == Some("error");
                if self.last_attempt_failed {
                    self.error = Some(
                        msg.get("errorMessage")
                            .and_then(Value::as_str)
                            .unwrap_or("provider error")
                            .to_string(),
                    );
                }
            }
            "turn_end" => {
                emit(WorkerEvent::TurnEnd);
                // A provider error that Pi retries is not a turn the agent used.
                if !self.last_attempt_failed {
                    self.turns += 1;
                    if self.turns >= self.max_turns {
                        return Flow::Stop;
                    }
                }
            }
            "auto_retry_start" => {
                // Pi is retrying: the previous error no longer decides the outcome.
                self.error = None;
                emit(WorkerEvent::Retry {
                    message: str_at("errorMessage"),
                });
            }
            "auto_retry_end" if v.get("success").and_then(Value::as_bool) == Some(true) => {
                self.error = None;
            }
            "agent_settled" => self.settled = true,
            "auto_retry_end" if v.get("success").and_then(Value::as_bool) == Some(false) => {
                let message = str_at("finalError");
                if looks_rate_limited(&message) {
                    emit(WorkerEvent::RateLimited {
                        message: message.clone(),
                    });
                }
                self.error = Some(message);
            }
            _ => {}
        }
        Flow::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn req(dir: &std::path::Path) -> StageRequest {
        StageRequest {
            sandbox: Default::default(),
            cwd: dir.to_path_buf(),
            prompt: "Fix the bug".into(),
            model: "gpt-5.5".into(),
            provider: Some("openai-codex".into()),
            tools: ToolProfile::ReadOnly,
            system_prompt_file: Some(dir.join("plan.md")),
            output_schema: Some(serde_json::json!({"type": "object"})),
            max_turns: 40,
            timeout: Duration::from_secs(60),
            session_dir: dir.join("session"),
        }
    }

    #[test]
    fn native_file_operations_use_only_sandboxed_bash() {
        for tools in [
            ToolProfile::Full,
            ToolProfile::ReadOnly,
            ToolProfile::UntrustedReadOnly,
        ] {
            assert_eq!(tools_arg(tools, false), "provefab_bash");
            assert_eq!(tools_arg(tools, true), "provefab_bash,submit_result");
        }
    }

    #[test]
    fn implicit_context_is_disabled_for_every_profile_with_explicit_prompt_kept() {
        let worker = PiWorker {
            program: "pi".into(),
            package: "/x/pi-provefab".into(),
            provefab_bin: "/x/provefab".into(),
        };
        for tools in [
            ToolProfile::Full,
            ToolProfile::ReadOnly,
            ToolProfile::UntrustedReadOnly,
            ToolProfile::NoTools,
        ] {
            let mut request = req(std::path::Path::new("/w"));
            request.tools = tools;
            for explicit in [None, Some(PathBuf::from("/trusted/stage.md"))] {
                request.system_prompt_file = explicit.clone();
                let args = worker.args(&request);
                assert!(args.iter().any(|v| v == "--no-context-files"));
                assert!(
                    args.windows(2)
                        .any(|v| v[0] == "--system-prompt" && v[1].is_empty())
                );
                let appended: Vec<_> = args
                    .windows(2)
                    .filter(|v| v[0] == "--append-system-prompt")
                    .map(|v| v[1].clone())
                    .collect();
                let mut expected = vec![OsString::new()];
                if let Some(explicit) = explicit {
                    expected.push(explicit.into_os_string());
                }
                assert_eq!(appended, expected);
                assert_eq!(args.last(), Some(&OsString::from("Fix the bug")));
            }
        }
    }

    #[test]
    fn args_select_json_mode_no_trust_and_submit_tool() {
        let w = PiWorker {
            program: "pi".into(),
            package: "/x/pi-provefab".into(),
            provefab_bin: "/x/provefab".into(),
        };
        let dir = std::path::Path::new("/w");
        let args: Vec<String> = w
            .args(&req(dir))
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        let joined = args.join(" ");
        assert!(
            joined.starts_with("--mode json --no-approve --no-extensions --no-skills --no-prompt-templates --no-context-files --system-prompt  --append-system-prompt  --provider openai-codex --model gpt-5.5"),
            "{joined}"
        );
        assert!(
            joined.contains("--tools provefab_bash,submit_result"),
            "{joined}"
        );
        assert!(
            joined.contains("-e /x/pi-provefab/extensions/provefab.ts"),
            "{joined}"
        );
        assert!(joined.contains("--skill /x/pi-provefab/skills"), "{joined}");
        // Final review I1: the user's global Pi extensions, skills and prompts must not load.
        assert!(
            joined.contains("--no-extensions --no-skills --no-prompt-templates"),
            "{joined}"
        );
        assert!(
            joined.contains("--append-system-prompt /w/plan.md"),
            "{joined}"
        );
        assert_eq!(args.last().unwrap(), "Fix the bug");
    }

    /// Repository rules pre-flight S2: only the answer tool, or none at all.
    #[test]
    fn no_tools_keeps_only_the_submit_tool() {
        let w = PiWorker {
            program: "pi".into(),
            package: "/x/pi-provefab".into(),
            provefab_bin: "/x/provefab".into(),
        };
        let mut r = req(std::path::Path::new("/w"));
        r.tools = ToolProfile::NoTools;
        let joined = |r: &StageRequest| {
            w.args(r)
                .into_iter()
                .map(|s| s.into_string().unwrap())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(
            joined(&r).contains("--tools submit_result "),
            "{}",
            joined(&r)
        );
        r.output_schema = None;
        assert!(joined(&r).contains(" --no-tools "), "{}", joined(&r));
        assert!(!joined(&r).contains("--tools"), "{}", joined(&r));
    }

    /// Final review I1 (pull request reviews): Pi reviews a person's pull
    /// request using sandboxed Bash; the guard restricts commands to read-only.
    #[test]
    fn an_untrusted_review_uses_only_sandboxed_bash() {
        assert_eq!(
            tools_arg(ToolProfile::UntrustedReadOnly, true),
            format!("provefab_bash,{SUBMIT_TOOL}")
        );
    }

    #[test]
    fn full_profile_without_schema_has_no_submit_tool() {
        assert_eq!(tools_arg(ToolProfile::Full, false), "provefab_bash");
    }

    fn feed(state: &mut PiState, records: &[Value]) -> Vec<WorkerEvent> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for r in records {
            state.on_record(r, &tx);
        }
        drop(tx);
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    #[test]
    fn parses_tools_text_usage_and_submitted_output() {
        use serde_json::json;
        let mut s = PiState::new(10);
        let events = feed(
            &mut s,
            &[
                json!({"type":"tool_execution_start","toolCallId":"1","toolName":"bash","args":{"command":"ls"}}),
                json!({"type":"tool_execution_end","toolCallId":"1","toolName":"bash","result":{},"isError":true}),
                json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"usage":{"input":100,"output":20,"cacheRead":300,"cacheWrite":40},"stopReason":"toolUse"}}),
                json!({"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input":1,"output":0,"cacheRead":5,"cacheWrite":0},"stopReason":"toolUse"}}),
                json!({"type":"tool_execution_end","toolCallId":"2","toolName":"submit_result","result":{"details":{"verdict":"approve"}},"isError":false}),
                json!({"type":"turn_end","message":{},"toolResults":[]}),
            ],
        );
        assert_eq!(
            events[0],
            WorkerEvent::ToolStart {
                name: "bash".into(),
                input: r#"{"command":"ls"}"#.into()
            }
        );
        assert_eq!(
            events[1],
            WorkerEvent::ToolEnd {
                name: "bash".into(),
                is_error: true
            }
        );
        assert_eq!(events[2], WorkerEvent::Text("done".into()));
        assert_eq!(s.output, Some(json!({"verdict":"approve"})));
        assert_eq!(
            s.usage,
            Usage {
                input_tokens: 101,
                output_tokens: 20,
                cache_read_tokens: 305,
                cache_write_tokens: 40,
            }
        );
        assert_eq!(s.turns, 1);
    }

    #[test]
    fn max_turns_stops_and_provider_errors_are_kept() {
        use serde_json::json;
        let mut s = PiState::new(1);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(matches!(
            s.on_record(&json!({"type":"turn_end"}), &tx),
            Flow::Stop
        ));
        let mut s = PiState::new(5);
        let events = feed(
            &mut s,
            &[
                json!({"type":"auto_retry_end","success":false,"attempt":3,"finalError":"429 usage limit reached"}),
            ],
        );
        assert_eq!(
            events,
            vec![WorkerEvent::RateLimited {
                message: "429 usage limit reached".into()
            }]
        );
        assert_eq!(s.error.as_deref(), Some("429 usage limit reached"));
    }

    /// Final review I2: an attempt that failed and was retried successfully is not an error,
    /// and the failed attempt does not use up a turn.
    #[test]
    fn review_i2_successful_retry_clears_the_error() {
        use serde_json::json;
        let mut s = PiState::new(5);
        feed(
            &mut s,
            &[
                json!({"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input":1,"output":0},"stopReason":"error","errorMessage":"529 overloaded"}}),
                json!({"type":"turn_end","message":{},"toolResults":[]}),
                json!({"type":"auto_retry_start","attempt":1,"maxAttempts":3,"delayMs":10,"errorMessage":"529 overloaded"}),
                json!({"type":"tool_execution_end","toolCallId":"2","toolName":"submit_result","result":{"details":{"ok":true}},"isError":false}),
                json!({"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input":5,"output":1},"stopReason":"toolUse"}}),
                json!({"type":"turn_end","message":{},"toolResults":[]}),
                json!({"type":"auto_retry_end","success":true,"attempt":2}),
                json!({"type":"agent_settled"}),
            ],
        );
        assert_eq!(s.error, None);
        assert_eq!(s.turns, 1, "the failed attempt counted as a turn");
        assert!(s.settled);
    }
}
