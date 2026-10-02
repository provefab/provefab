//! Reviewing pull requests that people wrote
//! (docs/specs/2026-10-02-pr-review-design.md). A `pr_review` task fetches
//! the pull request's head, classifies its risk, gives a reviewer the
//! repository rules, records the findings like any review and keeps one
//! summary comment on the pull request. Nothing from the pull request runs,
//! and Provefab never approves, requests changes or merges it.

use agent_workers::ToolProfile;
use serde_json::{Value, json};

use crate::agents::StageRunner;
use crate::config::RepoConfig;
use crate::forge::{Comment, ForgeError, PrState, is_bot_comment};
use crate::intake::IntakeError;
use crate::pipeline::{
    Claim, DIFF_LIMIT, Outcome, Pipeline, PipelineError, exit_name, pass_of, truncate,
};
use crate::ports::{Forge, Hub, Oracle};
use crate::prompts::{Template, render};
use crate::record::{Event, FindingRow, MergedBy, REVIEW_REQUEST, Rule, requests_review};
use crate::risk::{self, Detected};
use crate::router::resolve_tier;
use crate::stage::{ReviewOutput, ReviewVerdict, output_schema};
use crate::store::{Also, NewIssue, Store, StoreError, TaskRow, Write};
use crate::task::{Stage, TaskState, Tier};

/// The hidden line after the bot line of the summary comment (spec section 6).
pub const MARKER: &str = "<!-- provefab-pr-review -->";

/// `no blocking finding`, `1 blocking finding`, `3 blocking findings`.
pub fn verdict(blocking: usize) -> String {
    match blocking {
        0 => "no blocking finding".into(),
        1 => "1 blocking finding".into(),
        n => format!("{n} blocking findings"),
    }
}

/// The one comment a round leaves on the pull request (spec section 6):
/// the verdict, the round's findings, the rules given, the risk detected,
/// the reviewers and the commit, and how to answer. `round` is 0-based.
pub fn summary(
    findings: &[FindingRow],
    rules: Option<&str>,
    risk: &[Detected],
    reviewers: &[String],
    head: &str,
    round: u32,
) -> String {
    let blocking = findings.iter().filter(|f| f.severity == "blocking").count();
    let mut s = format!("{MARKER}\n**Provefab review: {}.**\n", verdict(blocking));
    if !findings.is_empty() {
        s.push('\n');
        for f in findings {
            let rule = f
                .rule
                .as_deref()
                .map(|r| format!("{r} · "))
                .unwrap_or_default();
            let at = f.line.map(|l| format!(":{l}")).unwrap_or_default();
            s.push_str(&format!(
                "- {} · {rule}{} · `{}{at}` · {}\n",
                f.key, f.severity, f.file, f.text
            ));
        }
    }
    s.push('\n');
    if let Some(r) = rules {
        s.push_str(&format!("Rules: {r}\n"));
    }
    let names: Vec<&str> = risk.iter().map(|d| d.name.as_str()).collect();
    let risk = if names.is_empty() {
        "none detected".to_string()
    } else {
        names.join(", ")
    };
    s.push_str(&format!("Risk: {risk}\n"));
    let by: Vec<String> = reviewers.iter().map(|m| format!("`{m}`")).collect();
    s.push_str(&format!(
        "Reviewed by {} at commit `{}` (round {}).\n\n",
        by.join(" and "),
        &head[..head.len().min(12)],
        round + 1
    ));
    if let Some(f) = findings.first() {
        s.push_str(&format!(
            "To record what you decide on a finding, reply `/provefab {} rejected: <reason>` (or accepted, fixed, waived).\n",
            f.key
        ));
    }
    s.push_str("After a push, comment `/provefab review` for another review. Provefab does not approve, request changes on or merge this pull request.\n");
    s
}

/// A failure's detail, safe to keep: a git or gh error can quote a remote
/// URL with its credentials (rule R3).
fn detail(e: &ForgeError) -> String {
    crate::rules::redact_credentials(&e.to_string())
}

/// A comment a person deleted: editing it answers GitHub's 404. A
/// repository that is gone also reads "not found" and is never one.
fn gone(e: &ForgeError) -> bool {
    !e.is_permanent() && e.is_not_found()
}

