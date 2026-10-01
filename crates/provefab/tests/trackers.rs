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
