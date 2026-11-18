//! MCP server for agent tool execution (Phase 6B C1–C3, C6).
//!
//! * Transport: MCP Streamable HTTP via the official SDK (`rmcp`), bound to a
//!   private (loopback) address. `Host` validation stays at the SDK default (loopback only).
//! * Authentication: every HTTP request must carry `Authorization: Bearer
//!   <capability token>` issued by the orchestrator. It is verified at the
//!   HTTP edge **before** the MCP layer sees the request.
//! * Authorization: every `tools/call` goes through
//!   `engineering::mcp::authorize_call` (token grant, then role policy, then
//!   arguments, then path scope) **before** anything executes. The workspace comes
//!   from the token, never from arguments.
//! * Audit: every call is recorded before its result is returned, whether
//!   it was allowed, refused or failed. If the audit write fails, the call fails
//!   (fail closed). Refused tokens are recorded too.

pub mod audit;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use engineering::mcp::{AuthorizedCall, CapabilityClaims, McpTool, authorize_call, verify_token};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, JsonObject, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::{Value, json};
use workspace::{CheckRunner, Workspace, sha256_hex};

pub use audit::{AuditError, AuditSink, MemoryAudit, Outcome, PgAudit, ToolCallRecord};

/// Largest JSON-RPC body accepted.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

pub struct McpState {
    pub key: Vec<u8>,
    pub workspaces_root: PathBuf,
    pub audit: Arc<dyn AuditSink>,
    /// `None` means `testing.run` fails with `sandbox_unavailable`. It never
    /// falls back to running on the host.
    pub runner: Option<Arc<dyn CheckRunner>>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The MCP router: `/mcp`, behind token verification.
pub fn router(state: Arc<McpState>) -> axum::Router {
    let st = state.clone();
    let mut cfg = StreamableHttpServerConfig::default().with_sse_keep_alive(None);
    cfg.max_request_body_bytes = MAX_REQUEST_BYTES;
    let svc: StreamableHttpService<McpServer, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(McpServer { state: st.clone() }),
        Default::default(),
        cfg,
    );
    axum::Router::new()
        .nest_service("/mcp", svc)
        .layer(axum::middleware::from_fn_with_state(state, authenticate))
}

/// HTTP-edge token check. On success the verified claims are attached to the
/// request; `call_tool` reads them from there and from nowhere else.
async fn authenticate(
    State(state): State<Arc<McpState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let verified = match token {
        None => Err("token_missing"),
        Some(t) => verify_token(&state.key, t, now()).map_err(|e| match e {
            engineering::mcp::TokenError::Expired => "token_expired",
            engineering::mcp::TokenError::BadSignature => "token_bad_signature",
            engineering::mcp::TokenError::OverGranted(_) => "token_over_granted",
            _ => "token_invalid",
        }),
    };
    match verified {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Err(code) => {
            let rec = ToolCallRecord::refused_transport(code);
            if let Err(e) = state.audit.record(rec).await {
                tracing::error!(error = %e, "audit write failed for refused token");
            }
            tracing::warn!(code, "mcp request refused");
            (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer error=\"invalid_token\"")],
                axum::Json(json!({"error": code})),
            )
                .into_response()
        }
    }
}

#[derive(Clone)]
pub struct McpServer {
    state: Arc<McpState>,
}

fn schema(v: Value) -> Arc<JsonObject> {
    match v {
        Value::Object(m) => Arc::new(m),
        _ => Arc::new(JsonObject::new()),
    }
}

/// Tool definitions (`tools/list`). Schemas are closed (`additionalProperties: false`);
/// the server validates arguments itself, it does not trust the schema alone.
pub fn tool_definitions() -> Vec<Tool> {
    let ro = || ToolAnnotations::new().read_only(true).open_world(false);
    vec![
        Tool::new(
            McpTool::ReadFile.name(),
            "Read one UTF-8 file of the run's workspace (repo-relative path inside the granted scope).",
            schema(json!({
                "type": "object",
                "properties": {"path": {"type": "string", "maxLength": 1024}},
                "required": ["path"],
                "additionalProperties": false
            })),
        )
        .with_annotations(ro()),
        Tool::new(
            McpTool::Diff.name(),
            "Unified diff of the workspace HEAD against the run's base commit (both fixed by the token).",
            schema(json!({"type": "object", "properties": {}, "additionalProperties": false})),
        )
        .with_annotations(ro()),
        Tool::new(
            McpTool::RunTests.name(),
            "Run one allowlisted check by id in the sandbox against the workspace HEAD.",
            schema(json!({
                "type": "object",
                "properties": {"check_id": {"type": "string", "enum":
                    engineering::roles::ALLOWED_CHECKS.iter().map(|(k, _)| *k).collect::<Vec<_>>()}},
                "required": ["check_id"],
                "additionalProperties": false
            })),
        )
        .with_annotations(ToolAnnotations::new().read_only(true).idempotent(true).open_world(false)),
    ]
}

