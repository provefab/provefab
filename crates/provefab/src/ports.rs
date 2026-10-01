//! What the pipeline needs from Jev and from GitHub, as traits, so the
//! pipeline can be tested with fakes. Real implementations live here too.

use std::future::Future;
use std::path::Path;

use agent_workers::WorkerEvent;
use jev::JevClient;

use crate::forge::{Comment, ForgeError, Gh, Issue, PrStatus};
use crate::jevq::{self, IssueContext, Triage};
use crate::task::Verdict;

/// Jev, seen by the pipeline. `None` means "no answer": the caller uses the
/// spec's fallback (§4). Jev never approves anything (D8).
pub trait Oracle {
    fn classify(&self, issue: &IssueContext) -> impl Future<Output = Option<Verdict>> + Send;
    fn loop_probability(&self, events: &[WorkerEvent]) -> impl Future<Output = Option<f64>> + Send;
    fn triage(&self, command: &str, output: &str) -> impl Future<Output = Option<Triage>> + Send;
    fn reply_answers(
        &self,
        question: &str,
        reply: &str,
    ) -> impl Future<Output = Option<f64>> + Send;
}

pub struct JevOracle {
    pub client: JevClient,
}

fn unavailable_line(what: &str, e: &jev::JevError) -> String {
    format!("provefab: jev_unavailable ({what}): {e}; the fallback applies")
}

/// A Jev answer, or `None` with the reason logged (spec §7). A silent `None`
/// hid a wrong model name for every task (seen live 2026-09-25).
fn answered<T>(what: &str, r: Result<T, jev::JevError>) -> Option<T> {
    r.map_err(|e| eprintln!("{}", unavailable_line(what, &e)))
        .ok()
}

impl Oracle for JevOracle {
    async fn classify(&self, issue: &IssueContext) -> Option<Verdict> {
        answered(
            "classify",
            jevq::classify(&self.client, issue, jevq::DEADLINE).await,
        )
    }

    async fn loop_probability(&self, events: &[WorkerEvent]) -> Option<f64> {
        answered(
            "loop check",
            jevq::loop_probability(&self.client, events, jevq::DEADLINE).await,
        )
    }

    async fn triage(&self, command: &str, output: &str) -> Option<Triage> {
        answered(
            "triage",
            jevq::triage(&self.client, command, output, jevq::DEADLINE).await,
        )
    }

    async fn reply_answers(&self, question: &str, reply: &str) -> Option<f64> {
        answered(
            "reply check",
            jevq::reply_answers(&self.client, question, reply, jevq::DEADLINE).await,
        )
    }
}

/// No Jev key configured: every question falls back (spec §7, `jev_unavailable`).
pub struct NoOracle;

impl Oracle for NoOracle {
    async fn classify(&self, _issue: &IssueContext) -> Option<Verdict> {
        None
    }
    async fn loop_probability(&self, _events: &[WorkerEvent]) -> Option<f64> {
        None
    }
    async fn triage(&self, _command: &str, _output: &str) -> Option<Triage> {
        None
    }
    async fn reply_answers(&self, _question: &str, _reply: &str) -> Option<f64> {
        None
    }
}

/// An optional oracle: `None` (no Jev key) answers nothing, so every question
/// takes its fallback.
impl<O: Oracle + Sync> Oracle for Option<O> {
    async fn classify(&self, issue: &IssueContext) -> Option<Verdict> {
        match self {
            Some(o) => o.classify(issue).await,
            None => None,
        }
    }
    async fn loop_probability(&self, events: &[WorkerEvent]) -> Option<f64> {
        match self {
            Some(o) => o.loop_probability(events).await,
            None => None,
        }
    }
    async fn triage(&self, command: &str, output: &str) -> Option<Triage> {
        match self {
            Some(o) => o.triage(command, output).await,
            None => None,
        }
    }
    async fn reply_answers(&self, question: &str, reply: &str) -> Option<f64> {
        match self {
            Some(o) => o.reply_answers(question, reply).await,
            None => None,
        }
    }
}

