//! Provefab's Jev question sets (spec §4). Every call has a total deadline
//! and a caller-side fallback; Jev can route, stop, retry or requeue, but it
//! never approves anything (D8).

use std::time::Duration;

use agent_workers::WorkerEvent;
use jev::{JevClient, JevError, Question, Questions, Response};
use serde_json::{Value, json};

use crate::task::{TaskKind, Verdict};

/// Whole-call budget, retries included (spec §4: "times out (2s)").
pub const DEADLINE: Duration = Duration::from_secs(2);
const BODY_LIMIT: usize = 20_000;
const LOOP_WINDOW: usize = 30;
const TRIAGE_TAIL_LINES: usize = 200;

/// What classification sees about an issue (spec §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueContext {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub repo_language: Option<String>,
    pub repo_size_kb: Option<u64>,
}

async fn ask(
    client: &JevClient,
    deadline: Duration,
    state: &Value,
    questions: &Questions,
) -> Result<Response, JevError> {
    tokio::time::timeout(deadline, client.evaluate(state, questions))
        .await
        .map_err(|_| JevError::Timeout)?
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

pub fn classify_questions() -> Questions {
    let mut q = Questions::new();
    q.insert(
        "task_kind".into(),
        Question::choice(
            "What kind of change does this issue ask for?",
            [
                ("bugfix", "Something that worked, or should work, is broken"),
                ("feature", "New behaviour or capability"),
                ("refactor", "Restructure code without changing behaviour"),
                ("docs", "Documentation only"),
                ("test", "Add or fix tests only"),
                ("chore", "Build, dependencies, tooling or configuration"),
            ],
        ),
    );
    q.insert(
        "difficulty".into(),
        Question::score(
            "How hard is this for a capable engineer who does not know the codebase?",
            [
                "Trivial: a one-line or mechanical change",
                "Easy: a small, well-understood change",
                "Moderate: some design choices or several files",
                "Hard: subtle logic, concurrency, or broad impact",
                "Very hard: deep redesign or research needed",
            ],
        ),
    );
    q.insert(
        "scope".into(),
        Question::score(
            "How much of the codebase will the change touch?",
            [
                "One file",
                "A few files in one area",
                "Several modules",
                "Architectural: cross-cutting structure or interfaces",
            ],
        ),
    );
    q.insert(
        "underspecified".into(),
        Question::noul("Is information needed to implement this missing from the issue?"),
    );
    q
}

/// Classifies an issue once, at intake. On error the caller routes with
/// `router::fallback_tiers()` (spec §4.1).
pub async fn classify(
    client: &JevClient,
    issue: &IssueContext,
    deadline: Duration,
) -> Result<Verdict, JevError> {
    let state = json!({
        "title": issue.title,
        "body": truncate(&issue.body, BODY_LIMIT),
        "labels": issue.labels,
        "repo_language": issue.repo_language,
        "repo_size_kb": issue.repo_size_kb,
    });
    let r = ask(client, deadline, &state, &classify_questions()).await?;
    verdict_from(&r)
}

fn verdict_from(r: &Response) -> Result<Verdict, JevError> {
    let kind = r.choice("task_kind")?;
    let difficulty = r.score("difficulty")?;
    Ok(Verdict {
        task_kind: TaskKind::parse(&kind.choice).ok_or_else(|| JevError::WrongType {
            id: "task_kind".into(),
            expected: "known task kind",
        })?,
        difficulty: difficulty.score,
        difficulty_confidence: difficulty.confidence,
        scope: r.score("scope")?.score,
        underspecified: r.noul("underspecified")?,
        jev_model: r.model.clone(),
    })
}

/// Probability that the agent is stuck, from its last tool events (spec §4.3).
pub async fn loop_probability(
    client: &JevClient,
    events: &[WorkerEvent],
    deadline: Duration,
) -> Result<f64, JevError> {
    let recent: Vec<Value> = events
        .iter()
        .filter_map(|e| match e {
            WorkerEvent::ToolStart { name, input } => Some(json!({"call": name, "input": input})),
            WorkerEvent::ToolEnd { name, is_error } => {
                Some(json!({"result": name, "error": is_error}))
            }
            _ => None,
        })
        .rev()
        .take(LOOP_WINDOW)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut q = Questions::new();
    q.insert(
        "stuck".into(),
        Question::noul("Is the agent stuck, repeating actions, or making no progress?"),
    );
    let r = ask(
        client,
        deadline,
        &json!({ "recent_tool_events": recent }),
        &q,
    )
    .await?;
    r.noul("stuck")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Triage {
    RealBug,
    FlakyTest,
    EnvProblem,
    MissingDependency,
    OutOfScope,
}

impl Triage {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "real_bug" => Some(Triage::RealBug),
            "flaky_test" => Some(Triage::FlakyTest),
            "env_problem" => Some(Triage::EnvProblem),
            "missing_dependency" => Some(Triage::MissingDependency),
            "out_of_scope" => Some(Triage::OutOfScope),
            _ => None,
        }
    }
}

