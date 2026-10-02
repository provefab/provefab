use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

/// One stage for one worker: everything needed to start the subprocess.
#[derive(Debug, Clone, PartialEq)]
pub struct StageRequest {
    /// The task's worktree. The agent runs here and the guard confines writes to it.
    pub cwd: PathBuf,
    pub prompt: String,
    /// Worker-specific model name (`opus`, `gpt-5.5`, ...).
    pub model: String,
    /// Pi only: `--provider`.
    pub provider: Option<String>,
    pub tools: ToolProfile,
    pub system_prompt_file: Option<PathBuf>,
    /// JSON Schema the stage's final answer must match. `None` for stages without one.
    pub output_schema: Option<Value>,
    pub max_turns: u32,
    pub timeout: Duration,
    /// Where the worker keeps its transcript for `provefab log`.
    pub session_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProfile {
    /// Read and search only (plan, review).
    ReadOnly,
    /// `ReadOnly` for a review of a person's pull request: the guard is told
    /// (`PROVEFAB_UNTRUSTED_REVIEW`) to let the shell run read-only commands
    /// only, so nothing the pull request wrote runs during its review.
    UntrustedReadOnly,
    /// Read, search, edit, write and shell (implement).
    Full,
    /// No tool: the model answers from its prompt alone (periodic work). The
    /// guard refuses every call but the structured answer's own.
    NoTools,
}

/// What the agent did, normalised across workers. Consumed by the loop detector,
/// cooldowns and `provefab log`.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerEvent {
    ToolStart { name: String, input: String },
    ToolEnd { name: String, is_error: bool },
    TurnEnd,
    Text(String),
    Retry { message: String },
    RateLimited { message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExitReason {
    Completed,
    MaxTurns,
    Timeout,
    RateLimited(String),
    ProviderError(String),
    Crashed {
        code: Option<i32>,
        stderr_tail: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Uncached input only; cache reads and writes are counted apart (D74).
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageResult {
    pub exit: ExitReason,
    /// Present only when the request had an `output_schema` and the agent submitted a valid answer.
    pub structured_output: Option<Value>,
    pub final_text: Option<String>,
    pub usage: Usage,
    pub turns: u32,
    /// The model the CLI reports it ran, when it says (an alias resolved).
    pub actual_model: Option<String>,
}

/// The worker could not be started or its output could not be read.
/// Everything that happens after a successful start is an `ExitReason`.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker: could not start `{program}`: {message}")]
    Spawn { program: String, message: String },
    #[error("worker: {0}")]
    Io(String),
}

pub trait Worker {
    fn run(
        &self,
        req: &StageRequest,
        events: UnboundedSender<WorkerEvent>,
    ) -> impl Future<Output = Result<StageResult, WorkerError>> + Send;
}

/// Tool input as a short string for events and logs.
pub(crate) fn digest(input: &Value) -> String {
    const MAX: usize = 400;
    let s = input.to_string();
    if s.len() <= MAX {
        return s;
    }
    let mut end = MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Provider errors that mean "this account is out of quota for now".
pub(crate) fn looks_rate_limited(message: &str) -> bool {
    let m = message.to_lowercase();
    [
        "429",
        "rate limit",
        "rate_limit",
        "usage limit",
        "quota",
        "too many requests",
    ]
    .iter()
    .any(|k| m.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn digest_truncates_on_a_char_boundary() {
        let long = json!({"command": "é".repeat(400)});
        let d = digest(&long);
        assert!(d.ends_with('…'));
        assert!(d.len() <= 404);
        assert_eq!(digest(&json!({"a": 1})), r#"{"a":1}"#);
    }

    #[test]
    fn rate_limit_detection() {
        assert!(looks_rate_limited("429 Too Many Requests"));
        assert!(looks_rate_limited("You have hit your usage limit"));
        assert!(!looks_rate_limited("529 overloaded"));
    }
}
