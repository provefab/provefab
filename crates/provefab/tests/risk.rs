//! Risk-aware policy in the pipeline (spec 2026-09-30-risk-policy §6-§7).
#![cfg(feature = "testkit")]

use provefab::risk::Detected;
use provefab::testkit::*;

/// Implements with a new migration and `feature.txt`, then approves.
fn risky(
    m: &ModelEntry,
    req: &StageRequest,
    tx: &UnboundedSender<WorkerEvent>,
) -> Option<StageResult> {
    match stage_of(&req.prompt) {
        "implement" => {
            std::fs::create_dir_all(req.cwd.join("migrations")).unwrap();
            std::fs::write(
                req.cwd.join("migrations/0005.sql"),
                "create table t(x int);\n",
            )
            .unwrap();
            std::fs::write(req.cwd.join("feature.txt"), "done\n").unwrap();
            done(None)
        }
        _ => happy(m, req, tx),
    }
}

fn migration_checks(checks: &str) -> Option<provefab::risk::RiskConfig> {
    Some(toml::from_str(&format!("[categories.migrations]\nchecks = {checks}")).unwrap())
}

async fn classified(p: &Pipeline<FakeRunner, FakeOracle, FakeHub>, id: i64) -> Vec<Vec<Detected>> {
    p.store
        .events(id)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "risk_classified")
        .map(|e| serde_json::from_value(e.payload["categories"].clone()).unwrap())
        .collect()
}

/// A frontier model from another provider than the implementer (`std-claude`).
fn with_codex_frontier(f: &mut Fixture) {
    f.config.models.push(
        toml::from_str(
            "id = \"top-codex\"\nworker = \"codex\"\nmodel = \"gpt-5.5-pro\"\ntier = \"frontier\"",
        )
        .unwrap(),
    );
}

fn reviewers(p: &Pipeline<FakeRunner, FakeOracle, FakeHub>) -> Vec<String> {
    p.runner
        .stages()
        .into_iter()
        .filter(|(_, s)| s == "review")
        .map(|(m, _)| m)
        .collect()
}

const NO_FRONTIER: &str =
    "reviewer: standard (no frontier reviewer from another provider is configured)";

fn gate_stages(stages: &[provefab::store::StageRunRecord]) -> Vec<String> {
    stages.iter().map(|r| r.stage.clone()).collect()
}

