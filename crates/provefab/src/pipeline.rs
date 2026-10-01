//! The task state machine (spec §3): one `step` moves a task out of its
//! current state. Every transition is written before the side effect it leads
//! to, and every step can be re-run after a crash (spec §3.2).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use agent_workers::{ExitReason, StageRequest, StageResult, ToolProfile, WorkerEvent};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc::unbounded_channel;

use crate::agents::StageRunner;
use crate::config::{Config, ModelEntry, RepoConfig};
use crate::cooldown::Cooldowns;
use crate::forge::{Change, ForgeError, Git, PrState, branch_name, is_bot_comment, weakened_tests};
use crate::gates::{GateReport, ProgressScore, run_gates};
use crate::intake::new_replies;
use crate::jevq::{IssueContext, Triage};
use crate::paths::Paths;
use crate::policy::{Approval, BoxFuture, MergeOutcome, MergeTools, PrOpened, ReviewPolicy};
use crate::ports::{Hub, Oracle};
use crate::prompts::{Template, render};
use crate::record::{Event, FindingRow, GateEntry, MergedBy, Rule, StoredEvent};
use crate::risk::{self, Detected};
use crate::router::{StageTiers, fallback_tiers, resolve_tier, select, stage_tiers};
use crate::stage::{Finding, PlanOutput, ReviewOutput, ReviewVerdict, Severity, output_schema};
use crate::store::{Also, StageRunRecord, Store, StoreError, TaskRow, Write, now, rfc3339};
use crate::task::{Stage, TaskKind, TaskState, Tier};

/// Loop detector cadence and window (spec §4.3).
const LOOP_EVERY: u32 = 15;
const LOOP_KEEP: usize = 60;
/// Reply check threshold (spec §4.5).
const REPLY_THRESHOLD: f64 = 0.7;
/// A retry that keeps improving may run at most this many implement attempts per round (D35).
const MAX_ATTEMPTS: u32 = 4;
/// The review prompt carries the diff up to this many characters.
const DIFF_LIMIT: usize = 60_000;
/// The `file` of a finding made from a person's comment on a closed pull
/// request (D52): periodic work reads these as change requests.
pub(crate) const PR_COMMENT_FILE: &str = "(pull request comment)";

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Forge(#[from] ForgeError),
    #[error("pipeline: no task {0}")]
    UnknownTask(i64),
}

/// Everything a step needs. Generic over the three ports so tests use fakes.
pub struct Pipeline<R, O, H> {
    pub store: Store,
    pub runner: R,
    pub oracle: O,
    pub hub: H,
    pub git: Git,
    pub paths: Paths,
    pub config: Config,
    pub cooldowns: Mutex<Cooldowns>,
    /// Model prices for ordering each tier (D71, D73); refreshed daily.
    pub prices: std::sync::RwLock<crate::prices::PriceTable>,
    /// When the loop last tried to refresh prices, so a failure (offline)
    /// waits `prices::RETRY_AFTER` instead of refetching on every tick.
    pub price_attempt: std::sync::atomic::AtomicI64,
    /// One lock per repo slug, so tasks in the same repo that run in parallel
    /// (`max_concurrency`) never fetch or `git worktree add` at once: git's
    /// shared per-repo administrative files aren't safe for concurrent writers.
    pub repo_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// Serialises the daily budget count+claim (in `claim`) with recording a
    /// worker run and marking it recorded (in `run_stage`), so the check and
    /// the claim are atomic under parallel workers (issue #12).
    pub budget: tokio::sync::Mutex<()>,
    /// What happens around review: approvals needed, and after the PR opens (D59).
    pub policy: Arc<dyn ReviewPolicy>,
}

/// What the retry ladder does after a failed attempt (spec §3.2, D34, D35).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ladder {
    Retry,
    Escalate,
    GiveUp,
}

/// Retry once on the same tier; keep retrying while the progress score strictly
/// improves (up to `MAX_ATTEMPTS`); otherwise move up one tier once; then give up.
pub fn ladder(attempts: u32, escalated: bool, improved: bool) -> Ladder {
    if attempts <= 1 || (improved && attempts < MAX_ATTEMPTS) {
        Ladder::Retry
    } else if !escalated {
        Ladder::Escalate
    } else {
        Ladder::GiveUp
    }
}

/// A model counted as running; freed on drop.
pub(crate) struct Slot<'a> {
    cooldowns: &'a Mutex<Cooldowns>,
    model_id: String,
    /// Whether this claim's `stage_run` has been recorded yet (issue #12).
    recorded: bool,
}

impl Slot<'_> {
    /// Marks this claim's `stage_run` as recorded, giving its share of the
    /// daily budget back to `Cooldowns::unrecorded`.
    fn recorded(&mut self) {
        self.cooldowns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recorded();
        self.recorded = true;
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        // A panic elsewhere must not leak the slot (review I5).
        let mut cooldowns = self
            .cooldowns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A claim abandoned before its run was recorded (rate limit, error,
        // loop detection) must give its budget back too (issue #12).
        if !self.recorded {
            cooldowns.recorded();
        }
        cooldowns.finish(&self.model_id);
    }
}

/// What `Pipeline::claim` found.
pub(crate) enum Claim<'a> {
    Run(Box<ModelEntry>, Slot<'a>),
    /// No model is free (cooling down or at `max_concurrency`).
    Busy,
    /// The daily worker budget is spent (D53, issue #12).
    OverBudget,
}

/// How a worker stage ended, as the pipeline sees it.
pub(crate) enum Outcome {
    Finished(StageResult),
    /// The loop detector stopped it (spec §4.3).
    Looping(f64),
}

/// The issue as it was when the task was classified; later stages work from this copy.
fn issue_snapshot(v: &Value) -> (String, String) {
    (
        v["title"].as_str().unwrap_or_default().to_string(),
        v["body"].as_str().unwrap_or_default().to_string(),
    )
}

/// The pass number of a task: 1, plus one per reopen.
pub(crate) fn pass_of(task: &TaskRow) -> u32 {
    task.reopen_count + 1
}

fn tiers_json(t: &StageTiers) -> Value {
    json!({"plan": t.plan, "implement": t.implement, "review": t.review})
}

fn tier_of(tiers: &Value, stage: Stage) -> Option<Tier> {
    let key = match stage {
        Stage::Plan => "plan",
        Stage::Implement => "implement",
        Stage::Review => "review",
    };
    serde_json::from_value(tiers.get(key)?.clone()).ok()
}

fn tier_name(t: Tier) -> &'static str {
    match t {
        Tier::Fast => "fast",
        Tier::Standard => "standard",
        Tier::Frontier => "frontier",
    }
}

/// The exit reason without any tool output: safe to post on GitHub (review I7).
pub(crate) fn exit_kind(e: &ExitReason) -> &'static str {
    match e {
        ExitReason::Completed => "completed",
        ExitReason::MaxTurns => "max turns reached",
        ExitReason::Timeout => "timed out",
        ExitReason::RateLimited(_) => "rate limited",
        ExitReason::ProviderError(_) => "provider error",
        ExitReason::Crashed { .. } => "crashed",
    }
}

fn exit_name(e: &ExitReason) -> String {
    match e {
        ExitReason::Completed => "completed".into(),
        ExitReason::MaxTurns => "max_turns".into(),
        ExitReason::Timeout => "timeout".into(),
        ExitReason::RateLimited(m) => format!("rate_limited: {m}"),
        ExitReason::ProviderError(m) => format!("provider_error: {m}"),
        ExitReason::Crashed { code, stderr_tail } => {
            format!("crashed ({code:?}): {stderr_tail}")
        }
    }
}

fn plan_text(p: &PlanOutput) -> String {
    let mut s = format!("{}\n\nSteps:\n", p.summary);
    for (i, step) in p.steps.iter().enumerate() {
        s.push_str(&format!("{}. {step}\n", i + 1));
    }
    if !p.files.is_empty() {
        s.push_str(&format!("\nFiles: {}\n", p.files.join(", ")));
    }
    if !p.risks.is_empty() {
        s.push_str(&format!("\nRisks: {}\n", p.risks.join("; ")));
    }
    if let Some(r) = &p.repro_command {
        s.push_str(&format!(
            "\nReproduction command (must fail before the fix and pass after): {r}\n"
        ));
    }
    s
}

