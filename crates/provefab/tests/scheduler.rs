//! `provefab run`: the loop and `--dry-run`, with the fakes in `common`.

use provefab::testkit::*;

#[tokio::test]
async fn run_once_polls_creates_labels_and_drives_to_a_pr() {
    let f = fixture(&["test -f feature.txt"]);
    let p = std::sync::Arc::new(
        pipeline(
            &f,
            Box::new(happy),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await,
    );
    let opts = provefab::scheduler::RunOptions {
        workers: 2,
        once: true,
    };
    provefab::scheduler::run(p.clone(), opts, std::future::pending::<()>())
        .await
        .unwrap();
    let t = p
        .store
        .task_by_url("https://github.com/o/r/issues/7")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.state, PrOpen);
    assert_eq!(
        *p.hub.ensured.lock().unwrap(),
        vec![
            "provefab:in-pr",
            "provefab:needs-info",
            "provefab:failed",
            "provefab:merged",
            "provefab:risk-ci",
            "provefab:risk-dependencies",
            "provefab:risk-migrations",
            "provefab:risk-infrastructure",
            "provefab:risk-secrets-config",
            "provefab:risk-unknown"
        ]
    );
    // A second run finds nothing new to do.
    provefab::scheduler::run(p.clone(), opts, std::future::pending::<()>())
        .await
        .unwrap();
    assert_eq!(p.hub.prs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn dry_run_reports_routes_and_touches_nothing() {
    let f = fixture(&["true"]);
    let hub = FakeHub::new("x");
    let oracle = FakeOracle {
        verdict: Some(verdict(TaskKind::Feature, 0.1)),
        ..Default::default()
    };
    let lines = provefab::scheduler::dry_run(&f.config, &hub, &oracle)
        .await
        .unwrap();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].ends_with("-> plan top-claude, implement std-claude, review top-claude"),
        "{}",
        lines[0]
    );
    // Architectural scope: plan frontier, implement fast (none configured, so
    // standard), review standard on the other provider.
    let mut wide = verdict(TaskKind::Refactor, 0.1);
    wide.difficulty = 0.5;
    wide.scope = 3.0;
    let routed = FakeOracle {
        verdict: Some(wide),
        ..Default::default()
    };
    let lines = provefab::scheduler::dry_run(&f.config, &hub, &routed)
        .await
        .unwrap();
    assert!(
        lines[0].ends_with("-> plan top-claude, implement std-claude, review std-codex"),
        "{}",
        lines[0]
    );
    let asks = FakeOracle {
        verdict: Some(verdict(TaskKind::Feature, 0.9)),
        ..Default::default()
    };
    let lines = provefab::scheduler::dry_run(&f.config, &hub, &asks)
        .await
        .unwrap();
    assert!(
        lines[0].contains("would ask for more information"),
        "{}",
        lines[0]
    );
    let none = provefab::scheduler::dry_run(&f.config, &hub, &FakeOracle::default())
        .await
        .unwrap();
    assert!(
        lines.len() == 1 && none[0].contains("Jev unavailable"),
        "{}",
        none[0]
    );
    assert!(hub.posted.lock().unwrap().is_empty() && hub.labels.lock().unwrap().is_empty());
}

// ---------- final review fixes ----------

const ONCE: provefab::scheduler::RunOptions = provefab::scheduler::RunOptions {
    workers: 1,
    once: true,
};

/// Bounded: a hang is the failure these tests catch.
async fn within<F: std::future::Future>(secs: u64, f: F) -> F::Output {
    tokio::time::timeout(std::time::Duration::from_secs(secs), f)
        .await
        .expect("the scheduler did not return")
}

