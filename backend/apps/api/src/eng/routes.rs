//! `/api/eng/*`: engineering workflows, human decisions, audit.
//!
//! Reads are open on the private network like the P0 API. Every write needs
//! a principal from the configured [`Authenticator`]; with none configured
//! (the default) writes answer 503 `auth_unavailable`, i.e. BLOCKED.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use engineering::state::{Budget, HumanRole};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::set_header::SetResponseHeaderLayer;
use uuid::Uuid;

use super::approval::{self, DecisionRequest};
use super::auth::{AuthError, Authenticator, Principal};
use super::orchestrator::Orchestrator;
use super::store::{self, StoreError};

#[derive(Clone)]
pub struct EngState {
    pub pool: sqlx::PgPool,
    pub orchestrator: Option<Arc<Orchestrator>>,
    pub auth: Arc<dyn Authenticator>,
    pub budget: Budget,
}

pub enum EngApiError {
    Auth(AuthError),
    Forbidden(&'static str),
    Validation(String),
    Store(StoreError),
    NoProviders,
}

impl From<StoreError> for EngApiError {
    fn from(e: StoreError) -> Self {
        EngApiError::Store(e)
    }
}

impl IntoResponse for EngApiError {
    fn into_response(self) -> Response {
        use engineering::state::TransitionError as T;
        let (status, code, msg) = match self {
            EngApiError::Auth(AuthError::NotConfigured) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                AuthError::NotConfigured.to_string(),
            ),
            EngApiError::Auth(e) => (StatusCode::UNAUTHORIZED, "unauthenticated", e.to_string()),
            EngApiError::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m.to_owned()),
            EngApiError::Validation(m) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "validation_failed", m)
            }
            EngApiError::NoProviders => (
                StatusCode::SERVICE_UNAVAILABLE,
                "provider_unconfigured",
                "no agent providers are configured".to_owned(),
            ),
            EngApiError::Store(e) => match &e {
                StoreError::NotFound => (StatusCode::NOT_FOUND, "not_found", e.to_string()),
                StoreError::Transition(T::Unauthorized { .. }) => {
                    (StatusCode::FORBIDDEN, "forbidden", e.to_string())
                }
                StoreError::Transition(_) => {
                    (StatusCode::CONFLICT, "invalid_transition", e.to_string())
                }
                StoreError::Stale { .. } => (StatusCode::CONFLICT, "stale_version", e.to_string()),
                StoreError::IdempotencyConflict => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "idempotency_key_mismatch",
                    e.to_string(),
                ),
                StoreError::Mismatch(_) => {
                    (StatusCode::CONFLICT, "artifact_mismatch", e.to_string())
                }
                StoreError::GatesFailed(_) => {
                    (StatusCode::CONFLICT, "quality_gates_failed", e.to_string())
                }
                StoreError::Contract(_) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "validation_failed",
                    e.to_string(),
                ),
                _ => {
                    tracing::error!(error = %e, "engineering store error");
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "service unavailable".to_owned(),
                    )
                }
            },
        };
        (
            status,
            Json(json!({"error": {"code": code, "message": msg}})),
        )
            .into_response()
    }
}

fn principal(s: &EngState, h: &HeaderMap) -> Result<Principal, EngApiError> {
    s.auth.authenticate(h).map_err(EngApiError::Auth)
}

fn json_body<T>(
    b: Result<Json<T>, axum::extract::rejection::JsonRejection>,
) -> Result<T, EngApiError> {
    b.map(|Json(v)| v)
        .map_err(|e| EngApiError::Validation(e.body_text()))
}

fn spawn_advance(s: &EngState, id: Uuid) {
    if let Some(o) = s.orchestrator.clone() {
        tokio::spawn(async move {
            if let Err(e) = o.advance(id).await {
                tracing::error!(workflow_id = %id, error = %e, "advance failed");
            }
        });
    }
}

