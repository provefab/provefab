//! Pipeline runs on a Jira-like and a Linear-like ticket (issue trackers spec
//! §10): `FakeHub` with a ticket key, the repository configured for that tracker.

use provefab::store::{now, rfc3339};
use provefab::testkit::*;
use provefab::tracker::{TrackerConfig, TrackerKind};

const JIRA_URL: &str = "https://acme.atlassian.net/browse/ENG-7";
const LINEAR_URL: &str = "https://linear.app/acme/issue/ENG-7/add-a-feature-file";

fn on(f: &mut Fixture, kind: TrackerKind) {
    f.config.repos[0].tracker = Some(TrackerConfig {
        kind,
        site: (kind == TrackerKind::Jira).then(|| "acme.atlassian.net".to_string()),
        project: Some("ENG".into()),
    });
}

fn url_of(kind: TrackerKind) -> &'static str {
    if kind == TrackerKind::Linear {
        LINEAR_URL
    } else {
        JIRA_URL
    }
}

fn ticket_hub(kind: TrackerKind, body: &str) -> FakeHub {
    let mut hub = FakeHub::new(body);
    hub.issue.key = Some("ENG-7".into());
    hub.issue.url = url_of(kind).into();
    hub
}

async fn opened(kind: TrackerKind) -> (Fixture, Pipeline<FakeRunner, FakeOracle, FakeHub>, i64) {
    let mut f = fixture(&["test -f feature.txt"]);
    on(&mut f, kind);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        ticket_hub(kind, "please"),
    )
    .await;
    let id = queue_ticket(&p, 7, "ENG-7", url_of(kind)).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    (f, p, id)
}

#[tokio::test]
async fn a_jira_ticket_becomes_a_pr_named_after_its_key() {
    let (f, p, id) = opened(TrackerKind::Jira).await;
    let (head, base, title, body) = p.hub.prs.lock().unwrap()[0].clone();
    assert_eq!(
        (head.as_str(), base.as_str()),
        ("provefab/ENG-7-add-a-feature-file", "main")
    );
    assert_eq!(title, "ENG-7: Add a feature file");
    assert_eq!(
        body.lines().next().unwrap(),
        "Issue: [ENG-7](https://acme.atlassian.net/browse/ENG-7)."
    );
    assert!(!body.contains("Closes #"), "{body}");
    let message = git(
        &f.origin,
        &[
            "log",
            "-1",
            "--format=%B",
            "provefab/ENG-7-add-a-feature-file",
        ],
    );
    assert!(
        message.contains(&format!("Provefab task {id}, issue ENG-7.")),
        "{message}"
    );
    let plan_prompt = p.runner.calls()[0].2.clone();
    assert!(
        plan_prompt.contains("Issue ENG-7: Add a feature file"),
        "{plan_prompt}"
    );
    assert_eq!(
        p.hub.last_labels(),
        (vec!["provefab:in-pr".into()], vec!["provefab".into()])
    );
}

#[tokio::test]
async fn a_linear_ticket_pr_lets_linear_link_it() {
    let (_f, p, _id) = opened(TrackerKind::Linear).await;
    let (head, _, title, body) = p.hub.prs.lock().unwrap()[0].clone();
    assert_eq!(head, "provefab/ENG-7-add-a-feature-file");
    assert_eq!(title, "ENG-7: Add a feature file");
    assert_eq!(
        body.lines().next().unwrap(),
        "Issue: [ENG-7](https://linear.app/acme/issue/ENG-7/add-a-feature-file). Fixes ENG-7"
    );
}

#[tokio::test]
async fn status_log_and_export_show_the_key() {
    let (_f, p, id) = opened(TrackerKind::Jira).await;
    let status = provefab::commands::status(&p.store).await.unwrap();
    assert!(status.contains("o/r ENG-7"), "{status}");
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(
        log.starts_with(&format!("task {id} o/r ENG-7 \"Add a feature file\"")),
        "{log}"
    );
    let export = provefab::commands::export(&p.store, None, None, false)
        .await
        .unwrap();
    let first: Value = serde_json::from_str(export.lines().next().unwrap()).unwrap();
    assert_eq!(
        (first["issue"].as_u64(), first["issue_key"].as_str()),
        (Some(7), Some("ENG-7"))
    );
}

