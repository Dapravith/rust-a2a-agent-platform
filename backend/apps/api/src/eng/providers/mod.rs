//! Real provider adapters and their configuration.
//!
//! `ENG_PROVIDER` selects them: unset or `none` (default) wires no agents, so
//! `/api/eng` submissions answer 503 `provider_unconfigured`; `anthropic`
//! wires [`anthropic::AnthropicRoleAgent`] for the roles it can serve; `a2a`
//! wires all six roles as remote A2A agents (`apps/role-agents`) with MCP
//! workspaces (`apps/mcp-server`). Roles
//! without a provider stay unconfigured and a workflow reaching them fails
//! with `provider_unconfigured`; nothing is ever filled in by simulation.

pub mod anthropic;

use std::collections::HashMap;
use std::sync::Arc;

use engineering::roles::Role;

use super::agents::RoleAgent;

pub struct ProviderConfig {
    pub agents: HashMap<Role, Arc<dyn RoleAgent>>,
    pub base_commit_sha: Option<String>,
    pub mcp_control: Option<Arc<super::mcp_control::McpControl>>,
}

fn required(key: &str) -> Result<String, String> {
    env(key).ok_or_else(|| format!("ENG_PROVIDER=a2a requires {key}"))
}

/// `ENG_PROVIDER=a2a`: every role is a remote A2A agent at
/// `ROLE_AGENTS_URL/agents/<role>` (Agent Card name checked); MCP-backed
/// roles get per-run tokens signed with `MCP_TOKEN_KEY`, and workspaces are
/// created through `MCP_CONTROL_URL`. All URLs are configuration only.
fn a2a_from_env(base_commit_sha: Option<String>) -> Result<ProviderConfig, String> {
    let agents_url = url::Url::parse(&required("ROLE_AGENTS_URL")?)
        .map_err(|e| format!("ROLE_AGENTS_URL: {e}"))?;
    let control_url = url::Url::parse(&required("MCP_CONTROL_URL")?)
        .map_err(|e| format!("MCP_CONTROL_URL: {e}"))?;
    let key = hex::decode(required("MCP_TOKEN_KEY")?).map_err(|_| "MCP_TOKEN_KEY must be hex")?;
    let provider = env("ENG_A2A_PROVIDER_LABEL").unwrap_or_else(|| "deterministic".into());
    let timeout = std::time::Duration::from_secs(
        env("ENG_STAGE_TIMEOUT_SECS")
            .map_or(Ok(600), |v| v.parse())
            .map_err(|_| "ENG_STAGE_TIMEOUT_SECS")?,
    );
    let gateway = crate::a2a::A2AGateway::new(timeout, std::time::Duration::from_millis(250))?;
    let mut agents: HashMap<Role, Arc<dyn RoleAgent>> = HashMap::new();
    for role in Role::ALL {
        let path = format!("/agents/{}", role.as_str().replace('_', "-"));
        let endpoint = crate::a2a::AgentEndpoint {
            expected_name: contracts::role::agent_name(role.as_str())
                .ok_or("unknown role")?
                .to_owned(),
            base_url: agents_url.join(&path).map_err(|e| e.to_string())?,
        };
        let identity = super::agents::AgentIdentity {
            agent_id: format!("a2a-{}", role.as_str()),
            provider: provider.clone(),
            model: None,
        };
        let agent = super::a2a_role::RemoteRoleAgent::new(
            role,
            identity,
            endpoint,
            gateway.clone(),
            key.clone(),
            std::time::Duration::from_secs(300),
        )?;
        agents.insert(role, Arc::new(agent));
    }
    Ok(ProviderConfig {
        agents,
        base_commit_sha,
        mcp_control: Some(Arc::new(super::mcp_control::McpControl::new(
            control_url,
            key,
        )?)),
    })
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Reads `ENG_PROVIDER`, `ENG_BASE_COMMIT_SHA` and (for `anthropic`) the
/// `ANTHROPIC_*` / `LLM_*` settings. `Ok(None)` means no providers.
pub fn from_env() -> Result<Option<ProviderConfig>, String> {
    let base_commit_sha = env("ENG_BASE_COMMIT_SHA");
    if let Some(sha) = &base_commit_sha {
        engineering::validate_commit_sha("ENG_BASE_COMMIT_SHA", sha).map_err(|e| e.to_string())?;
    }
    match env("ENG_PROVIDER").as_deref() {
        None | Some("none") => Ok(None),
        Some("anthropic") => {
            let cfg = agent_runtime::LlmConfig::anthropic_from_env("ENG_PROVIDER=anthropic")?;
            let client = agent_runtime::LlmClient::new(cfg)?;
            let mut agents: HashMap<Role, Arc<dyn RoleAgent>> = HashMap::new();
            for role in [
                Role::ProjectManager,
                Role::ProjectCoordinator,
                Role::CodeReviewer,
                Role::QualityLead,
            ] {
                let agent = anthropic::AnthropicRoleAgent::new(
                    role,
                    &format!("anthropic-{}", role.as_str()),
                    client.clone(),
                )
                .map_err(|e| e.to_string())?;
                agents.insert(role, Arc::new(agent));
            }
            Ok(Some(ProviderConfig {
                agents,
                base_commit_sha,
                mcp_control: None,
            }))
        }
        Some("a2a") => a2a_from_env(base_commit_sha).map(Some),
        Some(other) => Err(format!(
            "ENG_PROVIDER must be `none`, `anthropic` or `a2a`, got `{other}`"
        )),
    }
}
