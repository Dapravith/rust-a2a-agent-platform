//! Persistence (SQLx, parameterized queries only). The orchestrator is the
//! sole owner of the database.
//!
//! Every state transition is a guarded `UPDATE ... WHERE status = <expected>`
//! whose affected-row count is checked, so a workflow or run can reach a
//! terminal state at most once (no duplicate completion, no overwriting a
//! terminal state). No transaction is ever held open across an A2A or LLM
//! call: each function here is one short statement or transaction.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

use crate::error_code::ErrorCode;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

const ACTIVE_INDEX: &str = "workflows_single_active";
const IDEMPOTENCY_CONSTRAINT: &str = "workflows_idempotency_key_key";
/// Advisory-lock key held by the one running orchestrator ("A2A!" + 1).
const INSTANCE_LOCK_KEY: i64 = 0x4132_4121_0000_0001;
pub const INTERRUPTED_MSG: &str = "Interrupted by an orchestrator restart; resubmit to run again.";

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("another workflow is already queued or running")]
    ActiveWorkflowExists,
    #[error("Idempotency-Key was already used for a different question")]
    IdempotencyKeyMismatch,
    /// The row was not in the expected state (e.g. already terminal).
    #[error("{entity} {id} cannot move to `{to}` from its current state")]
    InvalidTransition {
        entity: &'static str,
        id: Uuid,
        to: &'static str,
    },
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Workflow {
    pub id: Uuid,
    pub question: String,
    pub status: String,
    pub verdict: Option<String>,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub failed_stage: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AgentRun {
    pub id: Uuid,
    pub agent: String,
    pub status: String,
    pub a2a_task_id: Option<String>,
    pub a2a_context_id: Option<String>,
    pub a2a_state: Option<String>,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StoredArtifact {
    pub kind: String,
    pub agent_run_id: Uuid,
    pub content: Value,
    pub created_at: DateTime<Utc>,
}

/// Column list shared by workflow queries; expands to a string literal so
/// every query stays a `&'static str` (sqlx `SqlSafeStr`).
macro_rules! wf_cols {
    () => {
        "id, question, status, verdict, error, error_code, failed_stage, created_at, started_at, finished_at"
    };
}

// ---------------------------------------------------------------------------
// Connection, schema and single-instance guard
// ---------------------------------------------------------------------------

/// Connects a pool. `url` has already passed [`crate::config::validate_database_url`].
pub async fn connect(url: &str, max_connections: u32) -> Result<PgPool, String> {
    let opts: PgConnectOptions = url
        .parse()
        .map_err(|_| "DATABASE_URL could not be parsed".to_string())?;
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts)
        .await
        // sqlx connection errors never include the password.
        .map_err(|e| format!("cannot connect to database: {e}"))
}

/// Refuses to serve against a schema that differs from this binary's
/// migrations (missing, failed, modified, or newer unknown migrations).
/// The runtime role cannot run DDL, so migrations are a separate step
/// (`api migrate`).
pub async fn verify_schema(pool: &PgPool) -> Result<(), String> {
    let rows: Vec<(i64, Vec<u8>, bool)> =
        sqlx::query_as("SELECT version, checksum, success FROM _sqlx_migrations")
            .fetch_all(pool)
            .await
            .map_err(|e| format!("cannot read migration history ({e}); run `api migrate` first"))?;
    let applied: HashMap<i64, (Vec<u8>, bool)> =
        rows.into_iter().map(|(v, c, s)| (v, (c, s))).collect();
    let known: Vec<_> = MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .collect();
    for m in &known {
        match applied.get(&m.version) {
            None => {
                return Err(format!(
                    "migration {} ({}) is not applied; run `api migrate`",
                    m.version, m.description
                ));
            }
            Some((_, false)) => return Err(format!("migration {} failed earlier", m.version)),
            Some((checksum, true)) if checksum.as_slice() != &*m.checksum => {
                return Err(format!(
                    "migration {} was modified after it was applied",
                    m.version
                ));
            }
            Some(_) => {}
        }
    }
    if let Some(v) = applied
        .keys()
        .find(|v| !known.iter().any(|m| m.version == **v))
    {
        return Err(format!(
            "database has migration {v} unknown to this binary (newer schema?)"
        ));
    }
    Ok(())
}

/// Session-level advisory lock held for the lifetime of the API process.
/// Restart recovery fails every queued/running workflow, which is only
/// correct if no other orchestrator is running against the same database.
pub struct InstanceLock {
    _conn: PgConnection,
}

impl InstanceLock {
    /// Takes the lock on a dedicated connection (detached from the pool).
    /// Session lifetime matters: with Supabase, use the direct connection or
    /// the session pooler, never the transaction pooler.
    pub async fn acquire(pool: &PgPool) -> Result<Self, String> {
        let mut conn = pool
            .acquire()
            .await
            .map_err(|e| format!("cannot acquire instance-lock connection: {e}"))?
            .detach();
        let (locked,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock($1)")
            .bind(INSTANCE_LOCK_KEY)
            .fetch_one(&mut conn)
            .await
            .map_err(|e| format!("cannot take instance lock: {e}"))?;
        if !locked {
            let _ = conn.close().await;
            return Err(
                "another orchestrator instance holds the lock on this database; \
                 only one API process may run against it"
                    .into(),
            );
        }
        Ok(Self { _conn: conn })
    }
}

// ---------------------------------------------------------------------------
// Workflow lifecycle
// ---------------------------------------------------------------------------

fn constraint_of(e: &sqlx::Error) -> Option<String> {
    match e {
        sqlx::Error::Database(db) => db.constraint().map(str::to_owned),
        _ => None,
    }
}

fn expect_one(rows: u64, entity: &'static str, id: Uuid, to: &'static str) -> Result<(), DbError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(DbError::InvalidTransition { entity, id, to })
    }
}

