//! Persistence (SQLx, parameterized queries only). The orchestrator is the
//! sole owner of the database.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

const ACTIVE_INDEX: &str = "workflows_single_active";
const IDEMPOTENCY_CONSTRAINT: &str = "workflows_idempotency_key_key";

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("another workflow is already queued or running")]
    ActiveWorkflowExists,
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
        "id, question, status, verdict, error, created_at, started_at, finished_at"
    };
}

fn constraint_of(e: &sqlx::Error) -> Option<String> {
    match e {
        sqlx::Error::Database(db) => db.constraint().map(str::to_owned),
        _ => None,
    }
}

/// Inserts a queued workflow. Returns `(workflow, created)`; `created` is
/// false when the idempotency key matched an existing workflow.
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
                Ok((w, false))
            }
            Some(ACTIVE_INDEX) => Err(DbError::ActiveWorkflowExists),
            _ => Err(e.into()),
        },
    }
}

pub async fn mark_running(pool: &PgPool, id: Uuid) -> Result<(), DbError> {
    sqlx::query("UPDATE workflows SET status = 'running', started_at = now() WHERE id = $1 AND status = 'queued'")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn complete_workflow(pool: &PgPool, id: Uuid, verdict: &str) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE workflows SET status = 'completed', verdict = $2, finished_at = now() \
         WHERE id = $1 AND status = 'running'",
    )
    .bind(id)
    .bind(verdict)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn fail_workflow(pool: &PgPool, id: Uuid, error: &str) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE workflows SET status = 'failed', error = left($2, 1000), finished_at = now() \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

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

/// Marks a run completed and stores its artifact atomically.
/// What a successful agent run produced over A2A.
pub struct RunResult<'a> {
    pub task_id: Option<&'a str>,
    pub context_id: Option<&'a str>,
    pub a2a_state: &'a str,
    pub kind: &'a str,
    pub content: &'a Value,
}

pub async fn complete_run(
    pool: &PgPool,
    workflow_id: Uuid,
    run_id: Uuid,
    result: RunResult<'_>,
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE agent_runs SET status = 'completed', a2a_task_id = $2, a2a_context_id = $3, \
         a2a_state = $4, finished_at = now() WHERE id = $1",
    )
    .bind(run_id)
    .bind(result.task_id)
    .bind(result.context_id)
    .bind(result.a2a_state)
    .execute(&mut *tx)
    .await?;
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

pub async fn fail_run(pool: &PgPool, run_id: Uuid, error: &str) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE agent_runs SET status = 'failed', error = left($2, 1000), finished_at = now() WHERE id = $1",
    )
    .bind(run_id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

/// Restart recovery: anything left queued/running by a previous process is
/// failed (never silently re-executed). Returns the number of workflows.
pub async fn recover_interrupted(pool: &PgPool) -> Result<u64, DbError> {
    const MSG: &str = "Interrupted by an orchestrator restart; resubmit to run again.";
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE agent_runs SET status = 'failed', error = $1, finished_at = now() WHERE status = 'running'",
    )
    .bind(MSG)
    .execute(&mut *tx)
    .await?;
    let n = sqlx::query(
        "UPDATE workflows SET status = 'failed', error = $1, finished_at = now() \
         WHERE status IN ('queued', 'running')",
    )
    .bind(MSG)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(n)
}

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
        "SELECT id, agent, status, a2a_task_id, a2a_context_id, a2a_state, error, started_at, finished_at \
         FROM agent_runs WHERE workflow_id = $1 ORDER BY started_at",
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
