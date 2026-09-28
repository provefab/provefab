//! Runs both workers end to end against a fake agent binary that records its
//! argv and environment, then prints a canned JSONL stream.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_workers::{
    ClaudeCodeWorker, CodexWorker, ExitReason, PiWorker, StageRequest, ToolProfile, Worker,
    WorkerEvent,
};
use serde_json::json;

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    /// A fake agent that logs argv/env to `log.txt`, prints `stream`, sleeps `sleep` seconds, exits `code`.
    fn new(stream: &str, sleep: u32, code: i32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("out.jsonl"), stream).unwrap();
        let script = format!(
            "#!/bin/sh\n\
             {{ for a in \"$@\"; do echo \"ARG $a\"; done\n\
               echo \"ENV PROVEFAB_WORKTREE=$PROVEFAB_WORKTREE\"\n\
               echo \"ENV PROVEFAB_BIN=$PROVEFAB_BIN\"\n\
               echo \"ENV PROVEFAB_OUTPUT_SCHEMA=${{PROVEFAB_OUTPUT_SCHEMA:-}}\"\n\
               echo \"ENV CLAUDE_CONFIG_DIR=${{CLAUDE_CONFIG_DIR:-}}\"\n\
               echo \"ENV ANTHROPIC_API_KEY=${{ANTHROPIC_API_KEY:-}}\"\n\
               echo \"ENV GIT_CONFIG_COUNT=${{GIT_CONFIG_COUNT:-}}\"\n\
               echo \"PWD $(pwd -P)\"; }} > {log}\n\
             cat {out}\n\
             sleep {sleep}\n\
             exit {code}\n",
            log = dir.path().join("log.txt").display(),
            out = dir.path().join("out.jsonl").display(),
        );
        let bin = dir.path().join("agent");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake { dir }
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("agent")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("log.txt")).unwrap()
    }
}

fn request(worktree: &Path, schema: bool) -> StageRequest {
    StageRequest {
        cwd: worktree.to_path_buf(),
        prompt: "Review the change".into(),
        model: "m".into(),
        provider: Some("openai-codex".into()),
        tools: ToolProfile::ReadOnly,
        system_prompt_file: None,
        output_schema: schema
            .then(|| json!({"type": "object", "properties": {"verdict": {"type": "string"}}})),
        max_turns: 10,
        timeout: Duration::from_secs(10),
        session_dir: worktree.join(".session"),
    }
}

async fn run<W: Worker>(
    w: &W,
    req: &StageRequest,
) -> (agent_workers::StageResult, Vec<WorkerEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = w.run(req, tx).await.unwrap();
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    (result, events)
}

const PI_STREAM: &str = concat!(
    r#"{"type":"session","version":3,"id":"s","cwd":"/w"}"#,
    "\n",
    r#"{"type":"tool_execution_start","toolCallId":"1","toolName":"read","args":{"path":"a.rs"}}"#,
    "\n",
    r#"{"type":"tool_execution_end","toolCallId":"1","toolName":"read","result":{},"isError":false}"#,
    "\n",
    r#"{"type":"tool_execution_end","toolCallId":"2","toolName":"submit_result","result":{"details":{"verdict":"approve"}},"isError":false}"#,
    "\n",
    r#"{"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input":10,"output":2},"stopReason":"toolUse"}}"#,
    "\n",
    r#"{"type":"turn_end","message":{},"toolResults":[]}"#,
    "\n",
    r#"{"type":"agent_settled"}"#,
    "\n",
);

#[tokio::test]
async fn pi_worker_end_to_end() {
    let fake = Fake::new(PI_STREAM, 0, 0);
    let wt = tempfile::tempdir().unwrap();
    let w = PiWorker {
        program: fake.bin(),
        package: "/ext/index.ts".into(),
        provefab_bin: "/bin/provefab".into(),
    };
    let req = request(wt.path(), true);
    let (result, events) = run(&w, &req).await;

    assert_eq!(result.exit, ExitReason::Completed);
    assert_eq!(
        result.structured_output,
        Some(json!({"verdict": "approve"}))
    );
    assert_eq!(result.turns, 1);
    assert!(events.contains(&WorkerEvent::ToolStart {
        name: "read".into(),
        input: r#"{"path":"a.rs"}"#.into()
    }));

    let log = fake.log();
    let schema_path = req.session_dir.join("output-schema.json");
    assert!(log.contains("ARG --no-approve"), "{log}");
    assert!(
        log.contains(&format!(
            "ENV PROVEFAB_OUTPUT_SCHEMA={}",
            schema_path.display()
        )),
        "{log}"
    );
    assert!(
        log.contains(&format!("ENV PROVEFAB_WORKTREE={}", wt.path().display())),
        "{log}"
    );
    assert!(log.contains("ENV PROVEFAB_BIN=/bin/provefab"), "{log}");
    // Three entries appended after any the test itself inherited (Provefab's
    // gates run this suite with their own three).
    let inherited: usize = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    assert!(
        log.contains(&format!("ENV GIT_CONFIG_COUNT={}", inherited + 3)),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "PWD {}",
            wt.path().canonicalize().unwrap().display()
        )),
        "{log}"
    );
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(schema_path).unwrap()).unwrap();
    assert_eq!(schema["type"], "object");
    let transcript = std::fs::read_to_string(req.session_dir.join("events.jsonl")).unwrap();
    assert_eq!(transcript.lines().count(), 7);
    assert!(
        req.session_dir.join("git-hooks/pre-push").is_file(),
        "no push-blocking hook"
    );
}

