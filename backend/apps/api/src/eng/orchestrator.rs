//! Deterministic orchestration of the engineering workflow.
//!
//! `advance` runs agent stages in role order until the workflow reaches a
//! human gate or a terminal state. Each stage: reserve budget + insert a
//! `running` run (duplicate-execution guard), call the adapter under a
//! deadline, validate the untrusted output (tool policy, contract, candidate
//! identity, separation of duties), then commit artifact + run + transition
//! in one transaction. Contract/policy violations fail the workflow without
//! retry; only `Unavailable` and timeouts are retried, within budget.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use engineering::artifacts::{
    self, ArtifactKind, CodeCandidate, DeliveryManifest, ManifestEntry, RequirementsPlan,
    WorkAssignment,
};
use engineering::roles::{self, PolicyError, Role};
use engineering::state::{Actor, Event, Status, TransitionError};
use serde_json::{Value, json};
use sqlx::PgPool;
use tracing::Instrument;
use uuid::Uuid;

use super::agents::{
    AgentErrorKind, AgentIdentity, AgentOutput, RoleAgent, RoleInput, readable_kinds,
};
use super::store::{
    self, Changes, EngArtifact, EngErrorCode, EngWorkflow, NewArtifact, RunStart, StoreError,
};

pub struct Orchestrator {
    pub pool: PgPool,
    agents: HashMap<Role, Arc<dyn RoleAgent>>,
    pub stage_timeout: Duration,
    pub max_attempts: i32,
    /// Repository base commit for work assignments (configuration until the
    /// M5 workspace provides it).
    pub base_commit_sha: Option<String>,
}

enum StageError {
    /// Another loop owns this stage, or the workflow moved on: stop quietly.
    Yield,
    Fail(EngErrorCode, String),
    Store(StoreError),
}

impl From<StoreError> for StageError {
    fn from(e: StoreError) -> Self {
        StageError::Store(e)
    }
}

struct Validated {
    kind: ArtifactKind,
    commit_sha: Option<String>,
    event: Option<Event>,
}

fn policy_code(e: &PolicyError) -> EngErrorCode {
    match e {
        PolicyError::ToolNotPermitted { .. } | PolicyError::CheckNotAllowed(_) => {
            EngErrorCode::ToolNotPermitted
        }
        _ => EngErrorCode::PolicyViolation,
    }
}

fn decode<T: serde::de::DeserializeOwned + artifacts::Contract>(
    a: Option<&EngArtifact>,
) -> Option<T> {
    a.and_then(|a| artifacts::decode::<T>(&a.content).ok())
}

impl Orchestrator {
    pub fn with_agents(pool: PgPool, agents: HashMap<Role, Arc<dyn RoleAgent>>) -> Self {
        Orchestrator {
            pool,
            agents,
            stage_timeout: Duration::from_secs(600),
            max_attempts: 2,
            base_commit_sha: None,
        }
    }

    pub fn identities(&self) -> Vec<(Role, AgentIdentity)> {
        Role::ALL
            .into_iter()
            .filter_map(|r| self.agents.get(&r).map(|a| (r, a.identity())))
            .collect()
    }

