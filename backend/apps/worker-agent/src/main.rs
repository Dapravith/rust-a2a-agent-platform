//! Worker A2A agent binary. Configuration comes from the environment
//! (`WORKER_BIND`, `WORKER_PUBLIC_URL`, `INFERENCE_MODE`, ...).

use std::process::ExitCode;

use agent_runtime::{LlmClient, ServeConfig};
use worker_agent::{DEFAULT_PORT, Drafter};

#[tokio::main]
async fn main() -> ExitCode {
    agent_runtime::init_tracing();
    let result = async {
        let llm = LlmClient::from_env()?;
        let cfg = ServeConfig::from_env("WORKER", DEFAULT_PORT)?;
        agent_runtime::serve(Drafter::new(llm), cfg).await
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("worker-agent: {e}");
            ExitCode::FAILURE
        }
    }
}
