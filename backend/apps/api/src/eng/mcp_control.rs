//! Orchestrator → MCP server control plane (Phase 6B B2): create a workflow's
//! workspace. The URL is configuration (`MCP_CONTROL_URL`); nothing a user
//! or an agent sends can change it. The request carries only a short-lived
//! control token (its own signature domain), which binds the workflow and base.

use std::time::Duration;

use engineering::mcp::{ControlClaims, issue_control_token};
use url::Url;
use uuid::Uuid;

pub struct McpControl {
    endpoint: Url,
    key: Vec<u8>,
    http: reqwest::Client,
}

impl McpControl {
    /// `base` is the MCP server's origin, e.g. `http://127.0.0.1:8090`.
    pub fn new(base: Url, key: Vec<u8>) -> Result<Self, String> {
        if key.len() < engineering::mcp::MIN_KEY_BYTES {
            return Err("MCP token key must be at least 32 bytes".into());
        }
        let endpoint = base
            .join("/control/workspaces")
            .map_err(|e| format!("MCP control URL: {e}"))?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(McpControl {
            endpoint,
            key,
            http,
        })
    }

    pub async fn create_workspace(
        &self,
        workflow_id: Uuid,
        base_sha: &str,
    ) -> Result<Uuid, String> {
        let now = chrono::Utc::now().timestamp();
        let token = issue_control_token(
            &self.key,
            &ControlClaims {
                workflow_id,
                base_sha: base_sha.to_owned(),
                iat: now,
                exp: now + 60,
            },
        )
        .map_err(|e| e.to_string())?;
        let resp = self
            .http
            .post(self.endpoint.clone())
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| format!("MCP control unreachable: {}", e.without_url()))?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        if status != reqwest::StatusCode::CREATED {
            let code = body["error"].as_str().unwrap_or("unknown");
            return Err(format!("MCP refused workspace creation ({status}): {code}"));
        }
        body["workspace_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| "MCP returned no workspace id".to_owned())
    }
}