#[tokio::test]
async fn pi_worker_reports_crash_timeout_and_missing_binary() {
    let wt = tempfile::tempdir().unwrap();
    let crash = Fake::new("", 0, 2);
    let w = PiWorker {
        program: crash.bin(),
        package: "/e".into(),
        provefab_bin: "/f".into(),
    };
    let (result, _) = run(&w, &request(wt.path(), false)).await;
    assert!(
        matches!(result.exit, ExitReason::Crashed { code: Some(2), .. }),
        "{:?}",
        result.exit
    );

    let hang = Fake::new("", 30, 0);
    let w = PiWorker {
        program: hang.bin(),
        package: "/e".into(),
        provefab_bin: "/f".into(),
    };
    let mut req = request(wt.path(), false);
    req.timeout = Duration::from_millis(500);
    let (result, _) = run(&w, &req).await;
    assert_eq!(result.exit, ExitReason::Timeout);

    let w = PiWorker {
        program: "/nonexistent/pi".into(),
        package: "/e".into(),
        provefab_bin: "/f".into(),
    };
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(w.run(&request(wt.path(), false), tx).await.is_err());
}

/// Final review I3: a Pi that exits 0 without ever settling did not complete the stage.
#[tokio::test]
async fn review_i3_silent_pi_exit_is_a_crash() {
    let wt = tempfile::tempdir().unwrap();
    let silent = Fake::new("", 0, 0);
    let w = PiWorker {
        program: silent.bin(),
        package: "/e".into(),
        provefab_bin: "/f".into(),
    };
    let (result, _) = run(&w, &request(wt.path(), true)).await;
    assert!(
        matches!(result.exit, ExitReason::Crashed { code: Some(0), .. }),
        "{:?}",
        result.exit
    );
}

/// Final review M1: an answer submitted on the last allowed turn completes the stage.
#[tokio::test]
async fn review_m1_output_on_the_last_turn_is_completed() {
    let stream = concat!(
        r#"{"type":"tool_execution_end","toolCallId":"2","toolName":"submit_result","result":{"details":{"verdict":"approve"}},"isError":false}"#,
        "\n",
        r#"{"type":"turn_end","message":{},"toolResults":[]}"#,
        "\n",
    );
    let fake = Fake::new(stream, 30, 0);
    let wt = tempfile::tempdir().unwrap();
    let w = PiWorker {
        program: fake.bin(),
        package: "/e".into(),
        provefab_bin: "/f".into(),
    };
    let mut req = request(wt.path(), true);
    req.max_turns = 1;
    let (result, _) = run(&w, &req).await;
    assert_eq!(result.exit, ExitReason::Completed);
    assert_eq!(
        result.structured_output,
        Some(json!({"verdict": "approve"}))
    );
}

const CLAUDE_STREAM: &str = concat!(
    r#"{"type":"system","subtype":"init","apiKeySource":"none","plugins":[]}"#,
    "\n",
    r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/w/a.rs"}}]}}"#,
    "\n",
    r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":false}]}}"#,
    "\n",
    r#"{"type":"result","subtype":"success","is_error":false,"num_turns":2,"result":"ok","structured_output":{"verdict":"changes"},"usage":{"input_tokens":5,"output_tokens":1}}"#,
    "\n",
);

