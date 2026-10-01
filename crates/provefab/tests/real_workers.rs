//! Real agent runs (spec §8, "plugins" row). They use the user's own logins and
//! a few cents of quota, so they are ignored by default:
//!
//!     cargo nextest run -p provefab --test real_workers --run-ignored only
//!
//! Claude runs need `claude` logged in under `~/.provefab/claude`
//! (`CLAUDE_CONFIG_DIR=~/.provefab/claude claude auth login`).
//!
//! Codex runs need `codex` logged in under `~/.provefab/codex`
//! (`CODEX_HOME=~/.provefab/codex codex login`); they trust Provefab guard
//! hook there through the app-server. `PROVEFAB_TEST_CODEX_MODEL` overrides the model.
//!
//! Pi runs are opt-in: set `PROVEFAB_TEST_PI_PROVIDER` and `PROVEFAB_TEST_PI_MODEL`
//! (for example an API-key provider). Without them they skip and say so. Using a
//! ChatGPT sign-in through Pi is an open question (spec R2), so no provider is
//! assumed here.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agent_workers::{
    ClaudeCodeWorker, CodexWorker, PiWorker, StageRequest, StageResult, ToolProfile, Worker,
    WorkerEvent,
};
use provefab::codex_setup;
use provefab::plugins;
use provefab::stage::{ReviewOutput, ReviewVerdict, output_schema};

const CLAUDE_MODEL: &str = "haiku";

/// `(provider, model)` for the Pi runs, or `None` (and a note on stderr) to skip them.
fn pi_route() -> Option<(String, String)> {
    match (
        std::env::var("PROVEFAB_TEST_PI_PROVIDER"),
        std::env::var("PROVEFAB_TEST_PI_MODEL"),
    ) {
        (Ok(p), Ok(m)) if !p.is_empty() && !m.is_empty() => Some((p, m)),
        _ => {
            eprintln!(
                "skipped: set PROVEFAB_TEST_PI_PROVIDER and PROVEFAB_TEST_PI_MODEL to run Pi (spec R2)"
            );
            None
        }
    }
}

/// A worktree with one commit and a local bare `origin`.
fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str], cwd: &Path| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "--bare", "remote.git"], dir.path());
    let work = dir.path().join("work");
    git(&["init", "-q", "work"], dir.path());
    std::fs::write(work.join("README.md"), "# demo\n").unwrap();
    git(&["add", "README.md"], &work);
    git(
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
        &work,
    );
    git(&["remote", "add", "origin", "../remote.git"], &work);
    (dir, work)
}

fn request(
    work: &Path,
    prompt: &str,
    model: &str,
    provider: Option<&str>,
    tools: ToolProfile,
) -> StageRequest {
    StageRequest {
        cwd: work.to_path_buf(),
        prompt: prompt.into(),
        model: model.into(),
        provider: provider.map(str::to_string),
        tools,
        system_prompt_file: None,
        output_schema: None,
        max_turns: 6,
        timeout: Duration::from_secs(240),
        session_dir: work.parent().unwrap().join("session"),
    }
}

async fn run<W: Worker>(w: &W, req: &StageRequest) -> (StageResult, Vec<WorkerEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = w.run(req, tx).await.expect("worker started");
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    (result, events)
}

fn installed() -> (tempfile::TempDir, plugins::InstalledPlugins) {
    let base = tempfile::tempdir().unwrap();
    let p = plugins::install(base.path()).unwrap();
    (base, p)
}

fn pi_worker(p: &plugins::InstalledPlugins) -> PiWorker {
    PiWorker {
        program: "pi".into(),
        package: p.pi_package.clone(),
        provefab_bin: env!("CARGO_BIN_EXE_provefab").into(),
    }
}

fn claude_worker(p: &plugins::InstalledPlugins) -> ClaudeCodeWorker {
    let home = std::env::var("HOME").unwrap();
    ClaudeCodeWorker {
        program: "claude".into(),
        plugin_dir: p.cc_plugin.clone(),
        config_dir: Path::new(&home).join(".provefab/claude"),
        provefab_bin: env!("CARGO_BIN_EXE_provefab").into(),
    }
}

/// `git tag` is reserved for Provefab by the guard, and nothing else in the
/// worker environment would stop it, so only the guard can make this test pass.
/// (Models sometimes refuse to even try `git push`; they do not hesitate over a local tag.)
const TAG_PROMPT: &str = "This is an automated test of a local sandbox repository. Use your shell tool to run exactly \
     this command once: git tag provefab-smoke . Do not try any other command. Then reply with one sentence saying what happened.";

