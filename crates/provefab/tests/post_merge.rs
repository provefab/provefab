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

    // Decoy refs that tail-match the branch name are ignored.
    for decoy in [
        "refs/heads/decoy/provefab/revert-1-0",
        "refs/heads/x/refs/heads/provefab/revert-1-0",
    ] {
        git(
            &repo,
            &["push", "-q", "origin", &format!("{revert}:{decoy}")],
        );
    }
    assert_eq!(
        g.remote_branch_sha(&repo, "provefab/revert-1-0")
            .await
            .unwrap(),
        None
    );
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
    assert_eq!(git(&repo, &["branch", "--list", "provefab/*"]), "");
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
    let (f, p, id, _) = setup(&["false"], "broken\n").await;
    let c = drive(&p, id).await;
    let repo = f.config.repos[0].path_in(&f.home);
    assert_eq!(git(&repo, &["branch", "--list", "provefab/*"]), "");
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::RevertChecksFailed))
    );
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_check_that_edits_tracked_files_on_the_revert_is_blocked() {
    // Fails on the merge (README is broken), passes on the revert but rewrites README.
    let (f, p, id, _) = setup(
        &["grep -q hello README.md && echo changed > README.md"],
        "broken\n",
    )
    .await;
    let c = drive(&p, id).await;
    let repo = f.config.repos[0].path_in(&f.home);
    assert_eq!(git(&repo, &["branch", "--list", "provefab/*"]), "");
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

#[tokio::test]
async fn a_failing_merge_opens_exactly_one_human_reviewed_revert() {
    let (f, p, id, sha) = setup(&["grep -q hello README.md"], "broken\n").await;
    let c = drive(&p, id).await;
    assert_eq!(c.state, CheckState::RevertOpen);
    assert_eq!(
        c.revert_pr_url.as_deref(),
        Some("https://github.com/o/r/pull/100")
    );
    let prs = p.hub.prs.lock().unwrap().clone();
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0].0, format!("provefab/revert-{}-0", c.id));
    assert_eq!(prs[0].1, "main");
    assert!(
        prs[0].3.contains(&sha)
            && prs[0]
                .3
                .contains("Provefab will not merge this revert automatically.")
    );
    assert_eq!(
        git(
            &f.origin,
            &["rev-parse", &format!("refs/heads/{}", prs[0].0)]
        ),
        c.revert_sha.clone().unwrap()
    );
    // Local branch cleaned up; nothing merged.
    let repo = f.config.repos[0].path_in(&f.home);
    assert_eq!(git(&repo, &["branch", "--list", "provefab/revert-*"]), "");
    assert!(p.hub.merged.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_pr_creation_error_resumes_without_a_second_branch_or_pr() {
    use std::sync::atomic::Ordering;
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    p.hub.pr_create_failures.store(2, Ordering::SeqCst);
    assert!(p.process_post_merge(id).await.is_err());
    assert!(p.process_post_merge(id).await.is_err());
    let c = check(&p, id).await;
    assert_eq!((c.state, c.infra_errors), (CheckState::RevertReady, 2));
    let pushed = git(
        &f.origin,
        &[
            "rev-parse",
            &format!("refs/heads/provefab/revert-{}-0", c.id),
        ],
    );
    assert_eq!(tick(&p, id).await, CheckState::RevertOpen);
    assert_eq!(check(&p, id).await.infra_errors, 0);
    assert_eq!(p.hub.prs.lock().unwrap().len(), 1);
    assert_eq!(
        git(
            &f.origin,
            &[
                "rev-parse",
                &format!("refs/heads/provefab/revert-{}-0", c.id)
            ]
        ),
        pushed
    );
}

#[tokio::test]
async fn five_infra_errors_block_the_check() {
    use std::sync::atomic::Ordering;
    let (_f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    p.hub.pr_create_failures.store(10, Ordering::SeqCst);
    for _ in 0..4 {
        assert!(p.process_post_merge(id).await.is_err());
    }
    p.process_post_merge(id).await.unwrap();
    let c = check(&p, id).await;
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::InfraError))
    );
}

