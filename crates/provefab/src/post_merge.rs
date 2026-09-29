//! Post-merge verification and safe rollback
//! (docs/specs/2026-09-29-post-merge-verification-design.md, revision 2).

use serde::{Deserialize, Serialize};

use crate::agents::StageRunner;
use crate::config::RepoConfig;
use crate::forge::ForgeError;
use crate::pipeline::{Pipeline, PipelineError};
use crate::ports::{Hub, Oracle};
use crate::store::{CheckPatch, NoticeTarget, PostMergeCheckRow, StoreError, TaskRow};
use std::path::PathBuf;

pub const ATTRIBUTION_WAIT_SECS: i64 = 3600;
pub const INFRA_ERROR_LIMIT: i64 = 5;
pub const BASE_MOVE_LIMIT: i64 = 3;
pub const SUMMARY_MAX: usize = 2000;

/// Where a check stands (spec section 5). One transition per scheduler tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Queued,
    Verifying,
    VerificationFailed,
    PreparingRevert,
    RevertReady,
    Passed,
    Superseded,
    RevertOpen,
    Blocked,
}

impl CheckState {
    pub const ALL: [CheckState; 9] = [
        Self::Queued,
        Self::Verifying,
        Self::VerificationFailed,
        Self::PreparingRevert,
        Self::RevertReady,
        Self::Passed,
        Self::Superseded,
        Self::RevertOpen,
        Self::Blocked,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Verifying => "verifying",
            Self::VerificationFailed => "verification_failed",
            Self::PreparingRevert => "preparing_revert",
            Self::RevertReady => "revert_ready",
            Self::Passed => "passed",
            Self::Superseded => "superseded",
            Self::RevertOpen => "revert_open",
            Self::Blocked => "blocked",
        }
    }

    /// Unknown text is `None`: the store turns it into a corruption error,
    /// never a default state.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Superseded | Self::RevertOpen | Self::Blocked
        )
    }
}

/// Why a check needs a human, or why verification failed (spec section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    CheckFailed,
    InfraError,
    RevertConflict,
    RevertChecksFailed,
    BaseMoved,
    BaseDiverged,
    BranchConflict,
    UnsafeMergeStrategy,
    AttributionMissing,
    DirtyTree,
}

impl FailureKind {
    pub const ALL: [FailureKind; 10] = [
        Self::CheckFailed,
        Self::InfraError,
        Self::RevertConflict,
        Self::RevertChecksFailed,
        Self::BaseMoved,
        Self::BaseDiverged,
        Self::BranchConflict,
        Self::UnsafeMergeStrategy,
        Self::AttributionMissing,
        Self::DirtyTree,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckFailed => "check_failed",
            Self::InfraError => "infra_error",
            Self::RevertConflict => "revert_conflict",
            Self::RevertChecksFailed => "revert_checks_failed",
            Self::BaseMoved => "base_moved",
            Self::BaseDiverged => "base_diverged",
            Self::BranchConflict => "branch_conflict",
            Self::UnsafeMergeStrategy => "unsafe_merge_strategy",
            Self::AttributionMissing => "attribution_missing",
            Self::DirtyTree => "dirty_tree",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// A command that failed twice. Only these fields may reach GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedCommand {
    pub command: String,
    pub exit: Option<i32>,
    pub timed_out: bool,
}

/// At most `SUMMARY_MAX` characters, cut on a character boundary.
pub fn bounded(s: &str) -> String {
    if s.chars().count() <= SUMMARY_MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(SUMMARY_MAX - 3).collect();
    out.push_str("...");
    out
}

use crate::gates::GateReport;

/// Commands after at most one rerun (spec section 5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Confirmed {
    pub failed: Vec<FailedCommand>,
    pub flaky: Vec<String>,
    /// A command modified tracked files: its result does not describe the commit.
    pub dirty: bool,
}

