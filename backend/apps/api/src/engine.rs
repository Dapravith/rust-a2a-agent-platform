//! Deterministic Worker -> Reviewer workflow.
//!
//! States: `queued -> running -> completed | failed`. A reviewer verdict of
//! `needs_changes` is a successful review, so the workflow is `completed`.

use contracts::{
    DRAFT_ARTIFACT_NAME, DraftArtifact, REVIEW_ARTIFACT_NAME, REVIEWER_AGENT_NAME, ReviewArtifact,
    ReviewerRequest, Verdict, WORKER_AGENT_NAME, WorkerRequest, data_part,
};
use sqlx::PgPool;
use tracing::Instrument;
use uuid::Uuid;

use crate::a2a::{A2AGateway, AgentEndpoint};
use crate::config::Config;
use crate::db::{self, DbError, Workflow};

#[derive(Clone)]
pub struct Engine {
    pub pool: PgPool,
    pub gateway: A2AGateway,
    pub worker: AgentEndpoint,
    pub reviewer: AgentEndpoint,
}

#[derive(Debug, thiserror::Error)]
/// `Display` is what clients see (stored in `workflow.error` / run `error`);
/// it never includes database or internal details. `Debug` keeps the
/// details and is only written to the server logs.
enum StepError {
    /// Already sanitized by [`crate::a2a::AgentError::safe_message`].
    #[error("{0}")]
    Agent(String),
    #[error("internal error: database operation failed")]
    Db(#[from] DbError),
    #[error("internal error")]
    Internal(String),
}

impl Engine {
    pub fn new(pool: PgPool, cfg: &Config) -> Result<Self, String> {
        Ok(Self {
            pool,
            gateway: A2AGateway::new(cfg.agent_timeout, cfg.poll_interval)?,
            worker: AgentEndpoint {
                expected_name: WORKER_AGENT_NAME.into(),
                base_url: cfg.worker_url.clone(),
            },
            reviewer: AgentEndpoint {
                expected_name: REVIEWER_AGENT_NAME.into(),
                base_url: cfg.reviewer_url.clone(),
            },
        })
    }

    /// Admits a new workflow and starts it in the background.
    /// Returns `(workflow, created)`.
    pub async fn submit(
        &self,
        question: &str,
        idempotency_key: Option<&str>,
    ) -> Result<(Workflow, bool), DbError> {
        let (wf, created) = db::create_workflow(&self.pool, question, idempotency_key).await?;
        if created {
            let engine = self.clone();
            let (id, question) = (wf.id, wf.question.clone());
            tokio::spawn(async move { engine.supervise(id, question).await });
        }
        Ok((wf, created))
    }

    /// Runs the workflow in its own task so that even a panic ends in a
    /// recorded `failed` state instead of a stuck `running` row.
    async fn supervise(self, id: Uuid, question: String) {
        let span = tracing::info_span!("workflow", workflow_id = %id);
        let inner = self.clone();
        let handle =
            tokio::spawn(async move { inner.execute(id, question).await }.instrument(span.clone()));
        let outcome = match handle.await {
            Ok(r) => r,
            Err(join) => Err(StepError::Internal(format!(
                "workflow task aborted: {join}"
            ))),
        };
        let _guard = span.enter();
        match outcome {
            Ok(verdict) => tracing::info!(outcome = "completed", verdict, "workflow finished"),
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(outcome = "failed", error = %msg, detail = ?e, "workflow failed");
                if let Err(db_err) = db::fail_workflow(&self.pool, id, &msg).await {
                    tracing::error!(error = %db_err, "could not record workflow failure");
                }
            }
        }
    }

    async fn execute(&self, id: Uuid, question: String) -> Result<&'static str, StepError> {
        db::mark_running(&self.pool, id).await?;

        let draft: DraftArtifact = self
            .step(
                id,
                "worker",
                &self.worker,
                DRAFT_ARTIFACT_NAME,
                "draft",
                &WorkerRequest {
                    question: question.clone(),
                },
            )
            .await?;

        let review: ReviewArtifact = self
            .step(
                id,
                "reviewer",
                &self.reviewer,
                REVIEW_ARTIFACT_NAME,
                "review",
                &ReviewerRequest { question, draft },
            )
            .await?;

        let verdict = match review.verdict {
            Verdict::Pass => "pass",
            Verdict::NeedsChanges => "needs_changes",
        };
        db::complete_workflow(&self.pool, id, verdict).await?;
        Ok(verdict)
    }

    /// One agent call: record the run, call over A2A, persist the artifact.
    /// Once the run is recorded, *any* failure marks it `failed`, so a failed
    /// workflow never shows a run stuck in `running`.
    async fn step<Req, Out>(
        &self,
        workflow_id: Uuid,
        agent: &str,
        endpoint: &AgentEndpoint,
        artifact_name: &str,
        kind: &str,
        request: &Req,
    ) -> Result<Out, StepError>
    where
        Req: serde::Serialize,
        Out: serde::de::DeserializeOwned + serde::Serialize + contracts::Validate,
    {
        let run_id = db::start_run(&self.pool, workflow_id, agent).await?;
        let result = self
            .run_step(
                workflow_id,
                run_id,
                agent,
                endpoint,
                artifact_name,
                kind,
                request,
            )
            .await;
        if let Err(e) = &result {
            tracing::warn!(agent, outcome = "failed", error = %e, detail = ?e, "agent step failed");
            if let Err(db_err) = db::fail_run(&self.pool, run_id, &e.to_string()).await {
                tracing::error!(agent, error = %db_err, "could not record agent run failure");
            }
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_step<Req, Out>(
        &self,
        workflow_id: Uuid,
        run_id: Uuid,
        agent: &str,
        endpoint: &AgentEndpoint,
        artifact_name: &str,
        kind: &str,
        request: &Req,
    ) -> Result<Out, StepError>
    where
        Req: serde::Serialize,
        Out: serde::de::DeserializeOwned + serde::Serialize + contracts::Validate,
    {
        let part = data_part(request).map_err(|e| StepError::Internal(e.to_string()))?;
        tracing::info!(agent, "calling agent over A2A");
        let outcome = self
            .gateway
            .call::<Out>(endpoint, part, artifact_name)
            .await
            .map_err(|e| {
                tracing::warn!(agent, detail = %e, "A2A call failed");
                StepError::Agent(format!("{agent}: {}", e.safe_message()))
            })?;
        let content =
            serde_json::to_value(&outcome.value).map_err(|e| StepError::Internal(e.to_string()))?;
        db::complete_run(
            &self.pool,
            workflow_id,
            run_id,
            db::RunResult {
                task_id: outcome.task_id.as_deref(),
                context_id: outcome.context_id.as_deref(),
                a2a_state: &outcome.state,
                kind,
                content: &content,
            },
        )
        .await?;
        tracing::info!(agent, a2a_state = %outcome.state, outcome = "completed", "agent step done");
        Ok(outcome.value)
    }
}