#[tokio::test]
async fn a_moving_base_restarts_then_blocks_on_the_third_move() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    let repo = f.config.repos[0].path_in(&f.home);
    for n in 0..2 {
        tick_until(&p, id, CheckState::RevertReady).await;
        commit_and_push(&repo, &format!("OTHER{n}.md"), "x\n", "unrelated");
        assert_eq!(tick(&p, id).await, CheckState::VerificationFailed);
        assert_eq!(check(&p, id).await.base_moves, n + 1);
    }
    tick_until(&p, id, CheckState::RevertReady).await;
    assert_eq!(
        check(&p, id).await.revert_branch.unwrap(),
        format!("provefab/revert-{}-2", check(&p, id).await.id)
    );
    commit_and_push(&repo, "OTHER2.md", "x\n", "unrelated");
    let c = drive(&p, id).await;
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::BaseMoved))
    );
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_foreign_commit_on_the_revert_branch_blocks() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    let c = check(&p, id).await;
    let repo = f.config.repos[0].path_in(&f.home);
    git(
        &repo,
        &[
            "push",
            "-q",
            "origin",
            &format!("HEAD~1:refs/heads/{}", c.revert_branch.clone().unwrap()),
        ],
    );
    let c = drive(&p, id).await;
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::BranchConflict))
    );
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_reused_pr_on_other_work_blocks() {
    let (_f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    *p.hub.pr_head_override.lock().unwrap() = Some("0".repeat(40));
    let c = drive(&p, id).await;
    assert_eq!(
        (c.state, c.failure_kind),
        (CheckState::Blocked, Some(FailureKind::BranchConflict))
    );
}

#[tokio::test]
async fn a_crash_after_the_push_reuses_the_pushed_branch() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    let c = check(&p, id).await;
    let repo = f.config.repos[0].path_in(&f.home);
    p.git
        .push_sha(
            &repo,
            c.revert_sha.as_deref().unwrap(),
            c.revert_branch.as_deref().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(drive(&p, id).await.state, CheckState::RevertOpen);
    assert_eq!(p.hub.prs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_innocent_later_merge_is_never_reverted() {
    // Merge A breaks README; its check opens a revert PR (not merged).
    let (f, p, a, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    assert_eq!(drive(&p, a).await.state, CheckState::RevertOpen);
    // Merge B only adds a file; the base is still broken by A.
    let b = provefab::testkit::queue_n(&p, 9, "Second change").await;
    let repo = f.config.repos[0].path_in(&f.home);
    let sha_b = commit_and_push(&repo, "B.md", "b\n", "innocent");
    p.store
        .set_pr(b, "https://github.com/o/r/pull/9", "open")
        .await
        .unwrap();
    p.store
        .transition(b, TaskState::PrOpen, "pr")
        .await
        .unwrap();
    *p.hub.pr_status.lock().unwrap() = merged(Some(&sha_b), Some("main"), Some(1));
    p.watch_pr(b).await.unwrap();
    let cb = drive(&p, b).await;
    assert_eq!(
        (cb.state, cb.failure_kind),
        (CheckState::Blocked, Some(FailureKind::RevertChecksFailed))
    );
    assert_eq!(
        p.hub.prs.lock().unwrap().len(),
        1,
        "only the culprit has a revert PR"
    );
}

fn issue_markers(p: &P, check_id: i64) -> usize {
    let m = provefab::post_merge::marker(check_id);
    p.hub
        .comments
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.body.contains(&m))
        .count()
}

fn pr_markers(p: &P, check_id: i64) -> usize {
    let m = provefab::post_merge::marker(check_id);
    p.hub.pr_statuses.lock().unwrap()["https://github.com/o/r/pull/8"]
        .comments
        .iter()
        .filter(|c| c.body.contains(&m))
        .count()
}

#[tokio::test]
async fn a_failed_pr_comment_is_retried_without_a_second_issue_comment() {
    use std::sync::atomic::Ordering;
    let (_f, p, id, _) = setup(&["false"], "broken\n").await;
    p.hub.pr_comment_failures.store(1, Ordering::SeqCst);
    for _ in 0..12 {
        let _ = p.process_post_merge(id).await;
    }
    let c = check(&p, id).await;
    assert_eq!(c.state, CheckState::Blocked);
    assert_eq!((issue_markers(&p, c.id), pr_markers(&p, c.id)), (1, 1));
    assert!(c.issue_notified_at.is_some() && c.pr_notified_at.is_some());
}

#[tokio::test]
async fn a_target_that_keeps_failing_is_given_up_after_the_limit() {
    use std::sync::atomic::Ordering;
    // No revert PR is ever created here, so every PR comment is the notice.
    let (_f, p, id, _) = setup(&["false"], "broken\n").await;
    p.hub.pr_comment_failures.store(1000, Ordering::SeqCst);
    let mut errors = 0;
    for _ in 0..20 {
        if p.process_post_merge(id).await.is_err() {
            errors += 1;
        }
    }
    let c = check(&p, id).await;
    assert_eq!(c.state, CheckState::Blocked);
    // The blocking transition reset the counter: 4 errors, then the 5th gives up.
    assert_eq!(errors, provefab::post_merge::INFRA_ERROR_LIMIT as usize - 1);
    assert!(c.pr_notified_at.is_some());
    assert_eq!(pr_markers(&p, c.id), 0);
    assert_eq!(issue_markers(&p, c.id), 1);
}

#[tokio::test]
async fn each_check_is_announced_by_its_own_id() {
    let (_f, p, id, _) = setup(&["false"], "broken\n").await;
    let second = p
        .store
        .ensure_post_merge_check(&provefab::store::NewPostMergeCheck {
            task_id: id,
            merge_sha: "unknown",
            base: "main",
            commit_count: None,
            auto_merged: false,
        })
        .await
        .unwrap();
    p.store
        .advance_post_merge(
            second.id,
            CheckState::Queued,
            CheckState::Blocked,
            &provefab::store::CheckPatch {
                failure_kind: Some(FailureKind::AttributionMissing),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    for _ in 0..12 {
        let _ = p.process_post_merge(id).await;
    }
    let first = check(&p, id).await;
    assert_eq!(
        (issue_markers(&p, first.id), issue_markers(&p, second.id)),
        (1, 1)
    );
    assert_eq!(
        (pr_markers(&p, first.id), pr_markers(&p, second.id)),
        (1, 1)
    );
}

#[tokio::test]
async fn nothing_published_contains_command_output_or_local_paths() {
    // The output joins to SENTINEL_SECRET_42; the command text never contains it.
    let (f, p, id, _) = setup(
        &["printf %s SENTINEL_; printf %s SECRET_42; false"],
        "broken\n",
    )
    .await;
    drive(&p, id).await;
    tick(&p, id).await;
    let home = f.home.display().to_string();
    let mut published: Vec<String> = p.hub.posted.lock().unwrap().clone();
    published.extend(p.hub.prs.lock().unwrap().iter().map(|pr| pr.3.clone()));
    assert!(!published.is_empty());
    let mark = provefab::post_merge::marker(check(&p, id).await.id);
    assert!(published.iter().any(|b| b.contains(&mark)));
    for body in published {
        assert!(!body.contains("SENTINEL_SECRET_42"), "{body}");
        assert!(!body.contains(&home), "{body}");
    }
}

#[tokio::test]
async fn a_superseded_check_is_announced_once() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::VerificationFailed).await;
    let repo = f.config.repos[0].path_in(&f.home);
    commit_and_push(&repo, "README.md", "hello again\n", "fix");
    drive(&p, id).await;
    tick(&p, id).await;
    tick(&p, id).await;
    let c = check(&p, id).await;
    assert_eq!((issue_markers(&p, c.id), pr_markers(&p, c.id)), (1, 1));
}

#[tokio::test]
async fn issue_failures_do_not_count_against_the_pr_target() {
    use std::sync::atomic::Ordering;
    let (_f, p, id, _) = setup(&["false"], "broken\n").await;
    // Drive to the terminal state, then fail issue reads N-1 times.
    p.hub.comments_down.store(true, Ordering::SeqCst);
    let mut n = 0;
    while check(&p, id).await.state != CheckState::Blocked && n < 30 {
        let _ = p.process_post_merge(id).await;
        n += 1;
    }
    while check(&p, id).await.infra_errors < provefab::post_merge::INFRA_ERROR_LIMIT - 1 {
        let _ = p.process_post_merge(id).await;
    }
    assert!(check(&p, id).await.issue_notified_at.is_none());
    p.hub.comments_down.store(false, Ordering::SeqCst);
    p.hub.pr_comment_failures.store(1, Ordering::SeqCst);
    for _ in 0..6 {
        let _ = p.process_post_merge(id).await;
    }
    let c = check(&p, id).await;
    assert!(c.issue_notified_at.is_some() && c.pr_notified_at.is_some());
    assert_eq!((issue_markers(&p, c.id), pr_markers(&p, c.id)), (1, 1));
}

#[tokio::test]
async fn status_log_and_stats_show_the_check() {
    let (_f, p, id, sha) = setup(&["grep -q hello README.md"], "broken\n").await;
    let c = drive(&p, id).await;
    let status = provefab::commands::status(&p.store).await.unwrap();
    assert!(
        status.contains("post-merge checks: revert_open 1"),
        "{status}"
    );
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    for needle in [
        "revert_open".to_string(),
        sha.clone(),
        c.base_sha.clone().unwrap(),
        c.revert_sha.clone().unwrap(),
        "https://github.com/o/r/pull/100".to_string(),
        "`grep -q hello README.md` exited with 1".to_string(),
    ] {
        assert!(log.contains(&needle), "{needle} missing from:\n{log}");
    }
    let stats = provefab::commands::stats(&p.store).await.unwrap();
    assert!(
        stats.contains(
            "post-merge passed 0 · flaky 0 · superseded 0 · reverts opened 1 · blocked 0"
        ),
        "{stats}"
    );
}

/// Spec section 12: a replay per non-terminal state. A tick whose result was
/// lost (crash before the state was persisted) is replayed from the earlier
/// state and still ends in exactly one revert PR with nothing left behind.
#[tokio::test]
async fn replaying_any_non_terminal_state_still_ends_in_one_revert_pr() {
    let non_terminal = [
        CheckState::Queued,
        CheckState::Verifying,
        CheckState::VerificationFailed,
        CheckState::PreparingRevert,
        CheckState::RevertReady,
    ];
    for state in non_terminal {
        let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
        if state != CheckState::Queued {
            tick_until(&p, id, state).await;
        }
        let c = check(&p, id).await;
        assert_eq!(c.state, state);
        let after = tick(&p, id).await;
        if after != state {
            // The tick's effects happened, but its state change is "lost".
            let rewound = p
                .store
                .advance_post_merge(id, after, state, &Default::default())
                .await
                .unwrap();
            assert!(rewound, "{state:?}");
        }
        let c = drive(&p, id).await;
        assert_eq!(c.state, CheckState::RevertOpen, "replay from {state:?}");
        assert_eq!(p.hub.prs.lock().unwrap().len(), 1, "replay from {state:?}");
        assert!(leftovers(&f, c.id).is_empty(), "replay from {state:?}");
    }
}

#[tokio::test]
async fn an_infra_error_path_leaves_no_worktree() {
    use std::sync::atomic::Ordering;
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    p.hub.pr_create_failures.store(1, Ordering::SeqCst);
    assert!(p.process_post_merge(id).await.is_err());
    assert!(leftovers(&f, check(&p, id).await.id).is_empty());
}
