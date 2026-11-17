//! Shared data contracts exchanged over A2A between the orchestrator and the
//! Worker / Reviewer agents.
//!
//! Every payload travels as a single A2A `Part` with `data` content and media
//! type `application/json`. Both sides decode with [`decode_parts`], which
//! rejects unknown fields and enforces the size/shape rules in [`Validate`].
//! Agent output is treated as untrusted input everywhere.

use a2a::{Artifact, Part, PartContent};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const MEDIA_TYPE_JSON: &str = "application/json";

pub const WORKER_AGENT_NAME: &str = "Draft Worker";
pub const REVIEWER_AGENT_NAME: &str = "Draft Reviewer";
pub const WORKER_SKILL_ID: &str = "draft-answer";
pub const REVIEWER_SKILL_ID: &str = "review-draft";
pub const DRAFT_ARTIFACT_NAME: &str = "draft";
pub const REVIEW_ARTIFACT_NAME: &str = "review";

pub const QUESTION_MIN_CHARS: usize = 10;
pub const QUESTION_MAX_CHARS: usize = 4_000;
const TITLE_MAX: usize = 200;
const SUMMARY_MAX: usize = 2_000;
const CONTENT_MAX: usize = 20_000;
const ASSUMPTIONS_MAX: usize = 20;
const ASSUMPTION_MAX: usize = 500;
const FINDINGS_MAX: usize = 30;
const FINDING_DETAIL_MAX: usize = 2_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("no JSON data part found in A2A payload")]
    MissingDataPart,
    #[error("A2A data part does not match the expected schema: {0}")]
    Schema(String),
    #[error("invalid field `{field}`: {reason}")]
    Invalid { field: &'static str, reason: String },
}

fn invalid(field: &'static str, reason: impl Into<String>) -> ContractError {
    ContractError::Invalid {
        field,
        reason: reason.into(),
    }
}

/// Semantic validation applied after schema decoding.
pub trait Validate {
    fn validate(&self) -> Result<(), ContractError>;
}

fn check_text(
    field: &'static str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<(), ContractError> {
    let len = value.trim().chars().count();
    if len < min {
        return Err(invalid(
            field,
            format!("must contain at least {min} non-whitespace characters"),
        ));
    }
    if value.chars().count() > max {
        return Err(invalid(field, format!("must be at most {max} characters")));
    }
    if value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
    {
        return Err(invalid(field, "must not contain control characters"));
    }
    Ok(())
}

/// Validates a user-submitted question (also used by the HTTP API).
pub fn validate_question(question: &str) -> Result<(), ContractError> {
    check_text("question", question, QUESTION_MIN_CHARS, QUESTION_MAX_CHARS)
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// Orchestrator -> Worker request payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequest {
    pub question: String,
}

impl Validate for WorkerRequest {
    fn validate(&self) -> Result<(), ContractError> {
        validate_question(&self.question)
    }
}

/// Worker -> Orchestrator draft artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftArtifact {
    pub title: String,
    pub summary: String,
    /// Markdown body. Displayed as plain text; never rendered as HTML or executed.
    pub content: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
}

