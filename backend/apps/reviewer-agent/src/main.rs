//! Reviewer A2A agent binary. Configuration comes from the environment
//! (`REVIEWER_BIND`, `REVIEWER_PUBLIC_URL`, `INFERENCE_MODE`, ...).

use std::process::ExitCode;

use agent_runtime::{LlmClient, ServeConfig};
use reviewer_agent::{DEFAULT_PORT, Reviewer};

#[tokio::main]
async fn main() -> ExitCode {
    agent_runtime::init_tracing();
    let result = async {
        let llm = LlmClient::from_env()?;
        let cfg = ServeConfig::from_env("REVIEWER", DEFAULT_PORT)?;
        agent_runtime::serve(Reviewer::new(llm), cfg).await
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("reviewer-agent: {e}");
            ExitCode::FAILURE
        }
    }
}