/// `<label>:review` (spec section 4), with its colour and description.
pub fn review_label(repo: &RepoConfig) -> (String, &'static str, &'static str) {
    (
        format!("{}:review", repo.label),
        "1d76db",
        "Ask Provefab to review this pull request",
    )
}

/// What the scheduler creates at startup: the review label goes on GitHub
/// pull requests, so only where GitHub is also the tracker (plan decision 5).
pub fn labels_to_create(repo: &RepoConfig) -> Vec<(String, &'static str, &'static str)> {
    if repo.tracker_kind() == crate::tracker::TrackerKind::Github {
        vec![review_label(repo)]
    } else {
        Vec::new()
    }
}

/// Provefab's own pull requests come from `provefab/` branches (spec section 4).
pub fn is_own_branch(head_ref: &str) -> bool {
    head_ref.starts_with("provefab/")
}

/// Who may ask for a review in a comment: a repository owner, an
/// organization member or a collaborator. The pull request's author alone
/// is not enough (spec decision 6), and Provefab's own comments never count.
pub fn may_request(c: &Comment) -> bool {
    !is_bot_comment(&c.body)
        && matches!(c.association.as_str(), "OWNER" | "MEMBER" | "COLLABORATOR")
}

/// The newest `/provefab review` from someone who may ask, posted after
/// `seen` (RFC 3339, as GitHub writes it, so it sorts as a string).
pub fn newest_request<'a>(comments: &'a [Comment], seen: Option<&str>) -> Option<&'a Comment> {
    comments
        .iter()
        .filter(|c| may_request(c) && requests_review(&c.body))
        .filter(|c| seen.is_none_or(|s| c.created_at.as_str() > s))
        .max_by(|a, b| a.created_at.cmp(&b.created_at))
}

/// Between rounds: a new request starts the next one (plan decision 8).
fn idle(task: &TaskRow) -> bool {
    matches!(
        task.state,
        TaskState::PrOpen | TaskState::NeedsYou | TaskState::Failed
    )
}

/// The newest request the task's rounds already answered.
pub(crate) async fn last_seen(store: &Store, id: i64) -> Result<Option<String>, StoreError> {
    Ok(store
        .last_output(id, "pr_trigger")
        .await?
        .and_then(|v| v["seen"].as_str().map(str::to_string)))
}

/// Starts the next round of an idle review task. The state moves first: a
/// crash before the trigger is kept costs one more round, never a lost
/// request. A closed pull request that was reopened is open again.
pub(crate) async fn start_round(
    store: &Store,
    task: &TaskRow,
    by: &str,
    login: Option<&str>,
    seen: Option<&str>,
) -> Result<(), StoreError> {
    let reason = match login {
        Some(l) => format!("review requested by {l}"),
        None => "review requested with the label".to_string(),
    };
    store
        .transition_and(task.id, TaskState::Queued, &reason, Also::NewRound)
        .await?;
    if task.pr_state.as_deref() != Some("open") {
        store.set_pr_state(task.id, "open").await?;
    }
    store
        .record_output(
            task.id,
            "pr_trigger",
            &json!({"round": task.review_rounds + 1, "by": by, "login": login, "seen": seen}),
        )
        .await
}

