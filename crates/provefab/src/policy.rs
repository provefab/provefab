//! What happens around the review stage (spec §3, D59): how many approvals a
//! head needs before its PR opens, and what to do once it is open. The free
//! core ships `OpenPrOnly`; Provefab Pro supplies the guarded auto-merge.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::config::{Config, RepoConfig};
use crate::forge::{Change, ForgeError};
use crate::pipeline::PipelineError;
use crate::record::Disposition;
use crate::store::{MaintenanceRun, TaskRow};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One recorded approval of the current pass and round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub model: String,
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged,
    Failed(String),
}

/// What a policy may do once the PR is open: read the change, and merge only
/// the head it checked. Nothing here bypasses the core's invariants.
pub trait MergeTools: Send + Sync {
    /// `git fetch origin` in the repo's checkout, under the repo lock.
    fn fetch_base(&self) -> BoxFuture<'_, Result<(), ForgeError>>;
    /// The freshly fetched base ref (`origin/<base>` for managed repos).
    fn base_ref(&self) -> BoxFuture<'_, String>;
    fn head(&self) -> BoxFuture<'_, Result<String, ForgeError>>;
    fn changed_files<'a>(&'a self, base: &'a str)
    -> BoxFuture<'a, Result<Vec<Change>, ForgeError>>;
    fn diff<'a>(&'a self, base: &'a str) -> BoxFuture<'a, Result<String, ForgeError>>;
    #[allow(clippy::type_complexity)]
    fn numstat<'a>(
        &'a self,
        base: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(Option<u32>, Option<u32>, String)>, ForgeError>>;
    fn is_ancestor<'a>(
        &'a self,
        ancestor: &'a str,
        of: &'a str,
    ) -> BoxFuture<'a, Result<bool, ForgeError>>;
    /// Whether the repository is public: anyone can write the issues agents read.
    fn repo_is_public(&self) -> BoxFuture<'_, Result<bool, ForgeError>>;
    /// `pr_merge` pinned to `head`; on success records the merge (labels,
    /// `merged_at`, worktree removal); on failure writes the reason to the task log.
    fn merge<'a>(&'a self, head: &'a str) -> BoxFuture<'a, Result<MergeOutcome, PipelineError>>;
}

/// The PR that just opened, as a policy sees it.
pub struct PrOpened<'a> {
    pub task: &'a TaskRow,
    pub repo: &'a RepoConfig,
    pub url: &'a str,
    /// Approvals of the current pass and round, oldest first.
    pub approvals: Vec<Approval>,
    /// This pass's reproduction really failed before the change.
    pub reproduced: bool,
    pub tools: &'a dyn MergeTools,
}

/// One fact of a repository's record that periodic work reads (repository
/// rules spec §7). Its text is what people and reviewers wrote, with
/// credential-looking words redacted (pre-flight S6); command output and
/// model-written commands are never in it (plan decision 17).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Signal {
    /// Stable across calls: `pr#41/F2`, `pr#41/c1`, `task#12/gates`,
    /// `task#12/revert-3`, `task#12/reopen-1`, `pr#90/closed`.
    pub id: String,
    /// When it happened (unix seconds).
    pub at: i64,
    /// The pull request it is about, else the issue.
    pub url: String,
    pub kind: SignalKind,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalKind {
    /// A finding people rejected or waived, or any finding citing a rule.
    Finding {
        key: String,
        file: String,
        text: String,
        rule: Option<u32>,
        disposition: Option<Disposition>,
        reason: Option<String>,
    },
    /// What a person asked for on a pull request closed without merging.
    ChangeRequest { text: String },
    /// Configured checks (gates, risk checks) that failed in the task.
    GateFailure { commands: Vec<String> },
    /// A post-merge check opened a revert of the task's change.
    Revert,
    /// The issue was reopened after its pull request merged.
    Reopen,
    /// An earlier periodic pull request, merged or closed without merging.
    Proposal {
        kind: String,
        merged: bool,
        detail: Option<Value>,
    },
}

/// A periodic pull request as `PeriodicTools::propose_file` left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// The commit pushed to the proposal branch: the next call's
    /// `last_pushed`, even when `pr` failed (the push already happened).
    pub sha: String,
    /// The pull request's URL, or the fixed text of why it could not be
    /// opened or updated.
    pub pr: Result<String, String>,
}

