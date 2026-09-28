use std::time::Duration;

use jev::{JevClient, JevError, Question, Questions};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> JevClient {
    JevClient::new("test-key", "jev-1.13", Duration::from_millis(500))
        .unwrap()
        .with_base_url(server.uri())
}

fn urgent() -> Questions {
    let mut q = Questions::new();
    q.insert(
        "is_urgent".into(),
        Question::noul("Does this convey urgency?"),
    );
    q
}

fn ok_body() -> serde_json::Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {"is_urgent": {"type": "noul", "noul": 0.95}},
        "usage": {"input_tokens": 296, "output_tokens": 20}
    })
}

#[tokio::test]
async fn sends_documented_request_and_parses_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_json(json!({
            "model": "jev-1.13",
            "state": "Help! My payouts have been failing for 3 days.",
            "questions": {"is_urgent": {"type": "noul", "instructions": "Does this convey urgency?"}}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;

    let resp = client(&server)
        .evaluate(
            &json!("Help! My payouts have been failing for 3 days."),
            &urgent(),
        )
        .await
        .unwrap();

    assert_eq!(resp.model, "jev-1.13.0");
    assert_eq!(resp.noul("is_urgent").unwrap(), 0.95);
}

#[tokio::test]
async fn retries_429_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .mount(&server)
        .await;

    let resp = client(&server)
        .evaluate(&json!("x"), &urgent())
        .await
        .unwrap();
    assert_eq!(resp.noul("is_urgent").unwrap(), 0.95);
}

#[tokio::test]
async fn persistent_529_gives_rate_limited_after_three_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529))
        .expect(3)
        .mount(&server)
        .await;

    let err = client(&server)
        .evaluate(&json!("x"), &urgent())
        .await
        .unwrap_err();
    assert!(matches!(err, JevError::RateLimited), "{err:?}");
}

#[tokio::test]
async fn maps_401_and_422() {
    let server = MockServer::start().await;
    Mock::given(header("authorization", "Bearer bad"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(header("authorization", "Bearer test-key"))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_string(r#"{"error":"questions.x.criteria required"}"#),
        )
        .mount(&server)
        .await;

    let bad = JevClient::new("bad", "jev-1.13", Duration::from_millis(500))
        .unwrap()
        .with_base_url(server.uri());
    assert!(matches!(
        bad.evaluate(&json!("x"), &urgent()).await,
        Err(JevError::Unauthorized)
    ));

    let err = client(&server)
        .evaluate(&json!("x"), &urgent())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, JevError::Invalid(body) if body.contains("criteria required")),
        "{err:?}"
    );
}

#[tokio::test]
async fn slow_server_times_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ok_body())
                .set_delay(Duration::from_secs(2)),
        )
        .mount(&server)
        .await;

    let err = client(&server)
        .evaluate(&json!("x"), &urgent())
        .await
        .unwrap_err();
    assert!(matches!(err, JevError::Timeout), "{err:?}");
}

#[tokio::test]
async fn garbage_200_body_is_a_decode_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>oops</html>"))
        .mount(&server)
        .await;

    let err = client(&server)
        .evaluate(&json!("x"), &urgent())
        .await
        .unwrap_err();
    assert!(matches!(err, JevError::Decode(_)), "{err:?}");
}