/// One poll of a repository's open pull requests (spec section 4, plan
/// decisions 2, 4 and 8): a label or an authorised `/provefab review`
/// creates a review task; on an idle task, the label put back since the
/// last poll, or (when the watch does not read its comments) a newer
/// command, starts the next round. Returns the tasks queued.
pub async fn poll_prs(
    forge: &impl Forge,
    repo: &RepoConfig,
    store: &Store,
) -> Result<Vec<i64>, IntakeError> {
    let label = review_label(repo).0;
    let mut queued = Vec::new();
    for pr in forge.open_pull_requests(&repo.slug, &repo.base).await? {
        if pr.base != repo.base || is_own_branch(&pr.head_ref) {
            continue;
        }
        let labelled = pr.labels.contains(&label);
        let snapshot = json!({"title": pr.title, "body": pr.body});
        let newest = newest_request(&pr.comments, None).map(|c| c.created_at.clone());
        let Some(task) = store.task_of_pr(&repo.slug, pr.number).await? else {
            let by = if labelled {
                ("label", None)
            } else if let Some(c) = newest_request(&pr.comments, None) {
                ("command", Some(c.author.clone()))
            } else {
                continue;
            };
            let trigger = json!({"round": 0, "by": by.0, "login": by.1, "seen": newest});
            let present = json!({"present": labelled});
            let new = NewIssue {
                repo: repo.slug.clone(),
                number: pr.number,
                issue_key: None,
                url: pr.url.clone(),
                title: pr.title.clone(),
                author: pr.author.clone(),
            };
            let outputs = [
                ("pr", &snapshot),
                ("pr_trigger", &trigger),
                ("pr_label", &present),
            ];
            if let Some(id) = store.add_pr_review(&new, &outputs).await? {
                queued.push(id);
            }
            continue;
        };
        if !idle(&task) {
            continue;
        }
        if store.last_output(task.id, "pr").await?.as_ref() != Some(&snapshot) {
            store.record_output(task.id, "pr", &snapshot).await?;
        }
        let before = store
            .last_output(task.id, "pr_label")
            .await?
            .and_then(|v| v["present"].as_bool())
            .unwrap_or(false);
        if before != labelled {
            store
                .record_output(task.id, "pr_label", &json!({"present": labelled}))
                .await?;
        }
        // An open, reviewed pull request's commands are read by the watch,
        // from all its comments (plan decision 2).
        let watched = task.state == TaskState::PrOpen && task.pr_state.as_deref() == Some("open");
        let seen = last_seen(store, task.id).await?;
        let command = (!watched)
            .then(|| newest_request(&pr.comments, seen.as_deref()))
            .flatten();
        let by = if labelled && !before {
            Some(("label", None))
        } else {
            command.map(|c| ("command", Some(c.author.as_str())))
        };
        if let Some((by, login)) = by {
            start_round(store, &task, by, login, newest.as_deref()).await?;
            queued.push(task.id);
        }
    }
    Ok(queued)
}

