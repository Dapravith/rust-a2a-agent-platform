//! The hosted-provider adapter against a controlled local HTTP test double
//! of the Messages API: success, HTTP errors, rate limits, timeouts,
//! refusals, malformed and oversized output, and secret redaction.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use agent_runtime::llm::MAX_RESPONSE_BYTES;
use agent_runtime::{LlmClient, LlmConfig, LlmError};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use contracts::{AgentFailureCode, DraftArtifact};
use serde_json::{Value, json};

const KEY: &str = "sk-stub-0123456789abcdef";

type Reply = Arc<dyn Fn(usize) -> Response + Send + Sync>;

/// Serves `reply(n)` for the n-th request (0-based); returns base URL and
/// the request counter. Asserts the wire contract on every request.
async fn stub(reply: Reply) -> (String, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let app = axum::Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap, body: String| {
            let (reply, c) = (reply.clone(), c.clone());
            async move {
                assert_eq!(headers["x-api-key"], KEY);
                assert_eq!(headers["anthropic-version"], "2026-10-21");
                assert_eq!(headers["anthropic-beta"], "server-side-fallback-2026-07-01");
                let req: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(req["output_config"]["format"]["type"], "json_schema");
                assert_eq!(req["fallbacks"], "default");
                let n = c.fetch_add(1, Ordering::SeqCst);
                reply(n)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, count)
}

fn json_reply(status: u16, body: Value) -> Response {
    (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response()
}

fn text_message(text: &str) -> Value {
    json!({"stop_reason":"end_turn","content":[{"type":"text","text":text}]})
}

fn good_draft() -> String {
    json!({"title":"T","summary":"S","content":"C","assumptions":[]}).to_string()
}

fn config(base_url: String) -> LlmConfig {
    LlmConfig {
        api_key: KEY.into(),
        model: "claude-opus-5-5".into(),
        base_url,
        timeout: Duration::from_secs(5),
        deadline: Duration::from_secs(10),
        max_tokens: 16_000,
        effort: "medium".into(),
        refusal_fallback: true,
    }
}

async fn generate(cfg: LlmConfig) -> Result<DraftArtifact, LlmError> {
    LlmClient::new(cfg)
        .unwrap()
        .generate::<DraftArtifact>("sys", "user", json!({}))
        .await
}

#[tokio::test]
async fn structured_output_round_trip() {
    let (base, count) = stub(Arc::new(|_| json_reply(200, text_message(&good_draft())))).await;
    let draft = generate(config(base)).await.unwrap();
    assert_eq!(draft.title, "T");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn provider_error_is_typed_bounded_and_scrubbed() {
    let (base, count) = stub(Arc::new(|_| {
        json_reply(
            400,
            json!({"error":{"type":"invalid_request_error",
                "message": format!("bad key {KEY} {}", "x".repeat(1000))}}),
        )
    }))
    .await;
    let err = generate(config(base)).await.unwrap_err();
    let LlmError::Provider { status, message } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*status, 400);
    assert!(message.chars().count() <= 300);
    assert!(message.contains("<redacted>"));
    assert!(!err.to_string().contains(KEY), "{err}");
    assert_eq!(err.code(), AgentFailureCode::LlmProviderError);
    assert_eq!(count.load(Ordering::SeqCst), 1, "4xx must not be retried");
}

#[tokio::test]
async fn rate_limit_is_retried_then_succeeds() {
    let (base, count) = stub(Arc::new(|n| {
        if n == 0 {
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "0")],
                axum::Json(json!({"error":{"type":"rate_limit_error","message":"slow"}})),
            )
                .into_response()
        } else {
            json_reply(200, text_message(&good_draft()))
        }
    }))
    .await;
    generate(config(base)).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn persistent_rate_limit_reports_stable_code() {
    let (base, count) = stub(Arc::new(|_| {
        json_reply(
            529,
            json!({"error":{"type":"overloaded_error","message":"overloaded"}}),
        )
    }))
    .await;
    let err = generate(config(base)).await.unwrap_err();
    assert!(
        matches!(err, LlmError::RateLimited { status: 529, .. }),
        "{err:?}"
    );
    assert_eq!(err.code(), AgentFailureCode::LlmRateLimited);
    assert_eq!(count.load(Ordering::SeqCst), 3, "bounded attempts");
}

