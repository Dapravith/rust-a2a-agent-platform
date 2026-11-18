//! `RoleAgent` adapter for a remote role agent reached over A2A (Phase 6B C5).
//!
//! Per run it:
//! 1. issues an MCP capability token bound to this workflow, run, role and
//!    agent id, the assignment scope, the base commit and the workspace. The
//!    tool set is the role's own (`McpTool::allowed_for`); a prompt or an agent cannot widen it.
//! 2. sends a [`RoleTask`] through the trusted-card [`A2AGateway`] (JSON-RPC,
//!    deadline, `CancelTask` on timeout).
//! 3. returns the agent's [`RoleResult`] as untrusted output. The
//!    orchestrator validates the content and checks the cited tool calls
//!    against the MCP audit.
//!
//! Workspace: the orchestrator passes the workflow's MCP-created workspace
//! (`RoleInput::workspace_id`); this adapter never derives or names a path.

use std::time::Duration;

use contracts::data_part;
use contracts::role::{ROLE_RESULT_ARTIFACT_NAME, RoleArtifact, RoleResult, RoleTask};
use engineering::mcp::{CapabilityClaims, McpTool, issue_token};
use engineering::roles::Role;

use super::agents::{
    AgentError, AgentErrorKind, AgentIdentity, AgentOutput, BoxFut, RoleAgent, RoleInput, Usage,
};
use crate::a2a::{self, A2AGateway, AgentEndpoint};

pub struct RemoteRoleAgent {
    role: Role,
    identity: AgentIdentity,
    endpoint: AgentEndpoint,
    gateway: A2AGateway,
    token_key: Vec<u8>,
    token_ttl: Duration,
}

impl RemoteRoleAgent {
    pub fn new(
        role: Role,
        identity: AgentIdentity,
        endpoint: AgentEndpoint,
        gateway: A2AGateway,
        token_key: Vec<u8>,
        token_ttl: Duration,
    ) -> Result<Self, String> {
        if token_key.len() < engineering::mcp::MIN_KEY_BYTES {
            return Err("MCP token key must be at least 32 bytes".into());
        }
        if token_ttl.as_secs() as i64 > engineering::mcp::MAX_TOKEN_TTL_SECS {
            return Err("MCP token TTL exceeds the server maximum".into());
        }
        Ok(RemoteRoleAgent {
            role,
            identity,
            endpoint,
            gateway,
            token_key,
            token_ttl,
        })
    }

    fn token(
        &self,
        input: &RoleInput,
        base: &str,
        workspace_id: uuid::Uuid,
    ) -> Result<String, AgentError> {
        let now = chrono::Utc::now().timestamp();
        let claims = CapabilityClaims {
            workflow_id: input.workflow_id,
            run_id: input.run_id,
            role: self.role,
            agent_id: self.identity.agent_id.clone(),
            workspace_id,
            tools: McpTool::allowed_for(self.role),
            scope: input.scope.clone(),
            base_sha: base.to_owned(),
            iat: now,
            exp: now + self.token_ttl.as_secs() as i64,
        };
        issue_token(&self.token_key, &claims).map_err(|e| failed(e.to_string()))
    }
}

fn failed(m: impl Into<String>) -> AgentError {
    AgentError {
        kind: AgentErrorKind::Failed,
        message: m.into(),
    }
}

fn map_err(e: a2a::AgentError) -> AgentError {
    let kind = match e {
        a2a::AgentError::Unreachable(_) | a2a::AgentError::Timeout(_) => {
            AgentErrorKind::Unavailable
        }
        _ => AgentErrorKind::Failed,
    };
    AgentError {
        kind,
        message: e.safe_message(),
    }
}

impl RoleAgent for RemoteRoleAgent {
    fn identity(&self) -> AgentIdentity {
        self.identity.clone()
    }

    fn requires_tool_evidence(&self) -> bool {
        !McpTool::allowed_for(self.role).is_empty()
    }

    fn run(&self, input: RoleInput) -> BoxFut<'_, Result<AgentOutput, AgentError>> {
        Box::pin(async move {
            if input.role != self.role {
                return Err(failed("adapter role mismatch"));
            }
            // The base is the human-approved assignment's (the workspace was created at it).
            let base = input
                .artifact(engineering::artifacts::ArtifactKind::WorkAssignment)
                .and_then(|a| a["base_commit_sha"].as_str().map(str::to_owned))
                .or_else(|| input.base_commit_sha.clone());
            let mcp_token = if self.requires_tool_evidence() {
                let base = base
                    .as_deref()
                    .ok_or_else(|| failed("no base commit for the MCP workspace"))?;
                let ws = input
                    .workspace_id
                    .ok_or_else(|| failed("no MCP workspace was provided for this run"))?;
                Some(self.token(&input, base, ws)?)
            } else {
                None
            };
            let task = RoleTask {
                workflow_id: input.workflow_id,
                run_id: input.run_id,
                role: self.role.as_str().to_owned(),
                requirements: input.requirements.clone(),
                artifacts: input
                    .artifacts
                    .iter()
                    .map(|(k, v)| RoleArtifact {
                        kind: k.as_str().to_owned(),
                        content: v.clone(),
                    })
                    .collect(),
                scope: input.scope.clone(),
                base_commit_sha: base,
                candidate_commit_sha: input.candidate_commit_sha.clone(),
                mcp_token,
            };
            let part = data_part(&task).map_err(|e| failed(e.to_string()))?;
            let out = self
                .gateway
                .call::<RoleResult>(&self.endpoint, part, ROLE_RESULT_ARTIFACT_NAME)
                .await
                .map_err(map_err)?;
            tracing::info!(run_id = %input.run_id, role = self.role.as_str(),
                task_id = ?out.task_id, tool_calls = out.value.tool_calls.len(), "remote role agent finished");
            Ok(AgentOutput {
                content: contracts::role::normalize_numbers(out.value.content),
                tool_requests: vec![],
                a2a_task_id: out.task_id,
                usage: Usage::default(),
                tool_calls: out.value.tool_calls,
            })
        })
    }
}
