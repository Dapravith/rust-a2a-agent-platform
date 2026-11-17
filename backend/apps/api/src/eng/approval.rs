//! Human approval engine.
//!
//! A decision is accepted only when, inside one transaction holding the
//! workflow row lock:
//! 1. the principal was authenticated server-side and holds the role the
//!    state machine requires for this gate (agents and the system never can);
//! 2. the request is for the current `workflow_version` (stale or concurrent
//!    decisions are refused, and the DB allows one decision per version);
//! 3. it references the exact artifact the human reviewed: the plan hash at
//!    the plan gate, the delivery-manifest hash *and* candidate commit at the
//!    delivery gate (any material change produces a new hash and version, so
//!    a new approval is required);
//! 4. for a delivery approval, every mandatory quality gate passes.
//!
//! The same `Idempotency-Key` with the same request returns the recorded
//! decision; with a different request it is refused.

use engineering::artifacts::{
    self, ArtifactKind, CodeCandidate, QaReport, QualityAssessment, RequirementsPlan, ReviewReport,
};
use engineering::gates::{self, Evidence, GateResult};
use engineering::state::{self, Event, HumanRole, Status};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::auth::Principal;
use super::store::{self, Changes, EngApproval, EngArtifact, NewArtifact, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    Plan,
    Delivery,
    Workflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    RequestChanges,
    Reject,
    Cancel,
}

/// Request body. There is intentionally no approver field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    pub gate: GateKind,
    pub decision: Decision,
    pub expected_version: i64,
    #[serde(default)]
    pub artifact_hash: Option<String>,
    #[serde(default)]
    pub reviewed_commit_sha: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
}

fn event_for(gate: GateKind, d: Decision) -> Option<Event> {
    Some(match (gate, d) {
        (GateKind::Plan, Decision::Approve) => Event::PlanApproved,
        (GateKind::Plan, Decision::RequestChanges) => Event::PlanChangesRequested,
        (GateKind::Delivery, Decision::Approve) => Event::DeliveryApproved,
        (GateKind::Delivery, Decision::RequestChanges) => Event::DeliveryChangesRequested,
        (GateKind::Delivery, Decision::Reject) => Event::DeliveryRejected,
        (GateKind::Workflow, Decision::Cancel) => Event::Cancel,
        _ => return None,
    })
}

fn request_hash(workflow_id: Uuid, req: &DecisionRequest) -> String {
    let v = json!({"workflow_id": workflow_id, "request": req});
    hex::encode(Sha256::digest(artifacts::canonical_json(&v).as_bytes()))
}

fn decode<T: serde::de::DeserializeOwned + artifacts::Contract>(
    a: Option<&EngArtifact>,
) -> Option<T> {
    a.and_then(|a| artifacts::decode::<T>(&a.content).ok())
}

/// Mandatory gates for the workflow's current candidate.
pub fn evaluate_gates(wf: &store::EngWorkflow, arts: &[EngArtifact]) -> Vec<GateResult> {
    let sha = wf.candidate_commit_sha.as_deref().unwrap_or_default();
    let candidate: Option<CodeCandidate> =
        decode(store::latest_for(arts, ArtifactKind::CodeCandidate, sha));
    let review: Option<ReviewReport> =
        decode(store::latest_for(arts, ArtifactKind::ReviewReport, sha));
    let qa: Option<QaReport> = decode(store::latest_for(arts, ArtifactKind::QaReport, sha));
    let assessment: Option<QualityAssessment> = decode(store::latest_for(
        arts,
        ArtifactKind::QualityAssessment,
        sha,
    ));
    let criteria: Vec<String> =
        decode::<RequirementsPlan>(store::latest(arts, ArtifactKind::RequirementsPlan))
            .map(|p| p.acceptance_criteria.into_iter().map(|c| c.id).collect())
            .unwrap_or_default();
    gates::evaluate(&Evidence {
        candidate: candidate.as_ref(),
        review: review.as_ref(),
        qa: qa.as_ref(),
        assessment: assessment.as_ref(),
        criteria: &criteria,
    })
}

