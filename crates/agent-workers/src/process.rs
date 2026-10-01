use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::jsonl::JsonlReader;
use crate::{ToolProfile, WorkerError};

/// Credentials a worker must never see. Pushing is Provefab's job (spec §3.2).
pub const SCRUBBED_ENV: [&str; 6] = [
    "SSH_AUTH_SOCK",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GH_ENTERPRISE_TOKEN",
];

/// Git config the worker appends through `GIT_CONFIG_*`, after any entries the
/// user already set: no credential helper, and every push URL rewritten to a
/// scheme that does not exist. A `core.hooksPath` with a failing `pre-push`
/// hook is appended too, for remotes whose explicit `pushurl` skips
/// `pushInsteadOf`. Fetch is unaffected.
pub const NO_PUSH_CONFIG: [(&str, &str); 2] = [
    ("credential.helper", ""),
    ("url.provefab-no-push://.pushInsteadOf", ""),
];

/// Time a worker gets to clean up after SIGTERM (Pi kills its own tool
/// commands on SIGTERM) before the whole tree gets SIGKILL.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// How long to keep reading records the child printed just before it exited.
const DRAIN: Duration = Duration::from_secs(2);
pub(crate) const STDERR_TAIL: usize = 4096;

pub(crate) enum Flow {
    Continue,
    Stop,
}

#[derive(Debug)]
pub(crate) struct Finished {
    pub timed_out: bool,
    pub stopped: bool,
    pub status: Option<ExitStatus>,
    pub stderr_tail: String,
}

/// Set (to `1`) for a stage without tools: `provefab guard` then refuses
/// every call but the structured answer's own.
pub const NO_TOOLS_ENV: &str = "PROVEFAB_NO_TOOLS";

/// Environment every worker gets, whatever the agent runtime.
pub fn apply_worker_env(cmd: &mut Command, worktree: &Path, hooks_dir: &Path, tools: ToolProfile) {
    for key in SCRUBBED_ENV {
        cmd.env_remove(key);
    }
    match tools {
        ToolProfile::NoTools => cmd.env(NO_TOOLS_ENV, "1"),
        ToolProfile::ReadOnly | ToolProfile::Full => cmd.env_remove(NO_TOOLS_ENV),
    };
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let mut n = current_env(cmd, "GIT_CONFIG_COUNT")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let hooks = hooks_dir.to_string_lossy().to_string();
    let entries = NO_PUSH_CONFIG
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .chain([("core.hooksPath".to_string(), hooks)]);
    for (key, value) in entries {
        cmd.env(format!("GIT_CONFIG_KEY_{n}"), key);
        cmd.env(format!("GIT_CONFIG_VALUE_{n}"), value);
        n += 1;
    }
    cmd.env("GIT_CONFIG_COUNT", n.to_string());
    cmd.env("PROVEFAB_WORKTREE", worktree);
}

/// The value `key` will have in the child: set on `cmd`, else inherited.
fn current_env(cmd: &Command, key: &str) -> Option<String> {
    for (k, v) in cmd.as_std().get_envs() {
        if k == key {
            return v.map(|v| v.to_string_lossy().to_string());
        }
    }
    std::env::var(key).ok()
}

/// Writes `<session_dir>/git-hooks/pre-push`, which always refuses, and returns the directory.
pub fn prepare_git_hooks(session_dir: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = session_dir.join("git-hooks");
    std::fs::create_dir_all(&dir)?;
    let hook = dir.join("pre-push");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'provefab: workers cannot push; Provefab pushes after its checks' >&2\nexit 1\n",
    )?;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    Ok(dir)
}

