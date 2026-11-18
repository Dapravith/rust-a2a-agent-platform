//! All six engineering role agents (A2A) on one listener.
//!
//! Environment:
//! * `ROLE_AGENTS_BIND` (default `127.0.0.1:9111`), `ROLE_AGENTS_PUBLIC_URL`
//!   (default `http://127.0.0.1:9111`). Agents live at `/agents/<role>`.
//! * `ROLE_AGENT_MCP_URL` (default `http://127.0.0.1:8090/mcp`): the only MCP
//!   server the agents call.
//! * `DEVELOPER_PATCH_FILE` (optional): the deterministic developer's patch.
//!   Without it the Developer fails honestly.
//! * `PM_SCOPE` (default `src`): comma-separated scope the planner proposes.

use std::process::ExitCode;

use role_agents::{AgentsConfig, DEFAULT_PORT, router};

async fn run() -> Result<(), String> {
    let bind =
        std::env::var("ROLE_AGENTS_BIND").unwrap_or_else(|_| format!("127.0.0.1:{DEFAULT_PORT}"));
    let public = std::env::var("ROLE_AGENTS_PUBLIC_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{DEFAULT_PORT}"));
    url::Url::parse(&public).map_err(|_| "ROLE_AGENTS_PUBLIC_URL is not a URL")?;
    let developer_patch = match std::env::var_os("DEVELOPER_PATCH_FILE") {
        Some(p) => {
            Some(std::fs::read_to_string(&p).map_err(|e| format!("DEVELOPER_PATCH_FILE: {e}"))?)
        }
        None => None,
    };
    let cfg = AgentsConfig {
        mcp_url: std::env::var("ROLE_AGENT_MCP_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8090/mcp".into()),
        developer_patch,
        pm_scope: std::env::var("PM_SCOPE")
            .unwrap_or_else(|_| "src".into())
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
    };
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("cannot bind {bind}: {e}"))?;
    tracing::info!(%bind, %public, "role agents listening");
    axum::serve(listener, router(&cfg, &public))
        .with_graceful_shutdown(agent_runtime::shutdown_signal())
        .await
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    agent_runtime::init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("role-agents: {e}");
            ExitCode::FAILURE
        }
    }
}
