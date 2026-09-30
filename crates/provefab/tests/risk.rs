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

fn gate_stages(stages: &[provefab::store::StageRunRecord]) -> Vec<String> {
    stages.iter().map(|r| r.stage.clone()).collect()
}

#[tokio::test]
async fn a_migration_is_classified_checked_reviewed_on_frontier_and_labelled() {
    let mut f = fixture(&["test -f feature.txt"]);
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
    assert!(
        p.runner
            .stages()
            .contains(&("top-claude".to_string(), "review".to_string())),
        "{:?}",
        p.runner.stages()
    );
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

#[tokio::test]
async fn no_frontier_model_parks_the_task() {
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
    assert_eq!(p.drive(id).await.unwrap(), NeedsYou);
    let posted = p.hub.posted.lock().unwrap().clone();
    assert!(
        posted
            .last()
            .unwrap()
            .contains("requires a frontier reviewer"),
        "{posted:?}"
    );
    assert!(!p.runner.stages().iter().any(|(_, s)| s == "review"));
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
    let f = fixture(&["test -f feature.txt"]);
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
    let reviewers: Vec<String> = p
        .runner
        .stages()
        .into_iter()
        .filter(|(_, s)| s == "review")
        .map(|(m, _)| m)
        .collect();
    assert_eq!(reviewers.len(), 2, "{reviewers:?}");
    assert_eq!(reviewers[0], "top-claude");
    assert_ne!(reviewers[1], "top-claude");
}
