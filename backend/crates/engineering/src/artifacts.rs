//! Typed, versioned handoff contracts.
//!
//! Every handoff is decoded with unknown fields rejected, validated, and
//! hashed over a canonical JSON encoding (object keys sorted, no
//! whitespace) so the same content always has the same hash. Hidden model
//! reasoning has no field anywhere: only observable outputs are stored.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{ContractError, check_len, check_text, invalid, validate_commit_sha, validate_sha256};

pub const SCHEMA_VERSION: i32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    RequirementsPlan,
    WorkAssignment,
    CodeCandidate,
    ReviewReport,
    QaReport,
    QualityAssessment,
    ApprovalDecision,
    DeliveryManifest,
}

impl ArtifactKind {
    pub const ALL: [ArtifactKind; 8] = [
        ArtifactKind::RequirementsPlan,
        ArtifactKind::WorkAssignment,
        ArtifactKind::CodeCandidate,
        ArtifactKind::ReviewReport,
        ArtifactKind::QaReport,
        ArtifactKind::QualityAssessment,
        ArtifactKind::ApprovalDecision,
        ArtifactKind::DeliveryManifest,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::RequirementsPlan => "requirements_plan",
            ArtifactKind::WorkAssignment => "work_assignment",
            ArtifactKind::CodeCandidate => "code_candidate",
            ArtifactKind::ReviewReport => "review_report",
            ArtifactKind::QaReport => "qa_report",
            ArtifactKind::QualityAssessment => "quality_assessment",
            ArtifactKind::ApprovalDecision => "approval_decision",
            ArtifactKind::DeliveryManifest => "delivery_manifest",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Canonical JSON: keys sorted recursively, compact. `serde_json::Map` is a
/// `BTreeMap` in this build (no `preserve_order`), but we sort explicitly so
/// the hash does not depend on a feature flag elsewhere in the graph.
pub fn canonical_json(v: &Value) -> String {
    fn canon(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for k in keys {
                    out.insert(k.clone(), canon(&m[k]));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
            other => other.clone(),
        }
    }
    canon(v).to_string()
}

/// `sha256(kind ":" schema_version ":" canonical_json(content))`, hex.
pub fn content_hash(kind: ArtifactKind, schema_version: i32, content: &Value) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_str().as_bytes());
    h.update(b":");
    h.update(schema_version.to_string().as_bytes());
    h.update(b":");
    h.update(canonical_json(content).as_bytes());
    hex::encode(h.finalize())
}

/// Decodes and validates an untrusted payload as contract `T`.
pub fn decode<T: DeserializeOwned + Contract>(v: &Value) -> Result<T, ContractError> {
    let t: T = serde_json::from_value(v.clone()).map_err(|e| ContractError::Schema {
        kind: T::KIND.as_str(),
        reason: e.to_string(),
    })?;
    t.validate()?;
    Ok(t)
}

pub trait Contract: Serialize {
    const KIND: ArtifactKind;
    fn validate(&self) -> Result<(), ContractError>;
    /// The candidate commit this artifact is about, when applicable.
    fn commit_sha(&self) -> Option<&str> {
        None
    }
}

/// Validates `content` as the contract for `kind`; returns its commit SHA.
pub fn validate_kind(kind: ArtifactKind, content: &Value) -> Result<Option<String>, ContractError> {
    fn go<T: DeserializeOwned + Contract>(v: &Value) -> Result<Option<String>, ContractError> {
        let t = decode::<T>(v)?;
        Ok(t.commit_sha().map(str::to_owned))
    }
    match kind {
        ArtifactKind::RequirementsPlan => go::<RequirementsPlan>(content),
        ArtifactKind::WorkAssignment => go::<WorkAssignment>(content),
        ArtifactKind::CodeCandidate => go::<CodeCandidate>(content),
        ArtifactKind::ReviewReport => go::<ReviewReport>(content),
        ArtifactKind::QaReport => go::<QaReport>(content),
        ArtifactKind::QualityAssessment => go::<QualityAssessment>(content),
        ArtifactKind::DeliveryManifest => go::<DeliveryManifest>(content),
        // Written by the approval engine from authenticated input only.
        ArtifactKind::ApprovalDecision => Err(invalid(
            "kind",
            "approval decisions are not agent artifacts",
        )),
    }
}

// ---------------------------------------------------------------------------
// Requirements plan (Project Manager)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceCriterion {
    /// `AC-1`, `AC-2`, ...
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementsPlan {
    pub summary: String,
    pub deliverables: Vec<String>,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    pub steps: Vec<String>,
    /// Repository path prefixes the change may touch (proposed; becomes
    /// binding only once a human approves the plan).
    pub scope: Vec<String>,
}

fn valid_criterion_id(id: &str) -> bool {
    id.strip_prefix("AC-")
        .is_some_and(|n| !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit()))
}