/// Runs `cmd` in its own process group and hands every JSON record on stdout
/// to `on_record`. The stage ends when the child exits, on timeout, or on
/// `Flow::Stop`; whatever it started is then terminated (see `terminate_tree`),
/// including commands that moved to their own session. Lines that are not
/// JSON are skipped.
pub(crate) async fn run_jsonl(
    mut cmd: Command,
    timeout: Duration,
    mut on_record: impl FnMut(Value) -> Flow,
) -> Result<Finished, WorkerError> {
    let program = cmd.as_std().get_program().to_string_lossy().to_string();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = cmd.spawn().map_err(|e| WorkerError::Spawn {
        program,
        message: e.to_string(),
    })?;
    let pid = child
        .id()
        .and_then(|p| i32::try_from(p).ok())
        .ok_or_else(|| WorkerError::Io("child has no pid".into()))?;
    // Kills the tree if this future is dropped (the stage was cancelled).
    let mut guard = TreeGuard { pid, armed: true };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorkerError::Io("no stdout".into()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| WorkerError::Io("no stderr".into()))?;

    let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
    let tail_writer = Arc::clone(&tail);
    let stderr_task = tokio::spawn(async move {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            if let Ok(mut t) = tail_writer.lock() {
                t.extend_from_slice(&chunk[..n]);
                let excess = t.len().saturating_sub(STDERR_TAIL);
                t.drain(..excess);
            }
        }
    });
    // Records travel over a channel so waiting on them is cancel-safe.
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let reader_task = tokio::spawn(async move {
        let mut reader = JsonlReader::new(stdout);
        while let Ok(Some(record)) = reader.next_record().await {
            if tx.send(record).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let (mut status, mut timed_out, mut stopped) = (None, false, false);
    let mut stdout_closed = false;
    loop {
        tokio::select! {
            record = rx.recv(), if !stdout_closed => match record {
                Some(r) => {
                    if deliver(&r, &mut on_record) {
                        stopped = true;
                        break;
                    }
                }
                None => stdout_closed = true,
            },
            s = child.wait() => {
                status = s.ok();
                let drain_until = std::cmp::min(deadline, Instant::now() + DRAIN);
                while let Ok(Some(r)) = tokio::time::timeout_at(drain_until, rx.recv()).await {
                    if deliver(&r, &mut on_record) {
                        stopped = true;
                        break;
                    }
                }
                break;
            }
            _ = tokio::time::sleep_until(deadline) => {
                timed_out = true;
                break;
            }
        }
    }

    terminate_tree(pid, status.is_none(), &mut child).await;
    guard.armed = false;
    reader_task.abort();
    if tokio::time::timeout(DRAIN, stderr_task).await.is_err() {
        // A process outside the tree still holds stderr; keep what we have.
    }
    let stderr_tail = tail
        .lock()
        .map(|t| String::from_utf8_lossy(&t).to_string())
        .unwrap_or_default();
    Ok(Finished {
        timed_out,
        stopped,
        status,
        stderr_tail,
    })
}

fn deliver(record: &[u8], on_record: &mut impl FnMut(Value) -> Flow) -> bool {
    match serde_json::from_slice::<Value>(record) {
        Ok(v) => matches!(on_record(v), Flow::Stop),
        Err(_) => false,
    }
}

/// Every process the stage started. The tree is read before any signal,
/// because processes are re-parented to launchd once their parent dies.
/// A still-running child first gets SIGTERM and a grace period, then
/// everything gets SIGKILL: the child's group, each descendant, and each
/// descendant's own group (Pi's bash tool runs commands in a new session).
/// Known gap: a command that already detached and whose parent already
/// exited is no longer in the tree (spec D26).
async fn terminate_tree(pid: i32, child_alive: bool, child: &mut Child) {
    let tree = descendants(pid);
    if child_alive {
        signal_tree(pid, &tree, libc::SIGTERM);
        let _ = tokio::time::timeout(EXIT_GRACE, child.wait()).await;
    }
    signal_tree(pid, &tree, libc::SIGKILL);
    let _ = tokio::time::timeout(EXIT_GRACE, child.wait()).await;
}

struct TreeGuard {
    pid: i32,
    armed: bool,
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        if self.armed {
            let tree = descendants(self.pid);
            signal_tree(self.pid, &tree, libc::SIGKILL);
        }
    }
}

/// `(pid, pgid)` of every process below `root`, from one `ps` snapshot.
fn descendants(root: i32) -> Vec<(i32, i32)> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid="])
        .output()
    else {
        return Vec::new();
    };
    let rows: Vec<(i32, i32, i32)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace().map(|x| x.parse::<i32>().ok());
            Some((it.next()??, it.next()??, it.next()??))
        })
        .collect();
    let mut found = Vec::new();
    let mut seen = HashSet::from([root]);
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for &(child, ppid, pgid) in &rows {
            if ppid == parent && seen.insert(child) {
                found.push((child, pgid));
                frontier.push(child);
            }
        }
    }
    found
}

fn signal_tree(pid: i32, tree: &[(i32, i32)], signal: libc::c_int) {
    // SAFETY: getpgrp(2) has no preconditions.
    let own_group = unsafe { libc::getpgrp() };
    let send = |target: i32| {
        // SAFETY: kill(2) only sends a signal; a target that is already gone returns ESRCH.
        unsafe {
            libc::kill(target, signal);
        }
    };
    if pid > 1 && pid != own_group {
        send(-pid);
    }
    for &(p, group) in tree {
        if group > 1 && group != own_group {
            send(-group);
        }
        if p > 1 {
            send(p);
        }
    }
}

/// Every raw record a worker printed, kept as `<session_dir>/events.jsonl` for `provefab log`.
pub(crate) struct Transcript {
    file: std::fs::File,
}

impl Transcript {
    pub(crate) fn open(session_dir: &Path) -> Result<Self, WorkerError> {
        std::fs::create_dir_all(session_dir).map_err(|e| WorkerError::Io(e.to_string()))?;
        let file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(session_dir.join("events.jsonl"))
            .map_err(|e| WorkerError::Io(e.to_string()))?;
        Ok(Self { file })
    }