#[tokio::test]
async fn a_member_answers_a_question_and_the_bot_line_without_emphasis_does_not() {
    let mut f = fixture(&["true"]);
    on(&mut f, TrackerKind::Jira);
    let oracle = FakeOracle {
        verdict: Some(verdict(TaskKind::Feature, 0.9)),
        reply: Some(0.9),
        ..Default::default()
    };
    let p = pipeline(
        &f,
        Box::new(happy),
        oracle,
        ticket_hub(TrackerKind::Jira, "do it"),
    )
    .await;
    let id = queue_ticket(&p, 7, "ENG-7", JIRA_URL).await;
    assert_eq!(p.drive(id).await.unwrap(), NeedsInfo);
    let member = |author: &str, body: &str, later: i64| Comment {
        author: author.into(),
        association: "MEMBER".into(),
        body: body.into(),
        created_at: rfc3339(now() + later),
    };
    // Provefab's own question as Jira reads it back: no asterisks.
    p.hub.comments.lock().unwrap().push(member(
        "acc-operator",
        "Posted by Provefab (automated), not typed by a person.\n\nWhich version?",
        5,
    ));
    assert_eq!(p.step(id).await.unwrap(), NeedsInfo);
    // The same account, typing as a person, is heard (plan decision 4).
    p.hub
        .comments
        .lock()
        .unwrap()
        .push(member("acc-operator", "It should create feature.txt", 6));
    assert_eq!(p.step(id).await.unwrap(), Queued);
    assert_eq!(
        p.hub.last_labels(),
        (vec!["provefab".into()], vec!["provefab:needs-info".into()])
    );
}

#[tokio::test]
async fn a_ticket_reopened_after_merge_starts_a_pass_on_a_keyed_branch() {
    let (_f, p, id) = opened(TrackerKind::Linear).await;
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: None,
        merge_sha: None,
        base_ref: None,
        commit_count: None,
    };
    assert_eq!(p.watch_pr(id).await.unwrap(), PrOpen);
    // Merged and the ticket done: nothing to do (as `tests/autonomy.rs` does it).
    assert_eq!(p.watch_pr(id).await.unwrap(), PrOpen);
    p.hub
        .issue_is_open
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(p.watch_pr(id).await.unwrap(), Queued);
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let prs = p.hub.prs.lock().unwrap().clone();
    assert_eq!(
        prs.last().unwrap().0,
        "provefab/ENG-7-add-a-feature-file-r2"
    );
}

/// A Jira-repository pipeline with ENG-7 queued, nothing run yet.
async fn queued_ticket() -> (Fixture, Pipeline<FakeRunner, FakeOracle, FakeHub>, i64) {
    let mut f = fixture(&["true"]);
    on(&mut f, TrackerKind::Jira);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        ticket_hub(TrackerKind::Jira, "please"),
    )
    .await;
    let id = queue_ticket(&p, 7, "ENG-7", JIRA_URL).await;
    (f, p, id)
}

fn nothing_written(p: &Pipeline<FakeRunner, FakeOracle, FakeHub>) {
    assert!(
        p.hub.posted.lock().unwrap().is_empty(),
        "{:?}",
        p.hub.posted
    );
    assert!(
        p.hub.labels.lock().unwrap().is_empty(),
        "{:?}",
        p.hub.labels
    );
}