impl Contract for RequirementsPlan {
    const KIND: ArtifactKind = ArtifactKind::RequirementsPlan;
    fn validate(&self) -> Result<(), ContractError> {
        check_text("summary", &self.summary, 1, 2_000)?;
        check_len("deliverables", &self.deliverables, 1, 20)?;
        for d in &self.deliverables {
            check_text("deliverables[]", d, 1, 500)?;
        }
        check_len("acceptance_criteria", &self.acceptance_criteria, 1, 30)?;
        let mut seen = std::collections::HashSet::new();
        for c in &self.acceptance_criteria {
            if !valid_criterion_id(&c.id) || !seen.insert(c.id.as_str()) {
                return Err(invalid(
                    "acceptance_criteria[].id",
                    "must be unique `AC-<n>`",
                ));
            }
            check_text(
                "acceptance_criteria[].description",
                &c.description,
                1,
                1_000,
            )?;
        }
        check_len("dependencies", &self.dependencies, 0, 20)?;
        check_len("risks", &self.risks, 0, 20)?;
        for t in self.dependencies.iter().chain(&self.risks) {
            check_text("dependencies/risks[]", t, 1, 500)?;
        }
        check_len("steps", &self.steps, 1, 30)?;
        for t in &self.steps {
            check_text("steps[]", t, 1, 1_000)?;
        }
        validate_scope(&self.scope)
    }
}

