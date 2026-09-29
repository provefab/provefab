#![cfg(feature = "testkit")]

use provefab::task::TaskState;
use provefab::testkit::{FakeHub, FakeOracle, Script, fixture, git, pipeline, queue};

fn unused_worker(
    _: &provefab::config::ModelEntry,
    _: &agent_workers::StageRequest,
    _: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>,
) -> Option<agent_workers::StageResult> {
    None
}
use serde_json::json;

async fn setup(
    command: &str,
    change: &str,
) -> (
    provefab::testkit::Fixture,
    provefab::pipeline::Pipeline<provefab::testkit::FakeRunner, FakeOracle, FakeHub>,
    i64,
    String,
) {
    let mut f = fixture(&["true"]);
    f.config.repos[0].post_merge_checks = vec![command.into()];
    let repo = f.config.repos[0].path_in(&f.home);
    std::fs::write(repo.join("README.md"), change).unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-qm", "Merged Provefab change"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    let p = pipeline(
        &f,
        Box::new(unused_worker) as Box<Script>,
        FakeOracle::default(),
        FakeHub::new("issue"),
    )
    .await;
    let id = queue(&p).await;
    p.store
        .set_pr(id, "https://github.com/o/r/pull/8", "merged")
        .await
        .unwrap();
    p.store
        .transition(id, TaskState::PrOpen, "merged")
        .await
        .unwrap();
    p.store
        .ensure_post_merge_check(id, &sha, "main")
        .await
        .unwrap();
    p.store
        .record_output(
            id,
            "merged_at",
            &json!({"sha": sha, "base": "main", "at": provefab::store::now()}),
        )
        .await
        .unwrap();
    (f, p, id, sha)
}

#[tokio::test]
async fn opt_in_does_not_retroactively_check_previous_merges() {
    let (_f, p, id, sha) = setup("false", "broken\n").await;
    // Emulate a merge that was recorded before checks were configured.
    let check = p.store.post_merge_checks(id).await.unwrap().remove(0);
    // A different task has a merge record but no opt-in check row.
    let other = p
        .store
        .add_issue(&provefab::store::NewIssue {
            repo: "o/r".into(),
            number: 9,
            url: "https://github.com/o/r/issues/9".into(),
            title: "old merge".into(),
            author: "alice".into(),
        })
        .await
        .unwrap()
        .unwrap();
    p.store
        .set_pr(other, "https://github.com/o/r/pull/9", "merged")
        .await
        .unwrap();
    p.store
        .transition(other, TaskState::PrOpen, "merged")
        .await
        .unwrap();
    p.store
        .record_output(other, "merged_at", &json!({"sha": sha, "base": "main"}))
        .await
        .unwrap();
    p.process_post_merge(other).await.unwrap();
    assert!(p.store.post_merge_checks(other).await.unwrap().is_empty());
    assert_eq!(check.state, "queued");
}

#[tokio::test]
async fn interrupted_verification_restarts_from_a_clean_merged_checkout() {
    let (f, p, id, sha) = setup("grep -q hello README.md", "hello world\n").await;
    let repo = f.config.repos[0].path_in(&f.home);
    let wt = f
        .home
        .join("post-merge")
        .join(format!("{id}-{}", &sha[..12]));
    p.git.worktree_detached(&repo, &wt, &sha).await.unwrap();
    std::fs::write(wt.join("README.md"), "dirty leftovers\n").unwrap();
    let check = p.store.post_merge_checks(id).await.unwrap().remove(0);
    p.store.set_post_merge_running(check.id, &wt).await.unwrap();
    p.process_post_merge(id).await.unwrap();
    assert_eq!(
        p.store.post_merge_checks(id).await.unwrap()[0].state,
        "passed"
    );
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn watcher_queues_only_a_known_provefab_merge_sha() {
    let (_f, p, id, sha) = setup("grep -q hello README.md", "hello world\n").await;
    p.store.set_pr_state(id, "open").await.unwrap();
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: Some("different-pr-head".into()),
        merge_sha: Some(sha.clone()),
        base_ref: Some("main".into()),
        commit_count: Some(1),
    };
    p.watch_pr(id).await.unwrap();
    assert_eq!(
        p.store.post_merge_checks(id).await.unwrap()[0].merge_sha,
        sha
    );
    p.process_post_merge(id).await.unwrap();
    assert_eq!(
        p.store.post_merge_checks(id).await.unwrap()[0].state,
        "passed"
    );
}

