//! `provefab run`: poll intake, check replies, and drive tasks, several at a
//! time (spec §2.2, §3.3). `--dry-run` classifies and routes only (§3.4 item 5).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::task::JoinSet;

use crate::agents::StageRunner;
use crate::config::{Config, RepoConfig};
use crate::forge::ForgeError;
use crate::intake::poll;
use crate::jevq::IssueContext;
use crate::pipeline::{Pipeline, PipelineError};
use crate::policy::PeriodicTools;
use crate::ports::{Hub, Oracle, Tracker};
use crate::router::{Availability, fallback_tiers, select, stage_tiers};
use crate::task::{TaskKind, TaskState, Tier};

/// How often the loop wakes when nothing finishes.
const TICK: Duration = Duration::from_secs(5);

/// States a worker slot can move forward.
const ACTIVE: [TaskState; 6] = [
    TaskState::Queued,
    TaskState::Classified,
    TaskState::Planning,
    TaskState::Implementing,
    TaskState::Gating,
    TaskState::Reviewing,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOptions {
    pub workers: usize,
    /// Poll once, drive everything that can move, then return.
    pub once: bool,
}

/// The labels the pipeline moves issues between (spec §3.2).
pub fn provefab_labels(repo: &RepoConfig) -> [(String, &'static str, &'static str); 4] {
    [
        (
            format!("{}:in-pr", repo.label),
            "0e8a16",
            "Provefab opened a pull request",
        ),
        (
            format!("{}:needs-info", repo.label),
            "fbca04",
            "Provefab needs more detail",
        ),
        (
            format!("{}:failed", repo.label),
            "d93f0b",
            "Provefab stopped; see its comment",
        ),
        (
            format!("{}:merged", repo.label),
            "6f42c1",
            "Provefab's pull request was merged",
        ),
    ]
}

fn repo_of<'a>(config: &'a Config, slug: &str) -> Option<&'a RepoConfig> {
    config
        .repos
        .iter()
        .find(|r| r.slug.eq_ignore_ascii_case(slug))
}

/// Longest wait before driving a task whose last step errored again (review I4).
const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// The kind of the scheduler's own maintenance row: one per periodic call
/// (repository rules plan decision 13).
pub const PERIODIC: &str = "periodic";

/// When `ReviewPolicy::periodic` runs (repository rules spec §7, plan
/// decision 14): at startup for a repository whose last call is a day old or
/// more, then on the first tick of each new local day; never while that
/// repository's previous call still runs.
#[derive(Debug, Default)]
pub struct PeriodicClock {
    prev_tick: Option<i64>,
    running: HashSet<String>,
}

impl PeriodicClock {
    /// No tick yet: `due` reads the last recorded call.
    pub fn starting(&self) -> bool {
        self.prev_tick.is_none()
    }

    /// Whether `slug`'s call is due at `now`; marks it running when it is.
    pub fn due(
        &mut self,
        slug: &str,
        last_call: Option<i64>,
        now: i64,
        day: impl Fn(i64) -> i64,
    ) -> bool {
        if self.running.contains(slug) {
            return false;
        }
        let due = match self.prev_tick {
            None => last_call.is_none_or(|at| now - at >= 86_400),
            Some(prev) => day(now) != day(prev),
        };
        if due {
            self.running.insert(slug.to_string());
        }
        due
    }

    /// Ends a tick: the next one compares its day with this one's.
    pub fn ticked(&mut self, now: i64) {
        self.prev_tick = Some(now);
    }

    pub fn finished(&mut self, slug: &str) {
        self.running.remove(slug);
    }
}

/// The calendar day of `at` (unix seconds) in the system's time zone.
pub fn local_day(at: i64) -> i64 {
    (at + utc_offset(at)).div_euclid(86_400)
}

// `time_t` and `c_long` are `i64` on 64-bit macOS and Linux.
#[allow(clippy::unnecessary_cast)]
fn utc_offset(at: i64) -> i64 {
    let t = at as libc::time_t;
    // SAFETY: `localtime_r` reads `t` and writes only into `tm`, which we
    // own; an all-zero `tm` is a valid value of this plain C struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if ok { tm.tm_gmtoff as i64 } else { 0 }
}

