#![cfg(feature = "testkit")]
//! Evidence and decision record (docs/specs/2026-09-30-evidence-record-design.md).

use provefab::task::TaskKind;
use provefab::testkit::*;

fn kinds(events: &[provefab::record::StoredEvent]) -> Vec<String> {
    events.iter().map(|e| e.kind.clone()).collect()
}

#[tokio::test]
async fn a_pass_records_routing_stage_runs_gates_and_the_plan_in_order() {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let ev = p.store.events(id).await.unwrap();
    let k = kinds(&ev);
    assert_eq!(k[0], "routed");
    for want in ["stage_run", "plan", "gates_run", "review"] {
        assert!(k.contains(&want.to_string()), "{want} missing from {k:?}");
    }
    // Every stage_runs row has exactly one stage_run or gates_run event.
    let runs = p.store.stage_runs(id).await.unwrap().len();
    let run_events = k
        .iter()
        .filter(|x| *x == "stage_run" || *x == "gates_run")
        .count();
    assert_eq!(runs, run_events);
    // seq is 1..=n.
    assert_eq!(
        ev.iter().map(|e| e.seq).collect::<Vec<_>>(),
        (1..=ev.len() as i64).collect::<Vec<_>>()
    );
    let plan = ev.iter().find(|e| e.kind == "plan").unwrap();
    assert_eq!(plan.source, "claim");
    let gates = ev.iter().find(|e| e.kind == "gates_run").unwrap();
    assert_eq!(
        gates.payload["results"][0]["command"],
        "test -f feature.txt"
    );
    assert_eq!(gates.payload["results"][0]["passed"], true);
}

fn bugfix(
    _: &provefab::config::ModelEntry,
    req: &agent_workers::StageRequest,
    _: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>,
) -> Option<agent_workers::StageResult> {
    match stage_of(&req.prompt) {
        "plan" => done(Some(plan_json(Some("test -f fixed.txt")))),
        "implement" => {
            std::fs::write(req.cwd.join("fixed.txt"), "ok\n").unwrap();
            done(None)
        }
        _ => done(Some(approve())),
    }
}

