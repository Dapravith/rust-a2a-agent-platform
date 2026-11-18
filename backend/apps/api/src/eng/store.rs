//! Engineering-workflow persistence.
//!
//! Every mutation runs in one short transaction that (1) locks the workflow
//! row, (2) asks the pure state machine in `engineering::state` whether the
//! change is allowed for this actor, (3) writes it with `WHERE version = $n`
//! and (4) appends an audit event. No transaction is held across an agent
//! call. Artifacts, approvals and audit events are append-only (DB trigger).

use chrono::{DateTime, Utc};
use engineering::artifacts::{self, ArtifactKind, SCHEMA_VERSION};
use engineering::roles::Role;
use engineering::state::{self, Actor, Budget, Event, Status, TransitionError, WorkflowState};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Closed set of failure codes (kept in sync with the migration CHECKs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngErrorCode {
    Interrupted,
    AgentUnavailable,
    AgentTimeout,
    AgentFailed,
    ArtifactInvalid,
    ToolNotPermitted,
    PolicyViolation,
    CandidateMismatch,
    BudgetExceeded,
    ProviderUnconfigured,
    /// Cited MCP tool calls are missing, foreign, refused, or about another commit.
    ToolEvidenceInvalid,
    InternalError,
}

impl EngErrorCode {
    pub const ALL: [EngErrorCode; 12] = [
        Self::Interrupted,
        Self::AgentUnavailable,
        Self::AgentTimeout,
        Self::AgentFailed,
        Self::ArtifactInvalid,
        Self::ToolNotPermitted,
        Self::PolicyViolation,
        Self::CandidateMismatch,
        Self::BudgetExceeded,
        Self::ProviderUnconfigured,
        Self::ToolEvidenceInvalid,
        Self::InternalError,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::AgentUnavailable => "agent_unavailable",
            Self::AgentTimeout => "agent_timeout",
            Self::AgentFailed => "agent_failed",
            Self::ArtifactInvalid => "artifact_invalid",
            Self::ToolNotPermitted => "tool_not_permitted",
            Self::PolicyViolation => "policy_violation",
            Self::CandidateMismatch => "candidate_mismatch",
            Self::BudgetExceeded => "budget_exceeded",
            Self::ProviderUnconfigured => "provider_unconfigured",
            Self::ToolEvidenceInvalid => "tool_evidence_invalid",
            Self::InternalError => "internal_error",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("workflow not found")]
    NotFound,
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error("stale request: workflow is at version {actual}, request was for {expected}")]
    Stale { expected: i64, actual: i64 },
    #[error("this stage already has a live or completed run")]
    DuplicateRun,
    #[error("Idempotency-Key was already used for a different decision")]
    IdempotencyConflict,
    #[error("{0}")]
    Mismatch(&'static str),
    #[error("mandatory quality gates failed: {}", .0.iter().filter(|g| !g.passed).map(|g| format!("{:?}", g.gate)).collect::<Vec<_>>().join(", "))]
    GatesFailed(Vec<engineering::gates::GateResult>),
    #[error(transparent)]
    Contract(#[from] engineering::ContractError),
    #[error("corrupt row: {0}")]
    Corrupt(&'static str),
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EngWorkflow {
    pub id: Uuid,
    pub title: String,
    pub requirements: String,
    pub requested_by: String,
    pub status: String,
    pub version: i64,
    pub plan_revisions: i32,
    pub delivery_revisions: i32,
    pub agent_runs: i32,
    pub max_plan_revisions: i32,
    pub max_delivery_revisions: i32,
    pub max_agent_runs: i32,
    pub candidate_commit_sha: Option<String>,
    pub pending_decision_hash: Option<String>,
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

macro_rules! wf_cols {
    () => {
        "id, title, requirements, requested_by, status, version, plan_revisions, delivery_revisions, \
         agent_runs, max_plan_revisions, max_delivery_revisions, max_agent_runs, candidate_commit_sha, \
         pending_decision_hash, error_code, error, created_at, updated_at, finished_at"
    };
}

fn u(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

impl EngWorkflow {
    pub fn status(&self) -> Result<Status, StoreError> {
        Status::parse(&self.status).ok_or(StoreError::Corrupt("unknown status"))
    }
    pub fn state(&self) -> Result<WorkflowState, StoreError> {
        Ok(WorkflowState {
            status: self.status()?,
            version: self.version,
            plan_revisions: u(self.plan_revisions),
            delivery_revisions: u(self.delivery_revisions),
            agent_runs: u(self.agent_runs),
        })
    }
    pub fn budget(&self) -> Budget {
        Budget {
            max_plan_revisions: u(self.max_plan_revisions),
            max_delivery_revisions: u(self.max_delivery_revisions),
            max_agent_runs: u(self.max_agent_runs),
        }
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EngRun {
    pub id: Uuid,
    pub role: String,
    pub stage_key: String,
    pub attempt: i32,
    pub agent_id: String,
    pub provider: String,
    pub model: Option<String>,
    pub correlation_id: Uuid,
    pub status: String,
    pub a2a_task_id: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EngArtifact {
    pub id: Uuid,
    pub agent_run_id: Option<Uuid>,
    pub kind: String,
    pub schema_version: i32,
    pub producer_kind: String,
    pub producer_id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub content: Value,
    pub content_hash: String,
    pub commit_sha: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EngApproval {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub gate: String,
    pub decision: String,
    pub approver_id: String,
    pub approver_role: String,
    pub identity_issuer: String,
    pub comment: Option<String>,
    pub reviewed_commit_sha: Option<String>,
    pub artifact_hash: Option<String>,
    pub workflow_version: i64,
    pub idempotency_key: String,
    pub request_hash: String,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AuditEvent {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    pub actor_kind: String,
    pub actor_id: String,
    pub action: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub workflow_version: i64,
    pub detail: Value,
}

pub type Tx<'a> = Transaction<'a, Postgres>;

// ---------------------------------------------------------------------------
// Workflow creation and locked reads
// ---------------------------------------------------------------------------

pub async fn create(
    pool: &PgPool,
    title: &str,
    requirements: &str,
    requested_by: &str,
    budget: Budget,
) -> Result<EngWorkflow, StoreError> {
    let mut tx = pool.begin().await?;
    const SQL: &str = concat!(
        "INSERT INTO eng_workflows (id, title, requirements, requested_by, status, \
         max_plan_revisions, max_delivery_revisions, max_agent_runs) \
         VALUES ($1, $2, $3, $4, 'draft', $5, $6, $7) RETURNING ",
        wf_cols!()
    );
    let wf = sqlx::query_as::<_, EngWorkflow>(SQL)
        .bind(Uuid::now_v7())
        .bind(title)
        .bind(requirements)
        .bind(requested_by)
        .bind(i32::try_from(budget.max_plan_revisions).unwrap_or(i32::MAX))
        .bind(i32::try_from(budget.max_delivery_revisions).unwrap_or(i32::MAX))
        .bind(i32::try_from(budget.max_agent_runs).unwrap_or(i32::MAX))
        .fetch_one(&mut *tx)
        .await?;
    audit(
        &mut tx,
        wf.id,
        "human",
        requested_by,
        "workflow_submitted",
        None,
        Some("draft"),
        wf.version,
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(wf)
}

pub async fn get(pool: impl sqlx::PgExecutor<'_>, id: Uuid) -> Result<EngWorkflow, StoreError> {
    const SQL: &str = concat!("SELECT ", wf_cols!(), " FROM eng_workflows WHERE id = $1");
    sqlx::query_as::<_, EngWorkflow>(SQL)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(StoreError::NotFound)
}

pub async fn lock(tx: &mut Tx<'_>, id: Uuid) -> Result<EngWorkflow, StoreError> {
    const SQL: &str = concat!(
        "SELECT ",
        wf_cols!(),
        " FROM eng_workflows WHERE id = $1 FOR UPDATE"
    );
    sqlx::query_as::<_, EngWorkflow>(SQL)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(StoreError::NotFound)
}

#[allow(clippy::too_many_arguments)]
pub async fn audit(
    tx: &mut Tx<'_>,
    workflow_id: Uuid,
    actor_kind: &str,
    actor_id: &str,
    action: &str,
    from: Option<&str>,
    to: Option<&str>,
    version: i64,
    detail: Value,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO eng_audit_events (id, workflow_id, actor_kind, actor_id, action, from_status, \
         to_status, workflow_version, detail) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(Uuid::now_v7())
    .bind(workflow_id)
    .bind(actor_kind)
    .bind(actor_id)
    .bind(action)
    .bind(from)
    .bind(to)
    .bind(version)
    .bind(detail)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Values that change together with a transition.
#[derive(Default)]
pub struct Changes<'a> {
    pub candidate_commit_sha: Option<Option<&'a str>>,
    pub pending_decision_hash: Option<Option<&'a str>>,
    pub error: Option<(EngErrorCode, &'a str)>,
}

/// Applies `event` by `actor` to the locked row `wf` (state machine + version
/// guard + audit). Returns the new state.
pub async fn transition(
    tx: &mut Tx<'_>,
    wf: &EngWorkflow,
    event: Event,
    actor: &Actor,
    changes: Changes<'_>,
    detail: Value,
) -> Result<WorkflowState, StoreError> {
    let from = wf.state()?;
    let to = state::next(&from, event, actor, &wf.budget())?;
    let candidate = changes
        .candidate_commit_sha
        .unwrap_or(wf.candidate_commit_sha.as_deref());
    let pending = changes
        .pending_decision_hash
        .unwrap_or(wf.pending_decision_hash.as_deref());
    let (code, msg) = changes
        .error
        .map_or((None, None), |(c, m)| (Some(c.as_str()), Some(m)));
    let r = sqlx::query(
        "UPDATE eng_workflows SET status = $3, version = $4, plan_revisions = $5, \
         delivery_revisions = $6, candidate_commit_sha = $7, pending_decision_hash = $8, \
         error_code = $9, error = left($10, 1000), updated_at = now(), \
         finished_at = CASE WHEN $11 THEN now() ELSE NULL END \
         WHERE id = $1 AND version = $2",
    )
    .bind(wf.id)
    .bind(wf.version)
    .bind(to.status.as_str())
    .bind(to.version)
    .bind(i32::try_from(to.plan_revisions).unwrap_or(i32::MAX))
    .bind(i32::try_from(to.delivery_revisions).unwrap_or(i32::MAX))
    .bind(candidate)
    .bind(pending)
    .bind(code)
    .bind(msg)
    .bind(to.status.is_terminal())
    .execute(&mut **tx)
    .await?;
    if r.rows_affected() != 1 {
        return Err(StoreError::Stale {
            expected: wf.version,
            actual: -1,
        });
    }
    audit(
        tx,
        wf.id,
        actor.kind(),
        actor.id(),
        event.as_str(),
        Some(from.status.as_str()),
        Some(to.status.as_str()),
        to.version,
        detail,
    )
    .await?;
    Ok(to)
}

// ---------------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------------

pub struct NewArtifact<'a> {
    pub kind: ArtifactKind,
    pub run_id: Option<Uuid>,
    pub producer_kind: &'a str,
    pub producer_id: &'a str,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub content: &'a Value,
    pub commit_sha: Option<&'a str>,
}

/// Inserts an artifact into a workflow whose row the caller holds locked and
/// which is not terminal. Returns `(id, content_hash)`.
pub async fn insert_artifact(
    tx: &mut Tx<'_>,
    wf: &EngWorkflow,
    a: NewArtifact<'_>,
) -> Result<(Uuid, String), StoreError> {
    if wf.status()?.is_terminal() {
        return Err(StoreError::Transition(TransitionError::Terminal(
            wf.status()?,
        )));
    }
    let id = Uuid::now_v7();
    let hash = artifacts::content_hash(a.kind, SCHEMA_VERSION, a.content);
    sqlx::query(
        "INSERT INTO eng_artifacts (id, workflow_id, agent_run_id, kind, schema_version, producer_kind, \
         producer_id, provider, model, content, content_hash, commit_sha) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(id)
    .bind(wf.id)
    .bind(a.run_id)
    .bind(a.kind.as_str())
    .bind(SCHEMA_VERSION)
    .bind(a.producer_kind)
    .bind(a.producer_id)
    .bind(a.provider)
    .bind(a.model)
    .bind(a.content)
    .bind(&hash)
    .bind(a.commit_sha)
    .execute(&mut **tx)
    .await?;
    Ok((id, hash))
}

pub async fn artifacts(
    pool: impl sqlx::PgExecutor<'_>,
    workflow_id: Uuid,
) -> Result<Vec<EngArtifact>, StoreError> {
    Ok(sqlx::query_as::<_, EngArtifact>(
        "SELECT id, agent_run_id, kind, schema_version, producer_kind, producer_id, provider, model, \
         content, content_hash, commit_sha, created_at FROM eng_artifacts \
         WHERE workflow_id = $1 ORDER BY created_at, id",
    )
    .bind(workflow_id)
    .fetch_all(pool)
    .await?)
}

/// Latest artifact of `kind` (UUIDv7 ids order by creation).
pub fn latest(list: &[EngArtifact], kind: ArtifactKind) -> Option<&EngArtifact> {
    list.iter()
        .filter(|a| a.kind == kind.as_str())
        .max_by_key(|a| a.id)
}

/// Latest artifact of `kind` about commit `sha`.
pub fn latest_for<'a>(
    list: &'a [EngArtifact],
    kind: ArtifactKind,
    sha: &str,
) -> Option<&'a EngArtifact> {
    list.iter()
        .filter(|a| a.kind == kind.as_str() && a.commit_sha.as_deref() == Some(sha))
        .max_by_key(|a| a.id)
}

// ---------------------------------------------------------------------------
// Agent runs
// ---------------------------------------------------------------------------

pub struct RunStart<'a> {
    pub role: Role,
    pub expected_status: Status,
    pub expected_version: i64,
    pub attempt: i32,
    pub agent_id: &'a str,
    pub provider: &'a str,
    pub model: Option<&'a str>,
}

pub fn stage_key(role: Role, version: i64) -> String {
    format!("{}:{}", role.as_str(), version)
}

/// Reserves budget and inserts a `running` run. `DuplicateRun` if the stage
/// already has a live or completed run (unique partial index); `Stale` if the
/// workflow moved on; `Transition(BudgetExceeded)` if out of budget.
pub async fn start_run(
    pool: &PgPool,
    workflow_id: Uuid,
    s: RunStart<'_>,
) -> Result<(Uuid, Uuid), StoreError> {
    let mut tx = pool.begin().await?;
    let wf = lock(&mut tx, workflow_id).await?;
    if wf.status()? != s.expected_status || wf.version != s.expected_version {
        return Err(StoreError::Stale {
            expected: s.expected_version,
            actual: wf.version,
        });
    }
    let reserved = state::reserve_agent_run(&wf.state()?, &wf.budget())?;
    let (id, correlation) = (Uuid::now_v7(), Uuid::new_v4());
    let key = stage_key(s.role, wf.version);
    let ins = sqlx::query(
        "INSERT INTO eng_agent_runs (id, workflow_id, role, stage_key, attempt, agent_id, provider, model, \
         correlation_id, status) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'running')",
    )
    .bind(id)
    .bind(workflow_id)
    .bind(s.role.as_str())
    .bind(&key)
    .bind(s.attempt)
    .bind(s.agent_id)
    .bind(s.provider)
    .bind(s.model)
    .bind(correlation)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db)) = &ins
        && db.is_unique_violation()
    {
        return Err(StoreError::DuplicateRun);
    }
    ins?;
    sqlx::query("UPDATE eng_workflows SET agent_runs = $2, updated_at = now() WHERE id = $1")
        .bind(workflow_id)
        .bind(i32::try_from(reserved.agent_runs).unwrap_or(i32::MAX))
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        workflow_id,
        "agent",
        s.agent_id,
        "agent_run_started",
        None,
        None,
        wf.version,
        json!({"run_id": id, "role": s.role.as_str(), "stage": key, "attempt": s.attempt,
               "provider": s.provider, "model": s.model, "correlation_id": correlation}),
    )
    .await?;
    tx.commit().await?;
    Ok((id, correlation))
}

pub async fn fail_run(
    pool: &PgPool,
    workflow_id: Uuid,
    run_id: Uuid,
    code: EngErrorCode,
    msg: &str,
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    let wf = lock(&mut tx, workflow_id).await?;
    let r = sqlx::query(
        "UPDATE eng_agent_runs SET status = 'failed', error_code = $2, error = left($3, 1000), \
         finished_at = now() WHERE id = $1 AND status = 'running' RETURNING agent_id",
    )
    .bind(run_id)
    .bind(code.as_str())
    .bind(msg)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((agent_id,)) = r.map(|row: sqlx::postgres::PgRow| {
        use sqlx::Row;
        (row.get::<String, _>("agent_id"),)
    }) else {
        return Err(StoreError::Mismatch("run is not running"));
    };
    audit(
        &mut tx,
        workflow_id,
        "agent",
        &agent_id,
        "agent_run_failed",
        None,
        None,
        wf.version,
        json!({"run_id": run_id, "error_code": code.as_str(), "error": msg}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn complete_run_tx(
    tx: &mut Tx<'_>,
    run_id: Uuid,
    a2a_task_id: Option<&str>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
) -> Result<(), StoreError> {
    let r = sqlx::query(
        "UPDATE eng_agent_runs SET status = 'completed', a2a_task_id = $2, input_tokens = $3, \
         output_tokens = $4, finished_at = now() WHERE id = $1 AND status = 'running'",
    )
    .bind(run_id)
    .bind(a2a_task_id)
    .bind(input_tokens)
    .bind(output_tokens)
    .execute(&mut **tx)
    .await?;
    if r.rows_affected() != 1 {
        return Err(StoreError::Mismatch("run is not running"));
    }
    Ok(())
}

pub async fn runs(
    pool: impl sqlx::PgExecutor<'_>,
    workflow_id: Uuid,
) -> Result<Vec<EngRun>, StoreError> {
    Ok(sqlx::query_as::<_, EngRun>(
        "SELECT id, role, stage_key, attempt, agent_id, provider, model, correlation_id, status, a2a_task_id, \
         input_tokens, output_tokens, error_code, error, started_at, finished_at \
         FROM eng_agent_runs WHERE workflow_id = $1 ORDER BY started_at, id",
    )
    .bind(workflow_id)
    .fetch_all(pool)
    .await?)
}

// ---------------------------------------------------------------------------
// Failure, recovery, reads
// ---------------------------------------------------------------------------

/// Fails a workflow (system actor). A workflow at a human gate or terminal
/// is left alone (`Transition` error returned).
pub async fn fail_workflow(
    pool: &PgPool,
    id: Uuid,
    code: EngErrorCode,
    msg: &str,
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    let wf = lock(&mut tx, id).await?;
    transition(
        &mut tx,
        &wf,
        Event::Fail,
        &Actor::System,
        Changes {
            error: Some((code, msg)),
            ..Changes::default()
        },
        json!({"error_code": code.as_str(), "error": msg}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Restart recovery. Stages that were in flight fail with `interrupted` (no
/// silent replay); workflows waiting for a human are preserved exactly.
/// Caller must hold the orchestrator instance lock.
pub async fn recover_interrupted(pool: &PgPool) -> Result<u64, StoreError> {
    let ids: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM eng_workflows WHERE status NOT IN \
         ('awaiting_plan_approval', 'awaiting_delivery_approval', 'approved_for_delivery', 'rejected', 'failed', 'cancelled')",
    )
    .fetch_all(pool)
    .await?;
    let mut n = 0;
    for (id, status) in ids {
        let Some(st) = Status::parse(&status) else {
            continue;
        };
        if state::recovery_event(st).is_none() {
            continue;
        }
        let mut tx = pool.begin().await?;
        let wf = lock(&mut tx, id).await?;
        sqlx::query(
            "UPDATE eng_agent_runs SET status = 'failed', error_code = 'interrupted', \
             error = 'Interrupted by an orchestrator restart', finished_at = now() \
             WHERE workflow_id = $1 AND status = 'running'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        match transition(
            &mut tx,
            &wf,
            Event::Fail,
            &Actor::System,
            Changes {
                error: Some((
                    EngErrorCode::Interrupted,
                    "Interrupted by an orchestrator restart; resubmit to run again.",
                )),
                ..Changes::default()
            },
            json!({"error_code": "interrupted"}),
        )
        .await
        {
            Ok(_) => {
                tx.commit().await?;
                n += 1;
            }
            Err(StoreError::Transition(_)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

pub async fn approvals(
    pool: impl sqlx::PgExecutor<'_>,
    workflow_id: Uuid,
) -> Result<Vec<EngApproval>, StoreError> {
    Ok(sqlx::query_as::<_, EngApproval>(
        "SELECT id, workflow_id, gate, decision, approver_id, approver_role, identity_issuer, comment, \
         reviewed_commit_sha, artifact_hash, workflow_version, idempotency_key, request_hash, decided_at \
         FROM eng_approvals WHERE workflow_id = $1 ORDER BY decided_at, id",
    )
    .bind(workflow_id)
    .fetch_all(pool)
    .await?)
}

pub async fn audit_events(
    pool: impl sqlx::PgExecutor<'_>,
    workflow_id: Uuid,
) -> Result<Vec<AuditEvent>, StoreError> {
    Ok(sqlx::query_as::<_, AuditEvent>(
        "SELECT id, at, actor_kind, actor_id, action, from_status, to_status, workflow_version, detail \
         FROM eng_audit_events WHERE workflow_id = $1 ORDER BY at, id",
    )
    .bind(workflow_id)
    .fetch_all(pool)
    .await?)
}

pub async fn list(pool: &PgPool, limit: i64) -> Result<Vec<EngWorkflow>, StoreError> {
    const SQL: &str = concat!(
        "SELECT ",
        wf_cols!(),
        " FROM eng_workflows ORDER BY created_at DESC, id DESC LIMIT $1"
    );
    Ok(sqlx::query_as::<_, EngWorkflow>(SQL)
        .bind(limit)
        .fetch_all(pool)
        .await?)
}

pub struct Detail {
    pub workflow: EngWorkflow,
    pub runs: Vec<EngRun>,
    pub artifacts: Vec<EngArtifact>,
    pub approvals: Vec<EngApproval>,
    pub audit: Vec<AuditEvent>,
}

/// One consistent snapshot of everything about a workflow.
pub async fn detail(pool: &PgPool, id: Uuid) -> Result<Detail, StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let workflow = get(&mut *tx, id).await?;
    let runs = runs(&mut *tx, id).await?;
    let artifacts = artifacts(&mut *tx, id).await?;
    let approvals = approvals(&mut *tx, id).await?;
    let audit = audit_events(&mut *tx, id).await?;
    tx.commit().await?;
    Ok(Detail {
        workflow,
        runs,
        artifacts,
        approvals,
        audit,
    })
}

/// One MCP audit row, as the orchestrator needs it to verify evidence.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ToolCallRow {
    pub id: Uuid,
    pub run_id: Option<Uuid>,
    pub role: Option<String>,
    pub tool: String,
    pub outcome: String,
    pub head_sha: Option<String>,
    pub exit_code: Option<i32>,
}

/// Audit rows for the given ids (written by the MCP server, read-only here).
pub async fn tool_calls(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<ToolCallRow>, StoreError> {
    Ok(sqlx::query_as::<_, ToolCallRow>(
        "SELECT id, run_id, role, tool, outcome, head_sha, exit_code \
         FROM eng_tool_calls WHERE id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?)
}
