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
    /// Short-lived MCP capability token for this run only. Never logged.
    pub mcp_token: String,
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
        if self.mcp_token.is_empty() || self.mcp_token.len() > MAX_TOKEN {
            return Err(invalid("mcp_token", "missing or too long"));
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