/// Records a human decision. Returns `(approval, replayed)`.
pub async fn decide(
    pool: &PgPool,
    who: &Principal,
    workflow_id: Uuid,
    idempotency_key: &str,
    req: &DecisionRequest,
) -> Result<(EngApproval, bool), StoreError> {
    let rhash = request_hash(workflow_id, req);
    let mut tx = pool.begin().await?;
    let wf = store::lock(&mut tx, workflow_id).await?;

    // Idempotent replay (checked under the row lock, so it is race-free).
    if let Some(existing) = store::approvals(&mut *tx, workflow_id)
        .await?
        .into_iter()
        .find(|a| a.idempotency_key == idempotency_key)
    {
        return if existing.request_hash == rhash {
            Ok((existing, true))
        } else {
            Err(StoreError::IdempotencyConflict)
        };
    }

    let event = event_for(req.gate, req.decision)
        .ok_or(StoreError::Mismatch("decision is not valid for this gate"))?;
    // Authorization and state validity first (agents/system never pass this).
    let actor = who.actor();
    state::next(&wf.state()?, event, &actor, &wf.budget())?;
    if req.expected_version != wf.version {
        return Err(StoreError::Stale {
            expected: req.expected_version,
            actual: wf.version,
        });
    }
    if req.decision == Decision::Cancel
        && !who.has(HumanRole::Approver)
        && who.subject != wf.requested_by
    {
        return Err(StoreError::Mismatch(
            "requesters may only cancel their own workflows",
        ));
    }
    let comment = req
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    if matches!(req.decision, Decision::RequestChanges | Decision::Reject) && comment.is_none() {
        return Err(StoreError::Mismatch(
            "a comment is required to request changes or reject",
        ));
    }
    if comment.is_some_and(|c| {
        c.chars().count() > 2000 || c.chars().any(|ch| ch.is_control() && ch != '\n')
    }) {
        return Err(StoreError::Mismatch(
            "comment must be at most 2000 printable characters",
        ));
    }
    if req.gate != GateKind::Workflow
        && (req.artifact_hash.is_none() || req.artifact_hash != wf.pending_decision_hash)
    {
        return Err(StoreError::Mismatch(
            "artifact_hash does not match the artifact awaiting this decision",
        ));
    }
    if req.gate == GateKind::Delivery
        && (req.reviewed_commit_sha.is_none() || req.reviewed_commit_sha != wf.candidate_commit_sha)
    {
        return Err(StoreError::Mismatch(
            "reviewed_commit_sha does not match the current candidate",
        ));
    }
    let gate_results = if event == Event::DeliveryApproved {
        let arts = store::artifacts(&mut *tx, workflow_id).await?;
        let g = evaluate_gates(&wf, &arts);
        if !gates::all_pass(&g) {
            return Err(StoreError::GatesFailed(g));
        }
        Some(g)
    } else {
        None
    };

    let approval_id = Uuid::now_v7();
    let approver_role = if who.has(HumanRole::Approver) {
        HumanRole::Approver
    } else {
        HumanRole::Requester
    };
    let reviewed = if req.gate == GateKind::Delivery {
        wf.candidate_commit_sha.as_deref()
    } else {
        None
    };
    let artifact_hash = if req.gate == GateKind::Workflow {
        None
    } else {
        req.artifact_hash.as_deref()
    };
    let content = json!({
        "approval_id": approval_id, "gate": req.gate, "decision": req.decision,
        "approver_id": who.subject, "approver_role": approver_role.as_str(), "identity_issuer": who.issuer,
        "comment": comment, "reviewed_commit_sha": reviewed, "artifact_hash": artifact_hash,
        "workflow_version": wf.version, "gates": gate_results,
    });
    let (artifact_id, _) = store::insert_artifact(
        &mut tx,
        &wf,
        NewArtifact {
            kind: ArtifactKind::ApprovalDecision,
            run_id: None,
            producer_kind: "human",
            producer_id: &who.subject,
            provider: None,
            model: None,
            content: &content,
            commit_sha: reviewed,
        },
    )
    .await?;
    let ins = sqlx::query(
        "INSERT INTO eng_approvals (id, workflow_id, gate, decision, approver_id, approver_role, identity_issuer, \
         comment, reviewed_commit_sha, artifact_hash, workflow_version, idempotency_key, request_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(approval_id)
    .bind(workflow_id)
    .bind(match req.gate { GateKind::Plan => "plan", GateKind::Delivery => "delivery", GateKind::Workflow => "workflow" })
    .bind(match req.decision {
        Decision::Approve => "approve",
        Decision::RequestChanges => "request_changes",
        Decision::Reject => "reject",
        Decision::Cancel => "cancel",
    })
    .bind(&who.subject)
    .bind(approver_role.as_str())
    .bind(&who.issuer)
    .bind(comment)
    .bind(reviewed)
    .bind(artifact_hash)
    .bind(wf.version)
    .bind(idempotency_key)
    .bind(&rhash)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db)) = &ins
        && db.is_unique_violation()
    {
        return Err(StoreError::Stale {
            expected: req.expected_version,
            actual: wf.version + 1,
        });
    }
    ins?;
    let pending = match event {
        // Keep the candidate binding until a new candidate replaces it.
        Event::DeliveryApproved | Event::DeliveryRejected | Event::Cancel => {
            wf.pending_decision_hash.as_deref()
        }
        _ => None,
    };
    store::transition(
        &mut tx,
        &wf,
        event,
        &actor,
        Changes { pending_decision_hash: Some(pending), ..Changes::default() },
        json!({"approval_id": approval_id, "artifact_id": artifact_id, "decision": req.decision, "gate": req.gate}),
    )
    .await?;
    tx.commit().await?;
    let approval = store::approvals(pool, workflow_id)
        .await?
        .into_iter()
        .find(|a| a.id == approval_id)
        .ok_or(StoreError::Corrupt("approval vanished"))?;
    Ok((approval, false))
}

/// Statuses in which the next step is a human's.
pub fn awaiting(status: Status) -> Option<GateKind> {
    match status {
        Status::AwaitingPlanApproval => Some(GateKind::Plan),
        Status::AwaitingDeliveryApproval => Some(GateKind::Delivery),
        _ => None,
    }
}