    /// Best effort: a full disk must not kill the stage.
    pub(crate) fn write(&mut self, record: &Value) {
        use std::io::Write;
        let _ = writeln!(self.file, "{record}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    #[tokio::test]
    async fn reads_records_across_split_writes_and_skips_noise() {
        let mut seen = Vec::new();
        let f = run_jsonl(
            sh(r#"printf '{"a":'; sleep 0.1; printf '1}\nnot json\n{"b":2}\n'"#),
            Duration::from_secs(5),
            |v| {
                seen.push(v);
                Flow::Continue
            },
        )
        .await
        .unwrap();
        assert_eq!(
            seen,
            vec![serde_json::json!({"a":1}), serde_json::json!({"b":2})]
        );
        assert!(f.status.unwrap().success());
        assert!(!f.timed_out && !f.stopped);
    }

    #[tokio::test]
    async fn timeout_kills_a_hung_process() {
        let started = std::time::Instant::now();
        let f = run_jsonl(
            sh("echo '{}'; sleep 30"),
            Duration::from_millis(300),
            |_| Flow::Continue,
        )
        .await
        .unwrap();
        assert!(f.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn stop_kills_the_process_and_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child-alive");
        let script = format!(
            "(sleep 1; touch {}) & echo '{{}}'; sleep 30",
            marker.display()
        );
        let f = run_jsonl(sh(&script), Duration::from_secs(5), |_| Flow::Stop)
            .await
            .unwrap();
        assert!(f.stopped);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!marker.exists(), "background child survived the stage");
    }

    #[tokio::test]
    async fn crash_keeps_stderr_tail() {
        let f = run_jsonl(sh("echo boom >&2; exit 3"), Duration::from_secs(5), |_| {
            Flow::Continue
        })
        .await
        .unwrap();
        assert_eq!(f.status.unwrap().code(), Some(3));
        assert!(f.stderr_tail.contains("boom"));
    }

    #[tokio::test]
    async fn missing_program_is_a_spawn_error() {
        let err = run_jsonl(
            Command::new("/nonexistent/agent"),
            Duration::from_secs(1),
            |_| Flow::Continue,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkerError::Spawn { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn worker_env_scrubs_credentials_and_blocks_push() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = sh(
            r#"printf '{"sock":"%s","token":"%s","wt":"%s","push":"%s"}\n' "${SSH_AUTH_SOCK:-}" "${GH_TOKEN:-}" "$PROVEFAB_WORKTREE" "$(git config --get-regexp 'url[.].*[.]pushinsteadof' >/dev/null && echo yes || echo no)""#,
        );
        cmd.env("SSH_AUTH_SOCK", "/tmp/agent.sock")
            .env("GH_TOKEN", "secret");
        apply_worker_env(
            &mut cmd,
            dir.path(),
            &dir.path().join("hooks"),
            ToolProfile::Full,
        );
        let mut seen = None;
        run_jsonl(cmd, Duration::from_secs(5), |v| {
            seen = Some(v);
            Flow::Continue
        })
        .await
        .unwrap();
        let v = seen.unwrap();
        assert_eq!(v["sock"], "");
        assert_eq!(v["token"], "");
        assert_eq!(v["wt"], dir.path().to_string_lossy().as_ref());
        assert_eq!(v["push"], "yes");
    }

    #[tokio::test]
    async fn worker_env_makes_git_push_fail() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"set -e
git init -q --bare remote.git
git init -q work && cd work
git -c user.email=t@t -c user.name=t commit -q --allow-empty -m x
git remote add origin ../remote.git
if git push -q origin HEAD:main 2>/dev/null; then echo '{"pushed":true}'; else echo '{"pushed":false}'; fi"#;
        let mut cmd = sh(script);
        cmd.current_dir(dir.path());
        apply_worker_env(
            &mut cmd,
            dir.path(),
            &dir.path().join("hooks"),
            ToolProfile::Full,
        );
        let mut seen = None;
        run_jsonl(cmd, Duration::from_secs(20), |v| {
            seen = Some(v);
            Flow::Continue
        })
        .await
        .unwrap();
        assert_eq!(seen.unwrap()["pushed"], false);
    }

    /// A `sh -c` script that starts a grandchild in its own session (as Pi's
    /// bash tool does) which touches `marker` after `delay` seconds.
    fn detached_grandchild(marker: &Path, delay: u32, then: &str) -> Command {
        sh(&format!(
            "perl -e 'use POSIX; setsid(); sleep {delay}; open(my $f, \">\", \"{}\")' </dev/null >/dev/null 2>&1 & {then}",
            marker.display()
        ))
    }

    /// Final review C1: commands in their own session survive a group kill.
    #[tokio::test]
    async fn review_c1_detached_grandchild_is_killed_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let f = run_jsonl(
            detached_grandchild(&marker, 2, "echo '{}'; sleep 30"),
            Duration::from_millis(500),
            |_| Flow::Continue,
        )
        .await
        .unwrap();
        assert!(f.timed_out);
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "detached grandchild outlived the stage");
    }

    /// Final review I6: the child exited but a background job still holds stdout.
    #[tokio::test]
    async fn review_i6_child_exit_is_seen_while_a_job_holds_stdout() {
        let started = std::time::Instant::now();
        let f = run_jsonl(
            sh("echo '{}'; sleep 25 & exit 0"),
            Duration::from_secs(20),
            |_| Flow::Continue,
        )
        .await
        .unwrap();
        assert!(!f.timed_out, "reported a timeout for a child that exited");
        assert!(f.status.unwrap().success());
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{:?}",
            started.elapsed()
        );
    }

    /// Final review I6: a detached grandchild holding stderr must not hang the runner.
    #[tokio::test]
    async fn review_i6_detached_stderr_holder_does_not_hang() {
        let started = std::time::Instant::now();
        let script =
            "perl -e 'use POSIX; setsid(); sleep 30' </dev/null >/dev/null & echo '{}'; sleep 30";
        let f = run_jsonl(sh(script), Duration::from_secs(1), |_| Flow::Continue)
            .await
            .unwrap();
        assert!(f.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "{:?}",
            started.elapsed()
        );
    }

    /// Final review M3: stderr keeps only a bounded tail.
    #[tokio::test]
    async fn review_m3_stderr_tail_is_bounded() {
        let f = run_jsonl(
            sh("i=0; while [ $i -lt 2000 ]; do echo 'noise noise noise noise' >&2; i=$((i+1)); done; echo END >&2"),
            Duration::from_secs(10),
            |_| Flow::Continue,
        )
        .await
        .unwrap();
        assert!(
            f.stderr_tail.len() <= STDERR_TAIL,
            "{}",
            f.stderr_tail.len()
        );
        assert!(f.stderr_tail.trim_end().ends_with("END"));
    }

    /// Final review M4: dropping the stage future (cancellation) kills the whole group.
    #[tokio::test]
    async fn review_m4_cancelling_the_stage_kills_the_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let script = format!("(sleep 2; touch {}) & sleep 30", marker.display());
        let fut = run_jsonl(sh(&script), Duration::from_secs(30), |_| Flow::Continue);
        let _ = tokio::time::timeout(Duration::from_millis(500), fut).await;
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            !marker.exists(),
            "background job outlived a cancelled stage"
        );
    }

    /// Final review I5: a remote with `pushurl` bypassed `pushInsteadOf`.
    #[tokio::test]
    async fn review_i5_push_fails_even_with_pushurl() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = prepare_git_hooks(&dir.path().join("session")).unwrap();
        let script = r#"set -e