#[tokio::test]
async fn review_i4_a_task_that_keeps_erroring_does_not_spin_and_once_ends() {
    let f = fixture(&["true"]);
    let p = std::sync::Arc::new(
        pipeline(
            &f,
            Box::new(happy),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await,
    );
    let id = queue(&p).await;
    p.store.transition(id, Planning, "test").await.unwrap();
    // A routing row the store cannot read: every step of this task errors.
    let db = sqlx::SqlitePool::connect(&format!("sqlite:{}", f.home.join("provefab.db").display()))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO routing_decisions (task_id, jev_model, verdict_json, tiers_json, reasons, at) VALUES (?, NULL, NULL, 'not json', '', 0)",
    )
    .bind(id)
    .execute(&db)
    .await
    .unwrap();
    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    assert_eq!(p.store.task(id).await.unwrap().unwrap().state, Planning);
}

#[tokio::test]
async fn review_i5_a_panicking_task_is_parked_for_a_person() {
    let f = fixture(&["true"]);
    let script = |_: &ModelEntry,
                  _: &StageRequest,
                  _: &UnboundedSender<WorkerEvent>|
     -> Option<StageResult> { panic!("worker adapter bug") };
    let p = std::sync::Arc::new(
        pipeline(
            &f,
            Box::new(script),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await,
    );
    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    let t = p
        .store
        .task_by_url("https://github.com/o/r/issues/7")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.state, NeedsYou);
    let posted = p.hub.posted.lock().unwrap().clone();
    assert!(
        posted.last().unwrap().contains("internal error"),
        "{posted:?}"
    );
    assert!(
        posted.iter().all(|c| !c.contains("worker adapter bug")),
        "{posted:?}"
    );
}

#[tokio::test]
async fn review_i6_stopping_cancels_running_stages_and_returns() {
    let f = fixture(&["true"]);
    let script = |_: &ModelEntry, _: &StageRequest, _: &UnboundedSender<WorkerEvent>| None; // hangs
    let p = std::sync::Arc::new(
        pipeline(
            &f,
            Box::new(script),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await,
    );
    let forever = provefab::scheduler::RunOptions {
        workers: 1,
        once: false,
    };
    let stop = tokio::time::sleep(std::time::Duration::from_millis(500));
    within(10, provefab::scheduler::run(p.clone(), forever, stop))
        .await
        .unwrap();
    // The stage was cancelled, not finished: the task resumes after a restart.
    let t = p
        .store
        .task_by_url("https://github.com/o/r/issues/7")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.state, Planning);
}

/// A failed comment becomes a `pending_github` effect and is retried on the
/// scheduler's next poll: the question still reaches the issue, exactly once.
#[tokio::test]
async fn pending_github_comment_is_retried_on_next_poll() {
    let f = fixture(&["true"]);
    let hub = FakeHub::new("x");
    hub.comment_failures
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let p = std::sync::Arc::new(
        pipeline(
            &f,
            Box::new(happy),
            FakeOracle {
                verdict: Some(verdict(TaskKind::Feature, 0.9)),
                ..Default::default()
            },
            hub,
        )
        .await,
    );
    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    let id = p
        .store
        .task_by_url("https://github.com/o/r/issues/7")
        .await
        .unwrap()
        .unwrap()
        .id;
    assert_eq!(p.store.task(id).await.unwrap().unwrap().state, NeedsInfo);
    assert!(p.hub.posted.lock().unwrap().is_empty());
    // The question comment failed; the label edit right after it queues behind
    // it too, so a later retry cannot post it out of order.
    assert_eq!(
        p.store.count_outputs(id, "pending_github").await.unwrap(),
        2
    );

    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    let posted = p.hub.posted.lock().unwrap().clone();
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert!(posted[0].contains("needs more detail"), "{posted:?}");
    assert_eq!(
        p.store.count_outputs(id, "pending_github").await.unwrap(),
        0
    );

    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    assert_eq!(p.hub.posted.lock().unwrap().len(), 1);
}

