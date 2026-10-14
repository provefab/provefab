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

// HOME isolation belongs to the test subprocess, never global mutation in parallel tests.
fn with_clean_home(test: &str) -> bool {
    if std::env::var_os("PROVEFAB_CLEAN_TEST_HOME").is_some() {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("HOME", home.path())
        .env("PROVEFAB_CLEAN_TEST_HOME", "1")
        .status()
        .unwrap();
    assert!(status.success());
    true
}

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    /// A fake agent that logs argv/env to `log.txt`, prints `stream`, sleeps `sleep` seconds, exits `code`.
    fn new(stream: &str, sleep: u32, code: i32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("out.jsonl"), stream).unwrap();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 2.1.296; exit 0; fi\n\
             if [ \"$1\" = --setting-sources ]; then echo '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"apiProvider\":\"firstParty\",\"subscriptionType\":\"max\"}}'; exit 0; fi\n\
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
    std::fs::create_dir_all(worktree.join("work")).unwrap();
    let runtime = worktree.join("fake-srt");
    std::fs::write(&runtime, "#!/bin/sh\necho 0.0.79\n").unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
    StageRequest {
        sandbox: agent_workers::SandboxPolicy {
            runtime,
            ..Default::default()
        },
        cwd: worktree.join("work"),
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

// Final review I1: final canonicalization must not hide a writable intermediate alias.
#[test]
fn trusted_adapter_aliases_refuse_both_writable_roots_before_version_probe() {
    for temporary in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path(), false);
        let writable = if temporary {
            req.session_dir.join("sandbox-tmp")
        } else {
            req.cwd.clone()
        };
        std::fs::create_dir_all(&writable).unwrap();
        let safe = root.path().join("safe");
        std::fs::create_dir(&safe).unwrap();
        let marker = root.path().join("probe-marker");
        let runtime = safe.join("runtime");
        std::fs::write(
            &runtime,
            format!("#!/bin/sh\ntouch '{}'\necho 0.0.79\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&safe, writable.join("bridge")).unwrap();
        let alias = root.path().join("external-alias");
        std::os::unix::fs::symlink(writable.join("bridge"), &alias).unwrap();
        let mut req = req;
        req.sandbox.runtime = runtime.clone();
        let worker = PiWorker {
            program: runtime.clone(),
            package: alias,
            provefab_bin: runtime,
        };
        let error = worker
            .prepare_command(&req)
            .expect_err("mutable adapter alias accepted");
        assert!(error.to_string().contains("trusted path"), "{error}");
        assert!(!marker.exists(), "version probe ran before alias refusal");
    }
}

// Final review I2: even a clean profile and NoTools must stop before any host CLI probe.
#[tokio::test]
async fn claude_all_profiles_refuse_before_version_auth_or_stage_invocation() {
    if with_clean_home("claude_all_profiles_refuse_before_version_auth_or_stage_invocation") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut req = request(root.path(), true);
    let bin = root.path().join("claude");
    let marker = root.path().join("host-invocations");
    std::fs::write(&bin, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = --version ]; then echo 2.1.296; else echo '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"apiProvider\":\"firstParty\",\"subscriptionType\":\"max\"}}'; fi\n", marker.display())).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let profile = root.path().join("clean-profile");
    std::fs::create_dir(&profile).unwrap();
    std::fs::write(profile.join("settings.json"), "{}").unwrap();
    let worker = ClaudeCodeWorker {
        program: bin.clone(),
        config_dir: profile.clone(),
        plugin_dir: root.path().join("plugin"),
        provefab_bin: bin,
    };
    for tools in [
        ToolProfile::Full,
        ToolProfile::ReadOnly,
        ToolProfile::UntrustedReadOnly,
        ToolProfile::NoTools,
    ] {
        req.tools = tools;
        let error = worker
            .prepare_command(&req)
            .expect_err("Claude implicit context boundary accepted");
        let message = error.to_string();
        assert!(message.contains("implicit host context"), "{message}");
        assert!(message.contains("Codex or Pi"), "{message}");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(worker.run(&req, tx).await.is_err());
        assert!(rx.try_recv().is_err());
        assert!(!marker.exists(), "Claude version/auth/stage invoked");
        assert!(!req.session_dir.exists(), "stage preparation mutated state");
    }
    assert_eq!(
        std::fs::read_to_string(profile.join("settings.json")).unwrap(),
        "{}"
    );
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
    let mut req = request(wt.path(), true);
    req.sandbox.runtime = fake.bin();
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
            schema_path.canonicalize().unwrap().display()
        )),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "ENV PROVEFAB_WORKTREE={}",
            req.cwd.canonicalize().unwrap().display()
        )),
        "{log}"
    );
    assert!(log.contains("ENV PROVEFAB_BIN=/bin/provefab"), "{log}");
    assert!(log.contains("ENV GIT_CONFIG_COUNT=3"), "{log}");
    assert!(
        log.contains(&format!(
            "PWD {}",
            req.cwd.canonicalize().unwrap().display()
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
    req.sandbox.runtime = hang.bin();
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
    let script = std::fs::read_to_string(fake.bin())
        .unwrap()
        .replace("2.1.296", "0.162.1");
    std::fs::write(fake.bin(), script).unwrap();
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

#[tokio::test]
async fn native_preparation_rejects_session_inside_worktree_before_launch() {
    let fake = Fake::new(CODEX_STREAM, 0, 0);
    let wt = tempfile::tempdir().unwrap();
    let mut r = request(wt.path(), false);
    r.session_dir = r.cwd.join("session");
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    assert!(codex(&fake).run(&r, tx).await.is_err());
    assert!(!fake.dir.path().join("log.txt").exists());
}

#[tokio::test]
async fn native_preparation_rejects_claude_user_sandbox_override_before_launch() {
    if with_clean_home("native_preparation_rejects_claude_user_sandbox_override_before_launch") {
        return;
    }
    let fake = Fake::new("", 0, 0);
    let wt = tempfile::tempdir().unwrap();
    let session = tempfile::tempdir().unwrap();
    std::fs::write(
        fake.dir.path().join("settings.json"),
        r#"{"sandbox":{"excludedCommands":["python"]}}"#,
    )
    .unwrap();
    let w = ClaudeCodeWorker {
        program: fake.bin(),
        plugin_dir: fake.dir.path().into(),
        config_dir: fake.dir.path().into(),
        provefab_bin: fake.bin(),
    };
    let mut r = request(wt.path(), false);
    r.session_dir = session.path().into();
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    assert!(w.run(&r, tx).await.is_err());
    assert!(!fake.dir.path().join("log.txt").exists());
}

#[tokio::test]
async fn prepared_native_workers_pin_the_executable_before_changing_cwd() {
    if with_clean_home("prepared_native_workers_pin_the_executable_before_changing_cwd") {
        return;
    }
    const ROOT: &str = "PROVEFAB_TEST_EXECUTABLE_ROOT";
    if let Some(root) = std::env::var_os(ROOT) {
        let root = PathBuf::from(root);
        let req = request(&root, false);
        let codex = CodexWorker {
            program: "codex-agent".into(),
            codex_home: root.join("auth"),
            provefab_bin: root.join("guard"),
        };
        for mut command in [codex.prepare_command(&req, "test").unwrap()] {
            assert_eq!(
                command.as_std().get_program(),
                root.join("trusted").canonicalize().unwrap().join(
                    std::path::Path::new(command.as_std().get_program())
                        .file_name()
                        .unwrap()
                )
            );
            let out = command.output().await.unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "trusted");
        }
        return;
    }
    let root = tempfile::tempdir().unwrap();
    for (directory, result) in [("trusted", "trusted"), ("work/trusted", "untrusted")] {
        let path = root.path().join(directory);
        std::fs::create_dir_all(&path).unwrap();
        let bin = path.join("agent");
        std::fs::write(&bin, format!("#!/usr/bin/env sh\nif [ \"$1\" = --version ]; then echo 2.1.296; elif [ \"$1\" = --setting-sources ]; then echo '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"apiProvider\":\"firstParty\",\"subscriptionType\":\"max\"}}'; else echo {result}; fi\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let codex_bin = path.join("codex-agent");
        std::fs::write(
            &codex_bin,
            std::fs::read_to_string(&bin)
                .unwrap()
                .replace("2.1.296", "0.162.1"),
        )
        .unwrap();
        std::fs::set_permissions(&codex_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let evil = root.path().join("work/trusted/sh");
    std::fs::write(
        &evil,
        format!(
            "#!/bin/sh\ntouch '{}'\nexit 77\n",
            root.path().join("interpreter-marker").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&evil, std::fs::Permissions::from_mode(0o755)).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "prepared_native_workers_pin_the_executable_before_changing_cwd",
            "--nocapture",
        ])
        .current_dir(root.path())
        .env(ROOT, root.path())
        .env(
            "PATH",
            format!(
                "./trusted:./work/trusted::{}",
                std::env::var("PATH").unwrap()
            ),
        )
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!root.path().join("interpreter-marker").exists());
}

#[tokio::test]
async fn claude_host_state_refuses_before_agent_start() {
    if with_clean_home("claude_host_state_refuses_before_agent_start") {
        return;
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    std::fs::create_dir_all(home.join(".claude/state")).unwrap();
    let fake = Fake::new("", 0, 0);
    let root = tempfile::tempdir().unwrap();
    let worker = ClaudeCodeWorker {
        program: fake.bin(),
        config_dir: fake.dir.path().join("auth"),
        plugin_dir: fake.dir.path().join("plugin"),
        provefab_bin: fake.bin(),
    };
    let error = worker
        .prepare_command(&request(root.path(), false))
        .unwrap_err();
    assert!(
        error.to_string().contains("implicit host context"),
        "{error}"
    );
    assert!(!fake.dir.path().join("log.txt").exists());
}

#[tokio::test]
async fn env_shebang_ignores_external_path_aliases_to_writable_interpreters() {
    const ROOT: &str = "PROVEFAB_TEST_INTERPRETER_ROOT";
    const MODE: &str = "PROVEFAB_TEST_INTERPRETER_MODE";
    if let Some(root) = std::env::var_os(ROOT) {
        let root = PathBuf::from(root);
        let req = request(&root, false);
        let worker = CodexWorker {
            program: root.join("runtime"),
            codex_home: root.join("auth"),
            provefab_bin: root.join("guard"),
        };
        let prepared = worker.prepare_command(&req, "test");
        assert!(
            !root.join("marker").exists(),
            "version probe executed a writable interpreter"
        );
        let mut command = prepared.unwrap();
        if std::env::var(MODE).unwrap() == "dangling"
            || std::env::var(MODE).unwrap() == "mutable_alias"
        {
            if std::env::var(MODE).unwrap() == "mutable_alias" {
                std::fs::remove_file(root.join("work/sh")).unwrap();
            }
            std::fs::write(
                root.join("work/sh"),
                format!(
                    "#!/bin/sh\ntouch '{}'\nexit 77\n",
                    root.join("marker").display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(root.join("work/sh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        if matches!(
            std::env::var(MODE).unwrap().as_str(),
            "ancestor_alias" | "directory_alias"
        ) {
            std::fs::remove_file(root.join("work/alias")).unwrap();
            std::fs::create_dir(root.join("work/alias")).unwrap();
            std::fs::write(
                root.join("work/alias/sh"),
                format!(
                    "#!/bin/sh\ntouch '{}'\nexit 77\n",
                    root.join("marker").display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(
                root.join("work/alias/sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let output = command.output().await.unwrap();
        assert!(
            !root.join("marker").exists(),
            "stage executed a writable interpreter"
        );
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "trusted");
        return;
    }
    for mode in [
        "worktree",
        "private_tmp",
        "dangling",
        "mutable_alias",
        "safe",
        "ancestor_alias",
        "directory_alias",
    ] {
        let root = tempfile::tempdir().unwrap();
        for name in ["bin", "work", ".session/sandbox-tmp"] {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
        }
        let target = match mode {
            "ancestor_alias" | "directory_alias" => root.path().join("outside/redirect/sh"),
            "private_tmp" => root.path().join(".session/sandbox-tmp/sh"),
            "safe" => PathBuf::from("/bin/sh"),
            _ => root.path().join("work/sh"),
        };
        if matches!(mode, "worktree" | "private_tmp") {
            std::fs::write(
                &target,
                format!(
                    "#!/bin/sh\ntouch '{}'\nexit 77\n",
                    root.path().join("marker").display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        if mode == "mutable_alias" {
            std::os::unix::fs::symlink("/bin/sh", &target).unwrap();
        }
        if matches!(mode, "ancestor_alias" | "directory_alias") {
            for name in ["outside", "safe-dir"] {
                std::fs::create_dir(root.path().join(name)).unwrap();
            }
            let safe = root.path().join("safe-dir/sh");
            std::fs::write(&safe, "#!/bin/sh\nexec /bin/sh \"$@\"\n").unwrap();
            std::fs::set_permissions(&safe, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::os::unix::fs::symlink(
                root.path().join("work/alias"),
                root.path().join("outside/redirect"),
            )
            .unwrap();
            std::os::unix::fs::symlink(
                root.path().join("safe-dir"),
                root.path().join("work/alias"),
            )
            .unwrap();
        }
        std::os::unix::fs::symlink(target, root.path().join("bin/sh")).unwrap();
        let runtime = root.path().join("runtime");
        std::fs::write(&runtime,"#!/usr/bin/env sh\nif [ \"$1\" = --version ]; then echo 0.162.1; else echo trusted; fi\n").unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "env_shebang_ignores_external_path_aliases_to_writable_interpreters",
                "--nocapture",
            ])
            .env(ROOT, root.path())
            .env(MODE, mode)
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin",
                    root.path()
                        .join(if mode == "directory_alias" {
                            "outside/redirect"
                        } else {
                            "bin"
                        })
                        .display()
                ),
            )
            .status()
            .unwrap();
        assert!(status.success(), "interpreter alias mode {mode}");
    }
}
