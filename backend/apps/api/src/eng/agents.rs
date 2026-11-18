//! Provider adapter interface for role agents.
//!
//! An adapter turns one role assignment into one observable output. It may
//! run in-process (deterministic simulation, Mode A), call a coding-agent
//! runtime, or talk A2A to a remote agent; the orchestrator does not care and
//! treats every output as untrusted. Adapters must report failures as
//! [`AgentError`]: there is no fallback from a failed provider to any other
//! adapter, and in particular never to the simulation.

use std::future::Future;
use std::pin::Pin;

use engineering::artifacts::ArtifactKind;
use engineering::roles::{Role, ToolRequest};
use serde_json::Value;
use uuid::Uuid;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    /// Stable id of the agent instance (separation-of-duties checks key on it).
    pub agent_id: String,
    /// `simulation`, `claude-agent-sdk`, `openai-codex`, ... recorded per run.
    pub provider: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RoleInput {
    pub workflow_id: Uuid,
    /// The `eng_agent_runs` row of this attempt. MCP capability tokens bind
    /// to it, so tool-call audit rows correlate with exactly one run.
    pub run_id: Uuid,
    pub correlation_id: Uuid,
    pub role: Role,
    pub attempt: i32,
    /// Number of delivery revisions so far (0 on the first pass).
    pub revision: u32,
    pub requirements: String,
    /// Latest validated artifact per kind (only kinds this role may read).
    pub artifacts: Vec<(ArtifactKind, Value)>,
    /// Human-approved scope (path prefixes), empty before plan approval.
    pub scope: Vec<String>,
    pub candidate_commit_sha: Option<String>,
    /// Base commit of the repository, supplied by the orchestrator (from the
    /// workspace in M5, or configuration). Never invented by an agent.
    pub base_commit_sha: Option<String>,
    /// The workflow's MCP workspace (created by the MCP server, recorded in
    /// `eng_workspaces`). Set only for MCP-backed adapters.
    pub workspace_id: Option<Uuid>,
    /// The exact candidate diff, read from the isolated workspace (M5).
    /// `None` until a real workspace exists.
    pub patch: Option<String>,
}

impl RoleInput {
    pub fn artifact(&self, kind: ArtifactKind) -> Option<&Value> {
        self.artifacts
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, v)| v)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct AgentOutput {
    /// Candidate artifact content for `role.produces()`; validated by the orchestrator.
    pub content: Value,
    /// Every tool the agent used or wants to use (authorized against policy).
    pub tool_requests: Vec<ToolRequest>,
    pub a2a_task_id: Option<String>,
    pub usage: Usage,
    /// MCP tool-call ids the output relies on. The orchestrator verifies each
    /// against the MCP server's audit (`eng_tool_calls`); the agent's word is not enough.
    pub tool_calls: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentErrorKind {
    /// Not reachable / not running / provider outage. Retryable.
    Unavailable,
    /// Provider rejected our credentials. Not retryable.
    Unauthenticated,
    /// Agent ran and reported failure. Not retryable.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct AgentError {
    pub kind: AgentErrorKind,
    pub message: String,
}

pub trait RoleAgent: Send + Sync {
    fn identity(&self) -> AgentIdentity;
    fn run(&self, input: RoleInput) -> BoxFut<'_, Result<AgentOutput, AgentError>>;
    /// `true` for adapters that work through MCP. Their output must then be
    /// backed by audited tool calls of the run (see `Orchestrator::verify_tool_evidence`).
    fn requires_tool_evidence(&self) -> bool {
        false
    }
}

/// Which artifact kinds a role sees as input (least privilege on context).
pub fn readable_kinds(role: Role) -> &'static [ArtifactKind] {
    use ArtifactKind::*;
    match role {
        Role::ProjectManager => &[],
        Role::ProjectCoordinator => &[RequirementsPlan],
        Role::Developer => &[RequirementsPlan, WorkAssignment],
        Role::CodeReviewer => &[RequirementsPlan, WorkAssignment, CodeCandidate],
        Role::QaTester => &[RequirementsPlan, WorkAssignment, CodeCandidate],
        Role::QualityLead => &[
            RequirementsPlan,
            WorkAssignment,
            CodeCandidate,
            ReviewReport,
            QaReport,
        ],
    }
}
