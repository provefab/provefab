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

#[tokio::test]
async fn another_base_without_a_sha_neither_waits_nor_creates_a_check() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    *p.hub.pr_status.lock().unwrap() = merged(None, Some("release"), Some(1));
    p.watch_pr(id).await.unwrap();
    assert!(p.store.post_merge_checks(id).await.unwrap().is_empty());
    assert_eq!(
        p.store.task(id).await.unwrap().unwrap().pr_state.as_deref(),
        Some("merged")
    );
}

/// README.md becomes `change` in one commit merged on origin/main, recorded as
/// a merged Provefab PR (pull/8) with a queued check.
async fn setup(checks: &[&str], change: &str) -> (Fixture, P, i64, String) {
    let (f, p, id) = open_pr_task(checks).await;
    let repo = f.config.repos[0].path_in(&f.home);
    let sha = commit_and_push(&repo, "README.md", change, "Merged Provefab change");
    *p.hub.pr_status.lock().unwrap() = merged(Some(&sha), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    // Later comments on pull/8 go to its own per-URL status.
    let status = p.hub.pr_status.lock().unwrap().clone();
    p.hub
        .pr_statuses
        .lock()
        .unwrap()
        .insert("https://github.com/o/r/pull/8".into(), status);
    (f, p, id, sha)
}

async fn check(p: &P, id: i64) -> provefab::store::PostMergeCheckRow {
    p.store.post_merge_checks(id).await.unwrap().remove(0)
}

async fn tick(p: &P, id: i64) -> CheckState {
    p.process_post_merge(id).await.unwrap();
    check(p, id).await.state
}

/// Ticks until the check is terminal.
async fn drive(p: &P, id: i64) -> provefab::store::PostMergeCheckRow {
    for _ in 0..12 {
        if tick(p, id).await.is_terminal() {
            return check(p, id).await;
        }
    }
    panic!(
        "not terminal after 12 ticks: {:?}",
        check(p, id).await.state
    );
}

fn leftovers(f: &Fixture, check_id: i64) -> Vec<String> {
    let dir = f.home.join("post-merge");
    std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(&format!("{check_id}-")))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_green_merge_passes_in_two_ticks_and_posts_nothing() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "hello world\n").await;
    assert_eq!(tick(&p, id).await, CheckState::Verifying);
    assert_eq!(tick(&p, id).await, CheckState::Passed);
    let c = check(&p, id).await;
    assert!(c.flaky.is_empty() && c.finished_at.is_some());
    assert!(p.hub.posted.lock().unwrap().is_empty());
    assert!(leftovers(&f, c.id).is_empty());
    // A terminal check costs nothing more.
    assert_eq!(tick(&p, id).await, CheckState::Passed);
}

#[tokio::test]
async fn a_failure_rescued_by_its_rerun_is_flaky_not_failed() {
    let (f, p, id, _) = setup(&["true"], "hello world\n").await;
    let mark = f.home.join("flaky-mark");
    let cmd = format!("test -f {0} || {{ touch {0}; false; }}", mark.display());
    let mut p = p;
    p.config.repos[0].post_merge_checks = vec![cmd.clone()];
    let c = drive(&p, id).await;
    assert_eq!(c.state, CheckState::Passed);
    assert_eq!(c.flaky, [cmd]);
}

#[tokio::test]
async fn a_failure_twice_is_confirmed_and_timeouts_count_as_failures() {
    let (_f, p, id, _) = setup(&["sleep 5"], "hello world\n").await;
    let mut p = p;
    p.config.limits.gate_timeout = std::time::Duration::from_millis(300);
    tick(&p, id).await;
    assert_eq!(tick(&p, id).await, CheckState::VerificationFailed);
    let c = check(&p, id).await;
    assert_eq!(c.failure_kind, Some(FailureKind::CheckFailed));
    assert!(c.failed_commands[0].timed_out);
}

#[tokio::test]
async fn crash_residue_that_looks_green_is_never_reused() {
    let (f, p, id, sha) = setup(&["grep -q hello README.md"], "broken\n").await;
    assert_eq!(tick(&p, id).await, CheckState::Verifying);
    let c = check(&p, id).await;
    // A crashed run left a worktree whose README would pass.
    let repo = f.config.repos[0].path_in(&f.home);
    let wt = f.home.join("post-merge").join(format!("{}-verify", c.id));
    p.git
        .worktree_fresh_detached(&repo, &wt, &sha)
        .await
        .unwrap();
    std::fs::write(wt.join("README.md"), "hello\n").unwrap();
    assert_eq!(tick(&p, id).await, CheckState::VerificationFailed);
    assert!(leftovers(&f, c.id).is_empty());
}

#[tokio::test]
async fn a_human_merged_multi_commit_pr_is_blocked_before_running_anything() {
    let (f, p, id) = open_pr_task(&["touch ran; true"]).await;
    let repo = f.config.repos[0].path_in(&f.home);
    let sha = commit_and_push(&repo, "README.md", "x\n", "change");
    *p.hub.pr_status.lock().unwrap() = merged(Some(&sha), Some("main"), Some(3));
    p.watch_pr(id).await.unwrap();
    let c = drive(&p, id).await;
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::UnsafeMergeStrategy))
    );
    assert!(
        p.store
            .stage_runs(id)
            .await
            .unwrap()
            .iter()
            .all(|r| r.stage != "post-merge")
    );
}