impl<R, O, H> Pipeline<R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// One step of a review task (spec section 5). `Waiting` resumes
    /// through `resume_waiting` like any task, so `step` never sends it here.
    pub(crate) async fn pr_review_step(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<TaskState, PipelineError> {
        match task.state {
            TaskState::Queued => self.pr_prepare(task, repo).await,
            TaskState::Reviewing => self.pr_review_run(task, repo).await,
            s @ (TaskState::PrOpen | TaskState::NeedsYou | TaskState::Failed) => Ok(s),
            s => {
                self.give_up(
                    task,
                    Some(repo),
                    TaskState::NeedsYou,
                    &format!("a pull request review cannot be in state {}", s.as_str()),
                    "",
                )
                .await
            }
        }
    }

    /// The current round's head and base, as `pr_prepare` pinned them.
    async fn pr_round(&self, task: &TaskRow) -> Result<Option<(String, String)>, PipelineError> {
        Ok(self
            .store
            .last_output(task.id, "pr_round")
            .await?
            .filter(|v| v["round"].as_u64() == Some(u64::from(task.review_rounds)))
            .and_then(|v| {
                Some((
                    v["head"].as_str()?.to_string(),
                    v["base"].as_str()?.to_string(),
                ))
            }))
    }

    /// The (model, provider) pairs that reviewed the current round, oldest first.
    async fn pr_reviewers(&self, task: &TaskRow) -> Result<Vec<(String, String)>, PipelineError> {
        Ok(self
            .store
            .recent_outputs(task.id, "pr_reviewed", u32::MAX)
            .await?
            .into_iter()
            .filter(|v| v["round"].as_u64() == Some(u64::from(task.review_rounds)))
            .map(|v| {
                (
                    v["model"].as_str().unwrap_or_default().to_string(),
                    v["provider"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect())
    }

    /// Spec section 5, plan decision 10: standard; frontier when the round's
    /// risk asks for it and a frontier model is configured (no implementer,
    /// so any provider); one tier up after a failed answer.
    pub(crate) async fn pr_review_tier(&self, task: &TaskRow) -> Result<Tier, PipelineError> {
        let resolve = |t: Tier| resolve_tier(t, &self.config.models).unwrap_or(t);
        let needs = match (self.risk_of_round(task).await?, self.repo(task)) {
            (Some(detected), Some(repo)) => risk::resolve(repo.risk.as_ref())
                .map(|p| p.needs_frontier(&detected))
                .unwrap_or(true),
            _ => false,
        };
        let tier = resolve(
            risk::risky_review_tier(&self.config.models, None, needs).unwrap_or(Tier::Standard),
        );
        Ok(if task.escalated {
            resolve(tier.up())
        } else {
            tier
        })
    }

    /// A second reviewer avoids the providers that reviewed the round already.
    pub(crate) async fn pr_review_avoid(
        &self,
        task: &TaskRow,
    ) -> Result<Vec<String>, PipelineError> {
        Ok(self
            .pr_reviewers(task)
            .await?
            .into_iter()
            .map(|(_, provider)| provider)
            .collect())
    }

    /// `failed`, with the error redacted before it is kept.
    async fn pr_forge_failed(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        public: &str,
        e: &ForgeError,
    ) -> Result<TaskState, PipelineError> {
        let detail = detail(e);
        if e.is_permanent() {
            self.give_up(task, Some(repo), TaskState::NeedsYou, public, &detail)
                .await
        } else {
            self.transient(task, repo, public, &detail).await
        }
    }

    /// Queued: fetch, pin the base, fetch the pull request's head (forks
    /// included) into a fresh detached worktree, read the rules from the
    /// base, classify the risk. Nothing from the pull request runs (plan
    /// decision 6).
    async fn pr_prepare(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<TaskState, PipelineError> {
        let checkout = self.checkout(repo);
        let wt = self.paths.worktree(task.id);
        let (head, base) = {
            // One task at a time writes to the shared checkout (PR #31).
            let lock = self.repo_lock(repo);
            let _guard = lock.lock().await;
            let fetched = match self.refresh_checkout(repo).await {
                Ok(f) => f,
                Err(e) => {
                    return self
                        .pr_forge_failed(task, repo, "could not clone the repository", &e)
                        .await;
                }
            };
            let base = match self.pin_base(task, repo, fetched).await {
                Ok(b) => b,
                Err(e) => {
                    return self
                        .transient(task, repo, "could not resolve the base branch", &e)
                        .await;
                }
            };
            let head = match self.git.fetch_pr_head(&checkout, task.issue_number).await {
                Ok(h) => h,
                Err(e) => {
                    return self
                        .pr_forge_failed(task, repo, "could not fetch the pull request's head", &e)
                        .await;
                }
            };
            if let Err(e) = self
                .git
                .worktree_fresh_detached(&checkout, &wt, &head)
                .await
            {
                return self
                    .pr_forge_failed(task, repo, "could not create the worktree", &e)
                    .await;
            }
            (head, base)
        };
        self.store
            .record_output(
                task.id,
                "pr_round",
                &json!({"round": task.review_rounds, "head": head, "base": base}),
            )
            .await?;
        // Repository rules spec section 4: the base commit just pinned (plan decision 16).
        self.pass_rules(task, repo).await?;
        let policy = match risk::resolve(repo.risk.as_ref()) {
            Ok(p) => p,
            Err(e) => {
                let public = format!("invalid [repos.risk]: {e}");
                return self
                    .give_up(task, Some(repo), TaskState::NeedsYou, &public, "")
                    .await;
            }
        };
        // Both sides of a rename count, as for Provefab's own changes.
        let detected = match self.git.changed_files(&wt, &base).await {
            Ok(changed) => {
                let paths: Vec<String> = changed
                    .into_iter()
                    .flat_map(|c| std::iter::once(c.path).chain(c.from))
                    .collect();
                risk::classify(&policy, &paths)
            }
            Err(_) => risk::unknown(),
        };
        self.store
            .write_with_events(
                task.id,
                Write::Nothing,
                &[Event::RiskClassified {
                    pass: pass_of(task),
                    round: task.review_rounds,
                    categories: detected,
                }],
            )
            .await?;
        self.enter(task.id, TaskState::Reviewing, "pull request head fetched")
            .await
    }

    /// Reviewing: one reviewer, a second from another provider when the
    /// policy asks for two approvals (plan decision 9), then the summary.
    async fn pr_review_run(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<TaskState, PipelineError> {
        let wt = self.paths.worktree(task.id);
        let round = self.pr_round(task).await?;
        let need = usize::from(self.policy.approvals_needed(repo).max(1));
        // A restart after the round's reviews were recorded: finish the
        // round, never pay for another review.
        if let Some((head, _)) = &round
            && self.pr_reviewers(task).await?.len() >= need
        {
            return self.pr_finish_round(task, repo, head).await;
        }
        let Some((head, base)) = round.filter(|_| wt.exists()) else {
            // A restart cleaned the worktree up, or the round was never prepared.
            return self
                .go(
                    task.id,
                    TaskState::Queued,
                    "the pull request's worktree is gone; fetching it again",
                )
                .await;
        };
        let (model, slot) = match self.claim(task, Stage::Review).await? {
            Claim::Run(m, s) => (*m, s),
            Claim::Busy => return self.wait(task, Stage::Review).await,
            Claim::OverBudget => return self.over_budget(task, repo).await,
        };
        let diff = match self.git.diff(&wt, &base).await {
            Ok(d) => d,
            Err(e) => return self.pr_forge_failed(task, repo, "diff failed", &e).await,
        };
        let changed: Vec<String> = self
            .git
            .changed_files(&wt, &base)
            .await
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| std::iter::once(c.path).chain(c.from))
            .collect();
        let snapshot = self
            .store
            .last_output(task.id, "pr")
            .await?
            .unwrap_or(Value::Null);
        let title = snapshot["title"]
            .as_str()
            .unwrap_or(&task.title)
            .to_string();
        let body = snapshot["body"].as_str().unwrap_or_default().to_string();
        let rules = self.pass_rules(task, repo).await?;
        let selected = crate::rules::select(&rules, Stage::Review, &changed);
        let (block, omitted) = crate::rules::render(&selected, crate::rules::BUDGET);
        let given = crate::rules::numbers(&selected, omitted);
        let block = if given.is_empty() {
            block
        } else {
            format!("{block}{}", crate::rules::REVIEW_ASK)
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
        let number = format!("#{}", task.issue_number);
        let base_shown = format!("{} at {}", repo.base, &base[..base.len().min(12)]);
        let diff = truncate(&diff, DIFF_LIMIT);
        let prompt = render(
            Template::PrReview,
            &[
                ("ref", &number),
                ("title", &title),
                ("body", &body),
                ("base", &base_shown),
                ("diff", &diff),
                ("rules", &block),
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
        let mut review = match review {
            Ok(r) => r,
            Err(reason) => return self.stage_failed(task, repo, "review", &reason).await,
        };
        for f in &mut review.findings {
            // Repository rules spec section 5: only a rule this review was given.
            f.rule = f
                .rule
                .as_deref()
                .and_then(crate::rules::rule_number)
                .filter(|n| given.contains(n))
                .map(|n| format!("R{n}"));
            // Plan decision 11: a pull request can steer its reviewer.
            f.text = crate::rules::redact_credentials(&f.text);
            f.file = crate::rules::redact_credentials(&f.file);
        }
        let value = serde_json::to_value(&review).unwrap_or(Value::Null);
        self.store
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
        self.store
            .record_output(
                task.id,
                "pr_reviewed",
                &json!({"round": task.review_rounds, "model": model.id, "provider": model.provider_key()}),
            )
            .await?;
        if self.pr_reviewers(task).await?.len() < need {
            return self
                .go(
                    task.id,
                    TaskState::Reviewing,
                    &format!(
                        "{} reviewed; a reviewer from another provider reviews too",
                        model.id
                    ),
                )
                .await;
        }
        self.pr_finish_round(task, repo, &head).await
    }

    /// The round's reviews are in: keep the summary and the head reviewed,
    /// wait in `pr_open` for the next request, then post the comment.
    async fn pr_finish_round(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        head: &str,
    ) -> Result<TaskState, PipelineError> {
        let findings = self
            .store
            .final_round_findings(task.id, pass_of(task))
            .await?;
        let reviewers: Vec<String> = self
            .pr_reviewers(task)
            .await?
            .into_iter()
            .map(|(model, _)| model)
            .collect();
        let rules = self.rules_given(task).await?;
        let risk = self.risk_of_round(task).await?.unwrap_or_default();
        let body = summary(
            &findings,
            rules.as_deref(),
            &risk,
            &reviewers,
            head,
            task.review_rounds,
        );
        self.store
            .record_output(
                task.id,
                "pr_summary",
                &json!({"round": task.review_rounds, "body": body}),
            )
            .await?;
        self.store.set_pr_head(task.id, head).await?;
        let blocking = findings.iter().filter(|f| f.severity == "blocking").count();
        self.go(
            task.id,
            TaskState::PrOpen,
            &format!(
                "reviewed {}: {}",
                &head[..head.len().min(12)],
                verdict(blocking)
            ),
        )
        .await?;
        self.post_summary(task, &repo.slug).await?;
        Ok(TaskState::PrOpen)
    }

    /// Posts the last round's summary unless it is already up: created on
    /// the first round, edited on later ones by the id GitHub gave it, and
    /// posted anew when a person deleted it (plan decision 3). A failure is
    /// logged; `watch_pr` tries again at the next poll.
    pub(crate) async fn post_summary(
        &self,
        task: &TaskRow,
        slug: &str,
    ) -> Result<(), PipelineError> {
        let Some(summary) = self.store.last_output(task.id, "pr_summary").await? else {
            return Ok(());
        };
        let posted = self.store.last_output(task.id, "pr_summary_posted").await?;
        if posted
            .as_ref()
            .is_some_and(|p| p["round"] == summary["round"])
        {
            return Ok(());
        }
        let body = summary["body"].as_str().unwrap_or_default();
        let url = task.issue_url.as_str();
        let earlier = posted.and_then(|p| p["comment"].as_u64());
        let result = match self.hub.pr_comment(slug, url, body, earlier).await {
            Err(e) if earlier.is_some() && gone(&e) => {
                self.hub.pr_comment(slug, url, body, None).await
            }
            other => other,
        };
        match result {
            Ok(id) => {
                self.store
                    .record_output(
                        task.id,
                        "pr_summary_posted",
                        &json!({"round": summary["round"], "comment": id}),
                    )
                    .await?
            }
            Err(e) => eprintln!(
                "provefab: task {}: could not post the review comment on {url}; tried again at the next poll: {}",
                task.id,
                detail(&e)
            ),
        }
        Ok(())
    }

    /// Follows a reviewed pull request once per poll (spec section 7): posts
    /// a summary still owed, records `/provefab F<n>` decisions, starts a
    /// round on a new `/provefab review`, and ends the task when the pull
    /// request merges (the last round's open findings are then inferred
    /// unaddressed) or closes (plan decision 12). A review parked in
    /// `needs_you` or `failed` is watched for its end only (pre-flight S2):
    /// its next round comes from `poll_prs`.
    pub(crate) async fn watch_pr_review(&self, task: &TaskRow) -> Result<TaskState, PipelineError> {
        let Some(repo) = self.repo(task).cloned() else {
            return Ok(task.state);
        };
        let parked = matches!(task.state, TaskState::NeedsYou | TaskState::Failed);
        if !(task.state == TaskState::PrOpen || parked) || task.pr_state.as_deref() != Some("open")
        {
            return Ok(task.state);
        }
        if !parked {
            self.post_summary(task, &repo.slug).await?;
        }
        let status = match self.hub.pr_status(&repo.slug, &task.issue_url).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "provefab: could not read {}: {}",
                    task.issue_url,
                    detail(&e)
                );
                return Ok(task.state);
            }
        };
        if parked && status.state == PrState::Open {
            return Ok(task.state);
        }
        // Before a merge is recorded: a decision posted since the last poll
        // must not be inferred unaddressed.
        self.apply_finding_commands(task, &status.comments).await?;
        match status.state {
            PrState::Open => {
                self.ignore_refused_requests(task, &status.comments).await?;
                let seen = last_seen(&self.store, task.id).await?;
                if let Some(c) = newest_request(&status.comments, seen.as_deref()) {
                    start_round(
                        &self.store,
                        task,
                        "command",
                        Some(c.author.as_str()),
                        Some(c.created_at.as_str()),
                    )
                    .await?;
                    return Ok(TaskState::Queued);
                }
                Ok(task.state)
            }
            PrState::Merged => {
                let pass = pass_of(task);
                // Provefab never merges a person's pull request: always a
                // human. No post-merge check, no revert (spec section 7).
                self.store
                    .write_with_inference(
                        task.id,
                        Write::SetPrState("merged"),
                        &[Event::Merged {
                            sha: status.merge_sha.clone(),
                            base: status.base_ref.clone(),
                            by: MergedBy::Human,
                            pass,
                        }],
                        Some((Rule::UnaddressedAtMerge, pass)),
                    )
                    .await?;
                self.discard_pr_worktree(task, &repo).await;
                Ok(task.state)
            }
            PrState::Closed => {
                self.store.set_pr_state(task.id, "closed").await?;
                self.discard_pr_worktree(task, &repo).await;
                Ok(task.state)
            }
        }
    }

    /// A `/provefab review` from someone who may not ask is recorded once as
    /// ignored (spec decision 6, plan decision 13).
    async fn ignore_refused_requests(
        &self,
        task: &TaskRow,
        comments: &[Comment],
    ) -> Result<(), PipelineError> {
        let refused = comments
            .iter()
            .filter(|c| !is_bot_comment(&c.body) && !may_request(c) && requests_review(&c.body));
        for c in refused {
            self.store
                .record_human(
                    task.id,
                    &Event::CommandIgnored {
                        comment: format!("{}@{}", c.author, c.created_at),
                        login: c.author.clone(),
                        line: REVIEW_REQUEST.into(),
                        why: "not authorized".into(),
                    },
                )
                .await?;
        }
        Ok(())
    }

    /// The round's worktree is not needed once the pull request is gone.
    async fn discard_pr_worktree(&self, task: &TaskRow, repo: &RepoConfig) {
        let lock = self.repo_lock(repo);
        let _guard = lock.lock().await;
        let wt = self.paths.worktree(task.id);
        if let Err(e) = self.git.worktree_discard(&self.checkout(repo), &wt).await {
            eprintln!(
                "provefab: task {}: could not remove {}: {}",
                task.id,
                wt.display(),
                detail(&e)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plan decision 5: the label is created where GitHub is the tracker only.
    #[test]
    fn the_review_label_is_created_only_where_github_is_the_tracker() {
        let mut repo: RepoConfig = toml::from_str("slug = \"o/r\"\ngates = [\"true\"]\n").unwrap();
        assert_eq!(
            labels_to_create(&repo),
            vec![(
                "provefab:review".to_string(),
                "1d76db",
                "Ask Provefab to review this pull request"
            )]
        );
        for kind in [
            crate::tracker::TrackerKind::Jira,
            crate::tracker::TrackerKind::Linear,
        ] {
            repo.tracker = Some(crate::tracker::TrackerConfig {
                kind,
                site: None,
                project: None,
            });
            assert!(labels_to_create(&repo).is_empty());
        }
    }

    /// Spec section 4: only someone with a role on the repository may ask;
    /// the author's association alone (a contributor) never is one.
    #[test]
    fn only_an_owner_a_member_or_a_collaborator_may_request() {
        let c = |association: &str, body: &str| Comment {
            author: "carol".into(),
            association: association.into(),
            body: body.into(),
            created_at: "2026-10-02T00:00:00Z".into(),
        };
        for a in ["OWNER", "MEMBER", "COLLABORATOR"] {
            assert!(may_request(&c(a, "/provefab review")), "{a}");
        }
        for a in [
            "CONTRIBUTOR",
            "FIRST_TIME_CONTRIBUTOR",
            "FIRST_TIMER",
            "NONE",
            "MANNEQUIN",
            "",
        ] {
            assert!(!may_request(&c(a, "/provefab review")), "{a}");
        }
        let bot = format!("{}\n/provefab review", crate::forge::BOT_PREFIX);
        assert!(!may_request(&c("OWNER", &bot)));
    }

    fn finding(
        key: &str,
        rule: Option<&str>,
        severity: &str,
        file: &str,
        line: Option<u32>,
        text: &str,
    ) -> FindingRow {
        FindingRow {
            id: 0,
            task_id: 1,
            key: key.into(),
            pass: 1,
            round: 1,
            reviewer_model: "std-claude".into(),
            severity: severity.into(),
            file: file.into(),
            line,
            text: text.into(),
            rule: rule.map(str::to_string),
            event_id: 0,
        }
    }

    /// Spec section 6: the verdict, the findings in the review-notes format,
    /// the rules, the risk, the reviewers, the commit and how to decide.
    #[test]
    fn the_summary_says_what_was_found_and_how_to_answer() {
        let findings = [
            finding(
                "F1",
                Some("R3"),
                "blocking",
                "src/a.rs",
                Some(12),
                "unwrap on user input",
            ),
            finding("F2", None, "minor", "notes.md", None, "typo"),
        ];
        let risk = [Detected {
            name: "migrations".into(),
            paths: vec!["db/0001.sql".into()],
        }];
        let reviewers = ["std-claude".to_string(), "std-codex".to_string()];
        assert_eq!(
            summary(
                &findings,
                Some("R1, R3"),
                &risk,
                &reviewers,
                "0123456789abcdef",
                1
            ),
            "<!-- provefab-pr-review -->\n\
             **Provefab review: 1 blocking finding.**\n\
             \n\
             - F1 · R3 · blocking · `src/a.rs:12` · unwrap on user input\n\
             - F2 · minor · `notes.md` · typo\n\
             \n\
             Rules: R1, R3\n\
             Risk: migrations\n\
             Reviewed by `std-claude` and `std-codex` at commit `0123456789ab` (round 2).\n\
             \n\
             To record what you decide on a finding, reply `/provefab F1 rejected: <reason>` (or accepted, fixed, waived).\n\
             After a push, comment `/provefab review` for another review. Provefab does not approve, request changes on or merge this pull request.\n"
        );
        let clean = summary(&[], None, &[], &reviewers[..1], "abc", 0);
        assert!(
            clean.contains("**Provefab review: no blocking finding.**\n\nRisk: none detected\n"),
            "{clean}"
        );
        assert!(!clean.contains("To record"), "{clean}");
    }

    #[test]
    fn verdicts_count_blocking_findings() {
        assert_eq!(verdict(0), "no blocking finding");
        assert_eq!(verdict(1), "1 blocking finding");
        assert_eq!(verdict(3), "3 blocking findings");
    }

    /// Rule R3: a git or gh error can quote a remote URL with its credentials.
    #[test]
    fn a_failure_is_kept_redacted_and_a_deleted_comment_is_recognised() {
        let e = ForgeError::Failed {
            program: "git".into(),
            args: "fetch".into(),
            code: Some(128),
            stderr: "fatal: https://x-access-token:ghs_abcdefghijklmnopqrstuvwxyz0123@github.com/o/r.git refused".into(),
        };
        assert!(
            !detail(&e).contains("ghs_abcdefghijklmnopqrstuvwxyz0123"),
            "{}",
            detail(&e)
        );
        assert!(!gone(&e));
        // A repository gone is permanent, never a deleted comment.
        assert!(!gone(&ForgeError::Failed {
            program: "gh".into(),
            args: "api".into(),
            code: Some(1),
            stderr: "ERROR: Repository not found.".into(),
        }));
        assert!(gone(&ForgeError::Failed {
            program: "gh".into(),
            args: "api".into(),
            code: Some(1),
            stderr: "gh: Not Found (HTTP 404)".into(),
        }));
    }
}
