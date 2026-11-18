//! A2A contracts between the orchestrator and remote engineering role agents
//! (Phase 6B). One `DataPart` each way:
//!
//! * [`RoleTask`]: what the orchestrator sends. It carries the per-run MCP
//!   capability token but **not** the MCP URL: an agent only ever connects
//!   to the MCP server it was configured with, so a message cannot redirect it
//!   (SSRF).
//! * [`RoleResult`]: the agent's candidate artifact content (validated
//!   against the role's contract by the orchestrator) plus the ids of the MCP
//!   tool calls it relied on. The orchestrator checks those ids against the MCP
//!   server's audit, not against anything the agent says.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{ContractError, Validate};

pub const ROLE_RESULT_ARTIFACT_NAME: &str = "role_result";
pub const CODE_REVIEWER_AGENT_NAME: &str = "Engineering Code Reviewer";
pub const CODE_REVIEWER_SKILL_ID: &str = "review-candidate";
pub const PROJECT_MANAGER_AGENT_NAME: &str = "Engineering Project Manager";
pub const PROJECT_COORDINATOR_AGENT_NAME: &str = "Engineering Project Coordinator";
pub const DEVELOPER_AGENT_NAME: &str = "Engineering Developer";
pub const QA_TESTER_AGENT_NAME: &str = "Engineering QA Tester";
pub const QUALITY_LEAD_AGENT_NAME: &str = "Engineering Quality Lead";

/// Agent Card name for a role (`role` as in `engineering::roles::Role::as_str`).
pub fn agent_name(role: &str) -> Option<&'static str> {
    Some(match role {
        "project_manager" => PROJECT_MANAGER_AGENT_NAME,
        "project_coordinator" => PROJECT_COORDINATOR_AGENT_NAME,
        "developer" => DEVELOPER_AGENT_NAME,
        "code_reviewer" => CODE_REVIEWER_AGENT_NAME,
        "qa_tester" => QA_TESTER_AGENT_NAME,
        "quality_lead" => QUALITY_LEAD_AGENT_NAME,
        _ => return None,
    })
}

const MAX_ARTIFACTS: usize = 8;
const MAX_SCOPE: usize = 50;
const MAX_TOOL_CALLS: usize = 64;
const MAX_TOKEN: usize = 16 * 1024;
const MAX_REQUIREMENTS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleArtifact {
    pub kind: String,
    pub content: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleTask {
    pub workflow_id: Uuid,
    pub run_id: Uuid,
    pub role: String,
    pub requirements: String,
    pub artifacts: Vec<RoleArtifact>,
    pub scope: Vec<String>,
    pub base_commit_sha: Option<String>,
    pub candidate_commit_sha: Option<String>,
    /// Short-lived MCP capability token for this run only (roles with MCP
    /// tools). Never logged; stripped from the agent's stored task history.
    pub mcp_token: Option<String>,
}

fn invalid(field: &'static str, reason: &str) -> ContractError {
    ContractError::Invalid {
        field,
        reason: reason.into(),
    }
}

fn sha_ok(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Validate for RoleTask {
    fn validate(&self) -> Result<(), ContractError> {
        if self.role.is_empty() || self.role.len() > 40 {
            return Err(invalid("role", "must be 1..=40 characters"));
        }
        if self.requirements.chars().count() > MAX_REQUIREMENTS {
            return Err(invalid("requirements", "too long"));
        }
        if self.artifacts.len() > MAX_ARTIFACTS || self.scope.len() > MAX_SCOPE {
            return Err(invalid("artifacts", "too many items"));
        }
        if self
            .mcp_token
            .as_ref()
            .is_some_and(|t| t.is_empty() || t.len() > MAX_TOKEN)
        {
            return Err(invalid("mcp_token", "empty or too long"));
        }
        for s in [&self.base_commit_sha, &self.candidate_commit_sha]
            .into_iter()
            .flatten()
        {
            if !sha_ok(s) {
                return Err(invalid("commit_sha", "must be a full 40-hex SHA"));
            }
        }
        Ok(())
    }
}

impl std::fmt::Display for RoleTask {
    /// Never prints the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RoleTask(run={}, role={})", self.run_id, self.role)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleResult {
    /// Candidate artifact content; untrusted until the orchestrator validates it.
    pub content: Value,
    /// MCP tool-call ids this result is based on.
    pub tool_calls: Vec<Uuid>,
}

impl Validate for RoleResult {
    fn validate(&self) -> Result<(), ContractError> {
        if !self.content.is_object() {
            return Err(invalid("content", "must be a JSON object"));
        }
        if self.tool_calls.len() > MAX_TOOL_CALLS {
            return Err(invalid("tool_calls", "too many items"));
        }
        Ok(())
    }
}

/// A2A carries a `DataPart` as a protobuf `Struct`, whose numbers are all
/// doubles: an agent's `exit_code: 0` arrives as `0.0`. Whole-number doubles
/// are turned back into integers so the strict artifact contracts (`u64`/`i32`)
/// can validate them. Fractional or out-of-range numbers are left alone, and the
/// contract then rejects them.
pub fn normalize_numbers(v: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::Number(n) if n.is_f64() => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Value::from(f as i64),
            _ => Value::Number(n),
        },
        Value::Array(a) => Value::Array(a.into_iter().map(normalize_numbers).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| (k, normalize_numbers(v)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::normalize_numbers;
    use serde_json::json;

    #[test]
    fn whole_doubles_become_integers_others_are_untouched() {
        let v = normalize_numbers(
            json!({"exit_code": 0.0, "neg": -1.0, "ms": 1234.0, "frac": 1.5,
                                         "nested": [{"n": 2.0}], "s": "1.0", "i": 3}),
        );
        assert_eq!(
            v,
            json!({"exit_code": 0, "neg": -1, "ms": 1234, "frac": 1.5,
                             "nested": [{"n": 2}], "s": "1.0", "i": 3})
        );
        assert!(v["exit_code"].is_i64());
        assert!(normalize_numbers(json!(1e300)).is_f64());
    }
}