/// One line per finding, keyed when `keys` matches them one to one. A review
/// approved before the record existed has no `review` event, so no keys.
fn findings_text(r: &ReviewOutput, keys: &[String]) -> String {
    let keyed = keys.len() == r.findings.len();
    r.findings
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let at = f.line.map(|l| format!(":{l}")).unwrap_or_default();
            let sev = match f.severity {
                Severity::Blocking => "blocking",
                Severity::Minor => "minor",
            };
            let key = if keyed {
                format!("{} · ", keys[i])
            } else {
                String::new()
            };
            let rule = f
                .rule
                .as_deref()
                .map(|r| format!("{r} · "))
                .unwrap_or_default();
            format!("- {key}{rule}{sev} · `{}{at}` · {}", f.file, f.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The PR body's "Risk" section, `None` when nothing was detected: one line
/// per category with its paths, the checks it added and the reviewer tier.
/// `frontier_ok`: `risk::risky_review_tier` found a frontier reviewer from
/// another provider, so a category wanting frontier got one. `ran`: the tier
/// of the model that ran the round's review, which another rule (Jev's
/// review risk, a second approver) may have made frontier anyway; every
/// line states it, and a category wanting frontier keeps the reason the risk
/// rule could not pick one. `None` (no review run, a model not in the
/// catalog) falls back to what the risk rule decided.
/// The detected categories' checks that are not already gates, each with the
/// categories that asked for it: what the `risk-gates` pass runs.
fn risk_checks(
    policy: &risk::Policy,
    detected: &[Detected],
    gates: &[String],
) -> Vec<(String, Vec<String>)> {
    policy
        .checks(detected)
        .into_iter()
        .filter(|c| !gates.contains(c))
        .map(|c| {
            let names = policy
                .categories
                .iter()
                .filter(|cat| {
                    cat.checks.contains(&c) && detected.iter().any(|d| d.name == cat.name)
                })
                .map(|cat| cat.name.clone())
                .collect();
            (c, names)
        })
        .collect()
}

/// A failure's text for the task state: the detail is left out when the
/// public text already ends with it.
fn reason_text(public: &str, detail: &str) -> String {
    if detail.is_empty() || public.ends_with(detail) {
        public.to_string()
    } else {
        format!("{public}: {detail}")
    }
}

fn risk_section(
    policy: &risk::Policy,
    detected: &[Detected],
    frontier_ok: bool,
    ran: Option<Tier>,
) -> Option<String> {
    const SHOWN: usize = 5;
    const NO_FRONTIER: &str = " (no frontier reviewer from another provider is configured)";
    if detected.is_empty() {
        return None;
    }
    let (wanted, other) = match ran {
        Some(t) => {
            let reason = if frontier_ok { "" } else { NO_FRONTIER };
            (format!("{}{reason}", tier_name(t)), tier_name(t))
        }
        None if frontier_ok => ("frontier".to_string(), "standard"),
        None => (format!("standard{NO_FRONTIER}"), "standard"),
    };
    let mut s = String::from("## Risk\n\n");
    for d in detected {
        if d.name == risk::UNKNOWN {
            s.push_str(&format!(
                "- unknown: the changed files could not be computed · reviewer: {wanted}\n"
            ));
            continue;
        }
        let cat = policy.categories.iter().find(|c| c.name == d.name);
        let mut paths: Vec<String> = d
            .paths
            .iter()
            .take(SHOWN)
            .map(|p| format!("`{p}`"))
            .collect();
        if d.paths.len() > SHOWN {
            paths.push(format!("and {} more", d.paths.len() - SHOWN));
        }
        s.push_str(&format!("- {}: {}", d.name, paths.join(", ")));
        if let Some(c) = cat.filter(|c| !c.checks.is_empty()) {
            let checks: Vec<String> = c.checks.iter().map(|k| format!("`{k}`")).collect();
            s.push_str(&format!(" · checks added: {}", checks.join(", ")));
        }
        let tier = if cat.is_none_or(|c| c.frontier) {
            wanted.as_str()
        } else {
            other
        };
        s.push_str(&format!(" · reviewer: {tier}\n"));
    }
    s.push('\n');
    Some(s)
}

/// The PR body's "Review notes" section, `None` without findings: every
/// keyed finding of the pass's last review round (both approvers' with two),
/// else the review's own findings without keys (approved before the record
/// existed), with no command help since nothing could be named.
fn review_notes(review: &ReviewOutput, final_round: &[FindingRow]) -> Option<String> {
    let Some(first) = final_round.first() else {
        if review.findings.is_empty() {
            return None;
        }
        return Some(format!(
            "\n## Review notes\n\n{}\n",
            findings_text(review, &[])
        ));
    };
    let lines = final_round
        .iter()
        .map(|f| {
            let at = f.line.map(|l| format!(":{l}")).unwrap_or_default();
            let rule = f
                .rule
                .as_deref()
                .map(|r| format!("{r} · "))
                .unwrap_or_default();
            format!(
                "- {} · {rule}{} · `{}{at}` · {} ({})",
                f.key, f.severity, f.file, f.text, f.reviewer_model
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "\n## Review notes\n\n{lines}\n\nReply `/provefab {} rejected` (or accepted, fixed, waived), optionally followed by a reason, to record what you decided.\n",
        first.key
    ))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}\n[... diff truncated ...]")
}

/// The plan's reproduction command, if it has a usable one.
/// A command that failed because it could not run (not found, not
/// executable), not because of what it checks (review I5).
pub fn could_not_run(exit: Option<i32>, output: &str) -> bool {
    matches!(exit, Some(126 | 127))
        || output.contains("command not found")
        || output.contains("No such file or directory")
        || output.contains("Permission denied")
}

/// What a new pass carries besides a fresh branch.
#[derive(Debug, Default)]
pub struct PassOptions {
    /// Started by Provefab itself: counts against `max_auto_passes` (D53).
    pub automatic: bool,
    /// Plan and implement on the frontier tier for this pass (D50).
    pub boost: bool,
    /// Blocking findings from outside a review stage, such as a person's
    /// comments on a closed PR (D52).
    pub findings: Vec<Finding>,
}

/// Starts a new pass on a parked task (D46, D50, D52): removes the worktree,
/// counts the pass (its branch gets `-r<pass>`), keeps every finding, resets
/// the counters, requeues, and puts the issue label back. `provefab add` and
/// the pipeline both go through here.
#[allow(clippy::too_many_arguments)]
pub async fn start_pass<H: Hub>(
    store: &Store,
    hub: &H,
    git: &Git,
    paths: &Paths,
    repo: &RepoConfig,
    task: &TaskRow,
    reason: &str,
    opts: PassOptions,
) -> Result<(), StoreError> {
    let wt = paths.worktree(task.id);
    if wt.exists()
        && let Err(e) = git.worktree_remove(&repo.path_in(&paths.home), &wt).await
    {
        eprintln!("provefab: could not remove {}: {e}", wt.display());
    }
    if !opts.findings.is_empty() {
        let review = ReviewOutput {
            verdict: ReviewVerdict::Changes,
            findings: opts.findings,
        };
        store
            .record_output(
                task.id,
                "review",
                &serde_json::to_value(&review).unwrap_or(Value::Null),
            )
            .await?;
    }
    let pass = store.record_reopen(task.id).await? + 1;
    if opts.automatic {
        store
            .record_output(
                task.id,
                "auto_pass",
                &json!({"pass": pass, "reason": reason}),
            )
            .await?;
    }
    if opts.boost {
        store
            .record_output(task.id, "boost", &json!({"pass": pass}))
            .await?;
    }
    store.reset_counters(task.id).await?;
    store.transition(task.id, TaskState::Queued, reason).await?;
    let others = [
        format!("{}:failed", repo.label),
        format!("{}:needs-info", repo.label),
        format!("{}:in-pr", repo.label),
    ];
    let others: Vec<&str> = others.iter().map(String::as_str).collect();
    if let Some(why) = crate::tracker::key_mismatch(task.issue_key.as_deref(), Some(repo)) {
        eprintln!(
            "provefab: task {}: not relabelling its issue: {why}",
            task.id
        );
        return Ok(());
    }
    github(
        store,
        hub,
        task.id,
        json!({
            "op": "labels",
            "slug": repo.slug,
            "number": task.issue_number,
            "add": [&repo.label],
            "remove": others,
        }),
    )
    .await?;
    Ok(())
}

/// The task's branch: `provefab/<issue>-<slug>`, then `-r2`, `-r3`... for each
/// pass after a requeue, so a new pass never builds on (or force-pushes over)
/// the rejected one (D46).
fn task_branch(task: &TaskRow) -> String {
    let id = task
        .issue_key
        .clone()
        .unwrap_or_else(|| task.issue_number.to_string());
    let base = branch_name(&id, &task.title);
    if task.reopen_count == 0 {
        base
    } else {
        format!("{base}-r{}", task.reopen_count + 1)
    }
}

/// Applies a tracker side effect (spec §3.2): `{"op":"comment",...}` posts a
/// comment, `{"op":"labels",...}` edits labels. What `github` records and
/// `Pipeline::retry_pending` replays.
async fn apply_effect<H: Hub>(hub: &H, effect: &Value) -> Result<(), ForgeError> {
    let slug = effect["slug"].as_str().unwrap_or_default();
    let number = effect["number"].as_u64().unwrap_or_default();
    if effect["op"].as_str() == Some("comment") {
        return hub
            .comment(slug, number, effect["body"].as_str().unwrap_or_default())
            .await;
    }
    let strs = |key: &str| -> Vec<String> {
        effect[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let add = strs("add");
    let remove = strs("remove");
    let add: Vec<&str> = add.iter().map(String::as_str).collect();
    let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
    hub.edit_labels(slug, number, &add, &remove).await
}

/// Applies a tracker effect now, or stores it for the scheduler to retry
/// (spec §3.2): a task with effects already waiting queues behind them, so a
/// newer label change can never overwrite an older one still to be retried.
/// A failure is logged, never the error text (review I7): gh and git errors
/// can hold tokens.
async fn github<H: Hub>(
    store: &Store,
    hub: &H,
    task_id: i64,
    effect: Value,
) -> Result<(), StoreError> {
    if store.count_outputs(task_id, "pending_github").await? > 0 {
        return store
            .record_output(task_id, "pending_github", &effect)
            .await;
    }
    if let Err(e) = apply_effect(hub, &effect).await {
        eprintln!("provefab: could not apply a tracker effect for task {task_id}: {e}");
        store
            .record_output(task_id, "pending_github", &effect)
            .await?;
    }
    Ok(())
}

fn repro(plan: &PlanOutput) -> Option<String> {
    plan.repro_command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

impl<R, O, H> Pipeline<R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    pub(crate) fn repo(&self, task: &TaskRow) -> Option<&RepoConfig> {
        self.config
            .repos
            .iter()
            .find(|r| r.slug.eq_ignore_ascii_case(&task.repo))
    }

    pub(crate) async fn task(&self, id: i64) -> Result<TaskRow, PipelineError> {
        self.store
            .task(id)
            .await?
            .ok_or(PipelineError::UnknownTask(id))
    }

    /// Moves the task out of its current state once and returns the new state.
    /// Terminal states and `NeedsInfo` without an answering reply stay where they are.
    pub async fn step(&self, id: i64) -> Result<TaskState, PipelineError> {
        let task = self.task(id).await?;
        let Some(repo) = self.repo(&task).cloned() else {
            return self
                .give_up(
                    &task,
                    None,
                    TaskState::Failed,
                    "the repo is not in provefab.toml",
                    "",
                )
                .await;
        };
        match task.state {
            TaskState::Queued => self.classify(&task, &repo).await,
            TaskState::Classified => self.prepare(&task, &repo).await,
            TaskState::Planning => self.plan(&task, &repo).await,
            TaskState::Implementing => self.implement(&task, &repo).await,
            TaskState::Gating => self.gate(&task, &repo).await,
            TaskState::Reviewing => self.review(&task, &repo).await,
            TaskState::Waiting => self.resume_waiting(&task).await,
            TaskState::NeedsInfo => self.check_replies(&task, &repo).await,
            s @ (TaskState::PrOpen | TaskState::NeedsYou | TaskState::Failed) => Ok(s),
        }
    }

    /// Steps the task until it reaches a state that waits on something outside the
    /// provefab: a terminal state, `NeedsInfo` or `Waiting`.
    pub async fn drive(&self, id: i64) -> Result<TaskState, PipelineError> {
        // Every path through the pipeline ends, but an unattended provefab must
        // not rely on that alone (D53).
        for _ in 0..self.config.limits.max_drive_steps {
            let state = self.step(id).await?;
            if state.is_terminal() || matches!(state, TaskState::NeedsInfo | TaskState::Waiting) {
                return Ok(state);
            }
        }
        self.park(
            id,
            &format!(
                "the task used its step budget ({} steps) without finishing",
                self.config.limits.max_drive_steps
            ),
            "",
        )
        .await
    }

    /// The rolling 24-hour worker budget is spent (D53): park the task in
    /// `Waiting`. The issue hears about it once per day.
    async fn over_budget(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<TaskState, PipelineError> {
        let cap = self.config.limits.max_stage_runs_per_day;
        let told = self
            .store
            .last_output(task.id, "budget_notice")
            .await?
            .and_then(|n| n["at"].as_i64())
            .is_some_and(|at| at > now() - 86_400);
        self.go(
            task.id,
            TaskState::Waiting,
            &format!("daily worker budget reached ({cap} runs in 24 hours)"),
        )
        .await?;
        if !told {
            self.store
                .record_output(task.id, "budget_notice", &json!({"at": now()}))
                .await?;
            self.tell(
                task.id,
                &repo.slug,
                task.issue_number,
                &format!(
                    "Provefab reached its daily worker budget ({cap} runs in 24 hours, `max_stage_runs_per_day`). This issue continues automatically when the budget allows."
                ),
            )
            .await?;
        }
        Ok(TaskState::Waiting)
    }

    async fn go(&self, id: i64, to: TaskState, reason: &str) -> Result<TaskState, PipelineError> {
        self.store.transition(id, to, reason).await?;
        Ok(to)
    }

    /// Parks a task for a person from outside a step (the scheduler, after a panic).
    pub async fn park(
        &self,
        id: i64,
        public: &str,
        detail: &str,
    ) -> Result<TaskState, PipelineError> {
        let task = self.task(id).await?;
        let repo = self.repo(&task).cloned();
        self.give_up(&task, repo.as_ref(), TaskState::NeedsYou, public, detail)
            .await
    }

    /// `NeedsYou` or `Failed`: record why, then tell the issue and label it.
    /// `public` goes on GitHub and must never carry tool output (stderr, git or
    /// gh errors can hold tokens: review I7); `detail` stays in the local log.
    async fn give_up(
        &self,
        task: &TaskRow,
        repo: Option<&RepoConfig>,
        to: TaskState,
        public: &str,
        detail: &str,
    ) -> Result<TaskState, PipelineError> {
        let reason = reason_text(public, detail);
        self.store.transition(task.id, to, &reason).await?;
        let what = if to == TaskState::NeedsYou {
            "needs a person to continue"
        } else {
            "gave up on this issue"
        };
        let body = format!(
            "Provefab {what}.\n\nReason: {public}\n\nDetails: `provefab log {}`",
            task.id
        );
        self.tell(task.id, &task.repo, task.issue_number, &body)
            .await?;
        if let Some(repo) = repo {
            let failed = format!("{}:failed", repo.label);
            self.relabel(
                task.id,
                &task.repo,
                task.issue_number,
                &[&failed],
                &[&repo.label],
            )
            .await?;
        }
        Ok(to)
    }

    /// Whether the task's issue no longer fits its repository's tracker (a
    /// ticket whose repository left `provefab.toml` or moved to GitHub, and
    /// the reverse): then nothing is written to it, with one log line, and
    /// the state change stays local (final review I1).
    async fn misrouted(&self, id: i64, what: &str) -> Result<bool, PipelineError> {
        let task = self.task(id).await?;
        let Some(why) = crate::tracker::key_mismatch(task.issue_key.as_deref(), self.repo(&task))
        else {
            return Ok(false);
        };
        eprintln!("provefab: task {id}: not {what} its issue: {why}");
        Ok(true)
    }

    /// Tracker side effects after the state is already recorded: a failure to
    /// reach the tracker is never fatal (the state in the store is the truth),
    /// but the effect is stored so the scheduler retries it (spec §3.2).
    pub(crate) async fn tell(
        &self,
        id: i64,
        slug: &str,
        number: u64,
        body: &str,
    ) -> Result<(), PipelineError> {
        if self.misrouted(id, "commenting on").await? {
            return Ok(());
        }
        github(
            &self.store,
            &self.hub,
            id,
            json!({"op": "comment", "slug": slug, "number": number, "body": body}),
        )
        .await?;
        Ok(())
    }

    async fn relabel(
        &self,
        id: i64,
        slug: &str,
        number: u64,
        add: &[&str],
        remove: &[&str],
    ) -> Result<(), PipelineError> {
        if self.misrouted(id, "relabelling").await? {
            return Ok(());
        }
        github(
            &self.store,
            &self.hub,
            id,
            json!({"op": "labels", "slug": slug, "number": number, "add": add, "remove": remove}),
        )
        .await?;
        Ok(())
    }

    /// Retries stored `pending_github` effects, oldest first (spec §3.2): a
    /// task still driven by a runner (`skip`) is left for the next poll, so
    /// its own effects are not reordered around this retry; once a task's
    /// retry fails, its later effects wait too, so posting order stays
    /// correct. Deletes each row that succeeds. Store errors are logged, not
    /// propagated: one bad read must not end `provefab run` (review I4).
    pub async fn retry_pending(&self, skip: &HashSet<i64>) {
        let pending = match self.store.pending_github().await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("provefab: could not read pending tracker effects: {e}");
                return;
            }
        };
        let mut failed: HashSet<i64> = HashSet::new();
        for (row_id, task_id, effect) in pending {
            if skip.contains(&task_id) || failed.contains(&task_id) {
                continue;
            }
            // Kept, not replayed: restoring the repository's tracker sends it
            // to the right issue (final review I1).
            let why = match self.store.task(task_id).await {
                Ok(task) => task.and_then(|t| {
                    crate::tracker::key_mismatch(t.issue_key.as_deref(), self.repo(&t))
                }),
                Err(e) => {
                    eprintln!("provefab: could not read task {task_id}: {e}");
                    failed.insert(task_id);
                    continue;
                }
            };
            if let Some(why) = why {
                eprintln!("provefab: task {task_id}: not replaying a tracker effect: {why}");
                failed.insert(task_id);
                continue;
            }
            match apply_effect(&self.hub, &effect).await {
                Ok(()) => {
                    if let Err(e) = self.store.delete_output(row_id).await {
                        eprintln!("provefab: could not clear a retried tracker effect: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("provefab: retry of a tracker effect for task {task_id} failed: {e}");
                    failed.insert(task_id);
                }
            }
        }
    }

    // ---- Queued: classify and route (spec §4.1, §4.2) ----

    async fn classify(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<TaskState, PipelineError> {
        let issue = match self.hub.issue(&repo.slug, task.issue_number).await {
            Ok(i) => i,
            Err(e) => {
                return self
                    .failed(task, repo, "could not read the issue", &e)
                    .await;
            }
        };
        let mut body = issue.body.clone();
        if let Some(c) = self.store.last_output(task.id, "clarifications").await?
            && let Some(text) = c["text"].as_str()
        {
            body.push_str("\n\n## Clarifications from the issue thread\n\n");
            body.push_str(text);
        }
        self.store
            .record_output(
                task.id,
                "issue",
                &json!({"title": issue.title, "body": body, "labels": issue.labels}),
            )
            .await?;
        let ctx = IssueContext {
            title: issue.title.clone(),
            body,
            labels: issue.labels.clone(),
            repo_language: None,
            repo_size_kb: None,
        };
        let verdict = self.oracle.classify(&ctx).await;
        if let Some(v) = &verdict
            && v.underspecified > self.config.jev.underspecified_threshold
        {
            return self.ask_for_info(task, repo, v.underspecified).await;
        }
        let tiers = match &verdict {
            Some(v) => stage_tiers(v),
            None => fallback_tiers(),
        };
        let kind = verdict.as_ref().map_or(TaskKind::Feature, |v| v.task_kind);
        self.store.set_kind(task.id, kind).await?;
        let verdict_json = verdict
            .as_ref()
            .map(|v| serde_json::to_value(v).unwrap_or(Value::Null));
        let jev = verdict.as_ref().map(|v| v.jev_model.as_str());
        let tiers_value = tiers_json(&tiers);
        self.store
            .write_with_events(
                task.id,
                Write::Routing {
                    jev_model: jev,
                    verdict: verdict_json.as_ref(),
                    tiers: &tiers_value,
                    reasons: &tiers.reasons,
                },
                &[Event::Routed {
                    tiers: tiers_value.clone(),
                    jev_model: jev.map(str::to_string),
                    fallback: verdict.is_none(),
                }],
            )
            .await?;
        self.go(
            task.id,
            TaskState::Classified,
            &format!("classified as {}", kind.as_str()),
        )
        .await
    }

    async fn ask_for_info(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        p: f64,
    ) -> Result<TaskState, PipelineError> {
        let question = "Before starting, Provefab needs more detail on this issue. \
Please reply with what should happen, what happens instead, and how to reproduce it \
(or, for a new feature, the expected behaviour and where it belongs).";
        // The question, its time and the state land together, so comments older
        // than the question never count as answers, even if GitHub is unreachable
        // right after posting (review I2).
        let seen_at = rfc3339(now());
        self.store
            .transition_and(
                task.id,
                TaskState::NeedsInfo,
                &format!(
                    "underspecified {p:.2} > {}",
                    self.config.jev.underspecified_threshold
                ),
                Also::Question {
                    question: &json!({"text": question}),
                    seen_at: &seen_at,
                },
            )
            .await?;
        self.tell(task.id, &repo.slug, task.issue_number, question)
            .await?;
        let needs = format!("{}:needs-info", repo.label);
        self.relabel(
            task.id,
            &repo.slug,
            task.issue_number,
            &[&needs],
            &[&repo.label],
        )
        .await?;
        // Only replies posted after the question count (Plan 3a deferred minor).
        if let Ok(comments) = self.hub.comments(&repo.slug, task.issue_number).await
            && let Some(last) = comments.iter().map(|c| c.created_at.as_str()).max()
        {
            self.store.set_last_reply_seen(task.id, last).await?;
        }
        Ok(TaskState::NeedsInfo)
    }

    // ---- NeedsInfo: reply check (spec §4.5) ----

    async fn check_replies(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<TaskState, PipelineError> {
        let Ok(comments) = self.hub.comments(&repo.slug, task.issue_number).await else {
            return Ok(TaskState::NeedsInfo);
        };
        let since = self.store.last_reply_seen(task.id).await?;
        let replies = new_replies(&comments, &task.author, since.as_deref());
        let Some(newest) = replies.last() else {
            return Ok(TaskState::NeedsInfo);
        };
        self.store
            .set_last_reply_seen(task.id, &newest.created_at)
            .await?;
        let question = self
            .store
            .last_output(task.id, "question")
            .await?
            .and_then(|q| q["text"].as_str().map(str::to_string))
            .unwrap_or_default();
        let text = replies
            .iter()
            .map(|c| c.body.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        match self.oracle.reply_answers(&question, &text).await {
            Some(p) if p > REPLY_THRESHOLD => {
                // The answer joins the issue text every later stage works from (review I3).
                let earlier = self
                    .store
                    .last_output(task.id, "clarifications")
                    .await?
                    .and_then(|c| c["text"].as_str().map(str::to_string))
                    .unwrap_or_default();
                let all = if earlier.is_empty() {
                    text.clone()
                } else {
                    format!("{earlier}\n\n{text}")
                };
                self.store
                    .record_output(task.id, "clarifications", &json!({"text": all}))
                    .await?;
                self.store.reset_counters(task.id).await?;
                self.store
                    .transition(
                        task.id,
                        TaskState::Queued,
                        &format!("reply answers the question ({p:.2})"),
                    )
                    .await?;
                let needs = format!("{}:needs-info", repo.label);
                self.relabel(
                    task.id,
                    &repo.slug,
                    task.issue_number,
                    &[&repo.label],
                    &[&needs],
                )
                .await?;
                Ok(TaskState::Queued)
            }
            _ => Ok(TaskState::NeedsInfo),
        }
    }

    // ---- Classified: worktree and branch ----

    /// The checkout this repo's worktrees come from (D48).
    pub(crate) fn checkout(&self, repo: &RepoConfig) -> PathBuf {
        repo.path_in(&self.paths.home)
    }

    /// What the task branches from and diffs against: `origin/<base>` once
    /// fetched, else the local `<base>` (D48).
    pub(crate) async fn base_ref(&self, repo: &RepoConfig) -> String {
        self.git.base_ref(&self.checkout(repo), &repo.base).await
    }

    /// Serializes git calls that write to a repo's shared checkout (`fetch`,
    /// `worktree add`/`remove`): tasks in the same repo can run in parallel
    /// (`max_concurrency`), but git's per-repo administrative files are not
    /// safe for concurrent writers, so without this two tasks preparing at
    /// once can make `git worktree add` fail outright.
    pub(crate) fn repo_lock(&self, repo: &RepoConfig) -> Arc<AsyncMutex<()>> {
        self.repo_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(repo.slug.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    async fn ensure_worktree(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<PathBuf, ForgeError> {
        let branch = task_branch(task);
        let wt = self.paths.worktree(task.id);
        if let Some(parent) = wt.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ForgeError::Parse(parent.display().to_string(), e.to_string()))?;
        }
        let base = self
            .pass_base(task, repo)
            .await
            .map_err(|e| ForgeError::Parse("base of the pass".into(), e.to_string()))?;
        self.git
            .worktree_add(&self.checkout(repo), &wt, &branch, &base)
            .await?;
        Ok(wt)
    }

    /// A managed repo is cloned on first use; every repo is fetched before a
    /// new pass branches, so it starts from the latest base (D48). Returns
    /// whether the fetch succeeded.
    pub(crate) async fn refresh_checkout(&self, repo: &RepoConfig) -> Result<bool, ForgeError> {
        let checkout = self.checkout(repo);
        if repo.managed() && !checkout.join(".git").exists() {
            self.hub.repo_clone(&repo.slug, &checkout).await?;
        }
        let fetched = match self.git.fetch(&checkout).await {
            Ok(()) => true,
            Err(e) => {
                // Offline or no `origin`: branch from what is there, and say so.
                // The error can quote a remote URL with its credentials.
                eprintln!(
                    "provefab: could not fetch {}: {}",
                    repo.slug,
                    crate::rules::redact_credentials(&e.to_string())
                );
                false
            }
        };
        Ok(fetched)
    }

    /// Pins the pass's base to one commit sha, right after the fetch: `origin/<base>`
    /// when it succeeded (falling back to the local `<base>` if that ref is somehow
    /// missing), else the local `<base>` directly. Recorded as the `base` stage
    /// output so the rest of the pass reads the same commit even if another task's
    /// fetch later moves `origin/<base>` (issue #9, D48).
    async fn pin_base(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        fetched: bool,
    ) -> Result<String, String> {
        let checkout = self.checkout(repo);
        let the_ref = if fetched {
            self.base_ref(repo).await
        } else {
            repo.base.clone()
        };
        let sha = self
            .git
            .rev_parse(&checkout, &the_ref)
            .await
            .map_err(|_| "could not resolve the base branch".to_string())?;
        self.store
            .record_output(
                task.id,
                "base",
                &json!({
                    "pass": u64::from(task.reopen_count) + 1,
                    "sha": sha,
                    "ref": the_ref,
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(sha)
    }

    /// The commit sha pinned for the task's current pass (issue #9, D48):
    /// resolved once in `prepare`, right after the fetch, so the rest of the
    /// pass never re-resolves `origin/<base>` (which another task's fetch could
    /// have moved) or silently falls back to a stale ref after a failed fetch.
    /// Falls back to `base_ref` for a task started before this was recorded.
    pub(crate) async fn pass_base(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<String, PipelineError> {
        let pass = u64::from(task.reopen_count) + 1;
        if let Some(b) = self.store.last_output(task.id, "base").await?
            && b["pass"].as_u64() == Some(pass)
            && let Some(sha) = b["sha"].as_str()
        {
            return Ok(sha.to_string());
        }
        Ok(self.base_ref(repo).await)
    }

    async fn prepare(&self, task: &TaskRow, repo: &RepoConfig) -> Result<TaskState, PipelineError> {
        // One task at a time writes to the shared checkout (PR #31).
        let lock = self.repo_lock(repo);
        let _guard = lock.lock().await;
        let fetched = match self.refresh_checkout(repo).await {
            Ok(f) => f,
            Err(e) => {
                return self
                    .failed(task, repo, "could not clone the repository", &e)
                    .await;
            }
        };
        if let Err(e) = self.pin_base(task, repo, fetched).await {
            return self
                .transient(task, repo, "could not resolve the base branch", &e)
                .await;
        }
        // Repository rules spec §4: read once per pass, at the commit just pinned.
        self.pass_rules(task, repo).await?;
        let wt = match self.ensure_worktree(task, repo).await {
            Ok(wt) => wt,
            Err(e) => {
                return self
                    .failed(task, repo, "could not create the worktree", &e)
                    .await;
            }
        };
        let branch = task_branch(task);
        self.store.set_worktree(task.id, &branch, &wt).await?;
        self.enter(task.id, TaskState::Planning, "worktree ready")
            .await
    }

    /// Enters a stage with a fresh attempt counter and tier.
    async fn enter(
        &self,
        id: i64,
        to: TaskState,
        reason: &str,
    ) -> Result<TaskState, PipelineError> {
        // One transaction: counters and state move together (review I1).
        self.store
            .transition_and(id, to, reason, Also::NewStage)
            .await?;
        Ok(to)
    }

    // ---- model choice ----

    async fn tier_for(&self, task: &TaskRow, stage: Stage) -> Result<Tier, PipelineError> {
        // Escalation must land above the tier that actually ran, not above the
        // tier Jev requested: `select` silently resolves a requested tier
        // missing from the catalog to the nearest configured one, so every
        // bump here resolves first (issue #39).
        let resolve = |t: Tier| resolve_tier(t, &self.config.models).unwrap_or(t);
        let decisions = self.store.routing_decisions(task.id).await?;
        let tiers = decisions.last().map(|d| d.2.clone());
        let jev_tier = tiers
            .as_ref()
            .and_then(|t| tier_of(t, stage))
            .unwrap_or(Tier::Standard);
        // Jev's frontier review keeps the cross-provider rule of the risk
        // policy: no frontier reviewer from another provider, no frontier tier.
        let base = if self.jev_frontier_downgrade(task, stage).await?.is_some() {
            resolve(Tier::Standard)
        } else {
            resolve(jev_tier)
        };
        let mut tier = if task.escalated {
            resolve(base.up())
        } else {
            base
        };
        // The second correction round means the change is harder than classified:
        // implement one tier up from the tier that actually ran, from then on (D45).
        if stage == Stage::Implement && task.review_rounds >= 2 {
            tier = resolve(tier.up());
        }
        // A later reviewer, when the policy needs several approvals, is frontier (D49).
        if stage == Stage::Review
            && self
                .repo(task)
                .is_some_and(|r| self.policy.approvals_needed(r) > 1)
            && self.first_approval(task).await?.is_some()
        {
            tier = resolve(Tier::Frontier);
        }
        // A boosted pass plans and implements on the frontier tier (D50).
        if matches!(stage, Stage::Plan | Stage::Implement)
            && task.reopen_count > 0
            && let Some(b) = self.store.last_output(task.id, "boost").await?
            && b["pass"].as_u64() == Some(u64::from(task.reopen_count) + 1)
        {
            tier = resolve(Tier::Frontier);
        }
        // A risky change is reviewed on the frontier tier when a frontier model
        // from another provider than the implementer's exists; otherwise the
        // usual cross-provider reviewer stays (risk policy §7, owner decision
        // 2026-09-30, option 1).
        if stage == Stage::Review
            && let Some(detected) = self.risk_of_round(task).await?
            && let Some(repo) = self.repo(task)
            && let Some(t) = risk::risky_review_tier(
                &self.config.models,
                self.implementer_provider(task.id).await?.as_deref(),
                risk::resolve(repo.risk.as_ref())
                    .map(|p| p.needs_frontier(&detected))
                    .unwrap_or(true),
            )
        {
            tier = resolve(t);
        }
        Ok(tier)
    }

    /// Follows a task whose PR Provefab opened (D52), once per poll:
    /// - merged: record it and when, and remove the worktree;
    /// - closed with comments, reviews or inline comments from the issue author
    ///   or a collaborator: they become blocking findings of a new pass;
    /// - closed without any: the user said stop; the task ends `Failed`;
    /// - merged, the issue then seen closed, then open again: a new pass. An
    ///   issue that merely stays open after the merge (a non-default base, an
    ///   edited "Closes") is not a reopen (Plan 4 review I1);
    /// - two weeks after the merge the task is archived and costs no more
    ///   GitHub calls (Plan 4 review I2).
    pub async fn watch_pr(&self, id: i64) -> Result<TaskState, PipelineError> {
        let task = self.task(id).await?;
        let (Some(repo), Some(url)) = (self.repo(&task).cloned(), task.pr_url.clone()) else {
            return Ok(task.state);
        };
        if task.state != TaskState::PrOpen {
            return Ok(task.state);
        }
        match task.pr_state.as_deref() {
            Some("open") => {}
            Some(s @ ("merged" | "done")) => {
                let s = s.to_string();
                return self.watch_merged(&task, &repo, &s).await;
            }
            _ => return Ok(task.state),
        }
        let status = match self.hub.pr_status(&repo.slug, &url).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("provefab: could not read {url}: {e}");
                return Ok(task.state);
            }
        };
        match status.state {
            PrState::Open => {
                self.apply_finding_commands(&task, &status.comments).await?;
                Ok(task.state)
            }
            PrState::Merged => {
                // Before the merge is recorded: a command posted between the
                // last poll and the merge must not be inferred "unaddressed".
                self.apply_finding_commands(&task, &status.comments).await?;
                self.record_merge(
                    &task,
                    &repo,
                    status.merge_sha.as_deref(),
                    status.base_ref.as_deref(),
                    status.commit_count,
                    status.head_sha.as_deref(),
                )
                .await?;
                Ok(task.state)
            }
            PrState::Closed => {
                self.apply_finding_commands(&task, &status.comments).await?;
                // Only the issue author, and repository owners, organization members and collaborators, steer the
                // provefab (the label rule, spec §3.3); never its own comments.
                let findings: Vec<Finding> = status
                    .comments
                    .iter()
                    .filter(|c| !is_bot_comment(&c.body))
                    .filter(|c| {
                        c.author == task.author
                            || matches!(c.association.as_str(), "OWNER" | "MEMBER" | "COLLABORATOR")
                    })
                    // Command lines are dispositions, not change requests.
                    .map(|c| (c, crate::record::strip_commands(&c.body)))
                    .filter(|(_, body)| !body.is_empty())
                    .map(|(c, body)| Finding {
                        file: PR_COMMENT_FILE.into(),
                        line: None,
                        severity: Severity::Blocking,
                        text: format!("{} wrote: {}", c.author, body),
                        rule: None,
                    })
                    .collect();
                // The state change comes first; `pr_state` only after it, so a
                // crash in between never strands the task (Plan 4 review I4).
                let state = if findings.is_empty() {
                    self.give_up(
                        &task,
                        Some(&repo),
                        TaskState::Failed,
                        "the pull request was closed without merging and without a comment from the issue author or a collaborator, so Provefab stops here (`provefab add` starts it again)",
                        "",
                    )
                    .await?
                } else {
                    self.auto_pass_with(
                        &task,
                        &repo,
                        "The pull request was closed without merging; its comments become findings for the next pass.",
                        false,
                        findings,
                    )
                    .await?
                };
                self.store.set_pr_state(task.id, "closed").await?;
                Ok(state)
            }
        }
    }

    /// A merged PR: relabel the issue `<label>:merged`, record the merge and
    /// when, and free the worktree. A failed label edit is kept as a pending
    /// GitHub effect and retried by the scheduler (PR #25), so it never blocks
    /// recording the merge. `head` is the merged PR's head commit: the
    /// `auto_merged` and `merge_seen` outputs count only for that PR, never for
    /// an earlier PR of the same task (final review F1).
    async fn record_merge(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        merge_sha: Option<&str>,
        base: Option<&str>,
        commit_count: Option<usize>,
        head: Option<&str>,
    ) -> Result<(), PipelineError> {
        if !repo.post_merge_checks.is_empty()
            && (merge_sha.is_none() || base.is_none())
            && base.is_none_or(|b| b == repo.base)
        {
            // GitHub may report `mergeCommit` a little after the merge: keep the PR
            // watched, but never longer than an hour (spec section 4).
            let seen = self
                .store
                .last_output(task.id, "merge_seen")
                .await?
                .filter(|v| v["pr"].as_str() == task.pr_url.as_deref());
            let first = match seen {
                Some(v) => v["at"].as_i64().unwrap_or(0),
                None => {
                    self.store
                        .record_output(
                            task.id,
                            "merge_seen",
                            &json!({"at": now(), "pr": task.pr_url}),
                        )
                        .await?;
                    now()
                }
            };
            if now() - first < crate::post_merge::ATTRIBUTION_WAIT_SECS {
                return Ok(());
            }
        }
        let merged = format!("{}:merged", repo.label);
        let in_pr = format!("{}:in-pr", repo.label);
        self.relabel(
            task.id,
            &repo.slug,
            task.issue_number,
            &[&merged],
            &[&in_pr],
        )
        .await?;
        let auto_merged = match (self.store.last_output(task.id, "auto_merged").await?, head) {
            (Some(v), Some(h)) => v["head"].as_str() == Some(h),
            _ => false,
        };
        if !repo.post_merge_checks.is_empty() {
            match (merge_sha, base) {
                (Some(sha), Some(b)) if b == repo.base => {
                    self.store
                        .ensure_post_merge_check(&crate::store::NewPostMergeCheck {
                            task_id: task.id,
                            merge_sha: sha,
                            base: b,
                            pr_url: task.pr_url.as_deref(),
                            commit_count,
                            auto_merged,
                        })
                        .await?;
                }
                // Merged into another branch: not what the checks describe.
                (_, Some(b)) if b != repo.base => {}
                _ => {
                    let row = self
                        .store
                        .ensure_post_merge_check(&crate::store::NewPostMergeCheck {
                            task_id: task.id,
                            merge_sha: merge_sha.unwrap_or("unknown"),
                            base: base.unwrap_or(&repo.base),
                            pr_url: task.pr_url.as_deref(),
                            commit_count,
                            auto_merged,
                        })
                        .await?;
                    self.store
                        .advance_post_merge(
                            row.id,
                            crate::post_merge::CheckState::Queued,
                            crate::post_merge::CheckState::Blocked,
                            &crate::store::CheckPatch {
                                failure_kind: Some(crate::post_merge::FailureKind::AttributionMissing),
                                failure_summary: Some(format!(
                                    "GitHub reported merge commit {merge_sha:?} and base {base:?} for over an hour"
                                )),
                                ..Default::default()
                            },
                        )
                        .await?;
                }
            }
        }
        self.store
            .record_output(
                task.id,
                "merged_at",
                &json!({"at": now(), "sha": merge_sha, "base": base}),
            )
            .await?;
        self.store
            .write_with_inference(
                task.id,
                Write::SetPrState("merged"),
                &[Event::Merged {
                    sha: merge_sha.map(str::to_string),
                    base: base.map(str::to_string),
                    by: if auto_merged {
                        MergedBy::Auto
                    } else {
                        MergedBy::Human
                    },
                    pass: pass_of(task),
                }],
                Some((Rule::UnaddressedAtMerge, pass_of(task))),
            )
            .await?;
        let wt = self.paths.worktree(task.id);
        if wt.exists() {
            let lock = self.repo_lock(repo);
            let _guard = lock.lock().await;
            if let Err(e) = self.git.worktree_remove(&self.checkout(repo), &wt).await {
                eprintln!("provefab: could not remove {}: {e}", wt.display());
            }
        }
        Ok(())
    }

    /// After a merge: `merged` becomes `done` once the issue is seen closed;
    /// only a `done` issue that is open again is a reopen (I1). Archived after
    /// two weeks (I2).
    async fn watch_merged(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        pr_state: &str,
    ) -> Result<TaskState, PipelineError> {
        const WATCH_FOR: i64 = 14 * 86_400;
        let merged_at = self
            .store
            .last_output(task.id, "merged_at")
            .await?
            .and_then(|m| m["at"].as_i64())
            .unwrap_or_else(now);
        if now() - merged_at > WATCH_FOR {
            self.store.set_pr_state(task.id, "archived").await?;
            return Ok(task.state);
        }
        if let Some(url) = &task.pr_url {
            match self.hub.pr_status(&repo.slug, url).await {
                Ok(status) => self.apply_finding_commands(task, &status.comments).await?,
                Err(e) => eprintln!("provefab: could not read {url}: {e}"),
            }
        }
        match self.hub.issue_open(&repo.slug, task.issue_number).await {
            Ok(false) if pr_state == "merged" => {
                self.store.set_pr_state(task.id, "done").await?;
                Ok(task.state)
            }
            Ok(true) if pr_state == "done" => {
                let previous = pass_of(task);
                let state = self
                    .auto_pass(
                        task,
                        repo,
                        "The issue was reopened after its pull request was merged.",
                        false,
                    )
                    .await?;
                self.store
                    .write_with_inference(
                        task.id,
                        Write::SetPrState("reopened"),
                        &[Event::IssueReopened {
                            previous_pass: previous,
                        }],
                        Some((Rule::FollowedByReopen, previous)),
                    )
                    .await?;
                Ok(state)
            }
            Ok(_) => Ok(task.state),
            Err(e) => {
                // `repo.slug` as configured, as before (GitHub output unchanged).
                eprintln!(
                    "provefab: could not read {}: {e}",
                    crate::tracker::repo_ref(
                        &repo.slug,
                        task.issue_number,
                        task.issue_key.as_deref()
                    )
                );
                Ok(task.state)
            }
        }
    }

    /// A failure that may pass by itself (network, GitHub, a busy repo): wait
    /// `retry_delays[n]` in `Waiting`, then retry from the same state; after the
    /// last delay, the user is needed (D51). The resume time is stored, so a
    /// restart keeps it.
    async fn transient(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        public: &str,
        detail: &str,
    ) -> Result<TaskState, PipelineError> {
        let pass = u64::from(task.reopen_count) + 1;
        let earlier = self
            .store
            .recent_outputs(task.id, "transient", u32::MAX)
            .await?
            .into_iter()
            .filter(|t| t["pass"].as_u64() == Some(pass))
            .count();
        let Some(delay) = self.config.limits.retry_delays.get(earlier).copied() else {
            return self
                .give_up(
                    task,
                    Some(repo),
                    TaskState::NeedsYou,
                    &format!("{public} (still failing after {earlier} retries)"),
                    detail,
                )
                .await;
        };
        let at = now() + delay.as_secs() as i64;
        self.store
            .record_output(
                task.id,
                "transient",
                &json!({"pass": pass, "at": at, "reason": public}),
            )
            .await?;
        self.go(
            task.id,
            TaskState::Waiting,
            &format!("{public}; retry in {} min: {detail}", delay.as_secs() / 60),
        )
        .await
    }

    /// A git or gh failure: one that waiting cannot clear (a missing issue or
    /// repository, a worktree on the wrong branch) needs the user now; anything
    /// else is transient (D51).
    async fn failed(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        public: &str,
        err: &ForgeError,
    ) -> Result<TaskState, PipelineError> {
        if err.is_permanent() {
            self.give_up(
                task,
                Some(repo),
                TaskState::NeedsYou,
                public,
                &err.to_string(),
            )
            .await
        } else {
            self.transient(task, repo, public, &err.to_string()).await
        }
    }

    /// Starts an automatic pass (D50, D52) within the pass budget (D53); past
    /// it, the user is needed.
    pub async fn auto_pass(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        why: &str,
        boost: bool,
    ) -> Result<TaskState, PipelineError> {
        self.auto_pass_with(task, repo, why, boost, Vec::new())
            .await
    }

    async fn auto_pass_with(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        why: &str,
        boost: bool,
        findings: Vec<Finding>,
    ) -> Result<TaskState, PipelineError> {
        let used = self.store.count_outputs(task.id, "auto_pass").await?;
        let budget = self.config.limits.max_auto_passes;
        if used >= budget {
            return self
                .give_up(
                    task,
                    Some(repo),
                    TaskState::NeedsYou,
                    &format!("{why}\n\nThe {budget} automatic passes for this issue are used up."),
                    "",
                )
                .await;
        }
        start_pass(
            &self.store,
            &self.hub,
            &self.git,
            &self.paths,
            repo,
            task,
            &format!("automatic pass {} of {budget}", used + 1),
            PassOptions {
                automatic: true,
                boost,
                findings,
            },
        )
        .await?;
        self.tell(
            task.id,
            &repo.slug,
            task.issue_number,
            &format!(
                "{why}\n\nProvefab starts a new pass from a fresh branch (automatic pass {} of {budget}), keeping these findings{}.",
                used + 1,
                if boost { ", on its strongest models" } else { "" }
            ),
        )
        .await?;
        Ok(TaskState::Queued)
    }

    /// The routing note when Jev asked for a frontier review (`review_risk >=
    /// 3.0`) that the catalog cannot give from another provider than the
    /// implementer's: the review then uses the standard tier. Reads only. With
    /// no implement run yet the implementer is unknown and the tier is kept.
    async fn jev_frontier_downgrade(
        &self,
        task: &TaskRow,
        stage: Stage,
    ) -> Result<Option<String>, PipelineError> {
        if stage != Stage::Review {
            return Ok(None);
        }
        let decisions = self.store.routing_decisions(task.id).await?;
        let Some((_, verdict, tiers, _)) = decisions.last() else {
            return Ok(None);
        };
        let Some(r) = verdict
            .as_ref()
            .and_then(|v| v["review_risk"].as_f64())
            .filter(|r| *r >= 3.0)
        else {
            return Ok(None);
        };
        if tier_of(tiers, stage) != Some(Tier::Frontier) {
            return Ok(None);
        }
        let implementer = self.implementer_provider(task.id).await?;
        Ok(
            risk::risky_review_tier(&self.config.models, implementer.as_deref(), true)
                .is_none()
                .then(|| {
                    format!(
                        "review_risk {r:.2} >= 3.0 -> review Frontier, no frontier reviewer from another provider -> Standard"
                    )
                }),
        )
    }

    /// The implementer's provider, which the reviewer should differ from (spec §3.2).
    async fn implementer_provider(&self, id: i64) -> Result<Option<String>, PipelineError> {
        let runs = self.store.stage_runs(id).await?;
        Ok(runs
            .iter()
            .rev()
            .find(|r| r.stage == "implement")
            .and_then(|r| self.config.models.iter().find(|m| m.id == r.model_id))
            .map(ModelEntry::provider_key))
    }

    async fn pick(
        &self,
        task: &TaskRow,
        stage: Stage,
    ) -> Result<Option<ModelEntry>, PipelineError> {
        let tier = self.tier_for(task, stage).await?;
        let avoid = match stage {
            Stage::Review => self.review_avoid(task).await?,
            _ => Vec::new(),
        };
        let catalog = self.ordered_models();
        let avail = self
            .cooldowns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .availability();
        Ok(select(tier, &catalog, &avail, SystemTime::now(), &avoid).cloned())
    }

    /// Swaps in a newer price table once the one held is a day old (D71). Cheap
    /// when fresh: only the timestamp is compared.
    pub async fn refresh_prices(&self) {
        let fetched_at = self
            .prices
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .fetched_at;
        let t = now();
        let last = self
            .price_attempt
            .load(std::sync::atomic::Ordering::Relaxed);
        if t - fetched_at < crate::prices::MAX_AGE || t - last < crate::prices::RETRY_AFTER {
            return;
        }
        self.price_attempt
            .store(t, std::sync::atomic::Ordering::Relaxed);
        let fresh = crate::prices::load(&self.paths, self.config.routing.price_urls(), now()).await;
        *self
            .prices
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fresh;
    }

    /// The catalog in `[routing] prefer` order, so `select` tries the cheapest
    /// usable model of the tier first (D73).
    fn ordered_models(&self) -> Vec<ModelEntry> {
        let prices = self
            .prices
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::routing::ordered(&self.config.models, &prices, self.config.routing.prefer)
    }

    fn route_why(&self, model: &ModelEntry) -> String {
        let prices = self
            .prices
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::routing::why(
            model,
            &self.config.models,
            &prices,
            self.config.routing.prefer,
        )
    }

    /// Picks a model and counts it as running in the same critical section, so
    /// tasks running in parallel respect `max_concurrency`; also checks the
    /// daily worker budget in that same section, so parallel workers cannot
    /// both pass the check before either is recorded (issue #12). The slot
    /// frees the model when dropped, whichever way the stage ends.
    async fn claim(&self, task: &TaskRow, stage: Stage) -> Result<Claim<'_>, PipelineError> {
        let tier = self.tier_for(task, stage).await?;
        let avoid = match stage {
            Stage::Review => self.review_avoid(task).await?,
            _ => Vec::new(),
        };
        self.claim_tier(tier, &avoid).await
    }

    /// `claim` for a tier and the providers to avoid; also how periodic
    /// work's model calls get a model (repository rules plan decision 15).
    pub(crate) async fn claim_tier(
        &self,
        tier: Tier,
        avoid: &[String],
    ) -> Result<Claim<'_>, PipelineError> {
        let catalog = self.ordered_models();
        // Serialises this count+claim with `run_stage`'s record+mark, so the
        // count below can never be undercut by a run that is about to be
        // recorded on another worker (issue #12).
        let _gate = self.budget.lock().await;
        let done = self.store.worker_runs_since(now() - 86_400).await?;
        let mut cooldowns = self
            .cooldowns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if done + cooldowns.unrecorded() >= self.config.limits.max_stage_runs_per_day {
            return Ok(Claim::OverBudget);
        }
        let avail = cooldowns.availability();
        let Some(model) = select(tier, &catalog, &avail, SystemTime::now(), avoid).cloned() else {
            return Ok(Claim::Busy);
        };
        cooldowns.start(&model.id);
        let slot = Slot {
            cooldowns: &self.cooldowns,
            model_id: model.id.clone(),
            recorded: false,
        };
        Ok(Claim::Run(Box::new(model), slot))
    }

    async fn wait(&self, task: &TaskRow, stage: Stage) -> Result<TaskState, PipelineError> {
        let tier = self.tier_for(task, stage).await?;
        self.go(
            task.id,
            TaskState::Waiting,
            &format!("no {tier:?} model is free (cooling down or at max_concurrency)"),
        )
        .await
    }

    async fn resume_waiting(&self, task: &TaskRow) -> Result<TaskState, PipelineError> {
        // The daily worker budget is still spent (D53).
        if self.store.worker_runs_since(now() - 86_400).await?
            >= self.config.limits.max_stage_runs_per_day
        {
            return Ok(TaskState::Waiting);
        }
        // A transient failure waits for its stored resume time (D51).
        if let Some(t) = self.store.last_output(task.id, "transient").await?
            && t["at"].as_i64().is_some_and(|at| at > now())
        {
            return Ok(TaskState::Waiting);
        }
        let log = self.store.transitions(task.id).await?;
        let prev = log
            .iter()
            .rev()
            .find(|t| t.to == TaskState::Waiting)
            .and_then(|t| t.from)
            .unwrap_or(TaskState::Queued);
        let stage = match prev {
            TaskState::Planning => Some(Stage::Plan),
            TaskState::Implementing => Some(Stage::Implement),
            TaskState::Reviewing => Some(Stage::Review),
            _ => None,
        };
        if let Some(stage) = stage
            && self.pick(task, stage).await?.is_none()
        {
            return Ok(TaskState::Waiting);
        }
        self.go(task.id, prev, "a model is free again").await
    }

    // ---- running a worker stage ----

    /// Runs one worker stage, watching for loops when `watch` is set, and keeps
    /// cooldowns and the stage log up to date. `None` when the provider hit a
    /// rate limit: the caller stays in its state and re-routes on the next step.
    async fn run_stage(
        &self,
        task: &TaskRow,
        model: &ModelEntry,
        stage: &str,
        req: StageRequest,
        watch: bool,
        mut slot: Slot<'_>,
    ) -> Result<Option<Result<Outcome, String>>, PipelineError> {
        let mut why = self.route_why(model);
        if stage == "review"
            && let Some(note) = self.jev_frontier_downgrade(task, Stage::Review).await?
        {
            why = format!("{why}; {note}");
        }
        self.store
            .record_output(
                task.id,
                "route",
                &json!({"stage": stage, "model": model.id, "why": why}),
            )
            .await?;
        let started = now();
        let started_at = SystemTime::now();
        let session = req.session_dir.clone();
        let outcome = self.watched(model, req, watch).await;
        let exit = match &outcome {
            Ok(Outcome::Finished(r)) => exit_name(&r.exit),
            Ok(Outcome::Looping(p)) => format!("loop_detected {p:.2}"),
            Err(e) => format!("worker_error: {e}"),
        };
        let (turns, usage, actual_model) = match &outcome {
            Ok(Outcome::Finished(r)) => (r.turns, r.usage, r.actual_model.clone()),
            _ => (0, Default::default(), None),
        };
        let cost = {
            let prices = self
                .prices
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            crate::cost::stage_cost(
                model,
                &self.config.models,
                &usage,
                actual_model.as_deref(),
                &prices,
            )
        };
        // Serialises this record+mark with `claim`'s count+claim, so a claim
        // freed here is never missed by another worker's count (issue #12).
        let _gate = self.budget.lock().await;
        let run = StageRunRecord {
            task_id: task.id,
            stage: stage.into(),
            model_id: model.id.clone(),
            exit,
            turns,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            session_dir: session,
            gate_score: None,
            started_at: started,
            finished_at: now(),
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            actual_model,
            cost_usd: cost.usd,
            quota_units: cost.quota_units,
        };
        self.store
            .write_with_events(
                task.id,
                Write::StageRun(&run),
                &[Event::StageRun {
                    stage: run.stage.clone(),
                    model_id: run.model_id.clone(),
                    actual_model: run.actual_model.clone(),
                    provider: Some(model.provider_key()),
                    input_tokens: run.input_tokens,
                    output_tokens: run.output_tokens,
                    cache_read_tokens: run.cache_read_tokens,
                    cache_write_tokens: run.cache_write_tokens,
                    cost_usd: run.cost_usd,
                    exit: run.exit.clone(),
                }],
            )
            .await?;
        slot.recorded();
        drop(_gate);
        match outcome {
            Ok(Outcome::Finished(r)) if matches!(r.exit, ExitReason::RateLimited(_)) => {
                self.cooldowns
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .strike(&model.cooldown_key(), SystemTime::now());
                Ok(None)
            }
            Ok(o) => {
                if let Outcome::Finished(r) = &o
                    && r.exit == ExitReason::Completed
                {
                    self.cooldowns
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clear(&model.cooldown_key(), started_at);
                }
                Ok(Some(Ok(o)))
            }
            Err(e) => Ok(Some(Err(e.to_string()))),
        }
    }

    pub(crate) async fn watched(
        &self,
        model: &ModelEntry,
        req: StageRequest,
        watch: bool,
    ) -> Result<Outcome, agent_workers::WorkerError> {
        let (tx, mut rx) = unbounded_channel::<WorkerEvent>();
        let run = self.runner.run(model, req, tx);
        tokio::pin!(run);
        let mut recent: Vec<WorkerEvent> = Vec::new();
        let mut calls = 0u32;
        let mut open = true;
        loop {
            tokio::select! {
                res = &mut run => return res.map(Outcome::Finished),
                ev = rx.recv(), if open => match ev {
                    None => open = false,
                    Some(ev) => {
                        let is_call = matches!(ev, WorkerEvent::ToolStart { .. });
                        recent.push(ev);
                        if recent.len() > LOOP_KEEP {
                            recent.remove(0);
                        }
                        if is_call {
                            calls += 1;
                        }
                        if watch && is_call && calls.is_multiple_of(LOOP_EVERY)
                            && let Some(p) = self.oracle.loop_probability(&recent).await
                            && p > self.config.jev.loop_threshold
                        {
                            // Dropping `run` kills the worker's process tree (D26).
                            return Ok(Outcome::Looping(p));
                        }
                    }
                },
            }
        }
    }

    pub(crate) fn request(
        &self,
        wt: &Path,
        prompt: String,
        tools: ToolProfile,
        schema: Option<Value>,
        max_turns: u32,
        session: PathBuf,
    ) -> StageRequest {
        StageRequest {
            cwd: wt.to_path_buf(),
            prompt,
            model: String::new(),
            provider: None,
            tools,
            system_prompt_file: None,
            output_schema: schema,
            max_turns,
            timeout: self.config.limits.stage_timeout,
            session_dir: session,
        }
    }

    async fn issue_text(&self, id: i64) -> Result<(String, String), PipelineError> {
        Ok(self
            .store
            .last_output(id, "issue")
            .await?
            .map(|v| issue_snapshot(&v))
            .unwrap_or_default())
    }

    async fn plan_output(&self, id: i64) -> Result<Option<PlanOutput>, PipelineError> {
        Ok(self
            .store
            .last_output(id, "plan")
            .await?
            .and_then(|v| serde_json::from_value(v).ok()))
    }

    /// A plan recorded during the current pass, if any (spec §3.1): reused
    /// when a crash happens between recording it and entering `Implementing`.
    async fn plan_of_this_pass(&self, task: &TaskRow) -> Result<Option<PlanOutput>, PipelineError> {
        Ok(self
            .store
            .last_output(task.id, "plan")
            .await?
            .filter(|v| v["pass"].as_u64() == Some(u64::from(task.reopen_count) + 1))
            .and_then(|v| serde_json::from_value(v).ok()))
    }

    /// Mirrors a stage answer into the worktree's git-ignored `.provefab/` (spec §3.2).
    fn mirror(wt: &Path, name: &str, value: &Value) {
        let dir = wt.join(".provefab");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_string_pretty(value).unwrap_or_default(),
        );
    }

    /// A plan or review stage failed to produce its answer (spec §3.4 item 3):
    /// retry, escalate once, then give up.
    async fn stage_failed(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        stage: &str,
        reason: &str,
    ) -> Result<TaskState, PipelineError> {
        let attempts = self.store.bump_attempts(task.id).await?;
        match ladder(attempts, task.escalated, false) {
            Ladder::Retry => {
                self.go(
                    task.id,
                    task.state,
                    &format!("{stage} failed, retrying: {reason}"),
                )
                .await
            }
            Ladder::Escalate => {
                self.store.set_escalated(task.id, true).await?;
                self.go(
                    task.id,
                    task.state,
                    &format!("{stage} failed, one tier up: {reason}"),
                )
                .await
            }
            Ladder::GiveUp => {
                self.give_up(
                    task,
                    Some(repo),
                    TaskState::Failed,
                    &format!("the {stage} stage failed {attempts} times"),
                    reason,
                )
                .await
            }
        }
    }

    // ---- Planning (spec §3.1, §3.4 items 1 and 3) ----

    async fn plan(&self, task: &TaskRow, repo: &RepoConfig) -> Result<TaskState, PipelineError> {
        let wt = match self.ensure_worktree(task, repo).await {
            Ok(wt) => wt,
            Err(e) => {
                return self.failed(task, repo, "worktree", &e).await;
            }
        };
        // A plan recorded before a crash in this pass is reused, so no
        // worker run is spent re-planning.
        if let Some(p) = self.plan_of_this_pass(task).await? {
            let kind = task.kind.unwrap_or(TaskKind::Feature);
            Self::mirror(
                &wt,
                "plan",
                &serde_json::to_value(&p).unwrap_or(Value::Null),
            );
            return self.plan_ready(task, repo, &wt, kind, &p).await;
        }
        let (model, slot) = match self.claim(task, Stage::Plan).await? {
            Claim::Run(m, s) => (*m, s),
            Claim::Busy => return self.wait(task, Stage::Plan).await,
            Claim::OverBudget => return self.over_budget(task, repo).await,
        };
        let (title, body) = self.issue_text(task.id).await?;
        let kind = task.kind.unwrap_or(TaskKind::Feature);
        let reference = task.reference();
        let rules = self.pass_rules(task, repo).await?;
        let (rules_block, _) = crate::rules::render(
            &crate::rules::select(&rules, Stage::Plan, &[]),
            crate::rules::BUDGET,
        );
        let prompt = render(
            Template::Plan,
            &[
                ("ref", &reference),
                ("title", &title),
                ("kind", kind.as_str()),
                ("body", &body),
                ("rules", &rules_block),
            ],
        );
        let req = self.request(
            &wt,
            prompt,
            ToolProfile::ReadOnly,
            Some(output_schema::<PlanOutput>()),
            self.config.limits.max_turns.plan,
            self.paths
                .session(task.id, "plan", self.next_run_seq(task.id).await?),
        );
        let Some(outcome) = self
            .run_stage(task, &model, "plan", req, false, slot)
            .await?
        else {
            return Ok(task.state);
        };
        let plan = match outcome {
            Ok(Outcome::Finished(r)) => r
                .structured_output
                .and_then(|v| serde_json::from_value::<PlanOutput>(v).ok())
                .ok_or_else(|| format!("no valid plan (exit {})", exit_name(&r.exit))),
            Ok(Outcome::Looping(p)) => Err(format!("loop detected ({p:.2})")),
            Err(e) => Err(e),
        };
        let plan = match plan {
            Ok(p) if kind == TaskKind::Bugfix && repro(&p).is_none() => {
                Err("a bugfix plan must name a reproduction command".to_string())
            }
            other => other,
        };
        let plan = match plan {
            Ok(p) => p,
            Err(reason) => return self.stage_failed(task, repo, "plan", &reason).await,
        };
        let mut value = serde_json::to_value(&plan).unwrap_or(Value::Null);
        if let Some(o) = value.as_object_mut() {
            o.insert("pass".into(), json!(u64::from(pass_of(task))));
        }
        self.store
            .write_with_events(
                task.id,
                Write::Output {
                    kind: "plan",
                    value: &value,
                },
                &[Event::Plan {
                    pass: pass_of(task),
                    summary: plan.summary.clone(),
                    steps: plan.steps.clone(),
                    risks: plan.risks.clone(),
                }],
            )
            .await?;
        Self::mirror(&wt, "plan", &value);
        self.plan_ready(task, repo, &wt, kind, &plan).await
    }

    /// The repro-command gate for a bugfix plan (spec §3.4 item 1), then the
    /// move into `Implementing`. Shared between a fresh plan and one reused
    /// after a crash (spec §3.1).
    async fn plan_ready(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        wt: &Path,
        kind: TaskKind,
        plan: &PlanOutput,
    ) -> Result<TaskState, PipelineError> {
        if kind == TaskKind::Bugfix
            && let Some(cmd) = repro(plan)
        {
            let report = self
                .gates(task, wt, std::slice::from_ref(&cmd), "repro")
                .await?;
            if report.passed() {
                return self
                    .give_up(
                        task,
                        Some(repo),
                        TaskState::NeedsYou,
                        &format!("the reproduction command `{cmd}` already passes before any fix"),
                        "",
                    )
                    .await;
            }
            // Did it fail because of the bug, or because it could not run at all
            // (a script that does not exist yet)? Only the first reproduces
            // anything (review I5); kept per pass.
            let genuine = report
                .first_failure()
                .is_some_and(|r| !could_not_run(r.exit, &r.output_tail));
            let result = json!({
                "pass": u64::from(pass_of(task)),
                "command": cmd,
                "reproduced": genuine,
            });
            self.store
                .write_with_events(
                    task.id,
                    Write::Output {
                        kind: "repro_result",
                        value: &result,
                    },
                    &[Event::Reproduction {
                        command: cmd.to_string(),
                        failed_before_fix: genuine,
                    }],
                )
                .await?;
        }
        self.enter(task.id, TaskState::Implementing, "plan ready")
            .await
    }

    /// Returns a number no other stage or gate run of this task has used yet,
    /// so a re-run without bumping `attempts` (a rate-limited retry, or a
    /// second reviewer in the same round) still gets its own session dir.
    async fn next_run_seq(&self, task_id: i64) -> Result<u32, PipelineError> {
        Ok(self.store.stage_runs(task_id).await?.len() as u32 + 1)
    }

    /// Runs `commands` as gates and logs the run with its score.
    pub(crate) async fn gates(
        &self,
        task: &TaskRow,
        wt: &Path,
        commands: &[String],
        stage: &str,
    ) -> Result<GateReport, PipelineError> {
        let n = self.next_run_seq(task.id).await?;
        let scratch = self.paths.session(task.id, stage, n);
        let _ = std::fs::create_dir_all(&scratch);
        let started = now();
        let report = run_gates(wt, commands, self.config.limits.gate_timeout, &scratch).await;
        if matches!(stage, "post-merge" | "post-merge-base" | "revert-check") {
            let results: Vec<Value> = report
                .results
                .iter()
                .map(|r| {
                    json!({
                        "command": r.command,
                        "exit": r.exit,
                        "passed": r.passed,
                        "timed_out": r.timed_out,
                        "output_tail": r.output_tail,
                    })
                })
                .collect();
            let path = scratch.join("results.json");
            std::fs::write(&path, json!({"results": results}).to_string())
                .map_err(|e| ForgeError::Parse(path.display().to_string(), e.to_string()))?;
        }
        let output_ref = scratch.display().to_string();
        let run = StageRunRecord {
            task_id: task.id,
            stage: stage.into(),
            model_id: String::new(),
            exit: if report.passed() { "passed" } else { "failed" }.into(),
            turns: 0,
            input_tokens: 0,
            output_tokens: 0,
            session_dir: scratch,
            gate_score: Some(report.score.to_string()),
            started_at: started,
            finished_at: now(),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            actual_model: None,
            cost_usd: None,
            quota_units: None,
        };
        self.store
            .write_with_events(
                task.id,
                Write::StageRun(&run),
                &[Event::GatesRun {
                    stage: stage.into(),
                    round: task.review_rounds,
                    results: report
                        .results
                        .iter()
                        .map(|r| GateEntry {
                            command: r.command.clone(),
                            exit: r.exit,
                            timed_out: r.timed_out,
                            passed: r.passed,
                            output_ref: output_ref.clone(),
                        })
                        .collect(),
                }],
            )
            .await?;
        Ok(report)
    }

    // ---- Implementing (spec §3.1, §4.3) ----

    async fn feedback(&self, task: &TaskRow) -> Result<String, PipelineError> {
        let mut out = String::new();
        // Every blocking finding of every round so far, not just the latest: a
        // round that saw only the newest one rewrote the fix and regressed an
        // earlier one (sandbox issue #5, D45). Every review of the task counts,
        // including those of a pass before a `provefab add` requeue (D46).
        let blocking: Vec<String> = self
            .store
            .recent_outputs(task.id, "review", u32::MAX)
            .await?
            .into_iter()
            .filter_map(|r| serde_json::from_value::<ReviewOutput>(r).ok())
            .flat_map(|r| r.findings)
            .filter(|f| f.severity == Severity::Blocking)
            .map(|f| {
                let at = f.line.map(|l| format!(":{l}")).unwrap_or_default();
                let rule = f.rule.map(|r| format!(" ({r})")).unwrap_or_default();
                format!("- {}{at}{rule} {}", f.file, f.text)
            })
            .collect();
        if !blocking.is_empty() {
            out.push_str(
                "Reviewers asked for these changes, across every round so far. Fix all of them \
                 together, and for each one add a regression test that fails without the fix \
                 and keep it in the change, so a later rewrite cannot bring it back:\n",
            );
            out.push_str(&blocking.join("\n"));
            out.push('\n');
        }
        if task.attempts > 0
            && let Some(f) = self.store.last_output(task.id, "failure").await?
        {
            out.push_str(&format!(
                "\nThe previous attempt failed: {}\n```\n{}\n```\n",
                f["reason"].as_str().unwrap_or_default(),
                f["output"].as_str().unwrap_or_default()
            ));
        }
        Ok(out)
    }

    fn gate_commands(repo: &RepoConfig, plan: Option<&PlanOutput>) -> Vec<String> {
        let mut gates = repo.gates.clone();
        if let Some(cmd) = plan.and_then(repro) {
            gates.push(cmd);
        }
        gates
    }

    async fn implement(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<TaskState, PipelineError> {
        let wt = match self.ensure_worktree(task, repo).await {
            Ok(wt) => wt,
            Err(e) => {
                return self.failed(task, repo, "worktree", &e).await;
            }
        };
        let (model, slot) = match self.claim(task, Stage::Implement).await? {
            Claim::Run(m, s) => (*m, s),
            Claim::Busy => return self.wait(task, Stage::Implement).await,
            Claim::OverBudget => return self.over_budget(task, repo).await,
        };
        let (title, body) = self.issue_text(task.id).await?;
        let plan = self.plan_output(task.id).await?;
        let plan_str = plan.as_ref().map(plan_text).unwrap_or_default();
        let feedback = self.feedback(task).await?;
        let gates = Self::gate_commands(repo, plan.as_ref())
            .iter()
            .map(|g| format!("`{g}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let reference = task.reference();
        let rules = self.pass_rules(task, repo).await?;
        let files = plan.as_ref().map(|p| p.files.clone()).unwrap_or_default();
        let (rules_block, _) = crate::rules::render(
            &crate::rules::select(&rules, Stage::Implement, &files),
            crate::rules::BUDGET,
        );
        let prompt = render(
            Template::Implement,
            &[
                ("ref", &reference),
                ("title", &title),
                ("body", &body),
                ("plan", &plan_str),
                ("feedback", &feedback),
                ("gates", &gates),
                ("rules", &rules_block),
            ],
        );
        let session = self.paths.session(
            task.id,
            &format!("implement-r{}", task.review_rounds),
            self.next_run_seq(task.id).await?,
        );
        let req = self.request(
            &wt,
            prompt,
            ToolProfile::Full,
            None,
            self.config.limits.max_turns.implement,
            session,
        );
        let Some(outcome) = self
            .run_stage(task, &model, "implement", req, true, slot)
            .await?
        else {
            return Ok(task.state);
        };
        match outcome {
            Ok(Outcome::Finished(r)) if r.exit == ExitReason::Completed => {
                self.go(task.id, TaskState::Gating, "implementation finished")
                    .await
            }
            Ok(Outcome::Finished(r)) => {
                let public = format!("the implement stage stopped ({})", exit_kind(&r.exit));
                let reason = format!("the implement stage stopped: {}", exit_name(&r.exit));
                self.failed_attempt(task, repo, "implement stage", (&public, &reason), "", None)
                    .await
            }
            Ok(Outcome::Looping(p)) => {
                let reason = format!("the loop detector stopped the agent ({p:.2})");
                self.failed_attempt(task, repo, "implement stage", (&reason, &reason), "", None)
                    .await
            }
            Err(e) => {
                let reason = format!("the worker failed: {e}");
                let why = ("the worker failed to run", reason.as_str());
                self.failed_attempt(task, repo, "implement stage", why, "", None)
                    .await
            }
        }
    }

    /// An implement attempt failed (gates or abort): triage, then the ladder (spec §4.4).
    async fn failed_attempt(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        command: &str,
        // (text safe to post on GitHub, full text for the log and the next attempt)
        why: (&str, &str),
        output: &str,
        score: Option<ProgressScore>,
    ) -> Result<TaskState, PipelineError> {
        let (public, reason) = why;
        let previous = self
            .store
            .last_output(task.id, "failure")
            .await?
            .and_then(|f| f["score"].as_str().and_then(ProgressScore::parse));
        self.store
            .record_output(
                task.id,
                "failure",
                &json!({
                    "reason": reason,
                    "output": output,
                    "score": score.map(|s| s.to_string()),
                }),
            )
            .await?;
        let attempts = self.store.bump_attempts(task.id).await?;
        let triage_input = if output.is_empty() { reason } else { output };
        let verdict = self.oracle.triage(command, triage_input).await;
        match verdict {
            Some(Triage::EnvProblem) => {
                return self
                    .transient(
                        task,
                        repo,
                        &format!("environment problem: {public}"),
                        reason,
                    )
                    .await;
            }
            Some(Triage::OutOfScope) => {
                return self
                    .give_up(
                        task,
                        Some(repo),
                        TaskState::NeedsYou,
                        &format!("out of scope: {public}"),
                        reason,
                    )
                    .await;
            }
            None if attempts > 1 => {
                return self
                    .give_up(
                        task,
                        Some(repo),
                        TaskState::NeedsYou,
                        &format!("failed twice and triage was unavailable: {public}"),
                        reason,
                    )
                    .await;
            }
            _ => {}
        }
        let improved = match (score, previous) {
            (Some(now), Some(before)) => now.improved_on(before),
            _ => false,
        };
        match ladder(attempts, task.escalated, improved) {
            Ladder::Retry => {
                self.go(
                    task.id,
                    TaskState::Implementing,
                    &format!("retry: {reason}"),
                )
                .await
            }
            Ladder::Escalate => {
                self.store.set_escalated(task.id, true).await?;
                self.go(
                    task.id,
                    TaskState::Implementing,
                    &format!("one tier up: {reason}"),
                )
                .await
            }
            Ladder::GiveUp => {
                self.give_up(
                    task,
                    Some(repo),
                    TaskState::Failed,
                    &format!("{attempts} attempts failed; last: {public}"),
                    reason,
                )
                .await
            }
        }
    }

    // ---- Gating (spec §3.1, §3.4 item 2) ----

    async fn gate(&self, task: &TaskRow, repo: &RepoConfig) -> Result<TaskState, PipelineError> {
        let wt = match self.ensure_worktree(task, repo).await {
            Ok(wt) => wt,
            Err(e) => {
                return self.failed(task, repo, "worktree", &e).await;
            }
        };
        let plan = self.plan_output(task.id).await?;
        let commands = Self::gate_commands(repo, plan.as_ref());
        let report = self.gates_rerun(task, &wt, &commands, "gates").await?;
        if let Some(state) = self.gate_failed(task, repo, &report).await? {
            return Ok(state);
        }
        let message = format!(
            "{}\n\nProvefab task {}, issue {}.",
            task.title,
            task.id,
            task.reference()
        );
        let base = self.pass_base(task, repo).await?;
        let changed = match self.git.commit_all(&wt, &message).await {
            Ok(_) => self.git.changed_files(&wt, &base).await,
            Err(e) => {
                return self.failed(task, repo, "commit failed", &e).await;
            }
        };
        // "Changed nothing" is about the branch, not this commit: after a crash
        // the work may already be committed (review I1). Paths that cannot be
        // computed are not "nothing": the risk policy classifies them `unknown`.
        if changed.as_ref().is_ok_and(Vec::is_empty) {
            return self
                .failed_attempt(
                    task,
                    repo,
                    "implement stage",
                    ("the agent changed no file", "the agent changed no file"),
                    "",
                    None,
                )
                .await;
        }
        if let Some(state) = self
            .risk_round(task, repo, &wt, &commands, changed.ok())
            .await?
        {
            return Ok(state);
        }
        self.enter(task.id, TaskState::Reviewing, "gates passed")
            .await
    }

    /// Runs gates; a failure the oracle calls flaky runs them once more before
    /// it counts (spec §4.4).
    async fn gates_rerun(
        &self,
        task: &TaskRow,
        wt: &Path,
        commands: &[String],
        stage: &str,
    ) -> Result<GateReport, PipelineError> {
        let report = self.gates(task, wt, commands, stage).await?;
        if let Some(failure) = report.first_failure() {
            let verdict = self
                .oracle
                .triage(&failure.command, &failure.output_tail)
                .await;
            if verdict == Some(Triage::FlakyTest) {
                return self.gates(task, wt, commands, stage).await;
            }
        }
        Ok(report)
    }

    /// A failing gate is a failed attempt (back to implementation); `None` when all passed.
    async fn gate_failed(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        report: &GateReport,
    ) -> Result<Option<TaskState>, PipelineError> {
        let Some(failure) = report.first_failure().cloned() else {
            return Ok(None);
        };
        let reason = format!("gate `{}` failed", failure.command);
        self.failed_attempt(
            task,
            repo,
            &failure.command,
            (&reason, &reason),
            &failure.output_tail,
            Some(report.score),
        )
        .await
        .map(Some)
    }

    /// Risk policy §6-§7 for this round: classify the changed paths (`None`:
    /// they could not be computed, fail closed as `unknown`), record it, label
    /// the issue and run the categories' extra checks (`tier_for` picks the
    /// reviewer). `Some` ends the round.
    async fn risk_round(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        wt: &Path,
        commands: &[String],
        changed: Option<Vec<Change>>,
    ) -> Result<Option<TaskState>, PipelineError> {
        let policy = match risk::resolve(repo.risk.as_ref()) {
            Ok(p) => p,
            Err(e) => {
                let public = format!("invalid [repos.risk]: {e}");
                return self
                    .give_up(task, Some(repo), TaskState::NeedsYou, &public, "")
                    .await
                    .map(Some);
            }
        };
        let detected = match changed {
            // Both sides of a rename count (Review Focus 1).
            Some(changed) => {
                let paths: Vec<String> = changed
                    .into_iter()
                    .flat_map(|c| std::iter::once(c.path).chain(c.from))
                    .collect();
                risk::classify(&policy, &paths)
            }
            None => risk::unknown(),
        };
        let previous = self.latest_risk(task.id, |_, _| true).await?;
        self.store
            .write_with_events(
                task.id,
                Write::Nothing,
                &[Event::RiskClassified {
                    pass: pass_of(task),
                    round: task.review_rounds,
                    categories: detected.clone(),
                }],
            )
            .await?;
        self.risk_labels(task, repo, &detected, previous.as_deref())
            .await?;
        // A check that is already a gate ran above: once, not twice (Review Focus 2).
        let extra: Vec<String> = risk_checks(&policy, &detected, commands)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        if !extra.is_empty() {
            let report = self.gates_rerun(task, wt, &extra, "risk-gates").await?;
            // Its score may be compared with a regular gates score of the previous
            // attempt; a wrong "improved" only costs a retry the attempt ladder bounds.
            if let Some(state) = self.gate_failed(task, repo, &report).await? {
                return Ok(Some(state));
            }
        }
        Ok(None)
    }

    /// `<label>:risk-<category>` on the issue for each detected category; the
    /// ones of the previous classification that no longer apply are removed
    /// (Review Focus 3). Labelled before the extra checks run. A label
    /// `ensure_label` could not create (to add or to remove) is left out of the edit, so the queued
    /// edit never names a label that may not exist (it would fail on every
    /// retry and hold the task's later GitHub effects). The edit is skipped
    /// only when every label to add was added earlier in the same round (a
    /// `risk_labelled` output) and none is removed, so a label whose creation
    /// failed is retried by the next classification of the round.
    async fn risk_labels(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        detected: &[Detected],
        previous: Option<&[Detected]>,
    ) -> Result<(), PipelineError> {
        if self.misrouted(task.id, "labelling").await? {
            return Ok(());
        }
        let mut remove: Vec<String> = Vec::new();
        for d in previous
            .unwrap_or_default()
            .iter()
            .filter(|d| !detected.iter().any(|a| a.name == d.name))
        {
            let (label, description) = risk::label(&repo.label, &d.name);
            match self
                .hub
                .ensure_label(&repo.slug, &label, risk::LABEL_COLOR, &description)
                .await
            {
                Ok(()) => remove.push(label),
                Err(e) => eprintln!(
                    "provefab: could not create label {label} on {}, not removing it: {e}",
                    repo.slug
                ),
            }
        }
        let mut add: Vec<String> = Vec::new();
        for d in detected {
            let (label, description) = risk::label(&repo.label, &d.name);
            match self
                .hub
                .ensure_label(&repo.slug, &label, risk::LABEL_COLOR, &description)
                .await
            {
                Ok(()) => add.push(label),
                Err(e) => eprintln!(
                    "provefab: could not create label {label} on {}, not adding it: {e}",
                    repo.slug
                ),
            }
        }
        let (pass, round) = (pass_of(task), task.review_rounds);
        let added_before: Vec<String> = self
            .store
            .recent_outputs(task.id, "risk_labelled", 16)
            .await?
            .iter()
            .filter(|o| o["pass"] == pass && o["round"] == round)
            .flat_map(|o| o["labels"].as_array().into_iter().flatten())
            .filter_map(|l| l.as_str().map(String::from))
            .collect();
        let already = !add.is_empty() && add.iter().all(|l| added_before.contains(l));
        if (add.is_empty() || already) && remove.is_empty() {
            return Ok(());
        }
        let labels: Vec<&str> = add.iter().map(String::as_str).collect();
        let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
        self.relabel(task.id, &task.repo, task.issue_number, &labels, &remove)
            .await?;
        if !add.is_empty() {
            self.store
                .record_output(
                    task.id,
                    "risk_labelled",
                    &json!({"pass": pass, "round": round, "labels": add}),
                )
                .await?;
        }
        Ok(())
    }

    /// The categories of the latest `risk_classified` event whose (pass, round) `keep` accepts.
    async fn latest_risk(
        &self,
        id: i64,
        keep: impl Fn(u32, u32) -> bool,
    ) -> Result<Option<Vec<Detected>>, PipelineError> {
        let events = self.store.events(id).await?;
        Ok(events
            .iter()
            .rev()
            .filter(|e| e.kind == "risk_classified")
            .filter_map(StoredEvent::typed)
            .find_map(|e| match e {
                Event::RiskClassified {
                    pass,
                    round,
                    categories,
                } if keep(pass, round) => Some(categories),
                _ => None,
            }))
    }

    /// The risk classification of the task's current pass and round, if any.
    pub(crate) async fn risk_of_round(
        &self,
        task: &TaskRow,
    ) -> Result<Option<Vec<Detected>>, PipelineError> {
        let pass = pass_of(task);
        self.latest_risk(task.id, |p, r| p == pass && r == task.review_rounds)
            .await
    }

    // ---- Reviewing and the pull request (spec §3.1, §3.4 item 6) ----

    async fn review(&self, task: &TaskRow, repo: &RepoConfig) -> Result<TaskState, PipelineError> {
        let wt = match self.ensure_worktree(task, repo).await {
            Ok(wt) => wt,
            Err(e) => {
                return self.failed(task, repo, "worktree", &e).await;
            }
        };
        // A resume after a failed push or `pr_create` must not re-run an
        // already approved review (issue #13): that would spend the
        // reviewer's quota again, and could flip an approval to "changes".
        if let Ok(sha) = self.git.head(&wt).await
            && let Some(state) = self.resume_approved(task, repo, &wt, &sha).await?
        {
            return Ok(state);
        }
        let (model, slot) = match self.claim(task, Stage::Review).await? {
            Claim::Run(m, s) => (*m, s),
            Claim::Busy => return self.wait(task, Stage::Review).await,
            Claim::OverBudget => return self.over_budget(task, repo).await,
        };
        let base = self.pass_base(task, repo).await?;
        let diff = match self.git.diff(&wt, &base).await {
            Ok(d) => d,
            Err(e) => {
                return self.failed(task, repo, "diff failed", &e).await;
            }
        };
        let (title, body) = self.issue_text(task.id).await?;
        let plan = self.plan_output(task.id).await?;
        let plan_str = plan.as_ref().map(plan_text).unwrap_or_default();
        let reference = task.reference();
        let diff = truncate(&diff, DIFF_LIMIT);
        let base_shown = format!("{} at {}", repo.base, &base[..base.len().min(12)]);
        let rules = self.pass_rules(task, repo).await?;
        // The round's changed files, both sides of a rename, as the risk policy reads them.
        let changed: Vec<String> = match self.git.changed_files(&wt, &base).await {
            Ok(c) => c,
            Err(e) => {
                // Only the rules without `paths:` reach this review.
                eprintln!(
                    "provefab: task {}: could not list the changed files for the rules: {e}",
                    task.id
                );
                Vec::new()
            }
        }
        .into_iter()
        .flat_map(|c| std::iter::once(c.path).chain(c.from))
        .collect();
        let selected = crate::rules::select(&rules, Stage::Review, &changed);
        let (rules_block, omitted) = crate::rules::render(&selected, crate::rules::BUDGET);
        let given = crate::rules::numbers(&selected, omitted);
        let rules_block = if given.is_empty() {
            rules_block
        } else {
            format!("{rules_block}{}", crate::rules::REVIEW_ASK)
        };
        if !given.is_empty() {
            self.store
                .record_output(
                    task.id,
                    "rules_given",
                    &json!({"pass": pass_of(task), "round": task.review_rounds, "numbers": given}),
                )
                .await?;
        }
        let prompt = render(
            Template::Review,
            &[
                ("ref", &reference),
                ("title", &title),
                ("body", &body),
                ("plan", &plan_str),
                ("base", &base_shown),
                ("diff", &diff),
                ("rules", &rules_block),
            ],
        );
        let req = self.request(
            &wt,
            prompt,
            ToolProfile::ReadOnly,
            Some(output_schema::<ReviewOutput>()),
            self.config.limits.max_turns.review,
            self.paths.session(
                task.id,
                &format!("review-r{}", task.review_rounds),
                self.next_run_seq(task.id).await?,
            ),
        );
        let Some(outcome) = self
            .run_stage(task, &model, "review", req, false, slot)
            .await?
        else {
            return Ok(task.state);
        };
        let review = match outcome {
            Ok(Outcome::Finished(r)) => r
                .structured_output
                .and_then(|v| serde_json::from_value::<ReviewOutput>(v).ok())
                .ok_or_else(|| format!("no valid review (exit {})", exit_name(&r.exit))),
            Ok(Outcome::Looping(p)) => Err(format!("loop detected ({p:.2})")),
            Err(e) => Err(e),
        };
        let review = match review {
            Ok(r) => r,
            Err(reason) => return self.stage_failed(task, repo, "review", &reason).await,
        };
        // Spec §5: a `rule` must name a rule this review was given; any other
        // value is dropped and the finding stays (plan decision 9).
        let mut review = review;
        for f in &mut review.findings {
            f.rule = f
                .rule
                .as_deref()
                .and_then(crate::rules::rule_number)
                .filter(|n| given.contains(n))
                .map(|n| format!("R{n}"));
        }
        let value = serde_json::to_value(&review).unwrap_or(Value::Null);
        let keys = self
            .store
            .record_review(
                task.id,
                &value,
                &model.id,
                pass_of(task),
                task.review_rounds,
                match review.verdict {
                    ReviewVerdict::Approve => "approve",
                    ReviewVerdict::Changes => "changes",
                },
                &review.findings,
            )
            .await?;
        Self::mirror(&wt, "review", &value);
        if review.verdict == ReviewVerdict::Changes {
            if task.review_rounds + 1 > self.config.limits.review_rounds {
                let why = format!(
                    "the reviewer still asks for changes after {} rounds:\n{}",
                    self.config.limits.review_rounds,
                    findings_text(&review, &keys)
                );
                // D50: a fresh pass on the frontier tier, with every finding kept.
                return self.auto_pass(task, repo, &why, true).await;
            }
            // One transaction: a crash cannot count the round without entering it (review I1).
            self.store
                .transition_and(
                    task.id,
                    TaskState::Implementing,
                    "review asked for changes",
                    Also::NewRound,
                )
                .await?;
            return Ok(TaskState::Implementing);
        }
        let sha = match self.git.head(&wt).await {
            Ok(s) => s,
            Err(e) => {
                return self
                    .transient(task, repo, "could not read HEAD", &e.to_string())
                    .await;
            }
        };
        self.store
            .record_output(
                task.id,
                "approved_head",
                &json!({
                    "pass": u64::from(task.reopen_count) + 1,
                    "round": task.review_rounds,
                    "head": sha,
                    "model": model.id,
                }),
            )
            .await?;
        // Every approval is recorded; the policy decides how many distinct
        // models the head needs before the PR opens (D59).
        self.store
            .record_output(
                task.id,
                "approval",
                &json!({
                    "pass": u64::from(task.reopen_count) + 1,
                    "round": task.review_rounds,
                    "model": model.id,
                    "provider": model.provider_key(),
                }),
            )
            .await?;
        let need = usize::from(self.policy.approvals_needed(repo));
        if self.approvals(task).await?.len() < need {
            return self
                .go(
                    task.id,
                    TaskState::Reviewing,
                    &format!(
                        "{} approved; another reviewer checks before the PR opens",
                        model.id
                    ),
                )
                .await;
        }
        self.open_pr(task, repo, &wt, &review, plan.as_ref()).await
    }

    /// This pass's reproduction really failed before the change (review I5).
    async fn reproduced_this_pass(&self, task: &TaskRow) -> Result<bool, PipelineError> {
        Ok(self
            .store
            .last_output(task.id, "repro_result")
            .await?
            .is_some_and(|r| {
                r["pass"].as_u64() == Some(u64::from(task.reopen_count) + 1)
                    && r["reproduced"].as_bool() == Some(true)
            }))
    }

    /// Approvals recorded in the current round and pass (D49).
    async fn approvals(&self, task: &TaskRow) -> Result<Vec<Value>, PipelineError> {
        Ok(self
            .store
            .recent_outputs(task.id, "approval", u32::MAX)
            .await?
            .into_iter()
            .filter(|a| {
                a["pass"].as_u64() == Some(u64::from(task.reopen_count) + 1)
                    && a["round"].as_u64() == Some(u64::from(task.review_rounds))
            })
            .collect())
    }

    /// Approvals of the current pass, round and HEAD (issue #13): what a
    /// resume after a failed push or `pr_create` may reuse instead of asking
    /// the reviewer again.
    async fn approved_heads(
        &self,
        task: &TaskRow,
        head: &str,
    ) -> Result<Vec<Value>, PipelineError> {
        Ok(self
            .store
            .recent_outputs(task.id, "approved_head", u32::MAX)
            .await?
            .into_iter()
            .filter(|a| {
                a["pass"].as_u64() == Some(u64::from(task.reopen_count) + 1)
                    && a["round"].as_u64() == Some(u64::from(task.review_rounds))
                    && a["head"].as_str() == Some(head)
            })
            .collect())
    }

    /// If the current HEAD already has enough approvals (one, or two before an
    /// auto-merge) for this pass and round, goes straight to `open_pr`
    /// instead of claiming a reviewer (issue #13). `None` falls through to a
    /// normal review, including when the model or the review can no longer be
    /// found.
    async fn resume_approved(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        wt: &Path,
        head: &str,
    ) -> Result<Option<TaskState>, PipelineError> {
        let need = usize::from(self.policy.approvals_needed(repo));
        let heads = self.approved_heads(task, head).await?;
        if heads.len() < need {
            return Ok(None);
        }
        let Some(review) = self
            .store
            .last_output(task.id, "review")
            .await?
            .and_then(|v| serde_json::from_value::<ReviewOutput>(v).ok())
        else {
            return Ok(None);
        };
        let plan = self.plan_output(task.id).await?;
        Ok(Some(
            self.open_pr(task, repo, wt, &review, plan.as_ref()).await?,
        ))
    }

    /// The first approval of the current round and pass, if one is waiting for
    /// the second reviewer (D49).
    async fn first_approval(&self, task: &TaskRow) -> Result<Option<Value>, PipelineError> {
        Ok(self
            .store
            .last_output(task.id, "approval")
            .await?
            .filter(|a| {
                a["pass"].as_u64() == Some(u64::from(task.reopen_count) + 1)
                    && a["round"].as_u64() == Some(u64::from(task.review_rounds))
            }))
    }

    /// The providers a reviewer should differ from, most important first: the
    /// first approver's for a second review (D49), then always the
    /// implementer's (spec §3.2), so a second review avoids both.
    async fn review_avoid(&self, task: &TaskRow) -> Result<Vec<String>, PipelineError> {
        let mut avoid = Vec::new();
        if let Some(a) = self.first_approval(task).await?
            && let Some(p) = a["provider"].as_str()
        {
            avoid.push(p.to_string());
        }
        if let Some(p) = self.implementer_provider(task.id).await? {
            avoid.push(p);
        }
        Ok(avoid)
    }

    async fn pr_body(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        wt: &Path,
        review: &ReviewOutput,
        plan: Option<&PlanOutput>,
    ) -> Result<String, PipelineError> {
        let mut b = format!(
            "{}\n\n",
            crate::tracker::pr_first_line(
                repo.tracker_kind(),
                task.issue_number,
                task.issue_key.as_deref(),
                &task.issue_url,
            )
        );
        if let Some(p) = plan {
            b.push_str(&format!("## Plan\n\n{}\n\n", p.summary));
        }
        b.push_str("## Routing\n\n");
        match self.store.routing_decisions(task.id).await?.last() {
            Some((Some(jev), Some(v), _, _)) => {
                b.push_str(&format!(
                    "Jev ({jev}): kind {}, difficulty {:.2}, scope {:.2}.\n",
                    v["task_kind"].as_str().unwrap_or("?"),
                    v["difficulty"].as_f64().unwrap_or_default(),
                    v["scope"].as_f64().unwrap_or_default()
                ));
            }
            _ => b.push_str("Jev was unavailable.\n"),
        }
        let runs = self.store.stage_runs(task.id).await?;
        for r in &runs {
            if r.model_id.is_empty() {
                continue;
            }
            match self.config.models.iter().find(|m| m.id == r.model_id) {
                Some(m) => b.push_str(&format!(
                    "- {}: `{}` ({})\n",
                    r.stage,
                    r.model_id,
                    tier_name(m.tier)
                )),
                None => b.push_str(&format!("- {}: `{}`\n", r.stage, r.model_id)),
            }
        }
        if let Some(cost) = crate::cost::summary(&runs) {
            b.push_str(&format!("\nCost: {cost}.\n"));
        }
        b.push('\n');
        b.push_str("## Checks\n\n");
        let gates = Self::gate_commands(repo, plan);
        for g in &gates {
            b.push_str(&format!("- `{g}`: passed\n"));
        }
        // The round's risk checks passed too, or the PR would not open.
        if let Some(detected) = self.risk_of_round(task).await?
            && let Ok(policy) = risk::resolve(repo.risk.as_ref())
        {
            for (c, names) in risk_checks(&policy, &detected, &gates) {
                b.push_str(&format!("- `{c}`: passed (risk: {})\n", names.join(", ")));
            }
        }
        // Spec §5: the rules the round's review was given; nothing without.
        if let Some(given) = self.rules_given(task).await? {
            b.push_str(&format!("\nRules: {given}\n"));
        }
        // Only a bugfix runs the repro before the fix, and only a failure of the
        // check itself (not a missing script) reproduces: never claim otherwise.
        let reproduced = self.reproduced_this_pass(task).await?;
        if reproduced && let Some(cmd) = plan.and_then(repro) {
            b.push_str(&format!(
                "\nReproduction: `{cmd}` failed before the change and passes after it.\n"
            ));
        }
        // The PR opens right after the final round's review, so the event of
        // the task's current (pass, round) is the change being proposed.
        if let Some(detected) = self.risk_of_round(task).await?
            && let Ok(policy) = risk::resolve(repo.risk.as_ref())
        {
            let implementer = self.implementer_provider(task.id).await?;
            let frontier_ok =
                risk::risky_review_tier(&self.config.models, implementer.as_deref(), true)
                    .is_some();
            // What actually reviewed: the round's last review run.
            let ran = runs
                .iter()
                .rfind(|r| r.stage == "review")
                .and_then(|r| self.config.models.iter().find(|m| m.id == r.model_id))
                .map(|m| m.tier);
            if let Some(section) = risk_section(&policy, &detected, frontier_ok, ran) {
                b.push('\n');
                b.push_str(&section);
            }
        }
        let base = self.pass_base(task, repo).await?;
        let changed = self.git.changed_files(wt, &base).await.unwrap_or_default();
        let diff = self.git.diff(wt, &base).await.unwrap_or_default();
        let weakened = weakened_tests(&changed, &diff);
        if !weakened.is_empty() {
            b.push_str("\n## Test changes to check\n\nThis change deletes or disables tests:\n");
            for w in weakened {
                b.push_str(&format!("- {w}\n"));
            }
        }
        let final_round = self
            .store
            .final_round_findings(task.id, pass_of(task))
            .await?;
        if let Some(notes) = review_notes(review, &final_round) {
            b.push_str(&notes);
        }
        b.push_str(&format!(
            "\n---\nOpened by Provefab (task {}). Details: `provefab log {}`.\n",
            task.id, task.id
        ));
        Ok(b)
    }

    async fn open_pr(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        wt: &Path,
        review: &ReviewOutput,
        plan: Option<&PlanOutput>,
    ) -> Result<TaskState, PipelineError> {
        let branch = task_branch(task);
        if let Err(e) = self.git.push(wt, &branch).await {
            return self.failed(task, repo, "push failed", &e).await;
        }
        let body = self.pr_body(task, repo, wt, review, plan).await?;
        let title = crate::tracker::pr_title(&task.title, task.issue_key.as_deref());
        let url = match self
            .hub
            .pr_create(&repo.slug, &branch, &repo.base, &title, &body)
            .await
        {
            Ok(u) => u,
            Err(e) => {
                return self.failed(task, repo, "could not open the PR", &e).await;
            }
        };
        self.store
            .write_with_events(
                task.id,
                Write::SetPr {
                    url: &url,
                    state: "open",
                },
                &[Event::PrOpened {
                    url: url.clone(),
                    head: self.git.head(wt).await.ok(),
                    base: repo.base.clone(),
                    pass: pass_of(task),
                }],
            )
            .await?;
        self.store
            .transition(task.id, TaskState::PrOpen, &format!("opened {url}"))
            .await?;
        let in_pr = format!("{}:in-pr", repo.label);
        self.relabel(
            task.id,
            &repo.slug,
            task.issue_number,
            &[&in_pr],
            &[&repo.label],
        )
        .await?;
        let approvals = self
            .approvals(task)
            .await?
            .iter()
            .map(|a| Approval {
                model: a["model"].as_str().unwrap_or_default().to_string(),
                provider: a["provider"].as_str().unwrap_or_default().to_string(),
            })
            .collect();
        let tools = PipelineTools {
            p: self,
            task,
            repo,
            wt,
            url: &url,
        };
        let said = self
            .policy
            .after_pr_opened(PrOpened {
                task,
                repo,
                url: &url,
                approvals,
                reproduced: self.reproduced_this_pass(task).await?,
                tools: &tools,
            })
            .await?;
        self.tell(task.id, &repo.slug, task.issue_number, &said)
            .await?;
        Ok(TaskState::PrOpen)
    }
}

/// The pipeline's side of `MergeTools`: git on the task's worktree, and a
/// merge pinned to the checked head.
struct PipelineTools<'a, R, O, H> {
    p: &'a Pipeline<R, O, H>,
    task: &'a TaskRow,
    repo: &'a RepoConfig,
    wt: &'a Path,
    url: &'a str,
}

impl<R, O, H> MergeTools for PipelineTools<'_, R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    fn fetch_base(&self) -> BoxFuture<'_, Result<(), ForgeError>> {
        Box::pin(async move {
            let checkout = self.p.checkout(self.repo);
            let lock = self.p.repo_lock(self.repo);
            let _guard = lock.lock().await;
            self.p.git.fetch(&checkout).await
        })
    }

    fn base_ref(&self) -> BoxFuture<'_, String> {
        Box::pin(self.p.base_ref(self.repo))
    }

    fn head(&self) -> BoxFuture<'_, Result<String, ForgeError>> {
        Box::pin(self.p.git.head(self.wt))
    }

    fn changed_files<'a>(
        &'a self,
        base: &'a str,
    ) -> BoxFuture<'a, Result<Vec<crate::forge::Change>, ForgeError>> {
        Box::pin(self.p.git.changed_files(self.wt, base))
    }

    fn diff<'a>(&'a self, base: &'a str) -> BoxFuture<'a, Result<String, ForgeError>> {
        Box::pin(self.p.git.diff(self.wt, base))
    }

    fn numstat<'a>(
        &'a self,
        base: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(Option<u32>, Option<u32>, String)>, ForgeError>> {
        Box::pin(self.p.git.numstat(self.wt, base))
    }

    fn is_ancestor<'a>(
        &'a self,
        ancestor: &'a str,
        of: &'a str,
    ) -> BoxFuture<'a, Result<bool, ForgeError>> {
        Box::pin(self.p.git.is_ancestor(self.wt, ancestor, of))
    }

    fn repo_is_public(&self) -> BoxFuture<'_, Result<bool, ForgeError>> {
        Box::pin(self.p.hub.repo_is_public(&self.repo.slug))
    }

    fn merge<'a>(&'a self, head: &'a str) -> BoxFuture<'a, Result<MergeOutcome, PipelineError>> {
        Box::pin(async move {
            // Only the reviewed head: a later push to the branch is not merged (review I6).
            match self.p.hub.pr_merge(&self.repo.slug, self.url, head).await {
                Ok(()) => {
                    // Tells automatic merges from merges by a person (`provefab stats`).
                    self.p
                        .store
                        .record_output(self.task.id, "auto_merged", &json!({"head": head}))
                        .await?;
                    let status = self.p.hub.pr_status(&self.repo.slug, self.url).await?;
                    self.p
                        .record_merge(
                            self.task,
                            self.repo,
                            status.merge_sha.as_deref(),
                            status.base_ref.as_deref(),
                            status.commit_count,
                            // The head just merged, not a re-read that may lag.
                            Some(head),
                        )
                        .await?;
                    Ok(MergeOutcome::Merged)
                }
                Err(e) => {
                    // Kept in the log the comment points to (review minor).
                    self.p
                        .store
                        .transition(
                            self.task.id,
                            TaskState::PrOpen,
                            &format!("auto-merge of {} failed: {e}", self.url),
                        )
                        .await?;
                    Ok(MergeOutcome::Failed(e.to_string()))
                }
            }
        })
    }
}

/// Added plus removed lines across `git diff --numstat` rows. A binary file
/// (`None`) counts as `u32::MAX` so a change touching one never auto-merges.
pub fn numstat_lines(rows: &[(Option<u32>, Option<u32>, String)]) -> u32 {
    rows.iter()
        .fold(0u32, |total, (added, removed, _)| match (added, removed) {
            (Some(a), Some(r)) => total.saturating_add(a.saturating_add(*r)),
            _ => u32::MAX,
        })
}

#[cfg(test)]
mod tests {
    /// A detail the public text already ends with is not repeated.
    #[test]
    fn reason_text_does_not_repeat_the_detail() {
        assert_eq!(reason_text("out of scope: x", "x"), "out of scope: x");
        assert_eq!(
            reason_text("gates failed", "cargo test"),
            "gates failed: cargo test"
        );
        assert_eq!(reason_text("gates failed", ""), "gates failed");
    }

    use super::*;

    fn cat(name: &str, checks: &[&str], frontier: bool) -> risk::Category {
        risk::Category {
            name: name.into(),
            paths: vec![],
            checks: checks.iter().map(|c| c.to_string()).collect(),
            frontier,
        }
    }

    fn det(name: &str, paths: &[&str]) -> Detected {
        Detected {
            name: name.into(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn risk_section_lists_paths_checks_and_tier() {
        let policy = risk::Policy {
            categories: vec![cat("migrations", &["./c.sh"], true), cat("ci", &[], false)],
        };
        let s = risk_section(
            &policy,
            &[det("migrations", &["a", "b"]), det("ci", &["x.yml"])],
            true,
            None,
        )
        .unwrap();
        assert_eq!(
            s,
            "## Risk\n\n- migrations: `a`, `b` · checks added: `./c.sh` · reviewer: frontier\n- ci: `x.yml` · reviewer: standard\n\n"
        );
        let s = risk_section(
            &policy,
            &[det("migrations", &["a"]), det("ci", &["x.yml"])],
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            s,
            "## Risk\n\n- migrations: `a` · checks added: `./c.sh` · reviewer: standard (no frontier reviewer from another provider is configured)\n- ci: `x.yml` · reviewer: standard\n\n"
        );
    }

    #[test]
    fn risk_section_truncates_after_five_paths() {
        let policy = risk::Policy {
            categories: vec![cat("ci", &[], true)],
        };
        let paths = ["1", "2", "3", "4", "5", "6", "7"];
        let s = risk_section(&policy, &[det("ci", &paths)], true, None).unwrap();
        assert!(
            s.contains("- ci: `1`, `2`, `3`, `4`, `5`, and 2 more · reviewer: frontier\n"),
            "{s}"
        );
        let five = risk_section(&policy, &[det("ci", &paths[..5])], true, None).unwrap();
        assert!(!five.contains("more"), "{five}");
    }

    #[test]
    fn risk_section_unknown_and_empty() {
        let policy = risk::Policy::default();
        assert_eq!(
            risk_section(&policy, &risk::unknown(), true, None).unwrap(),
            "## Risk\n\n- unknown: the changed files could not be computed · reviewer: frontier\n\n"
        );
        assert_eq!(
            risk_section(&policy, &risk::unknown(), false, None).unwrap(),
            "## Risk\n\n- unknown: the changed files could not be computed · reviewer: standard (no frontier reviewer from another provider is configured)\n\n"
        );
        assert_eq!(risk_section(&policy, &[], true, None), None);
    }

    /// The line states the tier that actually reviewed; the suffix says the
    /// risk rule found no frontier reviewer from another provider.
    #[test]
    fn risk_section_states_the_tier_that_ran() {
        const NO: &str = " (no frontier reviewer from another provider is configured)";
        let policy = risk::Policy {
            categories: vec![cat("migrations", &[], true), cat("ci", &[], false)],
        };
        let line = |frontier_ok, ran| {
            risk_section(
                &policy,
                &[det("migrations", &["a"]), det("ci", &["x.yml"])],
                frontier_ok,
                Some(ran),
            )
            .unwrap()
        };
        let body = |m: &str, c: &str| {
            format!(
                "## Risk\n\n- migrations: `a` · reviewer: {m}\n- ci: `x.yml` · reviewer: {c}\n\n"
            )
        };
        assert_eq!(
            line(false, Tier::Frontier),
            body(&format!("frontier{NO}"), "frontier")
        );
        assert_eq!(
            line(false, Tier::Standard),
            body(&format!("standard{NO}"), "standard")
        );
        assert_eq!(line(true, Tier::Frontier), body("frontier", "frontier"));
        assert_eq!(
            risk_section(&policy, &risk::unknown(), false, Some(Tier::Frontier)).unwrap(),
            format!(
                "## Risk\n\n- unknown: the changed files could not be computed · reviewer: frontier{NO}\n\n"
            )
        );
    }

    #[test]
    fn review_notes_and_findings_text_show_the_rule_a_finding_cites() {
        let review = ReviewOutput {
            verdict: ReviewVerdict::Changes,
            findings: vec![Finding {
                file: "src/a.rs".into(),
                line: Some(4),
                severity: Severity::Blocking,
                text: "uses anyhow".into(),
                rule: Some("R3".into()),
            }],
        };
        assert_eq!(
            findings_text(&review, &["F2".to_string()]),
            "- F2 · R3 · blocking · `src/a.rs:4` · uses anyhow"
        );
        let row = FindingRow {
            id: 2,
            task_id: 1,
            key: "F2".into(),
            pass: 1,
            round: 0,
            reviewer_model: "std-codex".into(),
            severity: "blocking".into(),
            file: "src/a.rs".into(),
            line: Some(4),
            text: "uses anyhow".into(),
            rule: Some("R3".into()),
            event_id: 1,
        };
        let notes = review_notes(&review, &[row]).unwrap();
        assert!(
            notes.contains("- F2 · R3 · blocking · `src/a.rs:4` · uses anyhow (std-codex)"),
            "{notes}"
        );
    }

    #[test]
    fn review_notes_without_keys_keep_the_findings_and_drop_the_command_help() {
        let review = ReviewOutput {
            verdict: ReviewVerdict::Approve,
            findings: vec![Finding {
                file: "src/a.rs".into(),
                line: Some(4),
                severity: Severity::Minor,
                text: "typo".into(),
                rule: None,
            }],
        };
        let row = FindingRow {
            id: 3,
            task_id: 1,
            key: "F3".into(),
            pass: 1,
            round: 0,
            reviewer_model: "std-codex".into(),
            severity: "minor".into(),
            file: "src/a.rs".into(),
            line: Some(4),
            text: "typo".into(),
            rule: None,
            event_id: 1,
        };
        let keyed = review_notes(&review, &[row]).unwrap();
        assert!(
            keyed.contains("- F3 · minor · `src/a.rs:4` · typo (std-codex)"),
            "{keyed}"
        );
        assert!(keyed.contains("Reply `/provefab F3 rejected`"), "{keyed}");
        // Approved before the record existed: no review event, so no keys.
        let plain = review_notes(&review, &[]).unwrap();
        assert!(plain.contains("- minor · `src/a.rs:4` · typo"), "{plain}");
        assert!(!plain.contains("/provefab"), "{plain}");
        let none = ReviewOutput {
            verdict: ReviewVerdict::Approve,
            findings: vec![],
        };
        assert_eq!(review_notes(&none, &[]), None);
    }

    #[test]
    fn numstat_lines_sums_rows_and_binary_never_merges() {
        assert_eq!(numstat_lines(&[]), 0);
        assert_eq!(
            numstat_lines(&[
                (Some(3), Some(2), "a".to_string()),
                (Some(1), Some(0), "b".to_string()),
            ]),
            6
        );
        assert_eq!(
            numstat_lines(&[
                (Some(1), Some(1), "a".to_string()),
                (None, None, "img.png".to_string()),
            ]),
            u32::MAX
        );
        assert_eq!(
            numstat_lines(&[
                (Some(u32::MAX - 1), Some(0), "a".to_string()),
                (Some(2), Some(0), "b".to_string()),
            ]),
            u32::MAX
        );
    }

    #[test]
    fn ladder_retries_once_then_escalates_then_gives_up() {
        assert_eq!(ladder(1, false, false), Ladder::Retry);
        assert_eq!(ladder(2, false, false), Ladder::Escalate);
        assert_eq!(ladder(3, true, false), Ladder::GiveUp);
    }

    #[test]
    fn ladder_keeps_retrying_while_the_score_improves() {
        assert_eq!(ladder(2, false, true), Ladder::Retry);
        assert_eq!(ladder(3, false, true), Ladder::Retry);
        assert_eq!(ladder(4, false, true), Ladder::Escalate);
        assert_eq!(ladder(4, true, true), Ladder::GiveUp);
        assert_eq!(ladder(2, true, true), Ladder::Retry);
    }
}