/// Why a gate failed (spec §4.4). On error the caller treats it as "unknown":
/// retry once, then `NeedsYou`.
pub async fn triage(
    client: &JevClient,
    command: &str,
    output_tail: &str,
    deadline: Duration,
) -> Result<Triage, JevError> {
    let lines: Vec<&str> = output_tail.lines().collect();
    let tail = lines[lines.len().saturating_sub(TRIAGE_TAIL_LINES)..].join("\n");
    let mut q = Questions::new();
    q.insert(
        "cause".into(),
        Question::choice(
            "Why did this check fail?",
            [
                ("real_bug", "The code under test is wrong"),
                (
                    "flaky_test",
                    "A test failed for timing or randomness reasons unrelated to the change",
                ),
                (
                    "env_problem",
                    "The toolchain, machine or environment is broken, not the code",
                ),
                (
                    "missing_dependency",
                    "A package, crate or tool the change needs is not installed or declared",
                ),
                (
                    "out_of_scope",
                    "Fixing it needs work outside what the issue asks for",
                ),
            ],
        ),
    );
    let r = ask(
        client,
        deadline,
        &json!({ "command": command, "output": tail }),
        &q,
    )
    .await?;
    let c = r.choice("cause")?;
    Triage::parse(&c.choice).ok_or_else(|| JevError::WrongType {
        id: "cause".into(),
        expected: "known triage cause",
    })
}