fn bugfix_oracle() -> FakeOracle {
    FakeOracle {
        verdict: Some(verdict(TaskKind::Bugfix, 0.1)),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_bugfix_records_whether_its_reproduction_failed_before_the_fix() {
    let f = fixture(&["test -f fixed.txt"]);
    let p = pipeline(&f, Box::new(bugfix), bugfix_oracle(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    p.drive(id).await.unwrap();
    let ev = p.store.events(id).await.unwrap();
    let r = ev
        .iter()
        .find(|e| e.kind == "reproduction")
        .expect("reproduction event");
    assert_eq!(r.payload["failed_before_fix"], true);
}

// Shared helpers, used by the tests of later tasks.
type P = provefab::pipeline::Pipeline<FakeRunner, FakeOracle, FakeHub>;

/// Approves with two minor findings; F1's text carries a sentinel secret for
/// the export redaction test.
fn approve_with_findings(
    m: &provefab::config::ModelEntry,
    req: &agent_workers::StageRequest,
    tx: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>,
) -> Option<agent_workers::StageResult> {
    match stage_of(&req.prompt) {
        "review" => done(Some(serde_json::json!({"verdict": "approve", "findings": [
            {"file": "src/a.rs", "line": 4, "severity": "minor", "text": "typo SENTINEL_SECRET_42"},
            {"file": "src/b.rs", "line": null, "severity": "minor", "text": "naming"}
        ]}))),
        _ => happy(m, req, tx),
    }
}

/// A task driven to PrOpen with findings F1 and F2 (pass 1).
async fn open_task_with_findings() -> (Fixture, P, i64) {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(
        &f,
        Box::new(approve_with_findings),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    (f, p, id)
}

/// `open_task_with_findings`, then merged by a person: README.md becomes
/// "broken" in one commit on origin/main (so `grep -q hello README.md`
/// post-merge checks fail and a revert is prepared).
async fn merged_task(checks: &[&str]) -> (Fixture, P, i64) {
    let mut f = fixture(&["test -f feature.txt"]);
    f.config.repos[0].post_merge_checks = checks.iter().map(|c| c.to_string()).collect();
    let p = pipeline(
        &f,
        Box::new(approve_with_findings),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    *p.hub.revert_origin.lock().unwrap() = Some(f.origin.clone());
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let repo = f.config.repos[0].path_in(&f.home);
    std::fs::write(repo.join("README.md"), "broken\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-qm", "squash merge"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: Some("pr-head".into()),
        merge_sha: Some(sha),
        base_ref: Some("main".into()),
        commit_count: Some(1),
    };
    // With `revert_origin` set, the fake hub tracks each PR by URL.
    let status = p.hub.pr_status.lock().unwrap().clone();
    let url = p.store.task(id).await.unwrap().unwrap().pr_url.unwrap();
    p.hub.pr_statuses.lock().unwrap().insert(url, status);
    p.watch_pr(id).await.unwrap();
    (f, p, id)
}

fn review_with(findings: serde_json::Value, verdict: &str) -> serde_json::Value {
    serde_json::json!({"verdict": verdict, "findings": findings})
}

#[tokio::test]
async fn review_findings_get_stable_keys_across_rounds() {
    // Round 1 asks for changes with two findings, round 2 approves with one minor note.
    let f = fixture(&["test -f feature.txt"]);
    let rounds = std::sync::Mutex::new(0);
    let script =
        move |m: &provefab::config::ModelEntry,
              req: &agent_workers::StageRequest,
              tx: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>| {
            match stage_of(&req.prompt) {
                "review" => {
                    let mut n = rounds.lock().unwrap();
                    *n += 1;
                    if *n == 1 {
                        done(Some(review_with(
                            serde_json::json!([
                                {"file": "src/a.rs", "line": 3, "severity": "blocking", "text": "off by one"},
                                {"file": "src/b.rs", "line": null, "severity": "minor", "text": "naming"}
                            ]),
                            "changes",
                        )))
                    } else {
                        done(Some(review_with(
                            serde_json::json!([
                                {"file": "src/a.rs", "line": 4, "severity": "minor", "text": "comment typo"}
                            ]),
                            "approve",
                        )))
                    }
                }
                _ => happy(m, req, tx),
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
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let fs = p.store.findings(id).await.unwrap();
    assert_eq!(
        fs.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(),
        ["F1", "F2", "F3"]
    );
    assert_eq!((fs[0].round, fs[2].round), (0, 1));
    assert!(fs.iter().all(|x| x.pass == 1));
    let reviews: Vec<_> = p
        .store
        .events(id)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "review")
        .collect();
    assert_eq!(
        reviews[0].payload["findings"],
        serde_json::json!(["F1", "F2"])
    );
    assert_eq!(reviews[1].payload["findings"], serde_json::json!(["F3"]));
    let body = &p.hub.prs.lock().unwrap()[0].3;
    assert!(
        body.contains("F3 · minor · `src/a.rs:4` · comment typo"),
        "{body}"
    );
    assert!(body.contains("Reply `/provefab F3 rejected`"), "{body}");
    assert!(!body.contains('\u{2014}'));
}

#[tokio::test]
async fn an_approval_without_findings_has_no_review_notes() {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    p.drive(id).await.unwrap();
    assert!(p.store.findings(id).await.unwrap().is_empty());
    let review = p
        .store
        .events(id)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "review")
        .unwrap();
    assert_eq!(review.payload["findings"], serde_json::json!([]));
    assert!(!p.hub.prs.lock().unwrap()[0].3.contains("Review notes"));
}

#[tokio::test]
async fn opening_and_merging_a_pr_are_facts_and_open_findings_are_inferred_unaddressed() {
    let (_f, p, id) = merged_task(&[]).await;
    let ev = p.store.events(id).await.unwrap();
    let opened = ev.iter().find(|e| e.kind == "pr_opened").unwrap();
    assert_eq!(opened.payload["pass"], 1);
    let merged = ev.iter().find(|e| e.kind == "merged").unwrap();
    assert_eq!(merged.payload["by"], "human");
    let inferred: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == "finding_unaddressed_at_merge")
        .collect();
    assert_eq!(inferred.len(), 2);
    assert_eq!(inferred[0].source, "inferred");
    assert_eq!(inferred[0].payload["finding"], "F1");
    assert_eq!(inferred[0].payload["rule_version"], 1);
    // Replaying the merge write infers nothing new.
    p.store
        .write_with_inference(
            id,
            provefab::store::Write::Nothing,
            &[],
            Some((provefab::record::Rule::UnaddressedAtMerge, 1)),
        )
        .await
        .unwrap();
    assert_eq!(
        p.store
            .events(id)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind == "finding_unaddressed_at_merge")
            .count(),
        2
    );
}

#[tokio::test]
async fn a_post_merge_revert_marks_the_merged_findings() {
    let (_f, p, id) = merged_task(&["grep -q hello README.md"]).await;
    for _ in 0..12 {
        let _ = p.process_post_merge(id).await;
    }
    let ev = p.store.events(id).await.unwrap();
    assert!(
        ev.iter()
            .any(|e| e.kind == "post_merge" && e.payload["state"] == "revert_open")
    );
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == "finding_followed_by_revert")
            .count(),
        2
    );
}

#[tokio::test]
async fn a_reopened_issue_marks_the_previous_pass_findings() {
    let (_f, p, id) = merged_task(&[]).await;
    p.store.set_pr_state(id, "done").await.unwrap();
    p.hub
        .issue_is_open
        .store(true, std::sync::atomic::Ordering::SeqCst);
    p.watch_pr(id).await.unwrap();
    let ev = p.store.events(id).await.unwrap();
    let reopened = ev.iter().find(|e| e.kind == "issue_reopened").unwrap();
    assert_eq!(reopened.payload["previous_pass"], 1);
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == "finding_followed_by_reopen")
            .count(),
        2
    );
}