#[tokio::test]
async fn multi_commit_merge_never_generates_a_partial_revert() {
    let (_f, p, id, sha) = setup("false", "broken\n").await;
    p.store.set_pr_state(id, "open").await.unwrap();
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: Some("last-head".into()),
        merge_sha: Some(sha),
        base_ref: Some("main".into()),
        commit_count: Some(2),
    };
    p.watch_pr(id).await.unwrap();
    p.process_post_merge(id).await.unwrap();
    assert_eq!(
        p.store.post_merge_checks(id).await.unwrap()[0].state,
        "blocked"
    );
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn passing_merge_is_verified_once_at_the_exact_commit() {
    let (f, p, id, sha) = setup("grep -q hello README.md", "hello world\n").await;
    p.process_post_merge(id).await.unwrap();
    p.process_post_merge(id).await.unwrap();
    let checks = p.store.post_merge_checks(id).await.unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].merge_sha, sha);
    assert_eq!(checks[0].state, "passed");
    assert!(p.hub.prs.lock().unwrap().is_empty());
    assert!(
        !f.home
            .join("post-merge")
            .join(format!("{id}-{}", &sha[..12]))
            .exists()
    );
}

#[tokio::test]
async fn failing_merge_opens_one_human_reviewed_revert_that_passes_checks() {
    let (f, p, id, sha) = setup("grep -q hello README.md", "broken\n").await;
    p.process_post_merge(id).await.unwrap();
    p.process_post_merge(id).await.unwrap();
    let checks = p.store.post_merge_checks(id).await.unwrap();
    assert_eq!(checks[0].state, "revert_open");
    assert_eq!(
        checks[0].revert_pr_url.as_deref(),
        Some("https://github.com/o/r/pull/8")
    );
    let prs = p.hub.prs.lock().unwrap();
    assert_eq!(prs.len(), 1);
    assert!(prs[0].3.contains(&sha));
    assert!(prs[0].3.contains("will not merge it automatically"));
    assert_eq!(
        git(
            &f.config.repos[0].path_in(&f.home),
            &["show", &format!("{}:README.md", prs[0].0)]
        ),
        "hello"
    );
    assert_eq!(p.hub.posted.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn failed_pr_creation_resumes_without_duplicate_revert_commit() {
    use std::sync::atomic::Ordering;
    let (f, p, id, sha) = setup("grep -q hello README.md", "broken\n").await;
    p.hub.pr_create_failures.store(1, Ordering::SeqCst);
    assert!(p.process_post_merge(id).await.is_err());
    let repo = f.config.repos[0].path_in(&f.home);
    let branch = format!("provefab/revert-{id}-{}", &sha[..12]);
    let before = git(&repo, &["rev-parse", &branch]);
    p.process_post_merge(id).await.unwrap();
    assert_eq!(git(&repo, &["rev-parse", &branch]), before);
    assert_eq!(
        p.store.post_merge_checks(id).await.unwrap()[0].state,
        "revert_open"
    );
    assert_eq!(p.hub.prs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn conflicting_revert_is_blocked_without_a_pr() {
    let (f, p, id, _) = setup("grep -q hello README.md", "broken\n").await;
    let repo = f.config.repos[0].path_in(&f.home);
    std::fs::write(repo.join("README.md"), "later independent change\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-qm", "later work"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    p.process_post_merge(id).await.unwrap();
    let check = &p.store.post_merge_checks(id).await.unwrap()[0];
    assert_eq!(check.state, "blocked");
    assert!(
        check
            .failure_summary
            .as_deref()
            .unwrap()
            .contains("revert conflict")
    );
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn revert_that_fails_checks_is_blocked_without_a_pr() {
    let (_f, p, id, _) = setup("false", "broken\n").await;
    p.process_post_merge(id).await.unwrap();
    assert_eq!(
        p.store.post_merge_checks(id).await.unwrap()[0].state,
        "blocked"
    );
    assert!(p.hub.prs.lock().unwrap().is_empty());
}
