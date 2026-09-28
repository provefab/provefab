//! `provefab add`, `status` and `log` against a real store.

use provefab::testkit::*;

#[tokio::test]
async fn add_queues_requeues_parked_tasks_and_leaves_running_ones() {
    use provefab::commands::{CommandError, add};
    let f = fixture(&["true"]);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let url = "https://github.com/o/r/issues/7";
    assert!(
        add(&p.store, &f.config, &p.hub, &p.git, &p.paths, url)
            .await
            .unwrap()
            .starts_with("queued as task")
    );
    let id = p.store.task_by_url(url).await.unwrap().unwrap().id;
    assert!(
        add(&p.store, &f.config, &p.hub, &p.git, &p.paths, url)
            .await
            .unwrap()
            .contains("already in progress")
    );
    p.store.bump_attempts(id).await.unwrap();
    p.store.transition(id, Failed, "test").await.unwrap();
    assert!(
        add(&p.store, &f.config, &p.hub, &p.git, &p.paths, url)
            .await
            .unwrap()
            .contains("requeued (was failed)")
    );
    let t = p.store.task(id).await.unwrap().unwrap();
    assert_eq!((t.state, t.attempts), (Queued, 0));
    assert!(matches!(
        add(
            &p.store,
            &f.config,
            &p.hub,
            &p.git,
            &p.paths,
            "https://github.com/x/y/issues/1"
        )
        .await,
        Err(CommandError::UnknownRepo(_))
    ));
    let status = provefab::commands::status(&p.store).await.unwrap();
    assert!(
        status.contains("o/r#7") && status.contains("requeued by provefab add"),
        "{status}"
    );
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(
        log.contains("failed -> queued: requeued by provefab add"),
        "{log}"
    );
}

/// Seen live 2026-09-25 (sandbox issue #5): a requeue forgot the blocking
/// findings of the pass before, and the next pass shipped the same regression.
/// A requeue now starts a fresh branch and worktree, keeps every blocking
/// finding, and puts the issue labels back (D46).
#[tokio::test]
async fn requeue_starts_a_fresh_pass_that_remembers_blocking_findings() {
    use provefab::commands::add;
    let f = fixture(&["true"]);
    // No automatic pass (D50): this test is about the manual requeue.
    let mut config = f.config.clone();
    config.limits.max_auto_passes = 0;
    let f = Fixture { config, ..f };
    let reviews = Mutex::new(0);
    let script = move |_: &ModelEntry, req: &StageRequest, _: &UnboundedSender<WorkerEvent>| {
        match stage_of(&req.prompt) {
            "plan" => done(Some(plan_json(None))),
            "implement" => {
                let p = req.cwd.join("feature.txt");
                let n = std::fs::read_to_string(&p).unwrap_or_default();
                std::fs::write(&p, format!("{n}x")).unwrap();
                done(None)
            }
            _ => {
                let mut r = reviews.lock().unwrap();
                *r += 1;
                if *r <= 3 {
                    done(Some(json!({"verdict": "changes", "findings": [
                        {"file": "src/lib.rs", "line": 9, "severity": "blocking", "text": "inf gives NaN"}
                    ]})))
                } else {
                    done(Some(approve()))
                }
            }
        }
    };
    let p = pipeline(
        &f,
        Box::new(script),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), NeedsYou);
    let url = "https://github.com/o/r/issues/7";
    assert!(
        add(&p.store, &f.config, &p.hub, &p.git, &p.paths, url)
            .await
            .unwrap()
            .contains("requeued (was needs_you)")
    );
    assert_eq!(
        p.hub.last_labels(),
        (
            vec!["provefab".to_string()],
            vec![
                "provefab:failed".to_string(),
                "provefab:needs-info".to_string(),
                "provefab:in-pr".to_string()
            ]
        )
    );
    assert_eq!(p.store.task(id).await.unwrap().unwrap().reopen_count, 1);
    let before = p.runner.calls().len();
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let pass2: Vec<String> = p.runner.calls()[before..]
        .iter()
        .filter(|c| c.1 == "implement")
        .map(|c| c.2.clone())
        .collect();
    assert!(pass2[0].contains("inf gives NaN"), "{}", pass2[0]);
    // A fresh branch: the old one holds the rejected work.
    let prs = p.hub.prs.lock().unwrap().clone();
    assert_eq!(prs[0].0, "provefab/7-add-a-feature-file-r2");
    let pushed = git(
        &f.origin,
        &["show", "provefab/7-add-a-feature-file-r2:feature.txt"],
    );
    assert_eq!(pushed, "x");
}