/// A 500 may have been processed (cost spent): reported, not retried.
#[tokio::test]
async fn uncertain_server_error_is_not_retried() {
    let (base, count) = stub(Arc::new(|_| {
        json_reply(500, json!({"error":{"type":"api_error","message":"boom"}}))
    }))
    .await;
    let err = generate(config(base)).await.unwrap_err();
    assert!(matches!(err, LlmError::Provider { status: 500, .. }));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

/// A read timeout is uncertain too: one attempt, typed timeout.
#[tokio::test]
async fn read_timeout_is_typed_and_not_retried() {
    let count = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        "/v1/messages",
        post({
            let count = count.clone();
            move || {
                count.fetch_add(1, Ordering::SeqCst);
                async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    axum::Json(json!({}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut cfg = config(base);
    cfg.timeout = Duration::from_secs(1);
    let started = Instant::now();
    let err = generate(cfg).await.unwrap_err();
    assert!(matches!(err, LlmError::Timeout), "{err:?}");
    assert_eq!(err.code(), AgentFailureCode::LlmTimeout);
    assert!(started.elapsed() < Duration::from_millis(1_800));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

/// REVIEW-005: retries must never outlast one overall deadline, which is
/// what keeps an agent inside the API's AGENT_TIMEOUT_SECS.
#[tokio::test]
async fn retries_stop_at_overall_deadline() {
    let (base, _) = stub(Arc::new(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "5")],
            axum::Json(json!({"error":{"type":"rate_limit_error","message":"slow"}})),
        )
            .into_response()
    }))
    .await;
    let mut cfg = config(base);
    cfg.timeout = Duration::from_secs(1);
    cfg.deadline = Duration::from_secs(2);
    let started = Instant::now();
    let err = generate(cfg).await.unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_millis(2_600), "took {elapsed:?}");
    assert!(matches!(err, LlmError::Deadline(_)), "{err}");
    assert_eq!(err.code(), AgentFailureCode::LlmTimeout);
}

#[tokio::test]
async fn refusal_and_truncation_are_typed() {
    let (base, _) = stub(Arc::new(|_| {
        json_reply(
            200,
            json!({"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"},"content":[]}),
        )
    }))
    .await;
    let err = generate(config(base)).await.unwrap_err();
    assert_eq!(err.code(), AgentFailureCode::LlmRefused);

    let (base, _) = stub(Arc::new(|_| {
        json_reply(
            200,
            json!({"stop_reason":"max_tokens","content":[{"type":"text","text":"{\"title\":"}]}),
        )
    }))
    .await;
    let err = generate(config(base)).await.unwrap_err();
    assert_eq!(err.code(), AgentFailureCode::LlmOutputTruncated);
}

#[tokio::test]
async fn malformed_and_contract_violating_output_is_rejected() {
    for reply in [
        // Not JSON at all.
        Arc::new(|_| (StatusCode::OK, "<html>gateway</html>").into_response()) as Reply,
        // JSON text, wrong shape.
        Arc::new(|_| json_reply(200, text_message("{\"answer\": 42}"))),
        // Valid shape, fails validation (empty content).
        Arc::new(|_| {
            json_reply(
                200,
                text_message(
                    &json!({"title":"T","summary":"S","content":"   ","assumptions":[]})
                        .to_string(),
                ),
            )
        }),
        // No text block.
        Arc::new(|_| {
            json_reply(
                200,
                json!({"stop_reason":"end_turn","content":[{"type":"thinking","thinking":""}]}),
            )
        }),
    ] {
        let (base, _) = stub(reply).await;
        let err = generate(config(base)).await.unwrap_err();
        assert_eq!(err.code(), AgentFailureCode::LlmOutputInvalid, "{err:?}");
    }
}

#[tokio::test]
async fn oversized_response_is_rejected() {
    let (base, _) = stub(Arc::new(|_| {
        let huge = "a".repeat(MAX_RESPONSE_BYTES + 1);
        json_reply(200, text_message(&huge))
    }))
    .await;
    let err = generate(config(base)).await.unwrap_err();
    assert!(matches!(err, LlmError::ResponseTooLarge(_)), "{err:?}");
}

#[tokio::test]
async fn unreachable_provider_is_typed() {
    // Port 1 is privileged: nothing listens there and no test can bind it, so
    // the connection is refused (bind-then-drop races with parallel tests).
    let base = "http://127.0.0.1:1".to_string();
    let err = generate(config(base)).await.unwrap_err();
    assert_eq!(err.code(), AgentFailureCode::LlmUnavailable, "{err:?}");
    assert!(!err.to_string().contains("127.0.0.1"), "{err}");
}