/// Inserts a queued workflow. Returns `(workflow, created)`; `created` is
/// false when the idempotency key matched an existing workflow for the same
/// question. Reusing a key for a different question is an error, never a
/// silent answer to another request.
pub async fn create_workflow(
    pool: &PgPool,
    question: &str,
    idempotency_key: Option<&str>,
) -> Result<(Workflow, bool), DbError> {
    let id = Uuid::now_v7();
    const SQL: &str = concat!(
        "INSERT INTO workflows (id, question, status, idempotency_key) ",
        "VALUES ($1, $2, 'queued', $3) RETURNING ",
        wf_cols!()
    );
    match sqlx::query_as::<_, Workflow>(SQL)
        .bind(id)
        .bind(question)
        .bind(idempotency_key)
        .fetch_one(pool)
        .await
    {
        Ok(w) => Ok((w, true)),
        Err(e) => match constraint_of(&e).as_deref() {
            Some(IDEMPOTENCY_CONSTRAINT) => {
                const SQL: &str = concat!(
                    "SELECT ",
                    wf_cols!(),
                    " FROM workflows WHERE idempotency_key = $1"
                );
                let w = sqlx::query_as::<_, Workflow>(SQL)
                    .bind(idempotency_key)
                    .fetch_one(pool)
                    .await?;
                if w.question != question {
                    return Err(DbError::IdempotencyKeyMismatch);
                }
                Ok((w, false))
            }
            Some(ACTIVE_INDEX) => Err(DbError::ActiveWorkflowExists),
            _ => Err(e.into()),
        },
    }
}

/// `queued -> running`.
pub async fn mark_running(pool: &PgPool, id: Uuid) -> Result<(), DbError> {
    let r = sqlx::query(
        "UPDATE workflows SET status = 'running', started_at = now() \
         WHERE id = $1 AND status = 'queued'",
    )
    .bind(id)
    .execute(pool)
    .await?;
    expect_one(r.rows_affected(), "workflow", id, "running")
}

/// `running -> completed`, exactly once.
pub async fn complete_workflow(pool: &PgPool, id: Uuid, verdict: &str) -> Result<(), DbError> {
    let r = sqlx::query(
        "UPDATE workflows SET status = 'completed', verdict = $2, finished_at = now() \
         WHERE id = $1 AND status = 'running'",
    )
    .bind(id)
    .bind(verdict)
    .execute(pool)
    .await?;
    expect_one(r.rows_affected(), "workflow", id, "completed")
}

/// What failed and where. `error` must already be client-safe.
pub struct Failure<'a> {
    pub code: ErrorCode,
    pub stage: Option<&'a str>,
    pub error: &'a str,
}

/// `queued|running -> failed`, exactly once; never overwrites a terminal state.
pub async fn fail_workflow(pool: &PgPool, id: Uuid, f: Failure<'_>) -> Result<(), DbError> {
    let r = sqlx::query(
        "UPDATE workflows SET status = 'failed', error = left($2, 1000), error_code = $3, \
         failed_stage = $4, finished_at = now() \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(id)
    .bind(f.error)
    .bind(f.code.as_str())
    .bind(f.stage)
    .execute(pool)
    .await?;
    expect_one(r.rows_affected(), "workflow", id, "failed")
}

// ---------------------------------------------------------------------------
// Agent runs and artifacts
// ---------------------------------------------------------------------------

pub async fn start_run(pool: &PgPool, workflow_id: Uuid, agent: &str) -> Result<Uuid, DbError> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO agent_runs (id, workflow_id, agent, status) VALUES ($1, $2, $3, 'running')",
    )
    .bind(id)
    .bind(workflow_id)
    .bind(agent)
    .execute(pool)
    .await?;
    Ok(id)
}

/// What a successful agent run produced over A2A.
pub struct RunResult<'a> {
    pub task_id: Option<&'a str>,
    pub context_id: Option<&'a str>,
    pub a2a_state: &'a str,
    pub kind: &'a str,
    pub content: &'a Value,
}