#[tokio::test]
async fn a_migration_is_classified_checked_reviewed_on_frontier_and_labelled() {
    let mut f = fixture(&["test -f feature.txt"]);
    with_codex_frontier(&mut f);
    f.config.repos[0].risk = migration_checks(r#"["test -f migrations/0005.sql"]"#);
    let label = format!("{}:risk-migrations", f.config.repos[0].label);
    let p = pipeline(
        &f,
        Box::new(risky),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let events = classified(&p, id).await;
    assert_eq!(
        events.last().unwrap(),
        &vec![Detected {
            name: "migrations".into(),
            paths: vec!["migrations/0005.sql".into()],
        }]
    );
    let stages = gate_stages(&p.store.stage_runs(id).await.unwrap());
    assert!(stages.contains(&"risk-gates".to_string()), "{stages:?}");
    assert_eq!(reviewers(&p), ["top-codex"], "{:?}", p.runner.stages());
    assert!(p.hub.ensured.lock().unwrap().contains(&label));
    let edits = p.hub.labels.lock().unwrap().clone();
    assert!(
        edits.iter().any(|(add, _)| add.contains(&label)),
        "{edits:?}"
    );
}

#[tokio::test]
async fn a_failing_risk_check_goes_back_to_implementation() {
    let mut f = fixture(&["test -f feature.txt"]);
    f.config.repos[0].risk = migration_checks(r#"["false"]"#);
    let oracle = FakeOracle {
        triage: Some(Triage::RealBug),
        ..Default::default()
    };
    let p = pipeline(&f, Box::new(risky), oracle, FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), Failed);
    let implements = p
        .runner
        .stages()
        .into_iter()
        .filter(|(_, s)| s == "implement")
        .count();
    assert_eq!(implements, 3);
    assert!(!p.runner.stages().iter().any(|(_, s)| s == "review"));
    let prompts: Vec<String> = p.runner.calls().into_iter().map(|c| c.2).collect();
    assert!(prompts[2].contains("gate `false` failed"), "{}", prompts[2]);
}

/// O1 (owner decision 2026-09-30): the only frontier model shares the
/// implementer's provider, so the review keeps its standard cross-provider
/// reviewer and the PR says so.
#[tokio::test]
async fn a_same_provider_frontier_keeps_the_cross_provider_reviewer() {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(
        &f,
        Box::new(risky),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let implementers: Vec<_> = p
        .runner
        .stages()
        .into_iter()
        .filter(|(_, s)| s == "implement")
        .collect();
    assert_eq!(implementers[0].0, "std-claude", "{implementers:?}");
    assert_eq!(reviewers(&p), ["std-codex"]);
    let body = p.hub.prs.lock().unwrap().last().unwrap().3.clone();
    assert!(
        body.contains(&format!(
            "- migrations: `migrations/0005.sql` · {NO_FRONTIER}\n"
        )),
        "{body}"
    );
}

#[tokio::test]
async fn no_frontier_model_keeps_the_cross_provider_reviewer() {
    let mut f = fixture(&["test -f feature.txt"]);
    f.config
        .models
        .retain(|m| m.tier != provefab::task::Tier::Frontier);
    let p = pipeline(
        &f,
        Box::new(risky),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    assert_eq!(reviewers(&p), ["std-codex"]);
    let body = p.hub.prs.lock().unwrap().last().unwrap().3.clone();
    assert!(body.contains(NO_FRONTIER), "{body}");
}

#[tokio::test]
async fn a_rename_out_of_a_risky_path_still_counts() {
    let f = fixture(&["true"]);
    let local = f.config.repos[0].path_in(&f.home);
    std::fs::create_dir_all(local.join("migrations")).unwrap();
    std::fs::write(
        local.join("migrations/0001.sql"),
        "create table a(x int);\n",
    )
    .unwrap();
    git(&local, &["add", "migrations/0001.sql"]);
    git(&local, &["commit", "-qm", "migration"]);
    git(&local, &["push", "-q", "origin", "main"]);
    let script =
        |m: &ModelEntry, req: &StageRequest, tx: &UnboundedSender<WorkerEvent>| match stage_of(
            &req.prompt,
        ) {
            "implement" => {
                std::fs::create_dir_all(req.cwd.join("docs")).unwrap();
                std::fs::rename(
                    req.cwd.join("migrations/0001.sql"),
                    req.cwd.join("docs/old.sql"),
                )
                .unwrap();
                done(None)
            }
            _ => happy(m, req, tx),
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
    let last = classified(&p, id).await.pop().unwrap();
    let migrations = last.iter().find(|d| d.name == "migrations").unwrap();
    assert!(
        migrations
            .paths
            .contains(&"migrations/0001.sql".to_string()),
        "{last:?}"
    );
}

#[tokio::test]
async fn a_risk_check_already_in_gates_runs_once() {
    let mut f = fixture(&["test -f feature.txt"]);
    f.config.repos[0].risk = migration_checks(r#"["test -f feature.txt"]"#);
    let p = pipeline(
        &f,
        Box::new(risky),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let stages = gate_stages(&p.store.stage_runs(id).await.unwrap());
    assert!(!stages.contains(&"risk-gates".to_string()), "{stages:?}");
    assert_eq!(stages.iter().filter(|s| *s == "gates").count(), 1);
}

#[tokio::test]
async fn a_later_round_without_the_category_removes_its_label() {
    let mut f = fixture(&["test -f feature.txt"]);
    with_codex_frontier(&mut f);
    let label = format!("{}:risk-migrations", f.config.repos[0].label);
    let implements = Mutex::new(0);
    let reviews = Mutex::new(0);
    let script = move |m: &ModelEntry, req: &StageRequest, tx: &UnboundedSender<WorkerEvent>| {
        match stage_of(&req.prompt) {
            "implement" => {
                let mut n = implements.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    risky(m, req, tx)
                } else {
                    std::fs::remove_file(req.cwd.join("migrations/0005.sql")).unwrap();
                    done(None)
                }
            }
            "review" => {
                let mut r = reviews.lock().unwrap();
                *r += 1;
                if *r == 1 {
                    done(Some(json!({"verdict": "changes", "findings": [
                        {"file": "migrations/0005.sql", "line": 1, "severity": "blocking", "text": "no migration"}
                    ]})))
                } else {
                    done(Some(approve()))
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
    let events = classified(&p, id).await;
    assert_eq!(events.last().unwrap(), &Vec::<Detected>::new());
    let edits = p.hub.labels.lock().unwrap().clone();
    let risk_edits: Vec<_> = edits
        .iter()
        .filter(|(a, r)| a.contains(&label) || r.contains(&label))
        .collect();
    assert!(risk_edits.last().unwrap().1.contains(&label), "{edits:?}");
    let reviewers = reviewers(&p);
    assert_eq!(reviewers.len(), 2, "{reviewers:?}");
    assert_eq!(reviewers[0], "top-codex");
    assert_ne!(reviewers[1], "top-codex");
}

#[tokio::test]
async fn a_risk_label_that_could_not_be_created_is_left_out_of_the_edit() {
    let f = fixture(&["test -f feature.txt"]);
    let label = format!("{}:risk-migrations", f.config.repos[0].label);
    let hub = FakeHub::new("x");
    hub.ensure_fails.lock().unwrap().push(label.clone());
    let p = pipeline(&f, Box::new(risky), FakeOracle::default(), hub).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let edits = p.hub.labels.lock().unwrap().clone();
    assert!(
        !edits
            .iter()
            .any(|(a, r)| a.contains(&label) || r.contains(&label)),
        "{edits:?}"
    );
    // Later GitHub effects are not held behind a label edit that cannot succeed.
    let in_pr = format!("{}:in-pr", f.config.repos[0].label);
    assert!(edits.iter().any(|(a, _)| a.contains(&in_pr)), "{edits:?}");
}

#[tokio::test]
async fn a_same_round_reclassification_does_not_relabel() {
    let mut f = fixture(&["test -f feature.txt"]);
    let flag = f.home.with_file_name("risk-check-ran");
    let check = format!(
        "[\"test -e {0} || {{ touch {0}; false; }}\"]",
        flag.display()
    );
    f.config.repos[0].risk = migration_checks(&check);
    let label = format!("{}:risk-migrations", f.config.repos[0].label);
    let oracle = FakeOracle {
        triage: Some(Triage::RealBug),
        ..Default::default()
    };
    let p = pipeline(&f, Box::new(risky), oracle, FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    assert_eq!(classified(&p, id).await.len(), 2);
    let edits = p.hub.labels.lock().unwrap().clone();
    let risk_edits = edits
        .iter()
        .filter(|(a, r)| a.contains(&label) || r.contains(&label))
        .count();
    assert_eq!(risk_edits, 1, "{edits:?}");
}

/// I4: the agent commits its own work, then the base commit object goes
/// missing, so `changed_files(base...HEAD)` fails right after the round's
/// commit. The review's own diff then fails too (the task waits); the
/// object is put back and the task retried at once.
#[tokio::test]
async fn changed_files_that_cannot_be_computed_are_unknown() {
    for (codex_frontier, reviewer, line) in [
        (false, "std-codex", NO_FRONTIER),
        (true, "top-codex", "reviewer: frontier"),
    ] {
        let mut f = fixture(&["test -f feature.txt"]);
        if codex_frontier {
            with_codex_frontier(&mut f);
        }
        let hidden: std::sync::Arc<Mutex<Option<(PathBuf, PathBuf)>>> = Default::default();
        let aside = hidden.clone();
        let script =
            move |m: &ModelEntry, req: &StageRequest, tx: &UnboundedSender<WorkerEvent>| {
                if stage_of(&req.prompt) != "implement" {
                    return happy(m, req, tx);
                }
                let base = git(&req.cwd, &["rev-parse", "HEAD"]);
                let common = req
                    .cwd
                    .join(git(&req.cwd, &["rev-parse", "--git-common-dir"]));
                std::fs::write(req.cwd.join("feature.txt"), "done\n").unwrap();
                git(&req.cwd, &["add", "-A"]);
                git(&req.cwd, &["commit", "-qm", "agent"]);
                let object = common.join("objects").join(&base[..2]).join(&base[2..]);
                let moved = common.join("base-object");
                std::fs::rename(&object, &moved).unwrap();
                *aside.lock().unwrap() = Some((moved, object));
                done(None)
            };
        let p = pipeline(
            &f,
            Box::new(script),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await;
        let id = queue(&p).await;
        let mut state = p.drive(id).await.unwrap();
        assert_eq!(classified(&p, id).await, vec![provefab::risk::unknown()]);
        if let Some((moved, object)) = hidden.lock().unwrap().take() {
            std::fs::rename(moved, object).unwrap();
        }
        if state == Waiting {
            // The review's diff failed on the missing object: retry now.
            p.store
                .record_output(id, "transient", &json!({"pass": 1, "at": 0}))
                .await
                .unwrap();
            state = p.drive(id).await.unwrap();
        }
        assert_eq!(state, PrOpen);
        assert_eq!(reviewers(&p), [reviewer]);
        let body = p.hub.prs.lock().unwrap().last().unwrap().3.clone();
        assert!(
            body.contains(&format!(
                "- unknown: the changed files could not be computed · {line}\n"
            )),
            "{body}"
        );
    }
}

fn headers(body: &str) -> Vec<&str> {
    body.lines().filter(|l| l.starts_with("## ")).collect()
}

#[tokio::test]
async fn the_pr_body_lists_the_risk_after_the_checks() {
    let mut f = fixture(&["test -f feature.txt"]);
    with_codex_frontier(&mut f);
    f.config.repos[0].risk = migration_checks(r#"["test -f migrations/0005.sql"]"#);
    let p = pipeline(
        &f,
        Box::new(risky),
        FakeOracle::default(),
        FakeHub::new("x"),
    )
    .await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let body = p.hub.prs.lock().unwrap().last().unwrap().3.clone();
    assert!(
        body.contains(
            "## Risk\n\n- migrations: `migrations/0005.sql` · checks added: `test -f migrations/0005.sql` · reviewer: frontier\n"
        ),
        "{body}"
    );
    let h = headers(&body);
    let pos = |n: &str| h.iter().position(|x| *x == n).unwrap();
    assert_eq!(pos("## Risk"), pos("## Checks") + 1, "{h:?}");
}

#[tokio::test]
async fn pr_body_is_unchanged_without_risk() {
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
    let body = p.hub.prs.lock().unwrap().last().unwrap().3.clone();
    assert!(!body.contains("Risk"), "{body}");
    assert_eq!(
        headers(&body),
        vec!["## Plan", "## Routing", "## Checks"],
        "{body}"
    );
}