fn validate_scope(scope: &[String]) -> Result<(), ContractError> {
    check_len("scope", scope, 1, 20)?;
    for p in scope {
        // A scope prefix must itself be a valid in-repo relative path.
        if !crate::roles::path_in_scope(p, std::slice::from_ref(p)) {
            return Err(invalid(
                "scope[]",
                "must be a relative path inside the repository",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Work assignment (Project Coordinator)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkAssignment {
    pub summary: String,
    /// Must be a subset of the approved plan's scope (checked by the orchestrator).
    pub scope: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    /// Check ids from [`crate::roles::ALLOWED_CHECKS`].
    pub required_checks: Vec<String>,
    pub base_commit_sha: String,
}

impl Contract for WorkAssignment {
    const KIND: ArtifactKind = ArtifactKind::WorkAssignment;
    fn validate(&self) -> Result<(), ContractError> {
        check_text("summary", &self.summary, 1, 2_000)?;
        validate_scope(&self.scope)?;
        check_len("acceptance_criteria", &self.acceptance_criteria, 1, 30)?;
        if self
            .acceptance_criteria
            .iter()
            .any(|c| !valid_criterion_id(c))
        {
            return Err(invalid("acceptance_criteria[]", "must be `AC-<n>` ids"));
        }
        check_len("required_checks", &self.required_checks, 1, 10)?;
        if let Some(c) = self
            .required_checks
            .iter()
            .find(|c| crate::roles::check_argv(c).is_none())
        {
            return Err(invalid(
                "required_checks[]",
                format!("`{c}` is not an allowed check"),
            ));
        }
        validate_commit_sha("base_commit_sha", &self.base_commit_sha)
    }
}

// ---------------------------------------------------------------------------
// Code candidate (Developer)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedFile {
    pub path: String,
    pub change: FileChange,
}

/// One executed command and its observable result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    pub check_id: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    /// Tail of combined output, already secret-redacted and size-capped.
    pub output_tail: String,
}

impl CommandResult {
    pub fn passed(&self) -> bool {
        self.exit_code == 0
    }
    fn validate(&self) -> Result<(), ContractError> {
        if crate::roles::check_argv(&self.check_id).is_none() {
            return Err(invalid("check_id", "not an allowed check"));
        }
        check_text("output_tail", &self.output_tail, 0, 8_000)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeCandidate {
    pub base_commit_sha: String,
    pub commit_sha: String,
    pub branch: String,
    pub changed_files: Vec<ChangedFile>,
    /// SHA-256 of `git diff --binary base..commit`.
    pub patch_sha256: String,
    pub checks: Vec<CommandResult>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

impl Contract for CodeCandidate {
    const KIND: ArtifactKind = ArtifactKind::CodeCandidate;
    fn validate(&self) -> Result<(), ContractError> {
        validate_commit_sha("base_commit_sha", &self.base_commit_sha)?;
        validate_commit_sha("commit_sha", &self.commit_sha)?;
        if self.commit_sha == self.base_commit_sha {
            return Err(invalid("commit_sha", "candidate must differ from its base"));
        }
        check_text("branch", &self.branch, 1, 200)?;
        check_len("changed_files", &self.changed_files, 1, 200)?;
        for f in &self.changed_files {
            check_text("changed_files[].path", &f.path, 1, 500)?;
        }
        validate_sha256("patch_sha256", &self.patch_sha256)?;
        check_len("checks", &self.checks, 0, 10)?;
        for c in &self.checks {
            c.validate()?;
        }
        check_len("limitations", &self.limitations, 0, 20)?;
        for l in &self.limitations {
            check_text("limitations[]", l, 1, 500)?;
        }
        Ok(())
    }
    fn commit_sha(&self) -> Option<&str> {
        Some(&self.commit_sha)
    }
}

// ---------------------------------------------------------------------------
// Code review report (Code Reviewer)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCategory {
    Correctness,
    Security,
    Maintainability,
    Testing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFinding {
    pub id: String,
    pub severity: FindingSeverity,
    pub category: FindingCategory,
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
    pub title: String,
    pub detail: String,
}

/// The reviewer's conclusion. Deliberately *not* "approved": a reviewer
/// produces evidence, humans approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewConclusion {
    NoBlockingFindings,
    BlockingFindings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReport {
    pub commit_sha: String,
    pub summary: String,
    pub conclusion: ReviewConclusion,
    pub findings: Vec<ReviewFinding>,
}

impl ReviewReport {
    pub fn blocking(&self) -> impl Iterator<Item = &ReviewFinding> {
        self.findings
            .iter()
            .filter(|f| f.severity >= FindingSeverity::High)
    }
}

impl Contract for ReviewReport {
    const KIND: ArtifactKind = ArtifactKind::ReviewReport;
    fn validate(&self) -> Result<(), ContractError> {
        validate_commit_sha("commit_sha", &self.commit_sha)?;
        check_text("summary", &self.summary, 1, 2_000)?;
        check_len("findings", &self.findings, 0, 50)?;
        for f in &self.findings {
            check_text("findings[].id", &f.id, 1, 40)?;
            check_text("findings[].file", &f.file, 1, 500)?;
            check_text("findings[].title", &f.title, 1, 200)?;
            check_text("findings[].detail", &f.detail, 1, 4_000)?;
        }
        // The conclusion must agree with the findings it summarizes.
        let has_blocking = self.blocking().next().is_some();
        if has_blocking != (self.conclusion == ReviewConclusion::BlockingFindings) {
            return Err(invalid(
                "conclusion",
                "must be `blocking_findings` iff a high/critical finding exists",
            ));
        }
        Ok(())
    }
    fn commit_sha(&self) -> Option<&str> {
        Some(&self.commit_sha)
    }
}

// ---------------------------------------------------------------------------
// QA report (QA Tester)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionStatus {
    Verified,
    Failed,
    NotTested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionResult {
    pub criterion_id: String,
    pub status: CriterionStatus,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaEnvironment {
    pub os: String,
    pub toolchain: String,
    pub sandbox: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaReport {
    pub commit_sha: String,
    pub environment: QaEnvironment,
    pub results: Vec<CommandResult>,
    pub criteria: Vec<CriterionResult>,
}

impl QaReport {
    pub fn all_passed(&self) -> bool {
        !self.results.is_empty()
            && self.results.iter().all(CommandResult::passed)
            && self
                .criteria
                .iter()
                .all(|c| c.status == CriterionStatus::Verified)
    }
}

impl Contract for QaReport {
    const KIND: ArtifactKind = ArtifactKind::QaReport;
    fn validate(&self) -> Result<(), ContractError> {
        validate_commit_sha("commit_sha", &self.commit_sha)?;
        check_text("environment.os", &self.environment.os, 1, 200)?;
        check_text("environment.toolchain", &self.environment.toolchain, 1, 200)?;
        check_text("environment.sandbox", &self.environment.sandbox, 1, 200)?;
        check_len("results", &self.results, 1, 20)?;
        for r in &self.results {
            r.validate()?;
        }
        check_len("criteria", &self.criteria, 1, 30)?;
        for c in &self.criteria {
            if !valid_criterion_id(&c.criterion_id) {
                return Err(invalid("criteria[].criterion_id", "must be `AC-<n>`"));
            }
            check_text("criteria[].evidence", &c.evidence, 1, 2_000)?;
        }
        Ok(())
    }
    fn commit_sha(&self) -> Option<&str> {
        Some(&self.commit_sha)
    }
}

// ---------------------------------------------------------------------------
// Quality assessment (AI Quality Lead) — a recommendation, never an approval
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    Ready,
    ChangesRequired,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityAssessment {
    pub commit_sha: String,
    pub recommendation: Recommendation,
    pub summary: String,
    #[serde(default)]
    pub open_issues: Vec<String>,
}

impl Contract for QualityAssessment {
    const KIND: ArtifactKind = ArtifactKind::QualityAssessment;
    fn validate(&self) -> Result<(), ContractError> {
        validate_commit_sha("commit_sha", &self.commit_sha)?;
        check_text("summary", &self.summary, 1, 2_000)?;
        check_len("open_issues", &self.open_issues, 0, 30)?;
        for i in &self.open_issues {
            check_text("open_issues[]", i, 1, 500)?;
        }
        Ok(())
    }
    fn commit_sha(&self) -> Option<&str> {
        Some(&self.commit_sha)
    }
}

// ---------------------------------------------------------------------------
// Delivery manifest (built by the orchestrator, bound by the human decision)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub kind: ArtifactKind,
    pub artifact_id: uuid::Uuid,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryManifest {
    pub workflow_id: uuid::Uuid,
    pub commit_sha: String,
    pub entries: Vec<ManifestEntry>,
}

impl DeliveryManifest {
    pub fn hash(&self) -> Result<String, ContractError> {
        let v = serde_json::to_value(self).map_err(|e| invalid("manifest", e.to_string()))?;
        Ok(content_hash(
            ArtifactKind::DeliveryManifest,
            SCHEMA_VERSION,
            &v,
        ))
    }
}

impl Contract for DeliveryManifest {
    const KIND: ArtifactKind = ArtifactKind::DeliveryManifest;
    fn validate(&self) -> Result<(), ContractError> {
        validate_commit_sha("commit_sha", &self.commit_sha)?;
        check_len("entries", &self.entries, 1, 20)?;
        for e in &self.entries {
            validate_sha256("entries[].content_hash", &e.content_hash)?;
        }
        Ok(())
    }
    fn commit_sha(&self) -> Option<&str> {
        Some(&self.commit_sha)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn hash_is_canonical_and_kind_bound() {
        let a = json!({"b": 1, "a": {"y": [1, 2], "x": "s"}});
        let b = json!({"a": {"x": "s", "y": [1, 2]}, "b": 1});
        assert_eq!(
            content_hash(ArtifactKind::QaReport, 1, &a),
            content_hash(ArtifactKind::QaReport, 1, &b)
        );
        assert_ne!(
            content_hash(ArtifactKind::QaReport, 1, &a),
            content_hash(ArtifactKind::ReviewReport, 1, &a)
        );
        assert_ne!(
            content_hash(ArtifactKind::QaReport, 1, &a),
            content_hash(ArtifactKind::QaReport, 2, &a)
        );
        assert_eq!(content_hash(ArtifactKind::QaReport, 1, &a).len(), 64);
    }

    #[test]
    fn unknown_fields_and_hidden_reasoning_are_rejected() {
        let v = json!({"commit_sha": A, "recommendation": "ready", "summary": "ok", "reasoning": "chain of thought"});
        assert!(matches!(
            decode::<QualityAssessment>(&v),
            Err(ContractError::Schema { .. })
        ));
    }

    #[test]
    fn abbreviated_sha_is_rejected() {
        let v = json!({"commit_sha": "abc1234", "recommendation": "ready", "summary": "ok"});
        assert!(decode::<QualityAssessment>(&v).is_err());
    }

    #[test]
    fn review_conclusion_must_match_findings() {
        let v = json!({"commit_sha": A, "summary": "s", "conclusion": "no_blocking_findings",
            "findings": [{"id": "F1", "severity": "high", "category": "security", "file": "a.rs", "title": "t", "detail": "d"}]});
        assert!(decode::<ReviewReport>(&v).is_err());
    }

    #[test]
    fn candidate_must_differ_from_base_and_use_allowed_checks() {
        let base = json!({"base_commit_sha": A, "commit_sha": A, "branch": "agent/x/candidate",
            "changed_files": [{"path": "a.rs", "change": "modified"}],
            "patch_sha256": "0".repeat(64), "checks": []});
        assert!(decode::<CodeCandidate>(&base).is_err());
        let mut ok = base.clone();
        ok["commit_sha"] = json!(B);
        assert!(decode::<CodeCandidate>(&ok).is_ok());
        ok["checks"] =
            json!([{"check_id": "bash -c x", "exit_code": 0, "duration_ms": 1, "output_tail": ""}]);
        assert!(decode::<CodeCandidate>(&ok).is_err());
    }

    #[test]
    fn plan_scope_must_be_repo_relative() {
        let mut v = json!({"summary": "s", "deliverables": ["d"], "acceptance_criteria": [{"id": "AC-1", "description": "x"}],
            "steps": ["s"], "scope": ["apps/api/src"]});
        assert!(decode::<RequirementsPlan>(&v).is_ok());
        for bad in ["/etc", "../x", ".git", ""] {
            v["scope"] = json!([bad]);
            assert!(decode::<RequirementsPlan>(&v).is_err(), "{bad}");
        }
    }

    #[test]
    fn approval_decisions_cannot_be_submitted_as_agent_artifacts() {
        assert!(validate_kind(ArtifactKind::ApprovalDecision, &json!({})).is_err());
    }
}