fn comment(author: &str, association: &str, body: &str, at: &str) -> provefab::forge::Comment {
    provefab::forge::Comment {
        author: author.into(),
        association: association.into(),
        body: body.into(),
        created_at: at.into(),
    }
}

#[tokio::test]
async fn authorized_commands_record_dispositions_once_and_the_latest_wins() {
    let (_f, p, id) = open_task_with_findings().await;
    let task = p.store.task(id).await.unwrap().unwrap();
    let cs = vec![
        comment(
            "alice",
            "NONE",
            "/provefab F1 rejected: false positive",
            "2026-09-30T10:00:00Z",
        ),
        comment(
            "mallory",
            "NONE",
            "/provefab F2 accepted",
            "2026-09-30T10:01:00Z",
        ),
        comment(
            "bob",
            "MEMBER",
            "/provefab F9 fixed\n/provefab F2 fixed",
            "2026-09-30T10:02:00Z",
        ),
        comment(
            "bob",
            "MEMBER",
            "/provefab F1 accepted",
            "2026-09-30T10:03:00Z",
        ),
    ];
    p.apply_finding_commands(&task, &cs).await.unwrap();
    p.apply_finding_commands(&task, &cs).await.unwrap();
    let ev = p.store.events(id).await.unwrap();
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == "finding_disposition")
            .count(),
        3
    );
    let ignored: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == "command_ignored")
        .map(|e| e.payload["why"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ignored, ["not authorized", "unknown finding"]);
    let current = p.store.current_dispositions(id).await.unwrap();
    assert_eq!(current["F1"], provefab::record::Disposition::Accepted);
    assert_eq!(current["F2"], provefab::record::Disposition::Fixed);
}

#[tokio::test]
async fn open_prs_are_read_every_tick() {
    let (_f, p, id) = open_task_with_findings().await;
    let mut status = p.hub.pr_status.lock().unwrap().clone();
    status.comments = vec![comment(
        "alice",
        "NONE",
        "/provefab F1 waived",
        "2026-09-30T10:00:00Z",
    )];
    *p.hub.pr_status.lock().unwrap() = status;
    p.watch_pr(id).await.unwrap();
    assert_eq!(
        p.store.current_dispositions(id).await.unwrap()["F1"],
        provefab::record::Disposition::Waived
    );
}

#[tokio::test]
async fn merged_prs_are_still_read_after_the_merge() {
    let (_f, p, id) = merged_task(&[]).await;
    let url = p.store.task(id).await.unwrap().unwrap().pr_url.unwrap();
    // The fake hub reads the per-URL entry first (merged_task filled it).
    p.hub
        .pr_statuses
        .lock()
        .unwrap()
        .get_mut(&url)
        .unwrap()
        .comments = vec![comment(
        "alice",
        "NONE",
        "/provefab F2 accepted",
        "2026-09-30T11:00:00Z",
    )];
    p.watch_pr(id).await.unwrap();
    assert_eq!(
        p.store.current_dispositions(id).await.unwrap()["F2"],
        provefab::record::Disposition::Accepted
    );
}

#[tokio::test]
async fn a_command_on_a_closed_pr_is_not_a_change_request() {
    let (_f, p, id) = open_task_with_findings().await;
    let mut status = p.hub.pr_status.lock().unwrap().clone();
    status.state = provefab::forge::PrState::Closed;
    status.comments = vec![comment(
        "alice",
        "NONE",
        "/provefab F1 rejected",
        "2026-09-30T10:00:00Z",
    )];
    *p.hub.pr_status.lock().unwrap() = status;
    let state = p.watch_pr(id).await.unwrap();
    assert_eq!(state, Failed);
    assert!(
        p.store
            .current_dispositions(id)
            .await
            .unwrap()
            .contains_key("F1")
    );
}