#[tokio::test]
async fn claude_worker_end_to_end() {
    let fake = Fake::new(CLAUDE_STREAM, 0, 0);
    let wt = tempfile::tempdir().unwrap();
    let w = ClaudeCodeWorker {
        program: fake.bin(),
        plugin_dir: "/p/cc-provefab".into(),
        config_dir: "/cfg/claude".into(),
        provefab_bin: "/bin/provefab".into(),
    };
    let req = request(wt.path(), true);
    let (result, events) = run(&w, &req).await;
    assert_eq!(result.exit, ExitReason::Completed);
    assert_eq!(
        result.structured_output,
        Some(json!({"verdict": "changes"}))
    );
    assert_eq!(
        events[1],
        WorkerEvent::ToolEnd {
            name: "Read".into(),
            is_error: false
        }
    );
    let log = fake.log();
    assert!(log.contains("ENV CLAUDE_CONFIG_DIR=/cfg/claude"), "{log}");
    assert!(log.contains("ARG --setting-sources\nARG user"), "{log}");
    assert!(log.contains("ARG --json-schema"), "{log}");
    assert!(
        req.session_dir.join("git-hooks/pre-push").is_file(),
        "no push-blocking hook"
    );
}

#[tokio::test]
async fn claude_worker_without_result_record_is_a_crash() {
    let fake = Fake::new(r#"{"type":"system","subtype":"init"}"#, 0, 1);
    let wt = tempfile::tempdir().unwrap();
    let w = ClaudeCodeWorker {
        program: fake.bin(),
        plugin_dir: "/p".into(),
        config_dir: "/c".into(),
        provefab_bin: "/f".into(),
    };
    let (result, _) = run(&w, &request(wt.path(), false)).await;
    assert!(
        matches!(result.exit, ExitReason::Crashed { code: Some(1), .. }),
        "{:?}",
        result.exit
    );
}

const CODEX_STREAM: &str = concat!(
    r#"{"type":"thread.started","thread_id":"t"}"#,
    "\n",
    r#"{"type":"turn.started"}"#,
    "\n",
    r#"{"type":"item.completed","item":{"id":"1","type":"agent_message","text":"{\"verdict\":\"approve\"}"}}"#,
    "\n",
    r#"{"type":"turn.completed","usage":{"input_tokens":9,"output_tokens":2}}"#,
    "\n",
);

fn codex(fake: &Fake) -> CodexWorker {
    CodexWorker {
        program: fake.bin(),
        codex_home: "/cfg/codex".into(),
        provefab_bin: "/bin/provefab".into(),
    }
}

#[tokio::test]
async fn codex_worker_end_to_end() {
    let fake = Fake::new(CODEX_STREAM, 0, 0);
    let wt = tempfile::tempdir().unwrap();
    let mut req = request(wt.path(), true);
    let prompt_file = wt.path().join("stage.md");
    std::fs::write(&prompt_file, "You are the reviewer.").unwrap();
    req.system_prompt_file = Some(prompt_file);
    let (result, _) = run(&codex(&fake), &req).await;
    assert_eq!(result.exit, ExitReason::Completed);
    assert_eq!(
        result.structured_output,
        Some(json!({"verdict": "approve"}))
    );
    let log = fake.log();
    assert!(log.contains("ARG exec\nARG --json"), "{log}");
    assert!(log.contains("ARG --output-schema"), "{log}");
    assert!(
        log.contains("ARG You are the reviewer.\n\nReview the change"),
        "{log}"
    );
    assert!(req.session_dir.join("output-schema.json").is_file());
    assert!(req.session_dir.join("git-hooks/pre-push").is_file());
}

#[tokio::test]
async fn codex_worker_without_a_completed_turn_is_a_crash() {
    let fake = Fake::new(r#"{"type":"thread.started","thread_id":"t"}"#, 0, 0);
    let wt = tempfile::tempdir().unwrap();
    let (result, _) = run(&codex(&fake), &request(wt.path(), false)).await;
    assert!(
        matches!(result.exit, ExitReason::Crashed { code: Some(0), .. }),
        "{:?}",
        result.exit
    );
}

/// Final review #5: Codex reports retried stream errors as `error` events; a turn that
/// then completes is a completed stage, not a provider error or a rate limit.
#[tokio::test]
async fn review_5_codex_retried_error_then_completed_turn_is_completed() {
    let stream = concat!(
        r#"{"type":"turn.started"}"#,
        "\n",
        r#"{"type":"error","message":"Reconnecting... 1/5 (stream error: 429 Too Many Requests)"}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"1","type":"agent_message","text":"done"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":3,"output_tokens":1}}"#,
        "\n",
    );
    let fake = Fake::new(stream, 0, 0);
    let wt = tempfile::tempdir().unwrap();
    let (result, _) = run(&codex(&fake), &request(wt.path(), false)).await;
    assert_eq!(result.exit, ExitReason::Completed);
}
