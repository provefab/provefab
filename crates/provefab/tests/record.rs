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
    merged_task_with(checks, vec![]).await
}

async fn merged_task_with(
    checks: &[&str],
    comments: Vec<provefab::forge::Comment>,
) -> (Fixture, P, i64) {
    merged_task_scripted(checks, comments, Box::new(approve_with_findings)).await
}

async fn merged_task_scripted(
    checks: &[&str],
    comments: Vec<provefab::forge::Comment>,
    script: Box<Script>,
) -> (Fixture, P, i64) {
    let mut f = fixture(&["test -f feature.txt"]);
    f.config.repos[0].post_merge_checks = checks.iter().map(|c| c.to_string()).collect();
    merged_task_in(f, comments, script).await
}

async fn merged_task_in(
    f: Fixture,
    comments: Vec<provefab::forge::Comment>,
    script: Box<Script>,
) -> (Fixture, P, i64) {
    let p = pipeline(&f, script, FakeOracle::default(), FakeHub::new("x")).await;
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
        comments,
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

/// Round 0 asks for changes with F1 and F2, round 1 approves with F3.
fn two_rounds() -> Box<Script> {
    let rounds = std::sync::Mutex::new(0);
    Box::new(
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
        },
    )
}

#[tokio::test]
async fn review_findings_get_stable_keys_across_rounds() {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(&f, two_rounds(), FakeOracle::default(), FakeHub::new("x")).await;
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

fn inferred(ev: &[provefab::record::StoredEvent], kind: &str) -> Vec<String> {
    ev.iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.payload["finding"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn only_the_findings_shown_in_the_merged_pr_are_inferred_unaddressed() {
    let (_f, p, id) = merged_task_scripted(&[], vec![], two_rounds()).await;
    let ev = p.store.events(id).await.unwrap();
    assert_eq!(inferred(&ev, "finding_unaddressed_at_merge"), ["F3"]);
    // A reopen still names every finding of the pass.
    p.store.set_pr_state(id, "done").await.unwrap();
    p.hub
        .issue_is_open
        .store(true, std::sync::atomic::Ordering::SeqCst);
    p.watch_pr(id).await.unwrap();
    let ev = p.store.events(id).await.unwrap();
    assert_eq!(
        inferred(&ev, "finding_followed_by_reopen"),
        ["F1", "F2", "F3"]
    );
}

/// Round 0 asks for changes with F1; round 1 approves without findings.
fn changes_then_clean_approval() -> Box<Script> {
    let reviews = std::sync::Mutex::new(0);
    Box::new(
        move |m: &provefab::config::ModelEntry,
              req: &agent_workers::StageRequest,
              tx: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>| {
            match stage_of(&req.prompt) {
                "review" => {
                    let mut n = reviews.lock().unwrap();
                    *n += 1;
                    if *n == 1 {
                        done(Some(review_with(
                            serde_json::json!([
                                {"file": "src/a.rs", "line": 3, "severity": "blocking", "text": "off by one"}
                            ]),
                            "changes",
                        )))
                    } else {
                        done(Some(review_with(serde_json::json!([]), "approve")))
                    }
                }
                _ => happy(m, req, tx),
            }
        },
    )
}

#[tokio::test]
async fn a_final_round_without_findings_shows_and_infers_none() {
    let (_f, p, id) = merged_task_scripted(&[], vec![], changes_then_clean_approval()).await;
    let fs = p.store.findings(id).await.unwrap();
    assert_eq!(
        fs.iter()
            .map(|x| (x.key.as_str(), x.round))
            .collect::<Vec<_>>(),
        [("F1", 0)]
    );
    let body = p.hub.prs.lock().unwrap()[0].3.clone();
    assert!(!body.contains("Review notes"), "{body}");
    let ev = p.store.events(id).await.unwrap();
    assert!(ev.iter().any(|e| e.kind == "merged"));
    assert!(
        inferred(&ev, "finding_unaddressed_at_merge").is_empty(),
        "{:?}",
        kinds(&ev)
    );
}

/// Pro's second reviewer: two approvals before the PR opens.
struct TwoApprovals;

impl provefab::policy::ReviewPolicy for TwoApprovals {
    fn approvals_needed(&self, _repo: &provefab::config::RepoConfig) -> u8 {
        2
    }
    fn after_pr_opened<'a>(
        &'a self,
        _cx: provefab::policy::PrOpened<'a>,
    ) -> provefab::policy::BoxFuture<'a, Result<String, provefab::pipeline::PipelineError>> {
        Box::pin(async move { Ok("opened".to_string()) })
    }
}

/// Round 0 asks for changes with F1; round 1 is approved twice, with F2 then F3.
fn two_approvers_two_rounds() -> Box<Script> {
    let reviews = std::sync::Mutex::new(0);
    Box::new(
        move |m: &provefab::config::ModelEntry,
              req: &agent_workers::StageRequest,
              tx: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>| {
            match stage_of(&req.prompt) {
                "review" => {
                    let mut n = reviews.lock().unwrap();
                    *n += 1;
                    let (file, text, verdict) = match *n {
                        1 => ("src/a.rs", "off by one", "changes"),
                        2 => ("src/b.rs", "first approver note", "approve"),
                        _ => ("src/c.rs", "second approver note", "approve"),
                    };
                    done(Some(review_with(
                        serde_json::json!([
                            {"file": file, "line": 5, "severity": "minor", "text": text}
                        ]),
                        verdict,
                    )))
                }
                _ => happy(m, req, tx),
            }
        },
    )
}

#[tokio::test]
async fn every_final_round_review_is_shown_and_inferred_unaddressed() {
    let mut f = fixture(&["test -f feature.txt"]);
    f.policy = std::sync::Arc::new(TwoApprovals);
    let (_f, p, id) = merged_task_in(f, vec![], two_approvers_two_rounds()).await;
    let fs = p.store.findings(id).await.unwrap();
    assert_eq!(
        fs.iter()
            .map(|x| (x.key.as_str(), x.round))
            .collect::<Vec<_>>(),
        [("F1", 0), ("F2", 1), ("F3", 1)]
    );
    assert_ne!(fs[1].reviewer_model, fs[2].reviewer_model);
    let body = p.hub.prs.lock().unwrap()[0].3.clone();
    assert!(
        body.contains(&format!(
            "- F2 · minor · `src/b.rs:5` · first approver note ({})",
            fs[1].reviewer_model
        )),
        "{body}"
    );
    assert!(
        body.contains(&format!(
            "- F3 · minor · `src/c.rs:5` · second approver note ({})",
            fs[2].reviewer_model
        )),
        "{body}"
    );
    assert!(!body.contains("F1"), "{body}");
    assert!(body.contains("Reply `/provefab F2 rejected`"), "{body}");
    let ev = p.store.events(id).await.unwrap();
    assert_eq!(inferred(&ev, "finding_unaddressed_at_merge"), ["F2", "F3"]);
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

#[tokio::test]
async fn a_command_posted_before_the_merge_is_read_before_it_is_inferred() {
    let (_f, p, id) = merged_task_with(
        &[],
        vec![comment(
            "alice",
            "NONE",
            "/provefab F1 rejected",
            "2026-09-30T10:00:00Z",
        )],
    )
    .await;
    assert_eq!(
        p.store.current_dispositions(id).await.unwrap()["F1"],
        provefab::record::Disposition::Rejected
    );
    let ev = p.store.events(id).await.unwrap();
    let inferred: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == "finding_unaddressed_at_merge")
        .map(|e| e.payload["finding"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(inferred, ["F2"]);
}

#[tokio::test]
async fn command_lines_are_stripped_from_closed_pr_change_requests() {
    let (_f, p, id) = open_task_with_findings().await;
    let mut status = p.hub.pr_status.lock().unwrap().clone();
    status.state = provefab::forge::PrState::Closed;
    status.comments = vec![comment(
        "alice",
        "NONE",
        "Please rename x\n/provefab F1 rejected",
        "2026-09-30T10:00:00Z",
    )];
    *p.hub.pr_status.lock().unwrap() = status;
    p.watch_pr(id).await.unwrap();
    let review = p.store.last_output(id, "review").await.unwrap().unwrap();
    let human: Vec<_> = review["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["text"].as_str())
        .filter(|t| t.contains("Please rename x"))
        .collect();
    assert_eq!(human.len(), 1, "{review}");
    assert!(!human[0].contains("/provefab"));
    assert!(
        p.store
            .current_dispositions(id)
            .await
            .unwrap()
            .contains_key("F1")
    );
}

#[tokio::test]
async fn export_redacts_free_text_unless_asked_and_every_line_is_json() {
    let (_f, p, id) = open_task_with_findings().await;
    let out = provefab::commands::export(&p.store, None, None, false)
        .await
        .unwrap();
    assert!(!out.contains("SENTINEL_SECRET_42"), "{out}");
    for line in out.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v["type"] == "event" || v["type"] == "finding");
    }
    assert!(out.contains("\"redacted\":true"));
    let full = provefab::commands::export(&p.store, None, None, true)
        .await
        .unwrap();
    assert!(full.contains("SENTINEL_SECRET_42"));
    let _ = id;
}

#[tokio::test]
async fn export_filters_findings_by_since_and_repo_case_insensitively() {
    let (_f, p, id) = open_task_with_findings().await;
    let repo = p.store.task(id).await.unwrap().unwrap().repo.to_uppercase();
    let all = provefab::commands::export(&p.store, Some(&repo), None, false)
        .await
        .unwrap();
    assert!(all.contains("\"type\":\"finding\""), "{all}");
    let past = provefab::commands::export(&p.store, None, Some("2000-01-01"), false)
        .await
        .unwrap();
    assert!(past.contains("\"type\":\"finding\""), "{past}");
    let future = provefab::commands::export(&p.store, None, Some("2999-01-01"), false)
        .await
        .unwrap();
    assert_eq!(future, "");
}

#[tokio::test]
async fn export_of_an_empty_record_prints_nothing() {
    let f = fixture(&["true"]);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    assert_eq!(
        provefab::commands::export(&p.store, None, None, false)
            .await
            .unwrap(),
        ""
    );
}

#[tokio::test]
async fn prune_deletes_only_with_yes_and_only_finished_tasks() {
    let (_f, p, id) = merged_task(&[]).await;
    p.store.set_pr_state(id, "archived").await.unwrap();
    let dry = provefab::commands::prune(&p.store, "2999-01-01", false)
        .await
        .unwrap();
    assert!(dry.contains(&format!("task {id}")), "{dry}");
    assert!(!p.store.events(id).await.unwrap().is_empty());
    provefab::commands::prune(&p.store, "2999-01-01", true)
        .await
        .unwrap();
    assert!(p.store.events(id).await.unwrap().is_empty());
    assert!(p.store.findings(id).await.unwrap().is_empty());
    assert!(
        provefab::commands::prune(&p.store, "30/09/2026", true)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn prune_keeps_an_open_pr() {
    let (_f, p, id) = open_task_with_findings().await;
    let task = p.store.task(id).await.unwrap().unwrap();
    assert_eq!(task.pr_state.as_deref(), Some("open"));
    let out = provefab::commands::prune(&p.store, "2999-01-01", true)
        .await
        .unwrap();
    assert!(!out.contains(&format!("task {id}")), "{out}");
    assert!(!p.store.events(id).await.unwrap().is_empty());
    assert!(!p.store.findings(id).await.unwrap().is_empty());
}

#[tokio::test]
async fn prune_keeps_a_task_whose_post_merge_check_is_still_running() {
    let (_f, p, id) = merged_task(&["grep -q hello README.md"]).await;
    p.store.set_pr_state(id, "archived").await.unwrap();
    let checks = p.store.post_merge_checks(id).await.unwrap();
    assert_eq!(checks.len(), 1);
    assert!(!checks[0].state.is_terminal(), "{:?}", checks[0].state);
    let out = provefab::commands::prune(&p.store, "2999-01-01", true)
        .await
        .unwrap();
    assert!(!out.contains(&format!("task {id}")), "{out}");
    assert!(!p.store.events(id).await.unwrap().is_empty());
}

async fn record_periodic_runs(store: &provefab::store::Store, starts: &[i64]) {
    for at in starts {
        store
            .record_maintenance_run(&provefab::store::MaintenanceRun {
                id: 0,
                repo: "o/r".into(),
                kind: "periodic".into(),
                started_at: *at,
                finished_at: Some(*at + 5),
                model_id: None,
                cost_usd: None,
                quota_units: None,
                outcome: "ok".into(),
                pr_url: None,
                detail: None,
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn prune_deletes_old_maintenance_runs_but_the_latest_of_each_kind() {
    let f = fixture(&["true"]);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    record_periodic_runs(&p.store, &[10, 20, 30]).await;
    let dry = provefab::commands::prune(&p.store, "2999-01-01", false)
        .await
        .unwrap();
    assert!(dry.contains("2 maintenance runs"), "{dry}");
    assert!(dry.contains("dry run: pass --yes to delete"), "{dry}");
    assert_eq!(p.store.maintenance_runs(None).await.unwrap().len(), 3);
    let done = provefab::commands::prune(&p.store, "2999-01-01", true)
        .await
        .unwrap();
    assert!(done.contains("2 maintenance runs"), "{done}");
    let left = p.store.maintenance_runs(None).await.unwrap();
    assert_eq!(left.iter().map(|r| r.started_at).collect::<Vec<_>>(), [30]);
}

#[tokio::test]
async fn prune_keeps_maintenance_runs_finished_after_the_date() {
    let f = fixture(&["true"]);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    record_periodic_runs(&p.store, &[1_700_000_000, 1_700_086_400]).await;
    let out = provefab::commands::prune(&p.store, "2000-01-01", true)
        .await
        .unwrap();
    assert!(out.contains("0 maintenance runs"), "{out}");
    assert_eq!(p.store.maintenance_runs(None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn log_shows_the_record_with_sources_and_current_dispositions() {
    let (_f, p, id) = open_task_with_findings().await;
    let task = p.store.task(id).await.unwrap().unwrap();
    p.apply_finding_commands(
        &task,
        &[comment(
            "alice",
            "NONE",
            "/provefab F1 rejected",
            "2026-09-30T10:00:00Z",
        )],
    )
    .await
    .unwrap();
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(log.contains("record:"), "{log}");
    assert!(log.contains("[fact] routed"), "{log}");
    assert!(log.contains("[claim] review"), "{log}");
    assert!(
        log.contains("[human] finding_disposition F1 rejected"),
        "{log}"
    );
    assert!(log.contains("F1 · "), "{log}");
    assert!(!log.to_lowercase().contains("proof"));
}
