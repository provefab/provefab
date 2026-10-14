use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::process::{Flow, Transcript, prepare_git_hooks, run_jsonl};
use crate::types::digest;
use crate::{ExitReason, StageRequest, StageResult, Usage, Worker, WorkerError, WorkerEvent};

/// Claude stages are temporarily unavailable: implicit host context has no verified boundary.
pub struct ClaudeCodeWorker {
    pub program: PathBuf,
    /// The installed cc-provefab plugin directory.
    pub plugin_dir: PathBuf,
    /// `CLAUDE_CONFIG_DIR`, logged in once with `claude auth login`.
    pub config_dir: PathBuf,
    /// The `provefab` binary the plugin's hook calls for `provefab guard`.
    pub provefab_bin: PathBuf,
}

impl ClaudeCodeWorker {
    pub fn prepare_command(&self, _req: &StageRequest) -> std::io::Result<Command> {
        // Final review I2: user memory/rules load on the host, before sandboxed tools.
        // Refuse every profile before filesystem preparation, version or auth probes.
        // Reopen only after exact-source controls and a supported-stage run are verified.
        Err(std::io::Error::other(
            "Claude stages are temporarily unavailable: the implicit host context boundary is not verified. Select Codex or Pi in the model catalog; do not delete user state or authentication to bypass this refusal",
        ))
    }
}

impl Worker for ClaudeCodeWorker {
    async fn run(
        &self,
        req: &StageRequest,
        events: UnboundedSender<WorkerEvent>,
    ) -> Result<StageResult, WorkerError> {
        let cmd = self
            .prepare_command(req)
            .map_err(|e| WorkerError::Io(e.to_string()))?;
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
    use serde_json::json;

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
