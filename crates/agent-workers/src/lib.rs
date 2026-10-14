//! Runs one agent stage in a subprocess and reports what it did.
//!
//! Knows nothing about Provefab: a `StageRequest` goes in, a stream of
//! normalised `WorkerEvent`s and a `StageResult` come out.

mod claude;
mod codex;
mod jsonl;
mod pi;
mod process;
mod sandbox;
mod types;
pub use sandbox::{
    CODEX_TOOL_BOUNDARY_OVERRIDES, SANDBOX_POLICY_ENV, SANDBOX_RUNTIME_ENV, SandboxPolicy,
    check_codex_host_settings, gate_command, resolve_trusted_executable, sanitize_environment,
    trusted_path, trusted_path_target,
};

pub use claude::ClaudeCodeWorker;
pub use codex::CodexWorker;
pub use jsonl::JsonlReader;
pub use pi::{PiWorker, SUBMIT_TOOL};
pub use process::{
    NO_PUSH_CONFIG, NO_TOOLS_ENV, SCRUBBED_ENV, UNTRUSTED_REVIEW_ENV, apply_worker_env,
    prepare_git_hooks,
};
pub use types::{
    ExitReason, StageRequest, StageResult, ToolProfile, Usage, Worker, WorkerError, WorkerEvent,
};