/// `rerun` holds only the commands that failed in `first`, run again on a
/// fresh checkout. A command that passes on the rerun is flaky, not failed.
pub fn confirm(first: &GateReport, rerun: Option<&GateReport>) -> Confirmed {
    let mut out = Confirmed::default();
    for r in first.results.iter().filter(|r| !r.passed) {
        let again = rerun.and_then(|rr| rr.results.iter().find(|x| x.command == r.command));
        match again {
            Some(x) if x.passed => out.flaky.push(r.command.clone()),
            Some(x) => out.failed.push(FailedCommand {
                command: x.command.clone(),
                exit: x.exit,
                timed_out: x.timed_out,
            }),
            None => out.failed.push(FailedCommand {
                command: r.command.clone(),
                exit: r.exit,
                timed_out: r.timed_out,
            }),
        }
    }
    out
}

/// How to undo a merge as one commit (spec section 6), or `None` when that
/// cannot be done safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevertPlan {
    Plain,
    Mainline1,
}

impl RevertPlan {
    pub fn mainline(self) -> Option<u8> {
        match self {
            Self::Plain => None,
            Self::Mainline1 => Some(1),
        }
    }
}

pub fn revert_plan(
    parents: usize,
    commit_count: Option<i64>,
    auto_merged: bool,
) -> Option<RevertPlan> {
    match (parents, commit_count) {
        (2, _) => Some(RevertPlan::Mainline1),
        (1, Some(1)) => Some(RevertPlan::Plain),
        // Provefab's own merges are always squashes: one commit holds the whole PR.
        (1, Some(n)) if n > 1 && auto_merged => Some(RevertPlan::Plain),
        _ => None,
    }
}

/// Local summary of confirmed failures (bounded by the store).
pub fn failure_summary(failed: &[FailedCommand]) -> String {
    command_lines(failed)
}

fn command_lines(failed: &[FailedCommand]) -> String {
    failed
        .iter()
        .map(|f| {
            let how = if f.timed_out {
                "timed out".to_string()
            } else {
                match f.exit {
                    Some(code) => format!("exited with {code}"),
                    None => "was killed".to_string(),
                }
            };
            format!("- `{}` {how}", f.command)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn marker(check_id: i64) -> String {
    format!("<!-- provefab-post-merge:{check_id} -->")
}

fn blocked_reason(kind: Option<FailureKind>) -> &'static str {
    match kind {
        Some(FailureKind::InfraError) => {
            "Provefab could not finish after repeated infrastructure errors. Details are in the local provefab log."
        }
        Some(FailureKind::RevertConflict) => {
            "The checks failed, and reverting the merge does not apply cleanly on the current base."
        }
        Some(FailureKind::RevertChecksFailed) => {
            "The checks failed, and the proposed revert does not pass them either."
        }
        Some(FailureKind::BaseMoved) => {
            "The base branch kept moving while Provefab prepared the revert."
        }
        Some(FailureKind::BaseDiverged) => {
            "The merged commit is no longer an ancestor of the base branch."
        }
        Some(FailureKind::BranchConflict) => {
            "The revert branch or pull request on GitHub does not hold the revert Provefab prepared."
        }
        Some(FailureKind::UnsafeMergeStrategy) => {
            "This merge cannot be undone as one commit (a multi-commit pull request merged by rebase, or an unknown merge shape), so Provefab did not verify it."
        }
        Some(FailureKind::AttributionMissing) => {
            "GitHub did not report the merge commit or its base branch, so Provefab cannot tell what to verify."
        }
        Some(FailureKind::DirtyTree) => {
            "A check modified tracked files, so its result does not describe the committed tree."
        }
        Some(FailureKind::CheckFailed) | None => {
            "The checks failed and no safe revert could be prepared."
        }
    }
}

/// The comment for a finished check, or `None` when nothing is published
/// (a pass, or a state that is not terminal). Fixed templates only (spec section 7).
pub fn render(row: &PostMergeCheckRow) -> Option<String> {
    let sha = &row.merge_sha;
    let failed = if row.failed_commands.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nFailed checks:\n{}",
            command_lines(&row.failed_commands)
        )
    };
    let text = match row.state {
        CheckState::Superseded => format!(
            "Provefab post-merge checks failed on the merged commit `{sha}`.{failed}\n\nThe current `{}` tip `{}` passes the same checks, so no revert is proposed.",
            row.base,
            row.base_sha.as_deref().unwrap_or("unknown")
        ),
        CheckState::RevertOpen => format!(
            "Provefab post-merge checks failed on the merged commit `{sha}`.{failed}\n\nA revert pull request is open for human review: {}\nProvefab will not merge it automatically.",
            row.revert_pr_url.as_deref().unwrap_or("unknown")
        ),
        CheckState::Blocked => format!(
            "Provefab post-merge verification of `{sha}` needs a human. {}{failed}",
            blocked_reason(row.failure_kind)
        ),
        _ => return None,
    };
    Some(format!("{text}\n\n{}", marker(row.id)))
}

