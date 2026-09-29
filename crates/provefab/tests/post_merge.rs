#![cfg(feature = "testkit")]
//! Post-merge verification (spec revision 2), against real git repos.

use std::path::Path;

use provefab::post_merge::{CheckState, FailureKind};
use provefab::task::TaskState;
use provefab::testkit::{FakeHub, FakeOracle, Fixture, Script, fixture, git, pipeline, queue};
use serde_json::json;

fn commit_and_push(repo: &Path, file: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(file), content).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-qm", msg]);
    git(repo, &["push", "-q", "origin", "main"]);
    git(repo, &["rev-parse", "HEAD"])
}

#[tokio::test]
async fn git_helpers_revert_exactly_and_never_reuse_residue() {
    let f = fixture(&["true"]);
    let repo = f.config.repos[0].path_in(&f.home);
    let g = provefab::forge::Git {
        program: "git".into(),
    };
    let sha = commit_and_push(&repo, "README.md", "broken\n", "change");
    assert_eq!(g.parent_count(&repo, &sha).await.unwrap(), 1);

    // Residue at the path is discarded, never reused.
    let wt = f.home.join("post-merge").join("1-verify");
    g.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    std::fs::write(wt.join("README.md"), "residue\n").unwrap();
    g.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(wt.join("README.md")).unwrap(),
        "broken\n"
    );
    assert!(g.clean(&wt).await.unwrap());

    // A plain directory (not a registered worktree) is discarded too.
    g.worktree_discard(&repo, &wt).await.unwrap();
    std::fs::create_dir_all(wt.join("junk")).unwrap();
    g.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    assert!(!wt.join("junk").exists());

    g.revert(&wt, &sha, None).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(wt.join("README.md")).unwrap(),
        "hello\n"
    );
    let revert = g.head(&wt).await.unwrap();

    assert_eq!(
        g.remote_branch_sha(&repo, "provefab/revert-1-0")
            .await
            .unwrap(),
        None
    );
    g.branch_force(&repo, "provefab/revert-1-0", &revert)
        .await
        .unwrap();
    g.push_sha(&repo, &revert, "provefab/revert-1-0")
        .await
        .unwrap();
    assert_eq!(
        g.remote_branch_sha(&repo, "provefab/revert-1-0")
            .await
            .unwrap()
            .as_deref(),
        Some(revert.as_str())
    );
    g.worktree_discard(&repo, &wt).await.unwrap();
    assert!(!wt.exists());
    g.branch_delete(&repo, "provefab/revert-1-0").await.unwrap();
    g.branch_delete(&repo, "provefab/revert-1-0").await.unwrap(); // missing is fine
}

#[tokio::test]
async fn a_merge_commit_reverts_with_mainline_one() {
    let f = fixture(&["true"]);
    let repo = f.config.repos[0].path_in(&f.home);
    let g = provefab::forge::Git {
        program: "git".into(),
    };
    git(&repo, &["switch", "-qc", "feature"]);
    std::fs::write(repo.join("README.md"), "broken\n").unwrap();
    git(&repo, &["commit", "-qam", "feature"]);
    git(&repo, &["switch", "-q", "main"]);
    git(
        &repo,
        &["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    let merge = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(g.parent_count(&repo, &merge).await.unwrap(), 2);
    let wt = f.home.join("post-merge").join("2-revert");
    g.worktree_fresh_detached(&repo, &wt, &merge).await.unwrap();
    assert!(g.revert(&wt, &merge, None).await.is_err());
    g.worktree_fresh_detached(&repo, &wt, &merge).await.unwrap();
    g.revert(&wt, &merge, Some(1)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(wt.join("README.md")).unwrap(),
        "hello\n"
    );
}

type P = provefab::pipeline::Pipeline<provefab::testkit::FakeRunner, FakeOracle, FakeHub>;

fn unused_worker(
    _: &provefab::config::ModelEntry,
    _: &agent_workers::StageRequest,
    _: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>,
) -> Option<agent_workers::StageResult> {
    None
}

/// A task whose Provefab PR (pull/8) is open, in a repo with `checks`.
async fn open_pr_task(checks: &[&str]) -> (Fixture, P, i64) {
    let mut f = fixture(&["true"]);
    f.config.repos[0].post_merge_checks = checks.iter().map(|c| c.to_string()).collect();
    let p = pipeline(
        &f,
        Box::new(unused_worker) as Box<Script>,
        FakeOracle::default(),
        FakeHub::new("issue"),
    )
    .await;
    *p.hub.revert_origin.lock().unwrap() = Some(f.origin.clone());
    let id = queue(&p).await;
    p.store
        .set_pr(id, "https://github.com/o/r/pull/8", "open")
        .await
        .unwrap();
    p.store
        .transition(id, TaskState::PrOpen, "pr")
        .await
        .unwrap();
    (f, p, id)
}

fn merged(
    sha: Option<&str>,
    base: Option<&str>,
    commits: Option<usize>,
) -> provefab::forge::PrStatus {
    provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: Some("pr-head".into()),
        merge_sha: sha.map(Into::into),
        base_ref: base.map(Into::into),
        commit_count: commits,
    }
}

#[tokio::test]
async fn a_merge_on_the_configured_base_queues_one_check() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    let c = p.store.post_merge_checks(id).await.unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].merge_sha.as_str(), c[0].state),
        ("abc", CheckState::Queued)
    );
    assert_eq!(
        p.store.task(id).await.unwrap().unwrap().pr_state.as_deref(),
        Some("merged")
    );
}

