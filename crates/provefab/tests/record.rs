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
    for want in ["stage_run", "plan", "gates_run"] {
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
