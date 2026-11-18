//! Code Reviewer role agent (A2A). Environment: `CODE_REVIEWER_BIND`,
//! `CODE_REVIEWER_PUBLIC_URL` (default port 9111) and `ROLE_AGENT_MCP_URL`
//! (default `http://127.0.0.1:8090/mcp`; the only MCP server it will call).

use std::process::ExitCode;

use agent_runtime::ServeConfig;
use role_agents::{CodeReviewer, DEFAULT_PORT};

#[tokio::main]
async fn main() -> ExitCode {
    agent_runtime::init_tracing();
    let result = async {
        let mcp = std::env::var("ROLE_AGENT_MCP_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8090/mcp".to_owned());
        let cfg = ServeConfig::from_env("CODE_REVIEWER", DEFAULT_PORT)?;
        agent_runtime::serve(CodeReviewer::new(mcp), cfg).await
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("role-agents: {e}");
            ExitCode::FAILURE
        }
    }
}