/// Probability that a reply answers Provefab's open question (spec §4.5).
pub async fn reply_answers(
    client: &JevClient,
    question: &str,
    reply: &str,
    deadline: Duration,
) -> Result<f64, JevError> {
    let mut q = Questions::new();
    q.insert(
        "answers".into(),
        Question::noul("Does the reply provide the information the question asked for?"),
    );
    let r = ask(
        client,
        deadline,
        &json!({ "question": question, "reply": truncate(reply, BODY_LIMIT) }),
        &q,
    )
    .await?;
    r.noul("answers")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> JevClient {
        JevClient::new("k", "jev-1.13", Duration::from_secs(5))
            .unwrap()
            .with_base_url(server.uri())
    }

    fn issue() -> IssueContext {
        IssueContext {
            title: "Crash on empty config".into(),
            body: "x".repeat(30_000),
            labels: vec!["provefab".into()],
            repo_language: Some("Rust".into()),
            repo_size_kb: Some(900),
        }
    }

    #[tokio::test]
    async fn classify_asks_the_four_questions_and_builds_a_verdict() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "questions": {
                    "task_kind": {"type": "choice"},
                    "difficulty": {"type": "score"},
                    "scope": {"type": "score"},
                    "underspecified": {"type": "noul"}
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "jev-1.13.0",
                "answers": {
                    "task_kind": {"type": "choice", "choice": "bugfix", "probabilities": {"bugfix": 0.9}, "confidence": 0.8},
                    "difficulty": {"type": "score", "score": 1.2, "probabilities": {}, "confidence": 0.7},
                    "scope": {"type": "score", "score": 0.4, "probabilities": {}, "confidence": 0.9},
                    "underspecified": {"type": "noul", "noul": 0.1}
                },
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let v = classify(&client(&server), &issue(), DEADLINE)
            .await
            .unwrap();
        assert_eq!(
            v,
            Verdict {
                task_kind: TaskKind::Bugfix,
                difficulty: 1.2,
                difficulty_confidence: 0.7,
                scope: 0.4,
                underspecified: 0.1,
                jev_model: "jev-1.13.0".into(),
            }
        );
        let sent: Value =
            serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
        assert_eq!(sent["state"]["body"].as_str().unwrap().len(), BODY_LIMIT);
    }

    #[tokio::test]
    async fn an_unknown_task_kind_is_an_error_so_the_caller_falls_back() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "jev-1.13.0",
                "answers": {
                    "task_kind": {"type": "choice", "choice": "epic", "probabilities": {}, "confidence": 0.8},
                    "difficulty": {"type": "score", "score": 1.0, "probabilities": {}, "confidence": 0.7},
                    "scope": {"type": "score", "score": 0.0, "probabilities": {}, "confidence": 0.9},
                    "underspecified": {"type": "noul", "noul": 0.1}
                },
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .mount(&server)
            .await;
        assert!(
            classify(&client(&server), &issue(), DEADLINE)
                .await
                .is_err()
        );
    }

    /// Plan 1 review M4: the deadline covers the whole call, retries included.
    #[tokio::test]
    async fn the_deadline_covers_retries() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(529))
            .mount(&server)
            .await;
        let started = std::time::Instant::now();
        let err = classify(&client(&server), &issue(), Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(matches!(err, JevError::Timeout), "{err:?}");
        // Generous bound: proves the deadline (not retry exhaustion or the
        // request's own timeout) fired, without flaking under machine load.
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    fn noul(id: &str, p: f64) -> Value {
        json!({
            "model": "jev-1.13.0",
            "answers": { id: {"type": "noul", "noul": p} },
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })
    }

    #[tokio::test]
    async fn loop_detector_sends_only_the_last_tool_events() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(noul("stuck", 0.93)))
            .mount(&server)
            .await;
        let mut events = vec![WorkerEvent::Text("thinking".into())];
        for i in 0..40 {
            events.push(WorkerEvent::ToolStart {
                name: "Bash".into(),
                input: format!("cargo test {i}"),
            });
        }
        let p = loop_probability(&client(&server), &events, DEADLINE)
            .await
            .unwrap();
        assert_eq!(p, 0.93);
        let sent: Value =
            serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
        let recent = sent["state"]["recent_tool_events"].as_array().unwrap();
        assert_eq!(recent.len(), LOOP_WINDOW);
        assert_eq!(recent.last().unwrap()["input"], "cargo test 39");
    }

    #[tokio::test]
    async fn triage_maps_the_cause_and_reply_check_returns_the_probability() {
        let server = MockServer::start().await;
        Mock::given(body_partial_json(json!({"questions": {"cause": {"type": "choice"}}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "jev-1.13.0",
                "answers": {"cause": {"type": "choice", "choice": "env_problem", "probabilities": {}, "confidence": 0.9}},
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .mount(&server)
            .await;
        Mock::given(body_partial_json(
            json!({"questions": {"answers": {"type": "noul"}}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(noul("answers", 0.81)))
        .mount(&server)
        .await;
        let c = client(&server);
        assert_eq!(
            triage(&c, "cargo test", "linker `cc` not found", DEADLINE)
                .await
                .unwrap(),
            Triage::EnvProblem
        );
        assert_eq!(
            reply_answers(&c, "Which version?", "v2.3.1", DEADLINE)
                .await
                .unwrap(),
            0.81
        );
    }
}
