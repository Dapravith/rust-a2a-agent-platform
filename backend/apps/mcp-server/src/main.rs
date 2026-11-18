//! MCP server binary.
//!
//! Environment:
//! * `MCP_BIND` (default `127.0.0.1:8090`): must be a loopback address.
//! * `MCP_TOKEN_KEY`: hex HMAC key shared with the orchestrator, at least 32 bytes.
//! * `MCP_WORKSPACES_ROOT`: directory holding `<workspace-id>/` git work trees.
//! * `MCP_DATABASE_URL`: login role in `a2a_mcp` (INSERT on `eng_tool_calls` only).
//! * `A2A_SANDBOX_IMAGE` (optional): digest-pinned image for `testing.run`.
//!   Unset means `testing.run` answers `sandbox_unavailable`.
//! * `A2A_SANDBOX_TIMEOUT_SECS` (default 600), `A2A_SANDBOX_MEMORY` (2g),
//!   `A2A_SANDBOX_CPUS` (2), `A2A_SANDBOX_PIDS` (256).

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use mcp_server::{McpState, PgAudit, router};
use workspace::{CheckRunner, DockerRunner, DockerRunnerConfig};

fn var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}

fn var_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

async fn run() -> Result<(), String> {
    let bind: SocketAddr = var_or("MCP_BIND", "127.0.0.1:8090")
        .parse()
        .map_err(|e| format!("MCP_BIND: {e}"))?;
    if !bind.ip().is_loopback() {
        return Err("MCP_BIND must be a loopback address (the MCP server is private)".into());
    }
    let key = hex::decode(var("MCP_TOKEN_KEY")?).map_err(|_| "MCP_TOKEN_KEY must be hex")?;
    if key.len() < engineering::mcp::MIN_KEY_BYTES {
        return Err("MCP_TOKEN_KEY must be at least 32 bytes".into());
    }
    let root = std::path::PathBuf::from(var("MCP_WORKSPACES_ROOT")?)
        .canonicalize()
        .map_err(|e| format!("MCP_WORKSPACES_ROOT: {e}"))?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&var("MCP_DATABASE_URL")?)
        .await
        .map_err(|e| format!("database: {e}"))?;
    let runner: Option<Arc<dyn CheckRunner>> = match std::env::var("A2A_SANDBOX_IMAGE") {
        Ok(image) => {
            let num = |n: &str, d: &str| var_or(n, d);
            let cfg = DockerRunnerConfig {
                image,
                timeout: Duration::from_secs(
                    num("A2A_SANDBOX_TIMEOUT_SECS", "600")
                        .parse()
                        .map_err(|_| "A2A_SANDBOX_TIMEOUT_SECS")?,
                ),
                memory: num("A2A_SANDBOX_MEMORY", "2g"),
                cpus: num("A2A_SANDBOX_CPUS", "2"),
                pids: num("A2A_SANDBOX_PIDS", "256")
                    .parse()
                    .map_err(|_| "A2A_SANDBOX_PIDS")?,
            };
            Some(Arc::new(DockerRunner::new(cfg).map_err(|e| e.to_string())?))
        }
        Err(_) => {
            tracing::warn!("A2A_SANDBOX_IMAGE unset: testing.run will answer sandbox_unavailable");
            None
        }
    };
    let state = Arc::new(McpState {
        key,
        workspaces_root: root,
        audit: Arc::new(PgAudit::new(pool)),
        runner,
        locks: Default::default(),
    });
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("bind {bind}: {e}"))?;
    tracing::info!(%bind, "mcp-server listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mcp-server: {e}");
            ExitCode::FAILURE
        }
    }
}
