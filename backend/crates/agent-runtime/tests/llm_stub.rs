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
    LlmClient::new(LlmConfig {
        api_key: "stub-key".into(),
        model: "claude-opus-5-5".into(),
        base_url,
        timeout: Duration::from_secs(5),
        max_tokens: 16_000,
        effort: "medium".into(),
        refusal_fallback: true,
    })
    .unwrap()
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
