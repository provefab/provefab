//! Deterministic checks between stages (spec §3.1) and the progress score that
//! decides whether a retry got better (spec §3.4 item 2, D34).

use std::collections::VecDeque;
use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_workers::{SCRUBBED_ENV, ToolProfile, apply_worker_env, prepare_git_hooks};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const TAIL_CHARS: usize = 4000;
/// A single line longer than this is counted on its first part only.
const MAX_LINE: usize = 64 * 1024;
/// How long to keep reading output after the gate exited or was killed.
const DRAIN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateResult {
    pub command: String,
    pub exit: Option<i32>,
    pub passed: bool,
    pub timed_out: bool,
    /// The last output bytes (stdout and stderr interleaved), for triage and `provefab log`.
    pub output_tail: String,
    /// Failing tests and error lines counted over the whole output, each stream separately.
    pub failing_tests: u32,
    pub error_lines: u32,
}

/// Lower is better; compared field by field in this order (D34).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct ProgressScore {
    pub failing_gates: u32,
    pub failing_tests: u32,
    pub error_lines: u32,
}

impl ProgressScore {
    pub fn is_clean(self) -> bool {
        self == ProgressScore::default()
    }

    /// A retry must strictly improve on the previous attempt (D34).
    pub fn improved_on(self, previous: ProgressScore) -> bool {
        self < previous
    }

    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.split(',').map(|p| p.trim().parse::<u32>().ok());
        let score = ProgressScore {
            failing_gates: it.next()??,
            failing_tests: it.next()??,
            error_lines: it.next()??,
        };
        it.next().is_none().then_some(score)
    }
}

impl fmt::Display for ProgressScore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{},{},{}",
            self.failing_gates, self.failing_tests, self.error_lines
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    pub results: Vec<GateResult>,
    pub score: ProgressScore,
}

impl GateReport {
    pub fn passed(&self) -> bool {
        self.results.iter().all(|r| r.passed)
    }

    /// The first failing gate, the one triage looks at.
    pub fn first_failure(&self) -> Option<&GateResult> {
        self.results.iter().find(|r| !r.passed)
    }
}

/// Runs every gate in `worktree`, in order, even after a failure, so the score
/// reflects the whole state. Gates run code the agent wrote, so they get the
/// workers' protection: push credentials removed, git's no-push config and
/// failing pre-push hook, and an empty `gh` config (spec D23). `scratch` holds
/// the hook and the empty `gh` config; it must be outside the worktree.
pub async fn run_gates(
    worktree: &Path,
    commands: &[String],
    timeout: Duration,
    scratch: &Path,
) -> GateReport {
    let mut results = Vec::new();
    let mut score = ProgressScore::default();
    for command in commands {
        let result = run_gate(worktree, command, timeout, scratch).await;
        if !result.passed {
            score.failing_gates += 1;
        }
        score.failing_tests += result.failing_tests;
        score.error_lines += result.error_lines;
        results.push(result);
    }
    GateReport { results, score }
}

/// Output of one gate as it streams: a bounded tail and failure counts.
#[derive(Default)]
struct Capture {
    tail: VecDeque<u8>,
    failing_tests: u32,
    error_lines: u32,
}

impl Capture {
    fn keep(&mut self, bytes: &[u8]) {
        self.tail.extend(bytes);
        let excess = self.tail.len().saturating_sub(TAIL_CHARS);
        self.tail.drain(..excess);
    }

    fn count(&mut self, line: &str) {
        let mut c = FailureCounter::default();
        c.line(line);
        self.failing_tests += c.tests;
        self.error_lines += c.errors;
    }
}