pub fn revert_pr_body(original_pr: &str, issue_url: &str, row: &PostMergeCheckRow) -> String {
    format!(
        "## Provefab post-merge verification failed\n\nOriginal pull request: {original_pr}\nIssue: {issue_url}\nMerged commit: `{}`\nBase tip: `{}`\n\nThese checks failed on the merged commit and still fail on the current base:\n\n{}\n\nThe reverted tree passes the same checks.\n\nProvefab will not merge this revert automatically.\n",
        row.merge_sha,
        row.base_sha.as_deref().unwrap_or("unknown"),
        command_lines(&row.failed_commands)
    )
}

impl<R, O, H> Pipeline<R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// Advances every post-merge check of a task by at most one transition,
    /// then publishes pending notices (spec sections 5 and 7). Rows exist only
    /// for merges attributed by `record_merge`.
    pub async fn process_post_merge(&self, task_id: i64) -> Result<(), PipelineError> {
        let task = self.task(task_id).await?;
        let Some(repo) = self.repo(&task).cloned() else {
            return Ok(());
        };
        if repo.post_merge_checks.is_empty() {
            return Ok(());
        }
        let mut first_error = None;
        for check in self.store.post_merge_checks(task_id).await? {
            if !check.state.is_terminal() {
                let lock = self.repo_lock(&repo);
                let guard = lock.lock().await;
                let result = self.step_post_merge(&task, &repo, &check).await;
                // Guard of spec section 9: no worktree outlives its handler.
                self.discard_check_worktrees(&repo, check.id).await;
                drop(guard);
                if let Err(e) = result {
                    let n = self.store.post_merge_infra_error(check.id).await?;
                    if n >= INFRA_ERROR_LIMIT {
                        let now = self.reload(&check).await?;
                        self.block(&now, FailureKind::InfraError, &e.to_string())
                            .await?;
                    } else {
                        eprintln!(
                            "provefab: post-merge check {} ({}): {e}",
                            check.id,
                            check.state.as_str()
                        );
                        first_error.get_or_insert(e);
                        continue;
                    }
                }
                let now = self.reload(&check).await?;
                if now.state.is_terminal() {
                    self.finish_terminal(&repo, &now).await;
                }
            }
            let now = self.reload(&check).await?;
            if now.state.is_terminal()
                && let Err(e) = self.notify_post_merge(&task, &repo, &now).await
            {
                first_error.get_or_insert(e);
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn reload(&self, check: &PostMergeCheckRow) -> Result<PostMergeCheckRow, PipelineError> {
        self.store
            .post_merge_check_by_id(check.id)
            .await?
            .ok_or_else(|| {
                StoreError::Corrupt(format!("post-merge check {} vanished", check.id)).into()
            })
    }

    async fn step_post_merge(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        match check.state {
            CheckState::Queued => self.pm_queued(repo, check).await,
            CheckState::Verifying => self.pm_verifying(task, repo, check).await,
            CheckState::VerificationFailed => self.pm_verification_failed(task, repo, check).await,
            CheckState::PreparingRevert => self.pm_preparing_revert(task, repo, check).await,
            CheckState::RevertReady => self.pm_revert_ready(task, repo, check).await,
            CheckState::Passed
            | CheckState::Superseded
            | CheckState::RevertOpen
            | CheckState::Blocked => Ok(()),
        }
    }

    fn pm_dir(&self, check_id: i64, suffix: &str) -> PathBuf {
        self.paths
            .home
            .join("post-merge")
            .join(format!("{check_id}-{suffix}"))
    }

    const WORKTREE_SUFFIXES: [&'static str; 7] = [
        "verify",
        "verify-rerun",
        "base",
        "base-rerun",
        "revert",
        "revert-check",
        "revert-check-rerun",
    ];

    async fn discard_check_worktrees(&self, repo: &RepoConfig, check_id: i64) {
        let repo_path = self.checkout(repo);
        for suffix in Self::WORKTREE_SUFFIXES {
            let wt = self.pm_dir(check_id, suffix);
            if let Err(e) = self.git.worktree_discard(&repo_path, &wt).await {
                eprintln!("provefab: could not remove {}: {e}", wt.display());
            }
        }
    }

    /// Terminal cleanup: the local revert branch goes; remote branches never do.
    async fn finish_terminal(&self, repo: &RepoConfig, check: &PostMergeCheckRow) {
        if let Some(branch) = &check.revert_branch {
            let _ = self.git.branch_delete(&self.checkout(repo), branch).await;
        }
    }

    async fn advance(
        &self,
        check: &PostMergeCheckRow,
        to: CheckState,
        patch: CheckPatch,
    ) -> Result<(), PipelineError> {
        self.store
            .advance_post_merge(check.id, check.state, to, &patch)
            .await?;
        Ok(())
    }

    async fn block(
        &self,
        check: &PostMergeCheckRow,
        kind: FailureKind,
        summary: &str,
    ) -> Result<(), PipelineError> {
        self.advance(
            check,
            CheckState::Blocked,
            CheckPatch {
                failure_kind: Some(kind),
                failure_summary: Some(summary.to_string()),
                ..Default::default()
            },
        )
        .await
    }

    /// Runs the checks on a fresh detached checkout of `commit`, then reruns
    /// each failure once on another fresh checkout (spec section 5).
    #[allow(clippy::too_many_arguments)]
    async fn pm_run(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
        commit: &str,
        suffix: &str,
        stage: &str,
        require_clean: bool,
    ) -> Result<Confirmed, PipelineError> {
        let repo_path = self.checkout(repo);
        let wt = self.pm_dir(check.id, suffix);
        self.git
            .worktree_fresh_detached(&repo_path, &wt, commit)
            .await?;
        let first = self
            .gates(task, &wt, &repo.post_merge_checks, stage)
            .await?;
        let dirty = require_clean && !self.git.clean(&wt).await?;
        self.git.worktree_discard(&repo_path, &wt).await?;
        if first.passed() || dirty {
            return Ok(Confirmed {
                dirty,
                ..confirm(&first, None)
            });
        }
        let failing: Vec<String> = first
            .results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| r.command.clone())
            .collect();
        let rerun_wt = self.pm_dir(check.id, &format!("{suffix}-rerun"));
        self.git
            .worktree_fresh_detached(&repo_path, &rerun_wt, commit)
            .await?;
        let rerun = self.gates(task, &rerun_wt, &failing, stage).await?;
        self.git.worktree_discard(&repo_path, &rerun_wt).await?;
        Ok(confirm(&first, Some(&rerun)))
    }

    async fn pm_queued(
        &self,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let repo_path = self.checkout(repo);
        self.git.fetch(&repo_path).await?;
        let parents = self.git.parent_count(&repo_path, &check.merge_sha).await?;
        match revert_plan(parents, check.commit_count, check.auto_merged) {
            Some(_) => {
                self.advance(check, CheckState::Verifying, CheckPatch::default())
                    .await
            }
            None => {
                self.block(
                    check,
                    FailureKind::UnsafeMergeStrategy,
                    &format!(
                        "{parents} parent(s), {:?} PR commit(s), auto-merged: {}",
                        check.commit_count, check.auto_merged
                    ),
                )
                .await
            }
        }
    }

    async fn pm_verifying(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let r = self
            .pm_run(
                task,
                repo,
                check,
                &check.merge_sha,
                "verify",
                "post-merge",
                false,
            )
            .await?;
        if r.failed.is_empty() {
            return self
                .advance(
                    check,
                    CheckState::Passed,
                    CheckPatch {
                        flaky: Some(r.flaky),
                        ..Default::default()
                    },
                )
                .await;
        }
        self.advance(
            check,
            CheckState::VerificationFailed,
            CheckPatch {
                failure_kind: Some(FailureKind::CheckFailed),
                failure_summary: Some(failure_summary(&r.failed)),
                failed_commands: Some(r.failed),
                flaky: Some(r.flaky),
                ..Default::default()
            },
        )
        .await
    }

    async fn pm_verification_failed(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let repo_path = self.checkout(repo);
        self.git.fetch(&repo_path).await?;
        let base_ref = self.git.base_ref(&repo_path, &repo.base).await;
        let tip = self.git.rev_parse(&repo_path, &base_ref).await?;
        if !self
            .git
            .is_ancestor(&repo_path, &check.merge_sha, &tip)
            .await?
        {
            return self
                .block(
                    check,
                    FailureKind::BaseDiverged,
                    &format!("{} is not an ancestor of {tip}", check.merge_sha),
                )
                .await;
        }
        let patch = CheckPatch {
            base_sha: Some(tip.clone()),
            ..Default::default()
        };
        // Nothing landed since the merge: the base is the commit that already failed twice.
        if tip == check.merge_sha {
            return self
                .advance(check, CheckState::PreparingRevert, patch)
                .await;
        }
        let r = self
            .pm_run(task, repo, check, &tip, "base", "post-merge-base", false)
            .await?;
        // failure_kind/failed_commands are kept on Superseded on purpose: they describe the
        // failure on the merged commit; render and CLI output key on state first.
        let to = if r.failed.is_empty() {
            CheckState::Superseded
        } else {
            CheckState::PreparingRevert
        };
        self.advance(check, to, patch).await
    }

    async fn pm_preparing_revert(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let base_sha = check
            .base_sha
            .clone()
            .ok_or_else(|| StoreError::Corrupt(format!("check {} has no base_sha", check.id)))?;
        let repo_path = self.checkout(repo);
        let parents = self.git.parent_count(&repo_path, &check.merge_sha).await?;
        let Some(plan) = revert_plan(parents, check.commit_count, check.auto_merged) else {
            return self
                .block(
                    check,
                    FailureKind::UnsafeMergeStrategy,
                    "merge shape changed",
                )
                .await;
        };
        let wt = self.pm_dir(check.id, "revert");
        self.git
            .worktree_fresh_detached(&repo_path, &wt, &base_sha)
            .await?;
        match self
            .git
            .revert(&wt, &check.merge_sha, plan.mainline())
            .await
        {
            Ok(()) => {}
            // git exits 1 on a conflict; anything else is infrastructure.
            Err(ForgeError::Failed {
                code: Some(1),
                stderr,
                ..
            }) => {
                return self
                    .block(check, FailureKind::RevertConflict, &stderr)
                    .await;
            }
            Err(e) => return Err(e.into()),
        }
        let revert_sha = self.git.head(&wt).await?;
        self.git.worktree_discard(&repo_path, &wt).await?;
        let r = self
            .pm_run(
                task,
                repo,
                check,
                &revert_sha,
                "revert-check",
                "revert-check",
                true,
            )
            .await?;
        if r.dirty {
            return self
                .block(
                    check,
                    FailureKind::DirtyTree,
                    "a check modified tracked files on the revert",
                )
                .await;
        }
        if !r.failed.is_empty() {
            return self
                .block(
                    check,
                    FailureKind::RevertChecksFailed,
                    &failure_summary(&r.failed),
                )
                .await;
        }
        // Created only once the revert is proven, so no blocked row leaks a branch.
        let branch = format!("provefab/revert-{}-{}", check.id, check.base_moves);
        self.git
            .branch_force(&repo_path, &branch, &revert_sha)
            .await?;
        self.advance(
            check,
            CheckState::RevertReady,
            CheckPatch {
                revert_sha: Some(revert_sha),
                revert_branch: Some(branch),
                ..Default::default()
            },
        )
        .await
    }
    async fn pm_revert_ready(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("check {} has no {what}", check.id));
        let base_sha = check.base_sha.clone().ok_or_else(|| corrupt("base_sha"))?;
        let revert_sha = check
            .revert_sha
            .clone()
            .ok_or_else(|| corrupt("revert_sha"))?;
        let branch = check
            .revert_branch
            .clone()
            .ok_or_else(|| corrupt("revert_branch"))?;
        let repo_path = self.checkout(repo);
        self.git.fetch(&repo_path).await?;
        let base_ref = self.git.base_ref(&repo_path, &repo.base).await;
        let tip = self.git.rev_parse(&repo_path, &base_ref).await?;
        if tip != base_sha {
            if check.base_moves + 1 >= BASE_MOVE_LIMIT {
                return self
                    .block(
                        check,
                        FailureKind::BaseMoved,
                        &format!("base moved {} times", check.base_moves + 1),
                    )
                    .await;
            }
            return self
                .advance(
                    check,
                    CheckState::VerificationFailed,
                    CheckPatch {
                        bump_base_moves: true,
                        ..Default::default()
                    },
                )
                .await;
        }
        match self.git.remote_branch_sha(&repo_path, &branch).await? {
            None => self.git.push_sha(&repo_path, &revert_sha, &branch).await?,
            Some(s) if s == revert_sha => {}
            Some(s) => {
                return self
                    .block(
                        check,
                        FailureKind::BranchConflict,
                        &format!("origin/{branch} is {s}, expected {revert_sha}"),
                    )
                    .await;
            }
        }
        let body = revert_pr_body(
            task.pr_url.as_deref().unwrap_or("unknown"),
            &task.issue_url,
            check,
        );
        let title = format!(
            "Revert Provefab change {}",
            &check.merge_sha[..check.merge_sha.len().min(12)]
        );
        let url = self
            .hub
            .pr_create(&repo.slug, &branch, &repo.base, &title, &body)
            .await?;
        let head = self.hub.pr_status(&repo.slug, &url).await?.head_sha;
        if head.as_deref() != Some(revert_sha.as_str()) {
            return self
                .block(
                    check,
                    FailureKind::BranchConflict,
                    &format!("{url} head is {head:?}, expected {revert_sha}"),
                )
                .await;
        }
        self.advance(
            check,
            CheckState::RevertOpen,
            CheckPatch {
                revert_pr_url: Some(url),
                ..Default::default()
            },
        )
        .await
    }
    /// One comment per target per check, from fixed templates only (spec section 7).
    /// The marker makes a crash between the GitHub write and the SQLite write
    /// harmless; `INFRA_ERROR_LIMIT` consecutive failures give a target up.
    async fn notify_post_merge(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let Some(body) = render(check) else {
            return Ok(());
        };
        let mark = marker(check.id);
        if check.issue_notified_at.is_none() {
            let posted = async {
                let seen = self
                    .hub
                    .comments(&repo.slug, task.issue_number)
                    .await?
                    .iter()
                    .any(|c| c.body.contains(&mark));
                // A queued effect from a crashed earlier tick already carries it.
                let queued = self
                    .store
                    .pending_github()
                    .await?
                    .iter()
                    .any(|(_, t, e)| *t == task.id && e.to_string().contains(&mark));
                if !seen && !queued {
                    self.tell(task.id, &repo.slug, task.issue_number, &body)
                        .await?;
                }
                Ok::<(), PipelineError>(())
            }
            .await;
            self.settle_notice(check, NoticeTarget::Issue, posted)
                .await?;
        }
        if check.pr_notified_at.is_none() {
            let Some(url) = task.pr_url.as_deref() else {
                self.store
                    .mark_post_merge_notified(check.id, NoticeTarget::Pr)
                    .await?;
                return Ok(());
            };
            let posted = async {
                let seen = self
                    .hub
                    .pr_status(&repo.slug, url)
                    .await?
                    .comments
                    .iter()
                    .any(|c| c.body.contains(&mark));
                if !seen {
                    self.hub.pr_comment(&repo.slug, url, &body).await?;
                }
                Ok::<(), PipelineError>(())
            }
            .await;
            self.settle_notice(check, NoticeTarget::Pr, posted).await?;
        }
        Ok(())
    }

    /// Marks a target done on success; on failure counts it and gives the
    /// target up at the limit (logged, returns `Ok`), else returns the error.
    async fn settle_notice(
        &self,
        check: &PostMergeCheckRow,
        target: NoticeTarget,
        posted: Result<(), PipelineError>,
    ) -> Result<(), PipelineError> {
        match posted {
            Ok(()) => {
                self.store.reset_post_merge_infra_errors(check.id).await?;
                self.store
                    .mark_post_merge_notified(check.id, target)
                    .await?;
                Ok(())
            }
            Err(e) => {
                let n = self.store.post_merge_infra_error(check.id).await?;
                if n >= INFRA_ERROR_LIMIT {
                    eprintln!(
                        "provefab: gave up notifying {target:?} for post-merge check {}: {e}",
                        check.id
                    );
                    self.store.reset_post_merge_infra_errors(check.id).await?;
                    self.store
                        .mark_post_merge_notified(check.id, target)
                        .await?;
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gates::{GateReport, GateResult, ProgressScore};

    #[test]
    fn states_and_kinds_round_trip_and_reject_unknown_text() {
        for s in CheckState::ALL {
            assert_eq!(CheckState::parse(s.as_str()), Some(s));
        }
        for k in FailureKind::ALL {
            assert_eq!(FailureKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(CheckState::parse("failed"), None);
        assert_eq!(FailureKind::parse(""), None);
        let terminal: Vec<_> = CheckState::ALL
            .into_iter()
            .filter(|s| s.is_terminal())
            .collect();
        assert_eq!(
            terminal,
            [
                CheckState::Passed,
                CheckState::Superseded,
                CheckState::RevertOpen,
                CheckState::Blocked
            ]
        );
    }

    fn result(command: &str, passed: bool, exit: Option<i32>, timed_out: bool) -> GateResult {
        GateResult {
            command: command.into(),
            exit,
            passed,
            timed_out,
            output_tail: "SENTINEL_SECRET_42 /Users/someone/.provefab".into(),
            failing_tests: 0,
            error_lines: 0,
        }
    }

    fn report(results: Vec<GateResult>) -> GateReport {
        GateReport {
            results,
            score: ProgressScore::default(),
        }
    }

    #[test]
    fn a_rerun_rescue_is_flaky_and_a_repeat_failure_is_confirmed() {
        let first = report(vec![
            result("a", false, Some(1), false),
            result("b", true, Some(0), false),
            result("c", false, None, true),
        ]);
        let rerun = report(vec![
            result("a", true, Some(0), false),
            result("c", false, None, true),
        ]);
        let c = confirm(&first, Some(&rerun));
        assert_eq!(c.flaky, ["a"]);
        assert_eq!(
            c.failed,
            [FailedCommand {
                command: "c".into(),
                exit: None,
                timed_out: true
            }]
        );
        let clean = confirm(&report(vec![result("b", true, Some(0), false)]), None);
        assert!(clean.failed.is_empty() && clean.flaky.is_empty());
    }

    #[test]
    fn revert_plans_follow_the_merge_shape() {
        assert_eq!(revert_plan(2, Some(5), false), Some(RevertPlan::Mainline1));
        assert_eq!(revert_plan(1, Some(1), false), Some(RevertPlan::Plain));
        assert_eq!(revert_plan(1, Some(4), true), Some(RevertPlan::Plain));
        assert_eq!(revert_plan(1, Some(4), false), None);
        assert_eq!(revert_plan(1, None, true), None);
        assert_eq!(revert_plan(3, Some(1), false), None);
        assert_eq!(revert_plan(0, Some(1), false), None);
        assert_eq!(RevertPlan::Mainline1.mainline(), Some(1));
        assert_eq!(RevertPlan::Plain.mainline(), None);
    }

    #[test]
    fn bounded_never_splits_a_character() {
        let s = "é".repeat(SUMMARY_MAX + 10);
        let b = bounded(&s);
        assert_eq!(b.chars().count(), SUMMARY_MAX);
        assert!(b.ends_with("..."));
        assert_eq!(bounded("short"), "short");
    }

    fn row(state: CheckState, kind: Option<FailureKind>) -> crate::store::PostMergeCheckRow {
        crate::store::PostMergeCheckRow {
            id: 42,
            task_id: 7,
            merge_sha: "abc123".into(),
            base: "main".into(),
            commit_count: Some(1),
            auto_merged: false,
            state,
            failure_kind: kind,
            failure_summary: Some(
                "SENTINEL_SECRET_42 at /Users/someone/.provefab/post-merge".into(),
            ),
            failed_commands: vec![FailedCommand {
                command: "cargo test".into(),
                exit: Some(101),
                timed_out: false,
            }],
            flaky: vec![],
            base_sha: Some("def456".into()),
            revert_sha: Some("fed789".into()),
            revert_branch: Some("provefab/revert-42-0".into()),
            revert_pr_url: Some("https://github.com/o/r/pull/100".into()),
            base_moves: 0,
            infra_errors: 0,
            started_at: None,
            finished_at: None,
            issue_notified_at: None,
            pr_notified_at: None,
        }
    }

    #[test]
    fn every_published_text_is_a_fixed_template_with_its_marker() {
        let mut outcomes = vec![
            row(CheckState::Superseded, Some(FailureKind::CheckFailed)),
            row(CheckState::RevertOpen, Some(FailureKind::CheckFailed)),
        ];
        for k in FailureKind::ALL {
            outcomes.push(row(CheckState::Blocked, Some(k)));
        }
        for r in &outcomes {
            let body = render(r).expect("terminal failure outcomes are published");
            assert!(body.ends_with(&marker(42)), "{body}");
            assert!(!body.contains("SENTINEL_SECRET_42"), "{body}");
            assert!(!body.contains("/Users/"), "{body}");
            assert!(!body.contains('\u{2014}'), "no em-dash: {body}");
            assert!(body.contains("abc123"), "{body}");
        }
        assert!(
            render(&row(CheckState::RevertOpen, None))
                .unwrap()
                .contains("https://github.com/o/r/pull/100")
        );
        assert!(
            render(&row(CheckState::Blocked, Some(FailureKind::RevertConflict)))
                .unwrap()
                .contains("`cargo test` exited with 101")
        );
        assert_eq!(render(&row(CheckState::Passed, None)), None);
        assert_eq!(render(&row(CheckState::Verifying, None)), None);
    }

    #[test]
    fn the_revert_pr_body_names_both_commits_and_never_auto_merges() {
        let body = revert_pr_body(
            "https://github.com/o/r/pull/8",
            "https://github.com/o/r/issues/7",
            &row(CheckState::RevertReady, Some(FailureKind::CheckFailed)),
        );
        for needle in [
            "https://github.com/o/r/pull/8",
            "https://github.com/o/r/issues/7",
            "abc123",
            "def456",
            "`cargo test` exited with 101",
            "Provefab will not merge this revert automatically.",
        ] {
            assert!(body.contains(needle), "{needle} missing from {body}");
        }
        assert!(!body.contains("SENTINEL_SECRET_42") && !body.contains('\u{2014}'));
    }
}