/// D52: each poll looks at open PRs; a merged one is recorded.
#[tokio::test]
async fn polling_watches_open_prs() {
    let f = fixture(&["test -f feature.txt"]);
    let p = std::sync::Arc::new(
        pipeline(
            &f,
            Box::new(happy),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await,
    );
    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: None,
        merge_sha: None,
        base_ref: None,
        commit_count: None,
    };
    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    let t = p
        .store
        .task_by_url("https://github.com/o/r/issues/7")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.pr_state.as_deref(), Some("merged"));
    // After the merge, the issue is checked at most once an hour (Plan 4 review I2).
    for _ in 0..3 {
        within(
            10,
            provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        p.hub
            .issue_open_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

/// The service swaps in a newer price table once the one it holds is a day old.
#[tokio::test]
async fn the_loop_refreshes_stale_prices_from_the_cache() {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    // A fresh cache on disk (no fetch), a stale table in memory.
    let mut cached = p.prices.read().unwrap().clone();
    cached.source = "seeded".into();
    cached.fetched_at = provefab::store::now();
    std::fs::write(p.paths.prices(), serde_json::to_string(&cached).unwrap()).unwrap();
    p.prices.write().unwrap().fetched_at = 0;
    let p = std::sync::Arc::new(p);
    let opts = provefab::scheduler::RunOptions {
        workers: 1,
        once: true,
    };
    provefab::scheduler::run(p.clone(), opts, std::future::pending::<()>())
        .await
        .unwrap();
    assert_eq!(p.prices.read().unwrap().source, "seeded");
}

/// Post-merge rows advance on every tick, not on the hourly reopen watch.
#[tokio::test]
async fn post_merge_checks_advance_on_every_tick() {
    let mut f = fixture(&["test -f feature.txt"]);
    f.config.repos[0].post_merge_checks = vec!["true".into()];
    let p = std::sync::Arc::new(
        pipeline(
            &f,
            Box::new(happy),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await,
    );
    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    // A real one-parent commit on origin/main stands in for the squash merge.
    let repo = f.config.repos[0].path_in(&f.home);
    std::fs::write(repo.join("MERGED.md"), "merged\n").unwrap();
    git(&repo, &["add", "MERGED.md"]);
    git(&repo, &["commit", "-qm", "squash merge"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: None,
        merge_sha: Some(sha),
        base_ref: Some("main".into()),
        commit_count: Some(1),
    };
    for _ in 0..4 {
        within(
            10,
            provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
        )
        .await
        .unwrap();
    }
    let t = p
        .store
        .task_by_url("https://github.com/o/r/issues/7")
        .await
        .unwrap()
        .unwrap();
    let c = p.store.post_merge_checks(t.id).await.unwrap();
    assert_eq!(c[0].state, provefab::post_merge::CheckState::Passed);
}

/// A task that left `PrOpen` after its merge (a reopen that needs a person)
/// still has its check driven to the end (final review F2).
#[tokio::test]
async fn post_merge_checks_advance_after_the_task_leaves_pr_open() {
    let mut f = fixture(&["test -f feature.txt"]);
    f.config.repos[0].post_merge_checks = vec!["true".into()];
    let p = std::sync::Arc::new(
        pipeline(
            &f,
            Box::new(happy),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await,
    );
    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    let repo = f.config.repos[0].path_in(&f.home);
    std::fs::write(repo.join("MERGED.md"), "merged\n").unwrap();
    git(&repo, &["add", "MERGED.md"]);
    git(&repo, &["commit", "-qm", "squash merge"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: None,
        merge_sha: Some(sha),
        base_ref: Some("main".into()),
        commit_count: Some(1),
    };
    within(
        10,
        provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    let t = p
        .store
        .task_by_url("https://github.com/o/r/issues/7")
        .await
        .unwrap()
        .unwrap();
    let c = p.store.post_merge_checks(t.id).await.unwrap();
    assert_eq!(c[0].state, provefab::post_merge::CheckState::Queued);
    p.store
        .transition(t.id, NeedsYou, "reopened")
        .await
        .unwrap();
    for _ in 0..4 {
        within(
            10,
            provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()),
        )
        .await
        .unwrap();
    }
    let c = p.store.post_merge_checks(t.id).await.unwrap();
    assert_eq!(c[0].state, provefab::post_merge::CheckState::Passed);
}
