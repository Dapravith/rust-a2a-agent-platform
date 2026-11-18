//! Tool-call audit (C6). One row per call, allowed or not, written before the
//! result is returned. Postgres in production (`eng_tool_calls`, append-only);
//! in memory for tests.

use std::sync::Mutex;

use engineering::mcp::CapabilityClaims;
use futures::future::BoxFuture;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Allowed,
    Refused,
    Failed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Allowed => "allowed",
            Outcome::Refused => "refused",
            Outcome::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolCallRecord {
    pub id: Uuid,
    pub workflow_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub role: Option<String>,
    pub agent_id: Option<String>,
    pub workspace_id: Option<Uuid>,
    pub tool: String,
    pub target: Option<String>,
    pub arguments_sha256: Option<String>,
    pub outcome: Outcome,
    pub error_code: Option<String>,
    pub head_sha: Option<String>,
    pub exit_code: Option<i32>,
    pub result_sha256: Option<String>,
    pub duration_ms: i64,
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

impl ToolCallRecord {
    /// A request refused at the HTTP edge (no verified token).
    pub fn refused_transport(code: &str) -> Self {
        ToolCallRecord {
            id: Uuid::now_v7(),
            workflow_id: None,
            run_id: None,
            role: None,
            agent_id: None,
            workspace_id: None,
            tool: "(transport)".into(),
            target: None,
            arguments_sha256: None,
            outcome: Outcome::Refused,
            error_code: Some(code.into()),
            head_sha: None,
            exit_code: None,
            result_sha256: None,
            duration_ms: 0,
        }
    }

    /// A call with verified claims; the outcome is filled in by the caller.
    pub fn for_call(c: &CapabilityClaims, tool: &str, args: &Value) -> Self {
        ToolCallRecord {
            id: Uuid::now_v7(),
            workflow_id: Some(c.workflow_id),
            run_id: Some(c.run_id),
            role: Some(c.role.as_str().into()),
            agent_id: Some(clip(&c.agent_id, 128)),
            workspace_id: Some(c.workspace_id),
            tool: clip(if tool.is_empty() { "(empty)" } else { tool }, 128),
            target: None,
            arguments_sha256: Some(workspace::sha256_hex(args.to_string().as_bytes())),
            outcome: Outcome::Refused,
            error_code: None,
            head_sha: None,
            exit_code: None,
            result_sha256: None,
            duration_ms: 0,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("audit write failed: {0}")]
pub struct AuditError(pub String);

pub trait AuditSink: Send + Sync + 'static {
    fn record(&self, rec: ToolCallRecord) -> BoxFuture<'_, Result<(), AuditError>>;
}

/// In-memory sink for tests.
#[derive(Default)]
pub struct MemoryAudit {
    rows: Mutex<Vec<ToolCallRecord>>,
    fail: bool,
}

impl MemoryAudit {
    /// A sink whose every write fails (tests the fail-closed path).
    pub fn failing() -> Self {
        MemoryAudit {
            rows: Mutex::default(),
            fail: true,
        }
    }

    pub fn rows(&self) -> Vec<ToolCallRecord> {
        self.rows.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

impl AuditSink for MemoryAudit {
    fn record(&self, rec: ToolCallRecord) -> BoxFuture<'_, Result<(), AuditError>> {
        Box::pin(async move {
            if self.fail {
                return Err(AuditError("configured to fail".into()));
            }
            self.rows
                .lock()
                .map_err(|_| AuditError("poisoned".into()))?
                .push(rec);
            Ok(())
        })
    }
}

/// Postgres sink: `INSERT` into `eng_tool_calls` (the server's role can do nothing else).
pub struct PgAudit {
    pool: sqlx::PgPool,
}

impl PgAudit {
    pub fn new(pool: sqlx::PgPool) -> Self {
        PgAudit { pool }
    }
}

impl AuditSink for PgAudit {
    fn record(&self, r: ToolCallRecord) -> BoxFuture<'_, Result<(), AuditError>> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO eng_tool_calls (id, workflow_id, run_id, role, agent_id, workspace_id, \
                 tool, target, arguments_sha256, outcome, error_code, head_sha, exit_code, \
                 result_sha256, duration_ms) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
            )
            .bind(r.id)
            .bind(r.workflow_id)
            .bind(r.run_id)
            .bind(&r.role)
            .bind(&r.agent_id)
            .bind(r.workspace_id)
            .bind(&r.tool)
            .bind(r.target.as_deref().map(|t| clip(t, 1024)))
            .bind(&r.arguments_sha256)
            .bind(r.outcome.as_str())
            .bind(&r.error_code)
            .bind(&r.head_sha)
            .bind(r.exit_code)
            .bind(&r.result_sha256)
            .bind(r.duration_ms)
            .execute(&self.pool)
            .await
            .map_err(|e| AuditError(e.to_string()))?;
            Ok(())
        })
    }
}