/// What the core lends a policy's periodic work for one repository
/// (repository rules spec §7). Errors are fixed text; the detail goes to
/// the log only, redacted (pre-flight S1).
pub trait PeriodicTools: Send + Sync {
    /// The repository's signals since `since` (unix seconds), plus the
    /// outcome of every earlier periodic pull request, whatever its age.
    /// `Err("could not read <url>")` when one of those cannot be read.
    fn signals(&self, since: i64) -> BoxFuture<'_, Result<Vec<Signal>, String>>;
    /// The highest rule number the record saw: loaded by a pass or cited by a finding.
    fn highest_rule_number(&self) -> BoxFuture<'_, Result<u32, String>>;
    /// `.provefab/rules.md` on the base branch, fetched first; `None` when absent.
    fn rules_at_base(&self) -> BoxFuture<'_, Result<Option<String>, String>>;
    /// One structured answer from a standard-tier model, routed like a stage
    /// (subscription first, then API keys by price), run in an empty
    /// directory with every tool call refused by the guard: the prompt can
    /// carry untrusted text (pre-flight S2). Its cost goes with the next
    /// recorded run.
    fn ask_model<'a>(
        &'a self,
        prompt: &'a str,
        schema: &'a Value,
    ) -> BoxFuture<'a, Result<Value, String>>;
    /// Commits `content` as `path` on the branch `provefab/<file stem>`,
    /// rebuilt from the current base, pushes it and opens its pull request or
    /// updates the open one. Never merges. `Err` means nothing was pushed.
    /// `last_pushed` is the `sha` of the
    /// previous proposal (`None`: there is none); when the branch holds
    /// anything else, a person changed it, and nothing is pushed or edited
    /// (pre-flight B1), with an error starting with
    /// `rules::CHANGED_BY_A_PERSON`. Two heads are not a person's change:
    /// one already in the base, and Provefab's own proposal commit whose
    /// push was never recorded, known by its trailers (final review I1).
    fn propose_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a str,
        title: &'a str,
        body: &'a str,
        last_pushed: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Proposal, String>>;
    /// The latest run of `kind` for this repository.
    fn last_run<'a>(
        &'a self,
        kind: &'a str,
    ) -> BoxFuture<'a, Result<Option<MaintenanceRun>, String>>;
    /// Records a run of `kind` with the cost of the model calls made since
    /// the last record; `detail` is the policy's own JSON, never shown.
    fn record_run<'a>(
        &'a self,
        kind: &'a str,
        outcome: &'a str,
        pr_url: Option<&'a str>,
        detail: Option<&'a Value>,
    ) -> BoxFuture<'a, Result<(), String>>;
}

pub trait ReviewPolicy: Send + Sync {
    /// Approvals from distinct models the current head needs before the PR opens.
    fn approvals_needed(&self, repo: &RepoConfig) -> u8;
    /// Called once the PR is open and the issue relabelled; returns the issue comment.
    fn after_pr_opened<'a>(
        &'a self,
        cx: PrOpened<'a>,
    ) -> BoxFuture<'a, Result<String, PipelineError>>;
    /// Lines `doctor` and `run` print at start.
    fn warnings(&self, _config: &Config) -> Vec<String> {
        Vec::new()
    }
    /// A config this policy cannot run with.
    fn check(&self, _config: &Config) -> Result<(), String> {
        Ok(())
    }
    /// Daily work for one repository (repository rules spec §7): called by
    /// the scheduler once a day, never twice at once for a repository. An
    /// `Err` is logged and recorded; the service goes on.
    fn periodic<'a>(
        &'a self,
        _repo: &'a RepoConfig,
        _tools: &'a dyn PeriodicTools,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// Whether a repo asks for auto-merge (`[repos.merge] auto = true`).
pub fn asks_auto_merge(repo: &RepoConfig) -> bool {
    repo.merge
        .as_ref()
        .and_then(|t| t.get("auto"))
        .and_then(toml::Value::as_bool)
        == Some(true)
}

/// The free core's policy: one approval, then the PR waits for a person.
pub struct OpenPrOnly;

impl ReviewPolicy for OpenPrOnly {
    fn approvals_needed(&self, _repo: &RepoConfig) -> u8 {
        1
    }

    fn after_pr_opened<'a>(
        &'a self,
        cx: PrOpened<'a>,
    ) -> BoxFuture<'a, Result<String, PipelineError>> {
        Box::pin(async move {
            let mut said = format!(
                "Opened {}. The configured gate commands passed locally and an automated review approved it; it still needs your review.",
                cx.url
            );
            if asks_auto_merge(cx.repo) {
                said.push_str(" Auto-merge is configured for this repository but needs Provefab Pro, so this PR waits for your review.");
            }
            Ok(said)
        })
    }

    fn warnings(&self, config: &Config) -> Vec<String> {
        config
            .repos
            .iter()
            .filter(|r| r.merge.is_some())
            .map(|r| {
                format!(
                    "{}: [repos.merge] is read by Provefab Pro; this binary opens PRs and stops",
                    r.slug
                )
            })
            .collect()
    }
}