/// Reads one stream to the end: every complete line is counted, every byte
/// goes to the shared bounded tail.
async fn pump(mut reader: impl AsyncRead + Unpin, capture: Arc<Mutex<Capture>>) {
    let mut chunk = [0u8; 8192];
    let mut line: Vec<u8> = Vec::new();
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let Ok(mut cap) = capture.lock() else { break };
        cap.keep(&chunk[..n]);
        for &b in &chunk[..n] {
            if b == b'\n' {
                cap.count(&String::from_utf8_lossy(&line));
                line.clear();
            } else if line.len() < MAX_LINE {
                line.push(b);
            }
        }
    }
    if !line.is_empty()
        && let Ok(mut cap) = capture.lock()
    {
        cap.count(&String::from_utf8_lossy(&line));
    }
}

async fn run_gate(worktree: &Path, command: &str, timeout: Duration, scratch: &Path) -> GateResult {
    let failed = |tail: String| GateResult {
        command: command.to_string(),
        exit: None,
        passed: false,
        timed_out: false,
        output_tail: tail,
        failing_tests: 0,
        error_lines: 0,
    };
    let gh_config = scratch.join("gh-empty");
    let hooks = match prepare_git_hooks(scratch)
        .and_then(|h| std::fs::create_dir_all(&gh_config).map(|_| h))
    {
        Ok(h) => h,
        Err(e) => return failed(format!("could not prepare the gate environment: {e}")),
    };
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    // Gate commands are not agent stages: no `PROVEFAB_NO_TOOLS`.
    apply_worker_env(&mut cmd, worktree, &hooks, ToolProfile::Full);
    for key in SCRUBBED_ENV {
        cmd.env_remove(key);
    }
    cmd.env("GH_CONFIG_DIR", &gh_config);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return failed(format!("could not start: {e}")),
    };
    let pid = child.id().and_then(|p| i32::try_from(p).ok());
    let capture = Arc::new(Mutex::new(Capture::default()));
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(tokio::spawn(pump(out, capture.clone())));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(tokio::spawn(pump(err, capture.clone())));
    }
    // The gate ends when its process exits, not when every inheritor of its
    // pipes closes them (a leaked background server must not fake a timeout).
    let status = tokio::time::timeout(timeout, child.wait()).await;
    kill_group(pid);
    let drained = tokio::time::timeout(DRAIN, async {
        for r in &mut readers {
            let _ = r.await;
        }
    })
    .await;
    if drained.is_err() {
        readers.iter().for_each(|r| r.abort());
    }
    let (tail, failing_tests, error_lines) = capture
        .lock()
        .map(|c| {
            let bytes: Vec<u8> = c.tail.iter().copied().collect();
            (
                String::from_utf8_lossy(&bytes).to_string(),
                c.failing_tests,
                c.error_lines,
            )
        })
        .unwrap_or_default();
    match status {
        Ok(Ok(s)) => GateResult {
            command: command.to_string(),
            exit: s.code(),
            passed: s.success(),
            timed_out: false,
            output_tail: tail,
            failing_tests,
            error_lines,
        },
        Ok(Err(e)) => failed(format!("{tail}\ncould not wait: {e}")),
        Err(_) => GateResult {
            command: command.to_string(),
            exit: None,
            passed: false,
            timed_out: true,
            output_tail: format!("{tail}\n[provefab] timed out after {}s", timeout.as_secs()),
            failing_tests,
            error_lines,
        },
    }
}