    /// Runs stages until a human gate or terminal state. Safe to call
    /// concurrently and repeatedly: duplicate stage starts are refused by the
    /// database and the losing caller just returns.
    pub async fn advance(&self, id: Uuid) -> Result<EngWorkflow, StoreError> {
        let span = tracing::info_span!("eng_workflow", workflow_id = %id);
        async move {
            loop {
                let wf = store::get(&self.pool, id).await?;
                let status = wf.status()?;
                if status.is_terminal() || status.is_human_gate() {
                    return Ok(wf);
                }
                let arts = store::artifacts(&self.pool, id).await?;
                let role = next_role(status, &wf, &arts);
                match self.stage(&wf, status, role, &arts).await {
                    Ok(()) => {}
                    Err(StageError::Yield) => return store::get(&self.pool, id).await,
                    Err(StageError::Fail(code, msg)) => {
                        tracing::warn!(role = role.as_str(), error_code = code.as_str(), error = %msg, "stage failed");
                        match store::fail_workflow(&self.pool, id, code, &msg).await {
                            Ok(()) | Err(StoreError::Transition(_)) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    Err(StageError::Store(e)) => return Err(e),
                }
            }
        }
        .instrument(span)
        .await
    }

    async fn stage(
        &self,
        wf: &EngWorkflow,
        status: Status,
        role: Role,
        arts: &[EngArtifact],
    ) -> Result<(), StageError> {
        let Some(agent) = self.agents.get(&role) else {
            return Err(StageError::Fail(
                EngErrorCode::ProviderUnconfigured,
                format!("no provider is configured for role `{}`", role.as_str()),
            ));
        };
        let ident = agent.identity();
        for attempt in 1..=self.max_attempts {
            let (run_id, correlation_id) = match store::start_run(
                &self.pool,
                wf.id,
                RunStart {
                    role,
                    expected_status: status,
                    expected_version: wf.version,
                    attempt,
                    agent_id: &ident.agent_id,
                    provider: &ident.provider,
                    model: ident.model.as_deref(),
                },
            )
            .await
            {
                Ok(r) => r,
                Err(StoreError::DuplicateRun | StoreError::Stale { .. }) => {
                    return Err(StageError::Yield);
                }
                Err(StoreError::Transition(TransitionError::BudgetExceeded(what))) => {
                    return Err(StageError::Fail(
                        EngErrorCode::BudgetExceeded,
                        format!("budget exceeded: {what}"),
                    ));
                }
                Err(e) => return Err(e.into()),
            };
            let mut input = build_input(wf, role, attempt, correlation_id, arts);
            input.base_commit_sha = self.base_commit_sha.clone();
            let result = tokio::time::timeout(self.stage_timeout, agent.run(input)).await;
            let (code, msg, retryable) = match result {
                Err(_) => (
                    EngErrorCode::AgentTimeout,
                    format!("no result within {:?}", self.stage_timeout),
                    true,
                ),
                Ok(Err(e)) => match e.kind {
                    AgentErrorKind::Unavailable => {
                        (EngErrorCode::AgentUnavailable, e.message, true)
                    }
                    AgentErrorKind::Unauthenticated | AgentErrorKind::Failed => {
                        (EngErrorCode::AgentFailed, e.message, false)
                    }
                },
                Ok(Ok(out)) => match self.validate(wf, role, &ident, &out, arts) {
                    Ok(v) => return self.commit(wf, role, &ident, run_id, &out, v, arts).await,
                    Err((code, msg)) => (code, msg, false),
                },
            };
            store::fail_run(&self.pool, wf.id, run_id, code, &msg).await?;
            if !(retryable && attempt < self.max_attempts) {
                return Err(StageError::Fail(
                    code,
                    format!("{} stage: {msg}", role.as_str()),
                ));
            }
        }
        Err(StageError::Fail(
            EngErrorCode::InternalError,
            "no attempts configured".into(),
        ))
    }

    /// Validates untrusted agent output. Never trusts anything the agent
    /// claims about authority: only content and requested tools.
    fn validate(
        &self,
        wf: &EngWorkflow,
        role: Role,
        ident: &AgentIdentity,
        out: &AgentOutput,
        arts: &[EngArtifact],
    ) -> Result<Validated, (EngErrorCode, String)> {
        let plan: Option<RequirementsPlan> =
            decode(store::latest(arts, ArtifactKind::RequirementsPlan));
        let assignment: Option<WorkAssignment> =
            decode(store::latest(arts, ArtifactKind::WorkAssignment));
        let scope: Vec<String> = match role {
            Role::Developer | Role::CodeReviewer | Role::QaTester => {
                assignment.as_ref().map(|a| a.scope.clone())
            }
            _ => plan.as_ref().map(|p| p.scope.clone()),
        }
        .unwrap_or_default();
        for req in &out.tool_requests {
            roles::authorize(role, req, &scope).map_err(|e| (policy_code(&e), e.to_string()))?;
        }
        let kind = role.produces();
        let commit_sha = artifacts::validate_kind(kind, &out.content)
            .map_err(|e| (EngErrorCode::ArtifactInvalid, e.to_string()))?;
        let candidate = wf.candidate_commit_sha.as_deref();
        let mismatch = |what: &str| {
            (
                EngErrorCode::CandidateMismatch,
                format!("{what} does not match the candidate commit"),
            )
        };
        let developer_id = || {
            candidate
                .and_then(|sha| store::latest_for(arts, ArtifactKind::CodeCandidate, sha))
                .map(|a| a.producer_id.clone())
        };
        let event = match role {
            Role::ProjectManager => Some(Event::PlanProposed),
            Role::ProjectCoordinator => {
                let a: WorkAssignment = artifacts::decode(&out.content)
                    .map_err(|e| (EngErrorCode::ArtifactInvalid, e.to_string()))?;
                let plan =
                    plan.ok_or((EngErrorCode::InternalError, "no approved plan".to_owned()))?;
                if !a.scope.iter().all(|p| roles::path_in_scope(p, &plan.scope)) {
                    return Err((
                        EngErrorCode::PolicyViolation,
                        "assignment scope exceeds the approved plan".into(),
                    ));
                }
                let ids: Vec<_> = plan
                    .acceptance_criteria
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect();
                if !a
                    .acceptance_criteria
                    .iter()
                    .all(|c| ids.contains(&c.as_str()))
                {
                    return Err((
                        EngErrorCode::ArtifactInvalid,
                        "assignment cites unknown acceptance criteria".into(),
                    ));
                }
                Some(Event::WorkAssigned)
            }
            Role::Developer => {
                let c: CodeCandidate = artifacts::decode(&out.content)
                    .map_err(|e| (EngErrorCode::ArtifactInvalid, e.to_string()))?;
                let a = assignment
                    .ok_or((EngErrorCode::InternalError, "no work assignment".to_owned()))?;
                roles::authorize_branch(wf.id, &c.branch)
                    .map_err(|e| (EngErrorCode::PolicyViolation, e.to_string()))?;
                if c.base_commit_sha != a.base_commit_sha {
                    return Err((
                        EngErrorCode::CandidateMismatch,
                        "candidate is not based on the assigned base commit".into(),
                    ));
                }
                if let Some(f) = c
                    .changed_files
                    .iter()
                    .find(|f| !roles::path_in_scope(&f.path, &a.scope))
                {
                    return Err((
                        EngErrorCode::PolicyViolation,
                        format!("changed file `{}` is outside the authorized scope", f.path),
                    ));
                }
                Some(Event::CandidateProduced)
            }
            Role::CodeReviewer | Role::QaTester | Role::QualityLead => {
                if commit_sha.as_deref() != candidate || candidate.is_none() {
                    return Err(mismatch(role.as_str()));
                }
                if role != Role::QualityLead {
                    let dev = developer_id().ok_or((
                        EngErrorCode::InternalError,
                        "candidate has no producer".to_owned(),
                    ))?;
                    roles::ensure_independent(&dev, &ident.agent_id)
                        .map_err(|e| (EngErrorCode::PolicyViolation, e.to_string()))?;
                }
                match role {
                    Role::CodeReviewer => Some(Event::ReviewRecorded),
                    Role::QaTester => None,
                    _ => Some(Event::EvidenceAssessed),
                }
            }
        };
        Ok(Validated {
            kind,
            commit_sha,
            event,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit(
        &self,
        wf: &EngWorkflow,
        role: Role,
        ident: &AgentIdentity,
        run_id: Uuid,
        out: &AgentOutput,
        v: Validated,
        arts: &[EngArtifact],
    ) -> Result<(), StageError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::from)?;
        let locked = store::lock(&mut tx, wf.id).await?;
        if locked.version != wf.version || locked.status != wf.status {
            drop(tx);
            store::fail_run(
                &self.pool,
                wf.id,
                run_id,
                EngErrorCode::InternalError,
                "workflow changed while the stage ran (e.g. cancelled)",
            )
            .await?;
            return Err(StageError::Yield);
        }
        let (artifact_id, hash) = store::insert_artifact(
            &mut tx,
            &locked,
            NewArtifact {
                kind: v.kind,
                run_id: Some(run_id),
                producer_kind: "agent",
                producer_id: &ident.agent_id,
                provider: Some(&ident.provider),
                model: ident.model.as_deref(),
                content: &out.content,
                commit_sha: v.commit_sha.as_deref(),
            },
        )
        .await?;
        store::complete_run_tx(
            &mut tx,
            run_id,
            out.a2a_task_id.as_deref(),
            out.usage.input_tokens,
            out.usage.output_tokens,
        )
        .await?;
        store::audit(
            &mut tx,
            wf.id,
            "agent",
            &ident.agent_id,
            "artifact_recorded",
            None,
            None,
            locked.version,
            json!({"run_id": run_id, "artifact_id": artifact_id, "kind": v.kind.as_str(), "content_hash": hash,
                   "commit_sha": v.commit_sha, "tools": out.tool_requests}),
        )
        .await?;
        if let Some(event) = v.event {
            let manifest_hash;
            let changes = match event {
                Event::PlanProposed => Changes {
                    pending_decision_hash: Some(Some(&hash)),
                    candidate_commit_sha: Some(None),
                    ..Changes::default()
                },
                Event::CandidateProduced => Changes {
                    candidate_commit_sha: Some(v.commit_sha.as_deref()),
                    pending_decision_hash: Some(None),
                    ..Changes::default()
                },
                Event::EvidenceAssessed => {
                    manifest_hash =
                        build_manifest(&mut tx, &locked, arts, artifact_id, &hash).await?;
                    Changes {
                        pending_decision_hash: Some(Some(&manifest_hash)),
                        ..Changes::default()
                    }
                }
                _ => Changes {
                    pending_decision_hash: Some(None),
                    ..Changes::default()
                },
            };
            let actor = Actor::Agent {
                role,
                agent_id: ident.agent_id.clone(),
            };
            store::transition(
                &mut tx,
                &locked,
                event,
                &actor,
                changes,
                json!({"artifact_id": artifact_id}),
            )
            .await?;
        }
        tx.commit().await.map_err(StoreError::from)?;
        Ok(())
    }
}

/// QA runs first in `testing`; once a QA report exists for the candidate,
/// the Quality Lead runs.
fn next_role(status: Status, wf: &EngWorkflow, arts: &[EngArtifact]) -> Role {
    if status == Status::Testing {
        let qa_done = wf
            .candidate_commit_sha
            .as_deref()
            .is_some_and(|sha| store::latest_for(arts, ArtifactKind::QaReport, sha).is_some());
        return if qa_done {
            Role::QualityLead
        } else {
            Role::QaTester
        };
    }
    status.active_role().unwrap_or(Role::ProjectCoordinator)
}

fn build_input(
    wf: &EngWorkflow,
    role: Role,
    attempt: i32,
    correlation_id: Uuid,
    arts: &[EngArtifact],
) -> RoleInput {
    let candidate = wf.candidate_commit_sha.clone();
    let artifacts: Vec<(ArtifactKind, Value)> = readable_kinds(role)
        .iter()
        .filter_map(|k| {
            let a = match (k, candidate.as_deref()) {
                (
                    ArtifactKind::CodeCandidate
                    | ArtifactKind::ReviewReport
                    | ArtifactKind::QaReport,
                    Some(sha),
                ) => store::latest_for(arts, *k, sha),
                _ => store::latest(arts, *k),
            };
            a.map(|a| (*k, a.content.clone()))
        })
        .collect();
    let scope = match role {
        Role::ProjectManager => vec![],
        Role::ProjectCoordinator => {
            decode::<RequirementsPlan>(store::latest(arts, ArtifactKind::RequirementsPlan))
                .map(|p| p.scope)
                .unwrap_or_default()
        }
        _ => decode::<WorkAssignment>(store::latest(arts, ArtifactKind::WorkAssignment))
            .map(|a| a.scope)
            .unwrap_or_default(),
    };
    RoleInput {
        workflow_id: wf.id,
        correlation_id,
        role,
        attempt,
        revision: u32::try_from(wf.delivery_revisions).unwrap_or(0),
        requirements: wf.requirements.clone(),
        artifacts,
        scope,
        candidate_commit_sha: candidate,
        base_commit_sha: None,
        patch: None,
    }
}

/// Binds the exact evidence for the candidate into one hash the human must
/// reference when deciding.
async fn build_manifest(
    tx: &mut store::Tx<'_>,
    wf: &EngWorkflow,
    arts: &[EngArtifact],
    assessment_id: Uuid,
    assessment_hash: &str,
) -> Result<String, StoreError> {
    let sha = wf
        .candidate_commit_sha
        .clone()
        .ok_or(StoreError::Mismatch("no candidate"))?;
    let mut entries = Vec::new();
    for kind in [ArtifactKind::RequirementsPlan, ArtifactKind::WorkAssignment] {
        let a = store::latest(arts, kind).ok_or(StoreError::Mismatch("missing evidence"))?;
        entries.push(ManifestEntry {
            kind,
            artifact_id: a.id,
            content_hash: a.content_hash.clone(),
        });
    }
    for kind in [
        ArtifactKind::CodeCandidate,
        ArtifactKind::ReviewReport,
        ArtifactKind::QaReport,
    ] {
        let a = store::latest_for(arts, kind, &sha)
            .ok_or(StoreError::Mismatch("missing evidence for the candidate"))?;
        entries.push(ManifestEntry {
            kind,
            artifact_id: a.id,
            content_hash: a.content_hash.clone(),
        });
    }
    entries.push(ManifestEntry {
        kind: ArtifactKind::QualityAssessment,
        artifact_id: assessment_id,
        content_hash: assessment_hash.to_owned(),
    });
    let manifest = DeliveryManifest {
        workflow_id: wf.id,
        commit_sha: sha.clone(),
        entries,
    };
    let content = serde_json::to_value(&manifest).map_err(|_| StoreError::Corrupt("manifest"))?;
    artifacts::validate_kind(ArtifactKind::DeliveryManifest, &content)?;
    let (_, hash) = store::insert_artifact(
        tx,
        wf,
        NewArtifact {
            kind: ArtifactKind::DeliveryManifest,
            run_id: None,
            producer_kind: "system",
            producer_id: "orchestrator",
            provider: None,
            model: None,
            content: &content,
            commit_sha: Some(&sha),
        },
    )
    .await?;
    debug_assert_eq!(Ok(hash.clone()), manifest.hash());
    Ok(hash)
}