const REVIEW_PROMPT: &str = "This is an automated test of structured output. Do not read any files. \
     Return the verdict \"approve\" with an empty findings list.";

fn assert_tag_refused(
    dir: &Path,
    work: &Path,
    result: &StageResult,
    events: &[WorkerEvent],
    shell_tool: &str,
) {
    let tried = events.iter().any(|e| matches!(e, WorkerEvent::ToolStart { name, input } if name == shell_tool && input.contains("git tag")));
    assert!(
        tried,
        "agent never attempted the command: {result:?} {events:#?}"
    );
    let refused = events
        .iter()
        .any(|e| matches!(e, WorkerEvent::ToolEnd { name, is_error: true } if name == shell_tool));
    assert!(refused, "the command was not refused: {events:#?}");
    let tags = Command::new("git")
        .args(["tag", "-l"])
        .current_dir(work)
        .output()
        .unwrap();
    assert!(tags.stdout.is_empty(), "the tag was created");
    let transcript = std::fs::read_to_string(dir.join("session/events.jsonl")).unwrap();
    assert!(
        transcript.contains("`git tag` is reserved for Provefab"),
        "the guard did not refuse it: {transcript}"
    );
}

/// Final review I4: a command the guard allows must actually run, not only denials work.
/// The path is absolute because Codex shell calls are checked with an unknown working
/// directory, so a relative write target would be refused (Plan 2b review #2, spec D30).
fn allowed_prompt(work: &Path) -> String {
    format!(
        "This is an automated test of a local sandbox repository. Use your shell tool to run exactly \
         this command once: touch {}/provefab-allowed.txt . Do not try any other command. \
         Then reply with one sentence saying what happened.",
        work.display()
    )
}

fn assert_allowed_ran(work: &Path, result: &StageResult, events: &[WorkerEvent]) {
    assert!(
        work.join("provefab-allowed.txt").exists(),
        "the allowed command did not run: {result:?} {events:#?}"
    );
}

#[tokio::test]
#[ignore = "real Pi run: opt-in through PROVEFAB_TEST_PI_PROVIDER / PROVEFAB_TEST_PI_MODEL"]
async fn real_pi_guard_refuses_reserved_git() {
    let Some((provider, model)) = pi_route() else {
        return;
    };
    let (_base, p) = installed();
    let (dir, work) = repo();
    let req = request(
        &work,
        TAG_PROMPT,
        &model,
        Some(&provider),
        ToolProfile::Full,
    );
    let (result, events) = run(&pi_worker(&p), &req).await;
    assert_tag_refused(dir.path(), &work, &result, &events, "bash");
}

#[tokio::test]
#[ignore = "real Pi run: opt-in through PROVEFAB_TEST_PI_PROVIDER / PROVEFAB_TEST_PI_MODEL"]
async fn real_pi_guard_allows_workspace_command() {
    let Some((provider, model)) = pi_route() else {
        return;
    };
    let (_base, p) = installed();
    let (_dir, work) = repo();
    let req = request(
        &work,
        &allowed_prompt(&work),
        &model,
        Some(&provider),
        ToolProfile::Full,
    );
    let (result, events) = run(&pi_worker(&p), &req).await;
    assert_allowed_ran(&work, &result, &events);
}

#[tokio::test]
#[ignore = "real Claude Code run: uses the ~/.provefab/claude subscription login"]
async fn real_claude_guard_allows_workspace_command() {
    let (_base, p) = installed();
    let (_dir, work) = repo();
    let req = request(
        &work,
        &allowed_prompt(&work),
        CLAUDE_MODEL,
        None,
        ToolProfile::Full,
    );
    let (result, events) = run(&claude_worker(&p), &req).await;
    assert_allowed_ran(&work, &result, &events);
}

#[tokio::test]
#[ignore = "real Pi run: opt-in through PROVEFAB_TEST_PI_PROVIDER / PROVEFAB_TEST_PI_MODEL"]
async fn real_pi_submit_result_returns_a_typed_review() {
    let Some((provider, model)) = pi_route() else {
        return;
    };
    let (_base, p) = installed();
    let (_dir, work) = repo();
    let mut req = request(
        &work,
        REVIEW_PROMPT,
        &model,
        Some(&provider),
        ToolProfile::ReadOnly,
    );
    req.output_schema = Some(output_schema::<ReviewOutput>());
    let (result, events) = run(&pi_worker(&p), &req).await;
    let out = result
        .structured_output
        .clone()
        .unwrap_or_else(|| panic!("no structured output: {result:?} {events:#?}"));
    let review: ReviewOutput = serde_json::from_value(out).unwrap();
    assert_eq!(review.verdict, ReviewVerdict::Approve);
}

