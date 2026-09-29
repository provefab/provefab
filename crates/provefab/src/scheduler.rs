//! `provefab run`: poll intake, check replies, and drive tasks, several at a
//! time (spec §2.2, §3.3). `--dry-run` classifies and routes only (§3.4 item 5).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::task::JoinSet;

use crate::agents::StageRunner;
use crate::config::{Config, RepoConfig};
use crate::forge::ForgeError;
use crate::intake::{IssueSource, poll};
use crate::jevq::IssueContext;
use crate::pipeline::{Pipeline, PipelineError};
use crate::ports::{Hub, Oracle};
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
pub async fn run<R, O, H, S>(
    p: Arc<Pipeline<R, O, H>>,
    source: &S,
    opts: RunOptions,
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), PipelineError>
where
    R: StageRunner + Send + Sync + 'static,
    O: Oracle + Send + Sync + 'static,
    H: Hub + Send + Sync + 'static,
    S: IssueSource,
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
    loop {
        p.refresh_prices().await;
        if !(opts.once && polled_once) {
            p.retry_pending(&in_flight).await;
            for repo in &p.config.repos {
                let due = next_poll
                    .get(&repo.slug)
                    .is_none_or(|t| Instant::now() >= *t);
                if !due {
                    continue;
                }
                next_poll.insert(repo.slug.clone(), Instant::now() + repo.poll_interval);
                match poll(source, repo, &p.store).await {
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
                // Merged, closed or reopened: the PR watcher (D52).
                for t in listed(&p, &[TaskState::PrOpen]).await {
                    if !in_flight.contains(&t.id)
                        && repo_of(&p.config, &t.repo).is_some_and(|r| r.slug == repo.slug)
                    {
                        // Post-merge rows advance every tick; the hourly throttle
                        // below is for the reopen watch only (spec section 5).
                        if let Err(e) = p.process_post_merge(t.id).await {
                            eprintln!("provefab: post-merge verification for task {}: {e}", t.id);
                        }
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
                while set.join_next().await.is_some() {}
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
pub async fn dry_run<O: Oracle, S: IssueSource>(
    config: &Config,
    source: &S,
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
        for issue in source.open_issues(repo).await? {
            let ctx = IssueContext {
                title: issue.title.clone(),
                body: issue.body.clone(),
                labels: issue.labels.clone(),
                repo_language: None,
                repo_size_kb: None,
            };
            let verdict = oracle.classify(&ctx).await;
            let head = format!("{}#{} {}", repo.slug, issue.number, issue.title);
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
    use super::should_report;
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
}
