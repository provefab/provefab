//! What happens around the review stage (spec §3, D59): how many approvals a
//! head needs before its PR opens, and what to do once it is open. The free
//! core ships `OpenPrOnly`; Provefab Pro supplies the guarded auto-merge.

use std::future::Future;
use std::pin::Pin;

use crate::config::{Config, RepoConfig};
use crate::forge::{Change, ForgeError};
use crate::pipeline::PipelineError;
use crate::store::TaskRow;

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
