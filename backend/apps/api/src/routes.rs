//! REST API consumed by the Astro dashboard. All routes are under `/api`.

use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::db::{self, AgentRun, DbError, Workflow};
use crate::engine::Engine;

const MAX_BODY_BYTES: usize = 16 * 1024;
pub(crate) const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// `Idempotency-Key` format shared by every write endpoint: 1-128 chars of `[A-Za-z0-9_-]`.
pub(crate) fn is_valid_idempotency_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    Validation(String),
    #[error("workflow not found")]
    NotFound,
    #[error("another workflow is already queued or running; wait for it to finish")]
    Busy,
    #[error("Idempotency-Key was already used for a different question")]
    IdempotencyKeyMismatch,
    #[error("request body too large")]
    TooLarge,
    #[error("service unavailable")]
    Unavailable,
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> Self {
        match e {
            DbError::ActiveWorkflowExists => ApiError::Busy,
            DbError::IdempotencyKeyMismatch => ApiError::IdempotencyKeyMismatch,
            DbError::Sqlx(err) => {
                // Details go to logs only, never to the client.
                tracing::error!(error = %err, "database error");
                ApiError::Unavailable
            }
            e @ DbError::InvalidTransition { .. } => {
                tracing::error!(error = %e, "unexpected state transition");
                ApiError::Unavailable
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ApiError::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_failed"),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::Busy => (StatusCode::CONFLICT, "workflow_active"),
            ApiError::IdempotencyKeyMismatch => {
                (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_mismatch")
            }
            ApiError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
            ApiError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        };
        (
            status,
            Json(json!({"error": {"code": code, "message": self.to_string()}})),
        )
            .into_response()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkflow {
    pub question: String,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub before: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct WorkflowDetail {
    #[serde(flatten)]
    pub workflow: Workflow,
    pub runs: Vec<AgentRun>,
    pub draft: Option<Value>,
    pub review: Option<Value>,
}

async fn health(State(engine): State<Engine>) -> Response {
    match db::ping(&engine.pool).await {
        Ok(()) => Json(json!({"status": "ok", "database": "ok"})).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "degraded", "database": "error"})),
        )
            .into_response(),
    }
}

/// Discovery status of both trusted agents (via their Agent Cards).
async fn agents(State(engine): State<Engine>) -> Json<Value> {
    let mut out = Vec::new();
    for (role, ep) in [("worker", &engine.worker), ("reviewer", &engine.reviewer)] {
        let entry = match engine.gateway.discover(ep).await {
            Ok(card) => json!({
                "role": role, "reachable": true, "name": card.name, "version": card.version,
                "description": card.description,
                "skills": card.skills.iter().map(|s| json!({"id": s.id, "name": s.name})).collect::<Vec<_>>(),
            }),
            Err(e) => {
                json!({"role": role, "reachable": false, "name": ep.expected_name, "error": e.safe_message()})
            }
        };
        out.push(entry);
    }
    Json(json!({"agents": out}))
}

async fn create_workflow(
    State(engine): State<Engine>,
    headers: HeaderMap,
    body: Result<Json<CreateWorkflow>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) = body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::TooLarge
        } else {
            ApiError::Validation(e.body_text())
        }
    })?;
    let question = body.question.trim();
    contracts::validate_question(question).map_err(|e| ApiError::Validation(e.to_string()))?;

    let key = match headers.get(IDEMPOTENCY_HEADER) {
        None => None,
        Some(v) => {
            let s = v
                .to_str()
                .map_err(|_| ApiError::Validation("invalid Idempotency-Key".into()))?;
            if !is_valid_idempotency_key(s) {
                return Err(ApiError::Validation(
                    "Idempotency-Key must be 1-128 chars of [A-Za-z0-9_-]".into(),
                ));
            }
            Some(s.to_owned())
        }
    };

    let (wf, created) = engine.submit(question, key.as_deref()).await?;
    let status = if created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    let location = HeaderValue::from_str(&format!("/api/workflows/{}", wf.id))
        .map_err(|_| ApiError::Unavailable)?;
    Ok((status, [(header::LOCATION, location)], Json(wf)).into_response())
}

async fn list_workflows(
    State(engine): State<Engine>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.unwrap_or(25);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::Validation(
            "limit must be between 1 and 100".into(),
        ));
    }
    let items = db::list_workflows(&engine.pool, limit, q.before).await?;
    let next_before = if items.len() as i64 == limit {
        items.last().map(|w| w.created_at)
    } else {
        None
    };
    Ok(Json(json!({"items": items, "next_before": next_before})))
}

async fn get_workflow(
    State(engine): State<Engine>,
    Path(id): Path<String>,
) -> Result<Json<WorkflowDetail>, ApiError> {
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
    let (workflow, runs, artifacts) = db::get_detail(&engine.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let pick = |kind: &str| {
        artifacts
            .iter()
            .find(|a| a.kind == kind)
            .map(|a| a.content.clone())
    };
    Ok(Json(WorkflowDetail {
        draft: pick("draft"),
        review: pick("review"),
        workflow,
        runs,
    }))
}

async fn api_not_found() -> ApiError {
    ApiError::NotFound
}

pub fn router(engine: Engine) -> Router {
    let metrics_route = get(metrics).with_state(engine.clone());
    let api = Router::new()
        .route("/health", get(health))
        .route("/agents", get(agents))
        .route("/workflows", get(list_workflows).post(create_workflow))
        .route("/workflows/{id}", get(get_workflow))
        .fallback(api_not_found)
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .with_state(engine);
    // `/api/v1` is the versioned path; `/api` stays for the dashboard. Both
    // mount the same handlers. `/healthz` is process liveness only (no DB),
    // while `/api/health` is readiness and reports the database.
    // `/metrics` is for the in-cluster Prometheus only: the API has no
    // ingress route and its NetworkPolicy admits just the portal and the
    // monitoring namespace.
    Router::new()
        .route("/healthz", get(liveness))
        .route("/metrics", metrics_route)
        .nest("/api/v1", api.clone())
        .nest("/api", api)
}

async fn liveness() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn metrics(State(engine): State<Engine>) -> Response {
    let up = tokio::time::timeout(Duration::from_secs(2), db::ping(&engine.pool))
        .await
        .is_ok_and(|r| r.is_ok());
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        crate::metrics::render(up),
    )
        .into_response()
}

/// Security headers, limits, timeouts and request tracing. Applied last, so
/// it wraps every route including the static dashboard fallback.
pub fn harden(router: Router) -> Router {
    router
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::overriding(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"),
        ))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30)))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
            let request_id = req
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_owned();
            tracing::info_span!("http", method = %req.method(), path = %req.uri().path(), request_id)
        }))
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}