#[tokio::test]
async fn a_ticket_whose_repository_left_the_config_gives_up_without_writing_to_github() {
    let (_f, mut p, id) = queued_ticket().await;
    p.config.repos.clear();
    assert_eq!(p.step(id).await.unwrap(), Failed);
    nothing_written(&p);
    // Skipped, not queued for a retry that would reach GitHub later.
    assert!(p.store.pending_github().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_ticket_on_a_repository_now_on_github_is_parked_without_writing_to_github() {
    let (_f, mut p, id) = queued_ticket().await;
    p.config.repos[0].tracker = None;
    assert_eq!(p.park(id, "stopped", "").await.unwrap(), NeedsYou);
    nothing_written(&p);
}

#[tokio::test]
async fn a_pending_effect_of_a_ticket_is_not_replayed_to_github() {
    let (_f, mut p, id) = queued_ticket().await;
    p.hub
        .comment_failures
        .store(1, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(p.park(id, "stopped", "").await.unwrap(), NeedsYou);
    assert_eq!(p.store.pending_github().await.unwrap().len(), 2);
    p.config.repos.clear();
    p.retry_pending(&std::collections::HashSet::new()).await;
    nothing_written(&p);
    // Kept: restoring the repository replays them to its tracker.
    assert_eq!(p.store.pending_github().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_task_that_disagrees_with_its_repository_tracker_stops_run_and_add() {
    use provefab::commands::{CommandError, tracker_history};
    let (_f, mut p, id) = queued_ticket().await;
    assert!(tracker_history(&p.store, &p.config).await.is_ok());
    // A key on a repository now on GitHub.
    p.config.repos[0].tracker = None;
    let err = tracker_history(&p.store, &p.config)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("o/r") && err.contains(&format!("task {id}")) && err.contains("ENG-7"),
        "{err}"
    );
    assert!(err.contains("Restore the previous tracker"), "{err}");
    let added = provefab::commands::add(
        &p.store,
        &p.config,
        &p.hub,
        &p.git,
        &p.paths,
        "https://github.com/o/r/issues/8",
    )
    .await;
    assert!(
        matches!(added, Err(CommandError::TrackerHistory(_))),
        "{added:?}"
    );
    // A key of another project.
    on_project(&mut p, "OPS");
    assert!(tracker_history(&p.store, &p.config).await.is_err());
    // A terminal task alone does not block; its pending effect does.
    on_project(&mut p, "OPS");
    p.store.transition(id, Failed, "test").await.unwrap();
    assert!(tracker_history(&p.store, &p.config).await.is_ok());
    p.store
        .record_output(
            id,
            "pending_github",
            &json!({"op": "comment", "slug": "o/r", "number": 7, "body": "x"}),
        )
        .await
        .unwrap();
    assert!(tracker_history(&p.store, &p.config).await.is_err());
}

#[tokio::test]
async fn a_github_task_on_a_repository_now_on_jira_stops_run() {
    let f = fixture(&["true"]);
    let mut p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    on_project(&mut p, "ENG");
    let err = provefab::commands::tracker_history(&p.store, &p.config)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(&format!("task {id}")) && err.contains("#7"),
        "{err}"
    );
}

fn on_project(p: &mut Pipeline<FakeRunner, FakeOracle, FakeHub>, project: &str) {
    p.config.repos[0].tracker = Some(TrackerConfig {
        kind: TrackerKind::Jira,
        site: Some("acme.atlassian.net".into()),
        project: Some(project.into()),
    });
}

#[tokio::test]
async fn intake_skips_a_ticket_whose_number_is_an_earlier_task_of_another_tracker() {
    let f = fixture(&["true"]);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    let mut repo = p.config.repos[0].clone();
    repo.tracker = Some(TrackerConfig {
        kind: TrackerKind::Jira,
        site: Some("acme.atlassian.net".into()),
        project: Some("ENG".into()),
    });
    let jira = ticket_hub(TrackerKind::Jira, "x");
    let added = provefab::intake::poll(&jira, &repo, &p.store)
        .await
        .unwrap();
    assert!(added.is_empty());
    let task = p.store.task(id).await.unwrap().unwrap();
    assert_eq!(task.issue_key, None);
    // The line it logs names both.
    let line = provefab::tracker::key_clash(&task, Some("ENG-7")).unwrap();
    assert!(
        line.contains("ENG-7") && line.contains("#7") && line.contains(&format!("task {id}")),
        "{line}"
    );
    assert_eq!(provefab::tracker::key_clash(&task, None), None);
}

#[tokio::test]
async fn add_refuses_a_stored_task_of_the_same_number_from_another_tracker() {
    let (_f, mut p, id) = queued_ticket().await;
    p.store.transition(id, Failed, "test").await.unwrap();
    p.config.repos[0].tracker = None;
    let hub = FakeHub::new("x");
    let err = provefab::commands::add(
        &p.store,
        &p.config,
        &hub,
        &p.git,
        &p.paths,
        "https://github.com/o/r/issues/7",
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("ENG-7") && err.contains("#7"), "{err}");
    assert_eq!(p.store.task(id).await.unwrap().unwrap().state, Failed);
    assert!(hub.labels.lock().unwrap().is_empty());
}