/// Where a repository's issues live and Provefab reports progress on them:
/// GitHub, Jira or Linear (issue trackers spec §3). Addressed by repository
/// slug and issue number; an adapter rebuilds a ticket key from its config.
pub trait Tracker {
    fn open_issues(
        &self,
        slug: &str,
        label: &str,
    ) -> impl Future<Output = Result<Vec<Issue>, ForgeError>> + Send;
    fn issue(
        &self,
        slug: &str,
        number: u64,
    ) -> impl Future<Output = Result<Issue, ForgeError>> + Send;
    fn comments(
        &self,
        slug: &str,
        number: u64,
    ) -> impl Future<Output = Result<Vec<Comment>, ForgeError>> + Send;
    fn comment(
        &self,
        slug: &str,
        number: u64,
        body: &str,
    ) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn edit_labels(
        &self,
        slug: &str,
        number: u64,
        add: &[&str],
        remove: &[&str],
    ) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn ensure_label(
        &self,
        slug: &str,
        name: &str,
        color: &str,
        description: &str,
    ) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn issue_open(
        &self,
        slug: &str,
        number: u64,
    ) -> impl Future<Output = Result<bool, ForgeError>> + Send;
}

/// Where the code lives: GitHub, always (pull requests, clone, visibility).
pub trait Forge {
    fn pr_comment(
        &self,
        slug: &str,
        url: &str,
        body: &str,
    ) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn pr_create(
        &self,
        slug: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> impl Future<Output = Result<String, ForgeError>> + Send;
    /// Replaces a pull request's title and body (a periodic proposal updated in place).
    fn pr_edit(
        &self,
        slug: &str,
        url: &str,
        title: &str,
        body: &str,
    ) -> impl Future<Output = Result<(), ForgeError>> + Send;
    /// Clones the repo into `dest` (a provefab-managed checkout, D48).
    fn repo_clone(
        &self,
        slug: &str,
        dest: &Path,
    ) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn pr_status(
        &self,
        slug: &str,
        url: &str,
    ) -> impl Future<Output = Result<PrStatus, ForgeError>> + Send;
    /// Squash-merges the PR and deletes its branch, only at `head` (auto-merge, D49).
    fn pr_merge(
        &self,
        slug: &str,
        url: &str,
        head: &str,
    ) -> impl Future<Output = Result<(), ForgeError>> + Send;
    /// Whether anyone can write the repository's issues (a public repository).
    fn repo_is_public(&self, slug: &str) -> impl Future<Output = Result<bool, ForgeError>> + Send;
}

/// Everything the pipeline needs: one tracker and the forge. A blanket
/// implementation, so the pipeline's bounds and Provefab Pro stay unchanged.
pub trait Hub: Tracker + Forge {}

impl<T: Tracker + Forge> Hub for T {}

impl Tracker for Gh {
    async fn open_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
        Gh::labeled_issues(self, slug, label).await
    }
    async fn issue(&self, slug: &str, number: u64) -> Result<Issue, ForgeError> {
        Gh::issue(self, slug, number).await
    }
    async fn comments(&self, slug: &str, number: u64) -> Result<Vec<Comment>, ForgeError> {
        Gh::comments(self, slug, number).await
    }
    async fn comment(&self, slug: &str, number: u64, body: &str) -> Result<(), ForgeError> {
        Gh::comment(self, slug, number, body).await
    }
    async fn edit_labels(
        &self,
        slug: &str,
        number: u64,
        add: &[&str],
        remove: &[&str],
    ) -> Result<(), ForgeError> {
        Gh::edit_labels(self, slug, number, add, remove).await
    }
    async fn ensure_label(
        &self,
        slug: &str,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), ForgeError> {
        Gh::ensure_label(self, slug, name, color, description).await
    }
    async fn issue_open(&self, slug: &str, number: u64) -> Result<bool, ForgeError> {
        Gh::issue_open(self, slug, number).await
    }
}