#[tokio::test]
#[ignore = "real Claude Code run: uses the ~/.provefab/claude subscription login"]
async fn real_claude_guard_refuses_reserved_git() {
    let (_base, p) = installed();
    let (dir, work) = repo();
    let req = request(&work, TAG_PROMPT, CLAUDE_MODEL, None, ToolProfile::Full);
    let (result, events) = run(&claude_worker(&p), &req).await;
    assert_tag_refused(dir.path(), &work, &result, &events, "Bash");
}

#[tokio::test]
#[ignore = "real Claude Code run: uses the ~/.provefab/claude subscription login"]
async fn real_claude_json_schema_returns_a_typed_review() {
    let (_base, p) = installed();
    let (_dir, work) = repo();
    let mut req = request(
        &work,
        REVIEW_PROMPT,
        CLAUDE_MODEL,
        None,
        ToolProfile::ReadOnly,
    );
    req.output_schema = Some(output_schema::<ReviewOutput>());
    let (result, events) = run(&claude_worker(&p), &req).await;
    let out = result
        .structured_output
        .clone()
        .unwrap_or_else(|| panic!("no structured output: {result:?} {events:#?}"));
    let review: ReviewOutput = serde_json::from_value(out).unwrap();
    assert_eq!(review.verdict, ReviewVerdict::Approve);
}

fn codex_home() -> PathBuf {
    Path::new(&std::env::var("HOME").unwrap()).join(".provefab/codex")
}

fn codex_model() -> String {
    std::env::var("PROVEFAB_TEST_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.5".into())
}

/// Installs and trusts the guard hook, then runs the per-stage check.
async fn codex_ready(work: &Path) -> CodexWorker {
    codex_setup::setup(Path::new("codex"), &codex_home(), work)
        .await
        .expect("codex setup");
    codex_setup::pin_untrusted(Path::new("codex"), &codex_home(), work)
        .await
        .expect("codex pin untrusted");
    codex_setup::check(Path::new("codex"), &codex_home(), work)
        .await
        .expect("codex check");
    CodexWorker {
        program: "codex".into(),
        codex_home: codex_home(),
        provefab_bin: env!("CARGO_BIN_EXE_provefab").into(),
    }
}

#[tokio::test]
#[ignore = "real Codex run: uses the ~/.provefab/codex ChatGPT login"]
async fn real_codex_guard_refuses_reserved_git() {
    let (dir, work) = repo();
    let w = codex_ready(&work).await;
    let req = request(&work, TAG_PROMPT, &codex_model(), None, ToolProfile::Full);
    let (result, events) = run(&w, &req).await;
    // A refused call never becomes a command item, so check the outcome and the guard's reason.
    let tags = Command::new("git")
        .args(["tag", "-l"])
        .current_dir(&work)
        .output()
        .unwrap();
    assert!(tags.stdout.is_empty(), "the tag was created: {result:?}");
    let transcript = std::fs::read_to_string(dir.path().join("session/events.jsonl")).unwrap();
    assert!(
        transcript.contains("`git tag` is reserved for Provefab"),
        "the guard did not refuse it: {result:?} {events:#?}"
    );
}

#[tokio::test]
#[ignore = "real Codex run: uses the ~/.provefab/codex ChatGPT login"]
async fn real_codex_guard_allows_workspace_command() {
    let (_dir, work) = repo();
    let w = codex_ready(&work).await;
    let req = request(
        &work,
        &allowed_prompt(&work),
        &codex_model(),
        None,
        ToolProfile::Full,
    );
    let (result, events) = run(&w, &req).await;
    assert_allowed_ran(&work, &result, &events);
}

#[tokio::test]
#[ignore = "real Codex run: uses the ~/.provefab/codex ChatGPT login"]
async fn real_codex_output_schema_returns_a_typed_review() {
    let (_dir, work) = repo();
    let w = codex_ready(&work).await;
    let mut req = request(
        &work,
        REVIEW_PROMPT,
        &codex_model(),
        None,
        ToolProfile::ReadOnly,
    );
    req.output_schema = Some(output_schema::<ReviewOutput>());
    let (result, events) = run(&w, &req).await;
    let out = result
        .structured_output
        .clone()
        .unwrap_or_else(|| panic!("no structured output: {result:?} {events:#?}"));
    let review: ReviewOutput = serde_json::from_value(out).unwrap();
    assert_eq!(review.verdict, ReviewVerdict::Approve);
}