/// One periodic call: the policy's `Err` is logged and recorded, redacted
/// (pre-flight S1), never fatal.
async fn periodic_call<R, O, H>(p: &Pipeline<R, O, H>, repo: &RepoConfig)
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    let tools = p.maintenance(repo);
    let outcome = match p.policy.periodic(repo, &tools).await {
        Ok(()) => "ok".to_string(),
        Err(e) => {
            let e = crate::rules::redact_credentials(&e);
            eprintln!("provefab: periodic work for {} failed: {e}", repo.slug);
            format!("error: {e}")
        }
    };
    if let Err(e) = tools.record_run(PERIODIC, &outcome, None, None).await {
        eprintln!(
            "provefab: could not record the periodic call for {}: {e}",
            repo.slug
        );
    }
}

/// Tasks listed in `states`, or none (logged) when the store cannot answer:
/// one bad read must not end `provefab run` (review I4).
async fn listed<R, O, H>(p: &Pipeline<R, O, H>, states: &[TaskState]) -> Vec<crate::store::TaskRow>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    p.store.tasks_in(states).await.unwrap_or_else(|e| {
        eprintln!("provefab: could not list tasks: {e}");
        Vec::new()
    })
}

/// Runs until `stop` resolves (Ctrl-C in `main`), or, with `once`, until
/// nothing can move. On stop, running stages are cancelled: dropping them
/// kills their process trees (D26), and the tasks resume on the next run.
pub async fn run<R, O, H>(
    p: Arc<Pipeline<R, O, H>>,
    opts: RunOptions,
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), PipelineError>
where
    R: StageRunner + Send + Sync + 'static,
    O: Oracle + Send + Sync + 'static,
    H: Hub + Send + Sync + 'static,
{
    tokio::pin!(stop);
    for repo in &p.config.repos {
        for (name, color, text) in provefab_labels(repo) {
            if let Err(e) = p.hub.ensure_label(&repo.slug, &name, color, text).await {
                eprintln!(
                    "provefab: could not create label {name} on {}: {e}",
                    repo.slug
                );
            }
        }
        // Risk labels up front too, so a round's label edit rarely meets one
        // that does not exist yet (the pipeline still ensures each on use).
        let risk: Vec<String> = crate::risk::resolve(repo.risk.as_ref())
            .map(|policy| policy.categories.into_iter().map(|c| c.name).collect())
            .unwrap_or_default();
        for category in risk
            .iter()
            .map(String::as_str)
            .chain([crate::risk::UNKNOWN])
        {
            let (name, text) = crate::risk::label(&repo.label, category);
            if let Err(e) = p
                .hub
                .ensure_label(&repo.slug, &name, crate::risk::LABEL_COLOR, &text)
                .await
            {
                eprintln!(
                    "provefab: could not create label {name} on {}: {e}",
                    repo.slug
                );
            }
        }
    }
    let workers = opts.workers.max(1);
    let mut next_poll: HashMap<String, Instant> = HashMap::new();
    let mut in_flight: HashSet<i64> = HashSet::new();
    let mut set: JoinSet<Result<TaskState, PipelineError>> = JoinSet::new();
    // Which task each spawned runner drives, so a panic can be traced back (review I5).
    let mut running: HashMap<tokio::task::Id, i64> = HashMap::new();
    // Errored tasks wait before the next try (under `once`, the run ends first).
    let mut failures: HashMap<i64, u32> = HashMap::new();
    let mut retry_at: HashMap<i64, Instant> = HashMap::new();
    let mut polled_once = false;
    let mut clock = PeriodicClock::default();
    let mut periodic: JoinSet<()> = JoinSet::new();
    let mut periodic_of: HashMap<tokio::task::Id, String> = HashMap::new();
    loop {
        p.refresh_prices().await;
        // Repository rules spec §7: the policy's daily work, beside the queue.
        while let Some(done) = periodic.try_join_next_with_id() {
            let id = match &done {
                Ok((id, ())) => *id,
                Err(e) => e.id(),
            };
            if let Some(slug) = periodic_of.remove(&id) {
                clock.finished(&slug);
            }
            if let Err(e) = done {
                eprintln!("provefab: a periodic call panicked: {}", panic_text(e));
            }
        }
        let t = crate::store::now();
        for repo in &p.config.repos {
            let last = if clock.starting() {
                match p.store.last_maintenance_run(&repo.slug, PERIODIC).await {
                    Ok(run) => run.map(|r| r.started_at),
                    Err(e) => {
                        eprintln!(
                            "provefab: could not read the last periodic call of {}: {e}",
                            repo.slug
                        );
                        None
                    }
                }
            } else {
                None
            };
            if clock.due(&repo.slug, last, t, local_day) {
                let (pc, rc) = (p.clone(), repo.clone());
                let handle = periodic.spawn(async move { periodic_call(&pc, &rc).await });
                periodic_of.insert(handle.id(), repo.slug.clone());
            }
        }
        clock.ticked(t);
        if !(opts.once && polled_once) {
            p.retry_pending(&in_flight).await;
            // Post-merge rows advance on every poll, whatever the task's state: a
            // reopen moves the task out of `PrOpen` but its check must still finish
            // (final review F2). Tasks a worker is driving are included on purpose:
            // a check only reads the task and takes the repo lock for git work.
            let post_merge: Vec<crate::store::TaskRow> =
                match p.store.tasks_with_open_post_merge().await {
                    Ok(ids) => {
                        let mut tasks = Vec::new();
                        for id in ids {
                            match p.store.task(id).await {
                                Ok(Some(t)) => tasks.push(t),
                                Ok(None) => {}
                                Err(e) => eprintln!("provefab: task {id}: {e}"),
                            }
                        }
                        tasks
                    }
                    Err(e) => {
                        eprintln!("provefab: could not list post-merge checks: {e}");
                        Vec::new()
                    }
                };
            for repo in &p.config.repos {
                let due = next_poll
                    .get(&repo.slug)
                    .is_none_or(|t| Instant::now() >= *t);
                if !due {
                    continue;
                }
                next_poll.insert(repo.slug.clone(), Instant::now() + repo.poll_interval);
                match poll(&p.hub, repo, &p.store).await {
                    Ok(ids) if !ids.is_empty() => {
                        eprintln!(
                            "provefab: {} new issue(s) queued from {}",
                            ids.len(),
                            repo.slug
                        )
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("provefab: intake for {} failed: {e}", repo.slug),
                }
                for t in listed(&p, &[TaskState::NeedsInfo]).await {
                    // A task a runner just parked may still be posting its question (review I2).
                    if !in_flight.contains(&t.id)
                        && repo_of(&p.config, &t.repo).is_some_and(|r| r.slug == repo.slug)
                    {
                        let result = p.step(t.id).await;
                        if should_report(t.state, &result) {
                            report(t.id, result);
                        }
                    }
                }
                // Once per task: each task belongs to one repository. The hourly
                // throttle below is for the reopen watch only (spec section 5).
                for t in &post_merge {
                    if repo_of(&p.config, &t.repo).is_some_and(|r| r.slug == repo.slug)
                        && let Err(e) = p.process_post_merge(t.id).await
                    {
                        eprintln!("provefab: post-merge verification for task {}: {e}", t.id);
                    }
                }
                // Merged, closed or reopened: the PR watcher (D52).
                for t in listed(&p, &[TaskState::PrOpen]).await {
                    if !in_flight.contains(&t.id)
                        && repo_of(&p.config, &t.repo).is_some_and(|r| r.slug == repo.slug)
                    {
                        // After the merge only a reopen matters: check hourly (Plan 4 review I2).
                        if matches!(t.pr_state.as_deref(), Some("merged" | "done")) {
                            let recent = p
                                .store
                                .last_output(t.id, "watched_at")
                                .await
                                .ok()
                                .flatten()
                                .and_then(|w| w["at"].as_i64())
                                .is_some_and(|at| crate::store::now() - at < 3600);
                            if recent {
                                continue;
                            }
                            let stamp = serde_json::json!({"at": crate::store::now()});
                            if let Err(e) = p.store.record_output(t.id, "watched_at", &stamp).await
                            {
                                eprintln!("provefab: task {}: {e}", t.id);
                            }
                        }
                        match p.watch_pr(t.id).await {
                            Ok(TaskState::PrOpen) => {}
                            other => report(t.id, other),
                        }
                    }
                }
            }
            polled_once = true;
        }
        for t in listed(&p, &[TaskState::Waiting]).await {
            if !in_flight.contains(&t.id) {
                let result = p.step(t.id).await;
                if should_report(t.state, &result) {
                    report(t.id, result);
                }
            }
        }
        for t in listed(&p, &ACTIVE).await {
            if set.len() >= workers {
                break;
            }
            let backing_off = retry_at.get(&t.id).is_some_and(|at| Instant::now() < *at);
            if backing_off || in_flight.contains(&t.id) {
                continue;
            }
            in_flight.insert(t.id);
            let p = p.clone();
            let id = t.id;
            let handle = set.spawn(async move { p.drive(id).await });
            running.insert(handle.id(), id);
        }
        if opts.once && set.is_empty() {
            // `--once` ends with the periodic calls it started.
            while periodic.join_next().await.is_some() {}
            return Ok(());
        }
        tokio::select! {
            Some(done) = set.join_next_with_id() => {
                let (tid, result) = match done {
                    Ok((tid, r)) => (tid, Ok(r)),
                    Err(e) => (e.id(), Err(e)),
                };
                let Some(id) = running.remove(&tid) else { continue };
                in_flight.remove(&id);
                match result {
                    Ok(Ok(state)) => {
                        failures.remove(&id);
                        retry_at.remove(&id);
                        report(id, Ok(state));
                    }
                    Ok(Err(e)) => {
                        let n = failures.entry(id).or_insert(0);
                        *n += 1;
                        let wait = TICK.saturating_mul(1 << (*n).min(8)).min(MAX_BACKOFF);
                        retry_at.insert(id, Instant::now() + wait);
                        eprintln!("provefab: task {id}: {e} (next try in {}s)", wait.as_secs());
                    }
                    Err(join) => {
                        // A bug, not a task outcome: park the task for a person instead of
                        // leaving it half-run and never driven again (review I5).
                        let detail = panic_text(join);
                        eprintln!("provefab: task {id} panicked: {detail}");
                        report(id, p.park(id, "internal error in Provefab", &detail).await);
                    }
                }
            },
            _ = tokio::time::sleep(TICK) => {}
            _ = &mut stop => {
                eprintln!("provefab: stopping; running stages are cancelled and resume on the next run");
                set.abort_all();
                periodic.abort_all();
                while set.join_next().await.is_some() {}
                while periodic.join_next().await.is_some() {}
                return Ok(());
            }
        }
    }
}

fn panic_text(e: tokio::task::JoinError) -> String {
    if e.is_cancelled() {
        return "cancelled".into();
    }
    let payload = e.into_panic();
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

fn report(id: i64, r: Result<TaskState, PipelineError>) {
    match r {
        Ok(state) => eprintln!("provefab: task {id} is {}", state.as_str()),
        Err(e) => eprintln!("provefab: task {id}: {e}"),
    }
}

/// Whether an inline step's outcome is worth a log line: state changes and
/// errors are, an unchanged state (the common case for a stuck task) is not.
fn should_report(before: TaskState, after: &Result<TaskState, PipelineError>) -> bool {
    match after {
        Ok(after) => before != *after,
        Err(_) => true,
    }
}

/// One line per open labelled issue: what Jev said and which models would run.
/// Touches nothing: no store, no worker, no comment, no label (spec §3.4 item 5).
pub async fn dry_run<O: Oracle, T: Tracker>(
    config: &Config,
    tracker: &T,
    oracle: &O,
) -> Result<Vec<String>, ForgeError> {
    let mut lines = Vec::new();
    let avail = Availability::default();
    let now = SystemTime::now();
    let name = |tier: Tier, avoid: &[String]| {
        select(tier, &config.models, &avail, now, avoid)
            .map(|m| m.id.clone())
            .unwrap_or_else(|| "(none)".into())
    };
    for repo in &config.repos {
        for issue in tracker.open_issues(&repo.slug, &repo.label).await? {
            let ctx = IssueContext {
                title: issue.title.clone(),
                body: issue.body.clone(),
                labels: issue.labels.clone(),
                repo_language: None,
                repo_size_kb: None,
            };
            let verdict = oracle.classify(&ctx).await;
            let head = format!(
                "{} {}",
                crate::tracker::repo_ref(&repo.slug, issue.number, issue.key.as_deref()),
                issue.title
            );
            let line = match &verdict {
                Some(v) if v.underspecified > config.jev.underspecified_threshold => format!(
                    "{head}: would ask for more information (underspecified {:.2})",
                    v.underspecified
                ),
                _ => {
                    let (tiers, said) = match &verdict {
                        Some(v) => (
                            stage_tiers(v),
                            format!(
                                "{} ({}): difficulty {:.2} (confidence {:.2}), scope {:.2}, underspecified {:.2}",
                                v.task_kind.as_str(),
                                v.jev_model,
                                v.difficulty,
                                v.difficulty_confidence,
                                v.scope,
                                v.underspecified
                            ),
                        ),
                        None => (
                            fallback_tiers(),
                            format!("{} (Jev unavailable)", TaskKind::Feature.as_str()),
                        ),
                    };
                    let implement = select(tiers.implement, &config.models, &avail, now, &[]);
                    let avoid = implement
                        .map(|m| vec![m.provider_key()])
                        .unwrap_or_default();
                    format!(
                        "{head}: {said} -> plan {}, implement {}, review {}",
                        name(tiers.plan, &[]),
                        name(tiers.implement, &[]),
                        name(tiers.review, &avoid),
                    )
                }
            };
            lines.push(line);
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::{PeriodicClock, local_day, should_report};
    use crate::pipeline::PipelineError;
    use crate::task::TaskState;

    #[test]
    fn should_report_is_silent_when_the_state_is_unchanged() {
        assert!(!should_report(TaskState::Waiting, &Ok(TaskState::Waiting)));
    }

    #[test]
    fn should_report_reports_a_changed_state() {
        assert!(should_report(TaskState::Waiting, &Ok(TaskState::Planning)));
    }

    #[test]
    fn should_report_always_reports_an_error() {
        assert!(should_report(
            TaskState::Waiting,
            &Err(PipelineError::UnknownTask(1))
        ));
    }

    #[test]
    fn the_periodic_call_runs_at_startup_after_a_day_then_once_per_local_day() {
        let day = |t: i64| t.div_euclid(86_400);
        let noon = 20_000 * 86_400 + 43_200;
        let mut c = PeriodicClock::default();
        assert!(c.starting());
        assert!(c.due("a", None, noon, day), "never called");
        assert!(
            !c.due("b", Some(noon - 3_600), noon, day),
            "called an hour ago"
        );
        assert!(c.due("c", Some(noon - 86_400), noon, day), "a day ago");
        c.ticked(noon);
        c.finished("a");
        c.finished("c");
        assert!(!c.starting());
        assert!(!c.due("a", None, noon + 5, day), "same day");
        c.ticked(noon + 5);
        let next = 20_001 * 86_400 + 5;
        assert!(c.due("a", None, next, day), "first tick after midnight");
        assert!(c.due("b", None, next, day));
        c.ticked(next);
        c.finished("b");
        // "a" is still running a day later: never twice at once.
        let after = 20_002 * 86_400 + 5;
        assert!(!c.due("a", None, after, day));
        assert!(c.due("b", None, after, day));
    }

    #[test]
    fn a_local_day_is_a_calendar_day() {
        let t = 1_790_726_400 + 43_200; // 2026-09-30T12:00:00Z
        assert_eq!(local_day(t + 86_400), local_day(t) + 1);
        assert!(local_day(t) >= 20_725 && local_day(t) <= 20_727);
    }
}
