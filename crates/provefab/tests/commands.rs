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

use provefab::tracker::{TrackerConfig, TrackerKind};

const JIRA: &str = "https://acme.atlassian.net/browse/ENG-7";

fn tracked(f: &mut Fixture, kind: TrackerKind) {
    f.config.repos[0].tracker = Some(TrackerConfig {
        kind,
        site: (kind == TrackerKind::Jira).then(|| "acme.atlassian.net".to_string()),
        project: Some("ENG".into()),
    });
}

fn keyed_hub(url: &str) -> FakeHub {
    let mut hub = FakeHub::new("x");
    hub.issue.key = Some("ENG-7".into());
    hub.issue.url = url.into();
    hub
}

#[tokio::test]
async fn add_queues_a_jira_ticket_by_its_url() {
    use provefab::commands::{CommandError, add};
    let mut f = fixture(&["true"]);
    tracked(&mut f, TrackerKind::Jira);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), keyed_hub(JIRA)).await;
    let out = add(
        &p.store,
        &f.config,
        &p.hub,
        &p.git,
        &p.paths,
        "https://acme.atlassian.net/browse/ENG-7?focusedCommentId=1",
    )
    .await
    .unwrap();
    assert!(out.starts_with("queued as task"), "{out}");
    let t = p.store.task_by_url(JIRA).await.unwrap().unwrap();
    assert_eq!((t.issue_number, t.issue_key.as_deref()), (7, Some("ENG-7")));
    for (url, what) in [
        ("https://github.com/o/r/issues/7", "wrong tracker"),
        ("https://acme.atlassian.net/browse/OPS-7", "unknown project"),
        ("https://other.atlassian.net/browse/ENG-7", "unknown site"),
        ("https://acme.atlassian.net/projects/ENG", "not a ticket"),
    ] {
        let r = add(&p.store, &f.config, &p.hub, &p.git, &p.paths, url).await;
        let ok = match what {
            "wrong tracker" => matches!(r, Err(CommandError::WrongTracker(_, _))),
            "not a ticket" => matches!(r, Err(CommandError::BadUrl(_))),
            _ => matches!(r, Err(CommandError::UnknownRepo(_))),
        };
        assert!(ok, "{url}: {what}");
    }
}

#[tokio::test]
async fn add_refuses_a_linear_ticket_from_another_workspace() {
    use provefab::commands::{CommandError, add};
    let mut f = fixture(&["true"]);
    tracked(&mut f, TrackerKind::Linear);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        keyed_hub("https://linear.app/acme/issue/ENG-7/add-a-feature-file"),
    )
    .await;
    let r = add(
        &p.store,
        &f.config,
        &p.hub,
        &p.git,
        &p.paths,
        "https://linear.app/other/issue/ENG-7",
    )
    .await;
    assert!(matches!(r, Err(CommandError::OtherWorkspace(_))));
    assert!(p.store.tasks_in(&TaskState::ALL).await.unwrap().is_empty());
    let out = add(
        &p.store,
        &f.config,
        &p.hub,
        &p.git,
        &p.paths,
        "https://linear.app/ACME/issue/eng-7/add",
    )
    .await
    .unwrap();
    assert!(out.starts_with("queued as task"), "{out}");
}

#[tokio::test]
async fn add_picks_the_repository_by_label_when_a_project_is_shared() {
    use provefab::commands::{CommandError, add};
    let mut f = fixture(&["true"]);
    tracked(&mut f, TrackerKind::Jira);
    f.config.repos[0].label = "api".into();
    let mut web = f.config.repos[0].clone();
    web.slug = "o/s".into();
    web.label = "web".into();
    f.config.repos.push(web);
    let mut p = pipeline(&f, Box::new(happy), FakeOracle::default(), keyed_hub(JIRA)).await;
    for labels in [vec![], vec!["api".to_string(), "web".to_string()]] {
        p.hub.issue.labels = labels;
        let r = add(&p.store, &f.config, &p.hub, &p.git, &p.paths, JIRA).await;
        assert!(
            matches!(&r, Err(CommandError::Ambiguous(key, slugs)) if key == "ENG-7" && slugs == "o/r, o/s")
        );
    }
    p.hub.issue.labels = vec!["web".into()];
    add(&p.store, &f.config, &p.hub, &p.git, &p.paths, JIRA)
        .await
        .unwrap();
    let t = p.store.task_by_url(JIRA).await.unwrap().unwrap();
    assert_eq!(t.repo, "o/s");
}

#[tokio::test]
async fn add_matches_the_ticket_label_case_insensitively() {
    use provefab::commands::add;
    let mut f = fixture(&["true"]);
    tracked(&mut f, TrackerKind::Jira);
    f.config.repos[0].label = "api".into();
    let mut web = f.config.repos[0].clone();
    web.slug = "o/s".into();
    web.label = "web".into();
    f.config.repos.push(web);
    let mut p = pipeline(&f, Box::new(happy), FakeOracle::default(), keyed_hub(JIRA)).await;
    p.hub.issue.labels = vec!["Web".into()];
    add(&p.store, &f.config, &p.hub, &p.git, &p.paths, JIRA)
        .await
        .unwrap();
    let t = p.store.task_by_url(JIRA).await.unwrap().unwrap();
    assert_eq!(t.repo, "o/s");
}

#[tokio::test]
async fn status_shows_pr_outcome_for_an_issue_task_in_pr_open() {
    use provefab::commands::{add, status};
    let f = fixture(&["true"]);
    let mut p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let cases = [
        ("open", "pr_open"),
        ("merged", "merged"),
        ("done", "merged"),
        ("archived", "archived"),
        ("closed", "closed"),
    ];
    for (n, (pr_state, _)) in cases.iter().enumerate() {
        let url = format!("https://github.com/o/r/issues/{}", n + 10);
        p.hub.issue.number = n as u64 + 10;
        p.hub.issue.url = url.clone();
        add(&p.store, &f.config, &p.hub, &p.git, &p.paths, &url)
            .await
            .unwrap();
        let id = p.store.task_by_url(&url).await.unwrap().unwrap().id;
        let pr = format!("https://github.com/o/r/pull/{}", n + 10);
        p.store.set_pr(id, &pr, pr_state).await.unwrap();
        p.store.transition(id, PrOpen, "opened").await.unwrap();
    }
    let out = status(&p.store).await.unwrap();
    for (n, (_, word)) in cases.iter().enumerate() {
        let url = format!("https://github.com/o/r/pull/{}", n + 10);
        let line = out.lines().find(|l| l.ends_with(&url)).unwrap();
        assert!(line.contains(&format!("  {word:<12} ")), "{line}");
        if *word != "pr_open" {
            assert!(!line.contains("pr_open"), "{line}");
        }
    }
}