fn kill_group(pid: Option<i32>) {
    if let Some(pid) = pid.filter(|p| *p > 1) {
        // SAFETY: kill(2) on the process group created with process_group(0); ESRCH if gone.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

/// Counts failing tests and error lines, one line at a time.
#[derive(Default)]
struct FailureCounter {
    tests: u32,
    errors: u32,
}

impl FailureCounter {
    fn line(&mut self, line: &str) {
        let l = line.trim();
        // cargo nextest: "Summary [ 1.2s] 42 tests run: 40 passed, 2 failed, 1 skipped"
        // cargo test:    "test result: FAILED. 40 passed; 2 failed; 0 ignored; ..."
        // pytest:        "==== 2 failed, 40 passed in 1.2s ===="
        // jest/vitest:   "Tests:       2 failed, 40 passed, 42 total"
        if l.starts_with("Summary [")
            || l.starts_with("test result:")
            || l.starts_with("Tests:")
            || l.starts_with('=')
        {
            self.tests += number_before(l, " failed");
        }
        // rustc / clippy: "error[E0308]: ..." or "error: ..." (not the final "could not compile" line)
        if (l.starts_with("error[") || l.starts_with("error:"))
            && !l.starts_with("error: could not compile")
        {
            self.errors += 1;
        }
    }
}

/// `(failing tests, error lines)` read from common test-runner and compiler output.
pub fn count_failures(output: &str) -> (u32, u32) {
    let mut c = FailureCounter::default();
    output.lines().for_each(|l| c.line(l));
    (c.tests, c.errors)
}

/// The number right before `word` in `line`, e.g. 2 in "40 passed, 2 failed".
fn number_before(line: &str, word: &str) -> u32 {
    let Some(pos) = line.find(word) else {
        return 0;
    };
    let digits: String = line[..pos]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_failures_from_common_runners() {
        let nextest = "     Summary [   1.2s] 42 tests run: 40 passed, 2 failed, 1 skipped";
        let cargo =
            "test result: FAILED. 40 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out";
        let pytest = "=========== 4 failed, 40 passed in 1.20s ===========";
        let jest = "Tests:       5 failed, 40 passed, 45 total";
        let clippy =
            "error[E0308]: mismatched types\nerror: unused variable\nerror: could not compile `x`";
        assert_eq!(count_failures(nextest), (2, 0));
        assert_eq!(count_failures(cargo), (3, 0));
        assert_eq!(count_failures(pytest), (4, 0));
        assert_eq!(count_failures(jest), (5, 0));
        assert_eq!(count_failures(clippy), (0, 2));
        assert_eq!(
            count_failures("all good\ntest result: ok. 10 passed; 0 failed"),
            (0, 0)
        );
    }

    #[test]
    fn score_orders_by_gates_then_tests_then_errors() {
        let s = |g, t, e| ProgressScore {
            failing_gates: g,
            failing_tests: t,
            error_lines: e,
        };
        assert!(s(1, 9, 9).improved_on(s(2, 0, 0)));
        assert!(s(2, 1, 9).improved_on(s(2, 3, 0)));
        assert!(s(2, 3, 1).improved_on(s(2, 3, 4)));
        assert!(
            !s(2, 3, 4).improved_on(s(2, 3, 4)),
            "equal is not an improvement"
        );
        assert!(!s(3, 0, 0).improved_on(s(2, 9, 9)));
        assert_eq!(
            ProgressScore::parse(&s(1, 2, 3).to_string()),
            Some(s(1, 2, 3))
        );
        assert_eq!(ProgressScore::parse("1,2"), None);
        assert!(s(0, 0, 0).is_clean());
    }

    #[tokio::test]
    async fn runs_every_gate_and_scores_them() {
        let dir = tempfile::tempdir().unwrap();
        let gates = vec![
            "true".to_string(),
            "echo 'test result: FAILED. 1 passed; 2 failed;'; exit 1".to_string(),
            "echo 'error: boom' >&2; exit 1".to_string(),
        ];
        let scratch = tempfile::tempdir().unwrap();
        let report = run_gates(dir.path(), &gates, Duration::from_secs(10), scratch.path()).await;
        assert!(!report.passed());
        assert_eq!(
            report.results.iter().map(|r| r.passed).collect::<Vec<_>>(),
            vec![true, false, false]
        );
        assert_eq!(
            report.score,
            ProgressScore {
                failing_gates: 2,
                failing_tests: 2,
                error_lines: 1
            }
        );
        assert_eq!(report.first_failure().unwrap().exit, Some(1));
    }

    #[tokio::test]
    async fn a_hung_gate_times_out_and_fails() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let report = run_gates(
            dir.path(),
            &["echo partial; sleep 30".to_string()],
            Duration::from_millis(300),
            dir.path(),
        )
        .await;
        assert!(report.results[0].timed_out && !report.passed());
        assert!(started.elapsed() < Duration::from_secs(5));
        // Final review (minor): a timed-out gate keeps the output it produced.
        assert!(
            report.results[0].output_tail.contains("partial"),
            "{:?}",
            report.results[0]
        );
    }

    #[tokio::test]
    async fn gates_do_not_see_push_credentials() {
        let dir = tempfile::tempdir().unwrap();
        // SSH_AUTH_SOCK is normally set in a desktop session; the gate must not see it either way.
        let report = run_gates(
            dir.path(),
            &["test -z \"${SSH_AUTH_SOCK:-}\" && test -z \"${GH_TOKEN:-}\"".to_string()],
            Duration::from_secs(5),
            dir.path(),
        )
        .await;
        assert!(report.passed(), "{:?}", report.results);
    }

    /// Final review I1: gates run agent-written code, so git gets the workers' no-push
    /// config (no credential helper, failing pre-push hook) and gh an empty config dir.
    #[tokio::test]
    async fn review_i1_gates_get_no_credential_helper_and_no_gh_config() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let check = "test -z \"$(git config --get credential.helper)\" \
             && test -n \"$(git config --get core.hooksPath)\" \
             && test -x \"$(git config --get core.hooksPath)/pre-push\" \
             && test -n \"${GH_CONFIG_DIR:-}\" && test ! -e \"$GH_CONFIG_DIR/hosts.yml\"";
        let report = run_gates(
            dir.path(),
            &[check.to_string()],
            Duration::from_secs(10),
            scratch.path(),
        )
        .await;
        assert!(report.passed(), "{:?}", report.results);
    }

    /// Final review I2: output is kept as a bounded tail, never fully in memory.
    #[tokio::test]
    async fn review_i2_huge_output_keeps_a_bounded_tail() {
        let dir = tempfile::tempdir().unwrap();
        let report = run_gates(
            dir.path(),
            &["yes 'some log line' | head -c 20000000; echo LAST".to_string()],
            Duration::from_secs(30),
            dir.path(),
        )
        .await;
        let tail = &report.results[0].output_tail;
        assert!(report.passed());
        assert!(tail.len() <= TAIL_CHARS * 4, "{}", tail.len());
        assert!(tail.trim_end().ends_with("LAST"));
    }

    /// Final review I3: a background child holding the pipes must not fake a timeout.
    #[tokio::test]
    async fn review_i3_background_child_does_not_fake_a_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let report = run_gates(
            dir.path(),
            &["sleep 30 & echo ok; exit 0".to_string()],
            Duration::from_secs(10),
            dir.path(),
        )
        .await;
        assert!(
            report.passed() && !report.results[0].timed_out,
            "{:?}",
            report.results
        );
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "{:?}",
            started.elapsed()
        );
    }

    /// Final review I4: the score reads the whole output of each stream, not a shared tail.
    #[tokio::test]
    async fn review_i4_score_reads_whole_streams() {
        let dir = tempfile::tempdir().unwrap();
        let noisy = "echo 'test result: FAILED. 1 passed; 5 failed;'; \
             i=0; while [ $i -lt 400 ]; do echo 'warning: something long enough to fill the tail' >&2; i=$((i+1)); done; exit 1";
        let errors = "i=0; while [ $i -lt 60 ]; do echo \"error[E0308]: mismatched types $i\"; i=$((i+1)); done; exit 1";
        let report = run_gates(
            dir.path(),
            &[noisy.to_string(), errors.to_string()],
            Duration::from_secs(10),
            dir.path(),
        )
        .await;
        assert_eq!(report.score.failing_tests, 5, "{:?}", report.score);
        assert_eq!(report.score.error_lines, 60, "{:?}", report.score);
    }
}
