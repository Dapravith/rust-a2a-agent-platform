//! Real provider adapters and their configuration.
//!
//! `ENG_PROVIDER` selects them: unset or `none` (default) wires no agents, so
//! `/api/eng` submissions answer 503 `provider_unconfigured`; `anthropic`
//! wires [`anthropic::AnthropicRoleAgent`] for the roles it can serve. Roles
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
            }))
        }
        Some(other) => Err(format!(
            "ENG_PROVIDER must be `none` or `anthropic`, got `{other}`"
        )),
    }
}