#[tokio::test]
async fn another_base_or_no_opt_in_creates_no_check() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("release"), Some(1));
    p.watch_pr(id).await.unwrap();
    assert!(p.store.post_merge_checks(id).await.unwrap().is_empty());

    let (_f, p, id) = open_pr_task(&[]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    assert!(p.store.post_merge_checks(id).await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_attribution_waits_an_hour_then_blocks() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    *p.hub.pr_status.lock().unwrap() = merged(None, Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    assert!(p.store.post_merge_checks(id).await.unwrap().is_empty());
    assert_eq!(
        p.store.task(id).await.unwrap().unwrap().pr_state.as_deref(),
        Some("open")
    );
    // An hour later GitHub still has no merge commit: record the merge, block the check.
    let long_ago = provefab::store::now() - provefab::post_merge::ATTRIBUTION_WAIT_SECS - 1;
    p.store
        .record_output(id, "merge_seen", &json!({"at": long_ago}))
        .await
        .unwrap();
    p.watch_pr(id).await.unwrap();
    let c = p.store.post_merge_checks(id).await.unwrap();
    assert_eq!(c[0].merge_sha, "unknown");
    assert_eq!(
        (c[0].state, c[0].failure_kind),
        (CheckState::Blocked, Some(FailureKind::AttributionMissing))
    );
    assert_eq!(
        p.store.task(id).await.unwrap().unwrap().pr_state.as_deref(),
        Some("merged")
    );
}

#[tokio::test]
async fn a_missing_base_is_never_inferred() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), None, Some(1));
    let long_ago = provefab::store::now() - provefab::post_merge::ATTRIBUTION_WAIT_SECS - 1;
    p.store
        .record_output(id, "merge_seen", &json!({"at": long_ago}))
        .await
        .unwrap();
    p.watch_pr(id).await.unwrap();
    let c = p.store.post_merge_checks(id).await.unwrap();
    assert_eq!(c[0].failure_kind, Some(FailureKind::AttributionMissing));
}

#[tokio::test]
async fn an_auto_merge_is_recorded_on_the_check() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    p.store
        .record_output(id, "auto_merged", &json!({"head": "pr-head"}))
        .await
        .unwrap();
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("main"), Some(3));
    p.watch_pr(id).await.unwrap();
    let c = p.store.post_merge_checks(id).await.unwrap();
    assert!(c[0].auto_merged);
    assert_eq!(c[0].commit_count, Some(3));
}

#[tokio::test]
async fn opting_in_later_never_checks_an_old_merge() {
    let (_f, p, id) = open_pr_task(&[]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    // The owner opts in after the merge was recorded.
    let mut p = p;
    p.config.repos[0].post_merge_checks = vec!["true".into()];
    p.watch_pr(id).await.unwrap();
    p.process_post_merge(id).await.unwrap();
    assert!(p.store.post_merge_checks(id).await.unwrap().is_empty());
}