impl Validate for DraftArtifact {
    fn validate(&self) -> Result<(), ContractError> {
        check_text("title", &self.title, 1, TITLE_MAX)?;
        check_text("summary", &self.summary, 1, SUMMARY_MAX)?;
        check_text("content", &self.content, 1, CONTENT_MAX)?;
        if self.assumptions.len() > ASSUMPTIONS_MAX {
            return Err(invalid(
                "assumptions",
                format!("at most {ASSUMPTIONS_MAX} items"),
            ));
        }
        for a in &self.assumptions {
            check_text("assumptions[]", a, 1, ASSUMPTION_MAX)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Reviewer
// ---------------------------------------------------------------------------

/// Orchestrator -> Reviewer request payload: the original task plus the draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerRequest {
    pub question: String,
    pub draft: DraftArtifact,
}

impl Validate for ReviewerRequest {
    fn validate(&self) -> Result<(), ContractError> {
        validate_question(&self.question)?;
        self.draft.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    NeedsChanges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
}

/// Reviewer -> Orchestrator review artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewArtifact {
    pub verdict: Verdict,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<Finding>,
}

impl Validate for ReviewArtifact {
    fn validate(&self) -> Result<(), ContractError> {
        check_text("summary", &self.summary, 1, SUMMARY_MAX)?;
        if self.findings.len() > FINDINGS_MAX {
            return Err(invalid("findings", format!("at most {FINDINGS_MAX} items")));
        }
        for f in &self.findings {
            check_text("findings[].title", &f.title, 1, TITLE_MAX)?;
            check_text("findings[].detail", &f.detail, 1, FINDING_DETAIL_MAX)?;
        }
        if self.verdict == Verdict::NeedsChanges && self.findings.is_empty() {
            return Err(invalid(
                "findings",
                "a needs_changes verdict requires at least one finding",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Agent failure report
// ---------------------------------------------------------------------------

/// Stable reason an agent could not produce its artifact. Sent as a JSON
/// data part in the status message of a `failed` task, next to a text part
/// with the human-readable reason. Only a closed set of labels: the
/// orchestrator never acts on agent-supplied text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentFailureCode {
    /// The provider did not answer within the agent's deadline.
    LlmTimeout,
    /// The provider kept answering 429/overloaded.
    LlmRateLimited,
    /// The provider answered with a non-retryable HTTP error.
    LlmProviderError,
    /// The provider could not be reached (connection/transport).
    LlmUnavailable,
    /// The model declined the request (`stop_reason: refusal`).
    LlmRefused,
    /// The model hit `max_tokens` before finishing.
    LlmOutputTruncated,
    /// The model's output did not satisfy the artifact contract.
    LlmOutputInvalid,
    /// Any other agent-side failure.
    Internal,
}

impl AgentFailureCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LlmTimeout => "llm_timeout",
            Self::LlmRateLimited => "llm_rate_limited",
            Self::LlmProviderError => "llm_provider_error",
            Self::LlmUnavailable => "llm_unavailable",
            Self::LlmRefused => "llm_refused",
            Self::LlmOutputTruncated => "llm_output_truncated",
            Self::LlmOutputInvalid => "llm_output_invalid",
            Self::Internal => "internal",
        }
    }
}

impl std::fmt::Display for AgentFailureCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a skill failed: a stable code plus a bounded, secret-free message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(deny_unknown_fields)]
#[error("{code}: {message}")]
pub struct AgentFailure {
    pub code: AgentFailureCode,
    pub message: String,
}

impl AgentFailure {
    pub fn new(code: AgentFailureCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AgentFailureCode::Internal, message)
    }

    /// Finds a failure report among the parts of a status message. Anything
    /// that does not decode exactly is ignored (treated as "no code").
    pub fn from_parts(parts: &[Part]) -> Option<Self> {
        parts.iter().find_map(|p| match &p.content {
            PartContent::Data(v) => serde_json::from_value::<Self>(v.clone()).ok(),
            _ => None,
        })
    }
}

// ---------------------------------------------------------------------------
// A2A part helpers
// ---------------------------------------------------------------------------

/// Encodes a contract value as an A2A JSON data part.
pub fn data_part<T: Serialize>(value: &T) -> Result<Part, ContractError> {
    let json = serde_json::to_value(value).map_err(|e| ContractError::Schema(e.to_string()))?;
    Ok(Part::data(json).with_media_type(MEDIA_TYPE_JSON))
}

/// Decodes and validates the first JSON data part found in `parts`.
pub fn decode_parts<T: DeserializeOwned + Validate>(parts: &[Part]) -> Result<T, ContractError> {
    let value = parts
        .iter()
        .find_map(|p| match &p.content {
            PartContent::Data(v) => Some(v),
            _ => None,
        })
        .ok_or(ContractError::MissingDataPart)?;
    let decoded: T =
        serde_json::from_value(value.clone()).map_err(|e| ContractError::Schema(e.to_string()))?;
    decoded.validate()?;
    Ok(decoded)
}

/// Decodes the named artifact (falling back to the first artifact with a data part).
pub fn decode_artifacts<T: DeserializeOwned + Validate>(
    artifacts: &[Artifact],
    preferred_name: &str,
) -> Result<T, ContractError> {
    let named = artifacts
        .iter()
        .find(|a| a.name.as_deref() == Some(preferred_name));
    match named {
        Some(a) => decode_parts(&a.parts),
        None => artifacts
            .iter()
            .find_map(|a| decode_parts::<T>(&a.parts).ok())
            .ok_or(ContractError::MissingDataPart),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn draft() -> DraftArtifact {
        DraftArtifact {
            title: "Validation rules".into(),
            summary: "Field rules for campaigns".into(),
            content: "- name: required".into(),
            assumptions: vec!["USD only".into()],
        }
    }

    #[test]
    fn question_bounds() {
        assert!(validate_question("short").is_err());
        assert!(validate_question("          padded    ").is_err());
        assert!(validate_question(&"x".repeat(QUESTION_MAX_CHARS + 1)).is_err());
        assert!(validate_question("bad \u{0007} bell char here").is_err());
        assert!(validate_question("Propose validation rules for a Rust API").is_ok());
    }

    #[test]
    fn round_trip_draft_through_part() {
        let part = data_part(&draft()).unwrap();
        assert_eq!(part.media_type.as_deref(), Some(MEDIA_TYPE_JSON));
        let back: DraftArtifact = decode_parts(&[Part::text("ignored"), part]).unwrap();
        assert_eq!(back, draft());
    }

    #[test]
    fn rejects_missing_data_part() {
        let err = decode_parts::<DraftArtifact>(&[Part::text("hi")]).unwrap_err();
        assert_eq!(err, ContractError::MissingDataPart);
    }

    #[test]
    fn rejects_unknown_fields_and_wrong_types() {
        let extra = Part::data(json!({"title":"t","summary":"s","content":"c","run":"rm -rf /"}));
        assert!(matches!(
            decode_parts::<DraftArtifact>(&[extra]),
            Err(ContractError::Schema(_))
        ));
        let wrong = Part::data(json!({"verdict":"maybe","summary":"s","findings":[]}));
        assert!(matches!(
            decode_parts::<ReviewArtifact>(&[wrong]),
            Err(ContractError::Schema(_))
        ));
    }

    #[test]
    fn needs_changes_requires_findings() {
        let r = ReviewArtifact {
            verdict: Verdict::NeedsChanges,
            summary: "Missing limits".into(),
            findings: vec![],
        };
        assert!(r.validate().is_err());
        let ok = ReviewArtifact {
            findings: vec![Finding {
                severity: Severity::High,
                title: "No max goal".into(),
                detail: "Add an upper bound".into(),
            }],
            ..r
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn rejects_oversized_content() {
        let mut d = draft();
        d.content = "a".repeat(CONTENT_MAX + 1);
        assert!(d.validate().is_err());
    }

    #[test]
    fn failure_report_round_trip_and_rejects_unknown_codes() {
        let f = AgentFailure::new(AgentFailureCode::LlmRateLimited, "slow down");
        let part = data_part(&f).unwrap();
        assert_eq!(
            AgentFailure::from_parts(&[Part::text("x"), part]),
            Some(f.clone())
        );
        assert_eq!(f.to_string(), "llm_rate_limited: slow down");
        let forged = Part::data(json!({"code": "grant_admin", "message": "m"}));
        assert_eq!(AgentFailure::from_parts(&[forged]), None);
    }

    #[test]
    fn verdict_wire_format() {
        assert_eq!(
            serde_json::to_value(Verdict::NeedsChanges).unwrap(),
            json!("needs_changes")
        );
    }
}