#[tokio::test]
async fn a_removed_repo_or_emptied_checks_leave_rows_untouched() {
    let (_f, p, id, _) = setup(&["true"], "hello world\n").await;
    let mut p = p;
    p.config.repos[0].post_merge_checks.clear();
    p.process_post_merge(id).await.unwrap();
    assert_eq!(check(&p, id).await.state, CheckState::Queued);
    p.config.repos.clear();
    p.process_post_merge(id).await.unwrap();
    assert_eq!(check(&p, id).await.state, CheckState::Queued);
    assert!(p.hub.posted.lock().unwrap().is_empty());
}

async fn tick_until(p: &P, id: i64, state: CheckState) {
    for _ in 0..12 {
        if check(p, id).await.state == state {
            return;
        }
        tick(p, id).await;
    }
    panic!("never reached {state:?}");
}

#[tokio::test]
async fn a_fix_already_on_the_base_supersedes_the_revert() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::VerificationFailed).await;
    let repo = f.config.repos[0].path_in(&f.home);
    let fix = commit_and_push(&repo, "README.md", "hello again\n", "fix");
    let c = drive(&p, id).await;
    assert_eq!(c.state, CheckState::Superseded);
    assert_eq!(c.base_sha.as_deref(), Some(fix.as_str()));
    assert!(p.hub.prs.lock().unwrap().is_empty());
    assert_eq!(git(&f.origin, &["branch", "--list", "provefab/*"]), "");
}

#[tokio::test]
async fn a_broken_base_prepares_a_revert_that_passes() {
    let (f, p, id, sha) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    let c = check(&p, id).await;
    assert_eq!(c.base_sha.as_deref(), Some(sha.as_str()));
    assert_eq!(
        c.revert_branch.as_deref(),
        Some(format!("provefab/revert-{}-0", c.id).as_str())
    );
    let repo = f.config.repos[0].path_in(&f.home);
    let revert = c.revert_sha.clone().unwrap();
    assert_eq!(
        git(&repo, &["show", &format!("{revert}:README.md")]),
        "hello"
    );
    assert_eq!(git(&repo, &["rev-parse", &format!("{revert}^")]), sha);
    assert!(leftovers(&f, c.id).is_empty());
}

#[tokio::test]
async fn a_conflicting_revert_is_blocked() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::VerificationFailed).await;
    let repo = f.config.repos[0].path_in(&f.home);
    commit_and_push(&repo, "README.md", "later independent change\n", "later");
    let c = drive(&p, id).await;
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::RevertConflict))
    );
    assert!(p.hub.prs.lock().unwrap().is_empty());
    assert!(leftovers(&f, c.id).is_empty());
}

#[tokio::test]
async fn a_revert_that_still_fails_is_blocked() {
    let (_f, p, id, _) = setup(&["false"], "broken\n").await;
    let c = drive(&p, id).await;
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::RevertChecksFailed))
    );
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_check_that_edits_tracked_files_on_the_revert_is_blocked() {
    // Fails on the merge (README is broken), passes on the revert but rewrites README.
    let (_f, p, id, _) = setup(
        &["grep -q hello README.md && echo changed > README.md"],
        "broken\n",
    )
    .await;
    let c = drive(&p, id).await;
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::DirtyTree))
    );
}

#[tokio::test]
async fn a_merge_commit_is_reverted_with_mainline_one() {
    let (f, p, id) = open_pr_task(&["grep -q hello README.md"]).await;
    let repo = f.config.repos[0].path_in(&f.home);
    git(&repo, &["switch", "-qc", "feature"]);
    std::fs::write(repo.join("README.md"), "broken\n").unwrap();
    git(&repo, &["commit", "-qam", "feature"]);
    git(&repo, &["switch", "-q", "main"]);
    git(
        &repo,
        &["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    *p.hub.pr_status.lock().unwrap() = merged(Some(&sha), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    tick_until(&p, id, CheckState::RevertReady).await;
    let revert = check(&p, id).await.revert_sha.unwrap();
    assert_eq!(
        git(&repo, &["show", &format!("{revert}:README.md")]),
        "hello"
    );
}

#[tokio::test]
async fn a_merge_no_longer_on_the_base_is_blocked() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::VerificationFailed).await;
    // Someone rewrote main without the merged commit.
    let repo = f.config.repos[0].path_in(&f.home);
    git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
    commit_and_push_force(&repo, "OTHER.md", "x\n", "rewrite");
    let c = drive(&p, id).await;
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::BaseDiverged))
    );
}

fn commit_and_push_force(repo: &Path, file: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(file), content).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-qm", msg]);
    git(repo, &["push", "-qf", "origin", "main"]);
    git(repo, &["rev-parse", "HEAD"])
}
