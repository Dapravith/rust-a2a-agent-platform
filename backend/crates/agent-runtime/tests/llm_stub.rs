//! The hosted-provider adapter against a local stub of the Messages API.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use agent_runtime::{LlmClient, LlmConfig, LlmError};
use axum::{Json, http::HeaderMap, routing::post};
use contracts::DraftArtifact;
use serde_json::json;

async fn stub(status: u16, body: serde_json::Value) -> String {
    let app = axum::Router::new().route(
        "/v1/messages",
        post(
            move |headers: HeaderMap, Json(req): Json<serde_json::Value>| {
                let body = body.clone();
                async move {
                    assert_eq!(headers["x-api-key"], "stub-key");
                    assert_eq!(headers["anthropic-version"], "2026-10-21");
                    assert_eq!(req["output_config"]["format"]["type"], "json_schema");
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        Json(body),
                    )
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn client(base_url: String) -> LlmClient {
    LlmClient::new(config(base_url)).unwrap()
}

fn config(base_url: String) -> LlmConfig {
    LlmConfig {
        api_key: "stub-key".into(),
        model: "claude-opus-5-5".into(),
        base_url,
        timeout: Duration::from_secs(5),
        deadline: Duration::from_secs(10),
        max_tokens: 16_000,
        effort: "medium".into(),
        refusal_fallback: true,
    }
}

#[tokio::test]
async fn structured_output_round_trip() {
    let text = json!({"title":"T","summary":"S","content":"C","assumptions":[]}).to_string();
    let base = stub(
        200,
        json!({"stop_reason":"end_turn","content":[{"type":"text","text":text}]}),
    )
    .await;
    let draft: DraftArtifact = client(base)
        .generate("sys", "user", json!({}))
        .await
        .unwrap();
    assert_eq!(draft.title, "T");
}

#[tokio::test]
async fn provider_error_is_typed_and_bounded() {
    let base = stub(
        400,
        json!({"error":{"type":"invalid_request_error","message":"bad model"}}),
    )
    .await;
    let err = client(base)
        .generate::<DraftArtifact>("sys", "user", json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Provider { status: 400, ref message } if message == "bad model")
    );
    assert!(!err.to_string().contains("stub-key"));
}

/// REVIEW-005: retries must never outlast one overall deadline, which is
/// what keeps an agent inside the API's AGENT_TIMEOUT_SECS.
#[tokio::test]
async fn retries_stop_at_overall_deadline() {
    let app = axum::Router::new().route(
        "/v1/messages",
        post(|| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Json(json!({}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut cfg = config(base);
    cfg.timeout = Duration::from_secs(1); // per attempt
    cfg.deadline = Duration::from_secs(2); // all attempts
    let started = std::time::Instant::now();
    let err = LlmClient::new(cfg)
        .unwrap()
        .generate::<DraftArtifact>("sys", "user", json!({}))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(2_600),
        "took {elapsed:?}: {err}"
    );
    assert!(matches!(err, LlmError::Deadline(_)), "{err}");
}