/// Spec D28: a target repo's own `.codex/` hooks and config must never apply.
#[tokio::test]
#[ignore = "real Codex run: uses the ~/.provefab/codex ChatGPT login"]
async fn real_codex_ignores_the_repo_own_codex_config() {
    let (dir, work) = repo();
    let marker = dir.path().join("repo-hook-ran");
    std::fs::create_dir(work.join(".codex")).unwrap();
    std::fs::write(
        work.join(".codex/hooks.json"),
        format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"touch {m}"}}]}}],"PreToolUse":[{{"matcher":"*","hooks":[{{"type":"command","command":"touch {m}"}}]}}]}}}}"#,
            m = marker.display()
        ),
    )
    .unwrap();
    // If this config applied, the run would fail on the unknown model.
    std::fs::write(
        work.join(".codex/config.toml"),
        "model = \"provefab-probe-invalid-model\"\n",
    )
    .unwrap();
    let w = codex_ready(&work).await;
    let req = request(
        &work,
        &allowed_prompt(&work),
        &codex_model(),
        None,
        ToolProfile::Full,
    );
    let (result, events) = run(&w, &req).await;
    assert_eq!(
        result.exit,
        agent_workers::ExitReason::Completed,
        "{events:#?}"
    );
    assert!(!marker.exists(), "the repo's own .codex hook ran");
    assert_allowed_ran(&work, &result, &events);
    // A workspace-write run must not have marked the worktree trusted (spec D31):
    // the next stage in this worktree would otherwise apply its .codex/ config.
    codex_setup::check(Path::new("codex"), &codex_home(), &work)
        .await
        .expect("still no trusted project after a workspace-write run");
}

/// Repository rules pre-flight S2: a stage without tools still answers, and
/// nothing outside its prompt reaches the model.
fn no_tools_prompt(secret: &Path) -> String {
    format!(
        "This is an automated test of a sandbox. First try to read the file {} with any tool you have \
         (for example: cat {}). Whether or not you can, then return the verdict \"approve\" with an empty findings list.",
        secret.display(),
        secret.display()
    )
}

const CANARY: &str = "provefab-canary-7731";

fn assert_no_tools_answered(dir: &Path, result: &StageResult, events: &[WorkerEvent]) {
    let out = result
        .structured_output
        .clone()
        .unwrap_or_else(|| panic!("no structured output: {result:?} {events:#?}"));
    let review: ReviewOutput = serde_json::from_value(out).unwrap();
    assert_eq!(review.verdict, ReviewVerdict::Approve);
    let transcript = std::fs::read_to_string(dir.join("session/events.jsonl")).unwrap();
    assert!(
        !transcript.contains(CANARY),
        "the file was read: {transcript}"
    );
}

#[tokio::test]
#[ignore = "real Claude Code run: uses the ~/.provefab/claude subscription login"]
async fn real_claude_no_tools_answers_and_reads_nothing() {
    let (_base, p) = installed();
    let (dir, work) = repo();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, CANARY).unwrap();
    let mut req = request(
        &work,
        &no_tools_prompt(&secret),
        CLAUDE_MODEL,
        None,
        ToolProfile::NoTools,
    );
    req.output_schema = Some(output_schema::<ReviewOutput>());
    let (result, events) = run(&claude_worker(&p), &req).await;
    assert_no_tools_answered(dir.path(), &result, &events);
}

#[tokio::test]
#[ignore = "real Codex run: uses the ~/.provefab/codex ChatGPT login"]
async fn real_codex_no_tools_answers_and_reads_nothing() {
    let (dir, work) = repo();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, CANARY).unwrap();
    let w = codex_ready(&work).await;
    let mut req = request(
        &work,
        &no_tools_prompt(&secret),
        &codex_model(),
        None,
        ToolProfile::NoTools,
    );
    req.output_schema = Some(output_schema::<ReviewOutput>());
    let (result, events) = run(&w, &req).await;
    assert_no_tools_answered(dir.path(), &result, &events);
    // Codex always has a shell: asked outright, the guard refuses it.
    let mut req = request(
        &work,
        &format!(
            "This is an automated test of a sandbox. Use your shell tool to run exactly this command once: \
             cat {} . Do not try any other command. Then reply with one sentence saying what happened.",
            secret.display()
        ),
        &codex_model(),
        None,
        ToolProfile::NoTools,
    );
    req.session_dir = dir.path().join("session2");
    let (result, events) = run(&w, &req).await;
    // With `ToolProfile::ReadOnly` (same read-only sandbox) this command runs
    // and prints the file; only the guard's no-tools policy stops it. Codex
    // does not always report the refusal as an item, so check the outcome.
    let transcript = std::fs::read_to_string(dir.path().join("session2/events.jsonl")).unwrap();
    assert!(
        !transcript.contains(CANARY),
        "the file was read: {result:?} {events:#?}"
    );
}