struct Executed {
    value: Value,
    head_sha: Option<String>,
    exit_code: Option<i32>,
}

impl McpServer {
    async fn execute(
        &self,
        claims: &CapabilityClaims,
        call: &AuthorizedCall,
    ) -> Result<Executed, (&'static str, String)> {
        let ws = Workspace::open(&self.state.workspaces_root, claims.workspace_id)
            .map_err(|e| (e.code(), e.to_string()))?;
        match call {
            AuthorizedCall::ReadFile { path } => {
                let f = ws
                    .read_file(path)
                    .await
                    .map_err(|e| (e.code(), e.to_string()))?;
                Ok(Executed {
                    head_sha: Some(f.head_sha.clone()),
                    exit_code: None,
                    value: serde_json::to_value(f).unwrap_or(Value::Null),
                })
            }
            AuthorizedCall::Diff => {
                let d = ws
                    .diff(&claims.base_sha)
                    .await
                    .map_err(|e| (e.code(), e.to_string()))?;
                Ok(Executed {
                    head_sha: Some(d.head_sha.clone()),
                    exit_code: None,
                    value: serde_json::to_value(d).unwrap_or(Value::Null),
                })
            }
            AuthorizedCall::RunTests { check_id } => {
                let runner = self.state.runner.as_ref().ok_or((
                    "sandbox_unavailable",
                    "no sandbox runner is configured".to_owned(),
                ))?;
                let o = runner
                    .run(&ws, check_id)
                    .await
                    .map_err(|e| (e.code(), e.to_string()))?;
                Ok(Executed {
                    head_sha: Some(o.head_sha.clone()),
                    exit_code: o.exit_code,
                    value: serde_json::to_value(o).unwrap_or(Value::Null),
                })
            }
        }
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tool_definitions()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let started = Instant::now();
        let claims = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<CapabilityClaims>())
            .cloned()
            // Unreachable behind `authenticate`; refuse rather than assume.
            .ok_or_else(|| McpError::invalid_request("missing capability", None))?;
        let args = Value::Object(request.arguments.clone().unwrap_or_default());
        let mut rec = ToolCallRecord::for_call(&claims, &request.name, &args);

        let result = match authorize_call(&claims, &request.name, &args) {
            Err(e) => {
                rec.outcome = Outcome::Refused;
                rec.error_code = Some(e.code().to_owned());
                CallToolResult::structured_error(json!({
                    "tool_call_id": rec.id, "error": e.code(), "message": e.to_string()
                }))
            }
            Ok(call) => {
                rec.target = match &call {
                    AuthorizedCall::ReadFile { path } => Some(path.clone()),
                    AuthorizedCall::RunTests { check_id } => Some(check_id.clone()),
                    AuthorizedCall::Diff => None,
                };
                match self.execute(&claims, &call).await {
                    Ok(mut ex) => {
                        if let Value::Object(m) = &mut ex.value {
                            m.insert("tool_call_id".into(), json!(rec.id));
                        }
                        rec.outcome = Outcome::Allowed;
                        rec.head_sha = ex.head_sha;
                        rec.exit_code = ex.exit_code;
                        rec.result_sha256 = Some(sha256_hex(ex.value.to_string().as_bytes()));
                        CallToolResult::structured(ex.value)
                    }
                    Err((code, msg)) => {
                        rec.outcome = Outcome::Failed;
                        rec.error_code = Some(code.to_owned());
                        CallToolResult::structured_error(json!({
                            "tool_call_id": rec.id, "error": code, "message": msg
                        }))
                    }
                }
            }
        };
        rec.duration_ms = started.elapsed().as_millis() as i64;
        tracing::info!(
            tool_call_id = %rec.id, run_id = ?rec.run_id, role = ?rec.role, tool = %rec.tool,
            outcome = rec.outcome.as_str(), error = ?rec.error_code, "mcp tool call"
        );
        // Fail closed: an unaudited result is never returned.
        self.state.audit.record(rec).await.map_err(|e| {
            tracing::error!(error = %e, "audit write failed; result withheld");
            McpError::internal_error("audit unavailable", None)
        })?;
        Ok(result.into())
    }
}