/// `running -> completed` for the run plus its artifact, in one transaction.
/// The artifact must already be validated against its contract.
pub async fn complete_run(
    pool: &PgPool,
    workflow_id: Uuid,
    run_id: Uuid,
    result: RunResult<'_>,
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    let r = sqlx::query(
        "UPDATE agent_runs SET status = 'completed', a2a_task_id = $2, a2a_context_id = $3, \
         a2a_state = $4, finished_at = now() WHERE id = $1 AND status = 'running'",
    )
    .bind(run_id)
    .bind(result.task_id)
    .bind(result.context_id)
    .bind(result.a2a_state)
    .execute(&mut *tx)
    .await?;
    // On error the transaction is dropped, i.e. rolled back.
    expect_one(r.rows_affected(), "agent run", run_id, "completed")?;
    sqlx::query("INSERT INTO artifacts (id, workflow_id, agent_run_id, kind, content) VALUES ($1, $2, $3, $4, $5)")
        .bind(Uuid::now_v7())
        .bind(workflow_id)
        .bind(run_id)
        .bind(result.kind)
        .bind(result.content)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// `running -> failed` for a run, recording the A2A task id when known.
pub async fn fail_run(
    pool: &PgPool,
    run_id: Uuid,
    code: ErrorCode,
    error: &str,
) -> Result<(), DbError> {
    let r = sqlx::query(
        "UPDATE agent_runs SET status = 'failed', error = left($2, 1000), error_code = $3, \
         finished_at = now() WHERE id = $1 AND status = 'running'",
    )
    .bind(run_id)
    .bind(error)
    .bind(code.as_str())
    .execute(pool)
    .await?;
    expect_one(r.rows_affected(), "agent run", run_id, "failed")
}

/// Restart recovery: anything left queued/running by a previous process is
/// failed with `interrupted` (never silently re-executed: the agent may or
/// may not have done the work, so replaying could duplicate LLM spend).
/// Returns the number of workflows. Caller must hold the [`InstanceLock`].
pub async fn recover_interrupted(pool: &PgPool) -> Result<u64, DbError> {
    let code = ErrorCode::Interrupted.as_str();
    let mut tx = pool.begin().await?;
    // Workflows first, so the stage can be read from the still-running run.
    let n = sqlx::query(
        "UPDATE workflows w SET status = 'failed', error = $1, error_code = $2, finished_at = now(), \
         failed_stage = (SELECT r.agent FROM agent_runs r \
                         WHERE r.workflow_id = w.id AND r.status = 'running' \
                         ORDER BY r.started_at DESC LIMIT 1) \
         WHERE w.status IN ('queued', 'running')",
    )
    .bind(INTERRUPTED_MSG)
    .bind(code)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query(
        "UPDATE agent_runs SET status = 'failed', error = $1, error_code = $2, finished_at = now() \
         WHERE status = 'running'",
    )
    .bind(INTERRUPTED_MSG)
    .bind(code)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

pub async fn list_workflows(
    pool: &PgPool,
    limit: i64,
    before: Option<DateTime<Utc>>,
) -> Result<Vec<Workflow>, DbError> {
    const SQL: &str = concat!(
        "SELECT ",
        wf_cols!(),
        " FROM workflows WHERE ($2::timestamptz IS NULL OR created_at < $2) ",
        "ORDER BY created_at DESC, id DESC LIMIT $1"
    );
    Ok(sqlx::query_as::<_, Workflow>(SQL)
        .bind(limit)
        .bind(before)
        .fetch_all(pool)
        .await?)
}

pub async fn get_workflow<'e>(
    pool: impl sqlx::PgExecutor<'e>,
    id: Uuid,
) -> Result<Option<Workflow>, DbError> {
    const SQL: &str = concat!("SELECT ", wf_cols!(), " FROM workflows WHERE id = $1");
    Ok(sqlx::query_as::<_, Workflow>(SQL)
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

pub async fn get_runs<'e>(
    pool: impl sqlx::PgExecutor<'e>,
    workflow_id: Uuid,
) -> Result<Vec<AgentRun>, DbError> {
    Ok(sqlx::query_as::<_, AgentRun>(
        "SELECT id, agent, status, a2a_task_id, a2a_context_id, a2a_state, error, error_code, \
         started_at, finished_at FROM agent_runs WHERE workflow_id = $1 ORDER BY started_at",
    )
    .bind(workflow_id)
    .fetch_all(pool)
    .await?)
}

pub async fn get_artifacts<'e>(
    pool: impl sqlx::PgExecutor<'e>,
    workflow_id: Uuid,
) -> Result<Vec<StoredArtifact>, DbError> {
    Ok(sqlx::query_as::<_, StoredArtifact>(
        "SELECT kind, agent_run_id, content, created_at FROM artifacts WHERE workflow_id = $1 ORDER BY created_at",
    )
    .bind(workflow_id)
    .fetch_all(pool)
    .await?)
}

/// Workflow, runs and artifacts read from one consistent snapshot, so a
/// concurrently advancing workflow never appears half-updated.
pub async fn get_detail(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<(Workflow, Vec<AgentRun>, Vec<StoredArtifact>)>, DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let Some(wf) = get_workflow(&mut *tx, id).await? else {
        return Ok(None);
    };
    let runs = get_runs(&mut *tx, id).await?;
    let artifacts = get_artifacts(&mut *tx, id).await?;
    tx.commit().await?;
    Ok(Some((wf, runs, artifacts)))
}

pub async fn ping(pool: &PgPool) -> Result<(), DbError> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}