/// Real configuration only: whether approvals can work and which providers
/// are wired. Nothing here is inferred or defaulted.
async fn status(State(s): State<EngState>) -> Json<Value> {
    let providers: Vec<Value> = s
        .orchestrator
        .as_ref()
        .map(|o| {
            o.identities()
                .into_iter()
                .map(|(r, i)| json!({"role": r.as_str(), "agent_id": i.agent_id, "provider": i.provider, "model": i.model}))
                .collect()
        })
        .unwrap_or_default();
    Json(json!({"approvals_enabled": s.auth.configured(), "providers": providers}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    title: String,
    requirements: String,
}

async fn create(
    State(s): State<EngState>,
    headers: HeaderMap,
    body: Result<Json<CreateBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, EngApiError> {
    let who = principal(&s, &headers)?;
    if !(who.has(HumanRole::Requester) || who.has(HumanRole::Approver)) {
        return Err(EngApiError::Forbidden("requester role required"));
    }
    let b = json_body(body)?;
    let title = b.title.trim();
    let req = b.requirements.trim();
    if title.is_empty()
        || title.chars().count() > 200
        || req.chars().count() < 10
        || req.chars().count() > 8000
    {
        return Err(EngApiError::Validation(
            "title 1-200 chars, requirements 10-8000 chars".into(),
        ));
    }
    if title
        .chars()
        .chain(req.chars())
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(EngApiError::Validation(
            "control characters are not allowed".into(),
        ));
    }
    if s.orchestrator.is_none() {
        return Err(EngApiError::NoProviders);
    }
    let wf = store::create(&s.pool, title, req, &who.subject, s.budget).await?;
    spawn_advance(&s, wf.id);
    Ok((StatusCode::ACCEPTED, Json(wf)).into_response())
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<i64>,
}

async fn list(
    State(s): State<EngState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, EngApiError> {
    let limit = q.limit.unwrap_or(25);
    if !(1..=100).contains(&limit) {
        return Err(EngApiError::Validation(
            "limit must be between 1 and 100".into(),
        ));
    }
    Ok(Json(json!({"items": store::list(&s.pool, limit).await?})))
}

async fn detail(
    State(s): State<EngState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, EngApiError> {
    let id = Uuid::parse_str(&id).map_err(|_| EngApiError::Store(StoreError::NotFound))?;
    let d = store::detail(&s.pool, id).await?;
    let gates = d
        .workflow
        .candidate_commit_sha
        .as_ref()
        .map(|_| approval::evaluate_gates(&d.workflow, &d.artifacts));
    let awaiting = d.workflow.status().ok().and_then(approval::awaiting);
    let tool_calls = store::tool_calls_for(&s.pool, id).await?;
    let workspace = store::workspace_for(&s.pool, id).await?;
    Ok(Json(json!({
        "workflow": d.workflow, "awaiting_decision": awaiting, "gates": gates,
        "runs": d.runs, "artifacts": d.artifacts, "approvals": d.approvals, "audit": d.audit,
        "tool_calls": tool_calls, "workspace_id": workspace,
    })))
}

async fn decide(
    State(s): State<EngState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<DecisionRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, EngApiError> {
    let who = principal(&s, &headers)?;
    let id = Uuid::parse_str(&id).map_err(|_| EngApiError::Store(StoreError::NotFound))?;
    let key = headers
        .get(crate::routes::IDEMPOTENCY_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|k| crate::routes::is_valid_idempotency_key(k))
        .ok_or_else(|| {
            EngApiError::Validation(
                "Idempotency-Key header (1-128 chars of [A-Za-z0-9_-]) is required".into(),
            )
        })?
        .to_owned();
    let req = json_body(body)?;
    let (approval, replayed) = approval::decide(&s.pool, &who, id, &key, &req).await?;
    if !replayed {
        spawn_advance(&s, id);
    }
    let code = if replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        code,
        Json(json!({"approval": approval, "replayed": replayed})),
    )
        .into_response())
}

pub fn router(state: EngState) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/workflows", get(list).post(create))
        .route("/workflows/{id}", get(detail))
        .route("/workflows/{id}/decisions", post(decide))
        // Responses carry requirements, approvals and the audit trail.
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .with_state(state)
}