git init -q --bare remote.git
git init -q work && cd work
git -c user.email=t@t -c user.name=t commit -q --allow-empty -m x
git remote add origin ../remote.git
git config remote.origin.pushurl "$(cd .. && pwd)/remote.git"
if git push -q origin HEAD:main 2>/dev/null; then echo '{"pushed":true}'; else echo '{"pushed":false}'; fi"#;
        let mut cmd = sh(script);
        cmd.current_dir(dir.path());
        apply_worker_env(&mut cmd, dir.path(), &hooks, ToolProfile::Full);
        let mut seen = None;
        run_jsonl(cmd, Duration::from_secs(20), |v| {
            seen = Some(v);
            Flow::Continue
        })
        .await
        .unwrap();
        assert_eq!(seen.unwrap()["pushed"], false);
    }

    /// Final review M5: the user's own GIT_CONFIG_* entries are kept, ours are appended.
    #[test]
    fn review_m5_user_git_config_env_is_preserved() {
        let mut cmd = sh("true");
        cmd.env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "safe.directory")
            .env("GIT_CONFIG_VALUE_0", "*");
        apply_worker_env(
            &mut cmd,
            Path::new("/w"),
            Path::new("/h"),
            ToolProfile::Full,
        );
        let envs: std::collections::HashMap<String, String> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_string_lossy().into(), v?.to_string_lossy().into())))
            .collect();
        assert_eq!(envs["GIT_CONFIG_KEY_0"], "safe.directory");
        assert_eq!(envs["GIT_CONFIG_COUNT"], "4");
        assert_eq!(envs["GIT_CONFIG_KEY_1"], "credential.helper");
        assert_eq!(envs["GIT_CONFIG_KEY_3"], "core.hooksPath");
        assert_eq!(envs["GIT_CONFIG_VALUE_3"], "/h");
    }
}