impl Forge for Gh {
    async fn pr_comment(&self, slug: &str, url: &str, body: &str) -> Result<(), ForgeError> {
        Gh::pr_comment(self, slug, url, body).await
    }
    async fn pr_create(
        &self,
        slug: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, ForgeError> {
        Gh::pr_create(self, slug, head, base, title, body).await
    }
    async fn pr_edit(
        &self,
        slug: &str,
        url: &str,
        title: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        Gh::pr_edit(self, slug, url, title, body).await
    }
    async fn repo_clone(&self, slug: &str, dest: &Path) -> Result<(), ForgeError> {
        Gh::repo_clone(self, slug, dest).await
    }
    async fn pr_status(&self, slug: &str, url: &str) -> Result<PrStatus, ForgeError> {
        Gh::pr_status(self, slug, url).await
    }
    async fn pr_merge(&self, slug: &str, url: &str, head: &str) -> Result<(), ForgeError> {
        Gh::pr_merge(self, slug, url, head).await
    }
    async fn repo_is_public(&self, slug: &str) -> Result<bool, ForgeError> {
        Gh::repo_is_public(self, slug).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The port split (issue trackers spec §3): `Gh` is both ports, so a hub.
    #[test]
    fn gh_is_a_tracker_and_a_forge_so_a_hub() {
        fn tracker<T: Tracker>() {}
        fn forge<F: Forge>() {}
        fn hub<H: Hub>() {}
        tracker::<Gh>();
        forge::<Gh>();
        hub::<Gh>();
    }

    #[tokio::test]
    async fn no_oracle_always_falls_back() {
        let o = NoOracle;
        let issue = IssueContext {
            title: "t".into(),
            body: "b".into(),
            labels: vec![],
            repo_language: None,
            repo_size_kb: None,
        };
        assert_eq!(o.classify(&issue).await, None);
        assert_eq!(o.loop_probability(&[]).await, None);
        assert_eq!(o.triage("c", "o").await, None);
        assert_eq!(o.reply_answers("q", "r").await, None);
    }

    struct Always;

    impl Oracle for Always {
        async fn classify(&self, _: &IssueContext) -> Option<Verdict> {
            None
        }
        async fn loop_probability(&self, _: &[WorkerEvent]) -> Option<f64> {
            Some(0.9)
        }
        async fn triage(&self, _: &str, _: &str) -> Option<Triage> {
            Some(Triage::FlakyTest)
        }
        async fn reply_answers(&self, _: &str, _: &str) -> Option<f64> {
            Some(0.8)
        }
    }

    #[test]
    fn jev_errors_become_a_logged_fallback() {
        assert_eq!(answered("classify", Ok::<u8, jev::JevError>(3)), Some(3));
        let failed: Result<u8, jev::JevError> =
            Err(jev::JevError::Invalid("Unknown model: jev-1.13".into()));
        assert_eq!(answered("classify", failed), None);
        assert_eq!(
            unavailable_line("classify", &jev::JevError::Timeout),
            "provefab: jev_unavailable (classify): jev: timed out; the fallback applies"
        );
    }

    #[tokio::test]
    async fn an_optional_oracle_delegates_or_falls_back() {
        let some = Some(Always);
        assert_eq!(some.loop_probability(&[]).await, Some(0.9));
        assert_eq!(some.triage("c", "o").await, Some(Triage::FlakyTest));
        assert_eq!(some.reply_answers("q", "r").await, Some(0.8));
        let none: Option<Always> = None;
        assert_eq!(none.loop_probability(&[]).await, None);
        assert_eq!(none.triage("c", "o").await, None);
        assert_eq!(none.reply_answers("q", "r").await, None);
    }
}
