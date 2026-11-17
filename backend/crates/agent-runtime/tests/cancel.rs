//! REVIEW-005: `CancelTask` must stop the agent's in-flight work (e.g. an LLM
//! call), not only mark the task canceled while the work keeps running.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use a2a::{AgentCard, AgentSkill};
use agent_runtime::Skill;
use contracts::{DraftArtifact, WorkerRequest};
use serde_json::{Value, json};

#[derive(Default)]
struct Flags {
    started: AtomicBool,
    finished: AtomicBool,
    dropped: AtomicBool,
}

/// Sets `dropped` when the in-flight work future is dropped.
struct DropGuard(Arc<Flags>);
impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

struct Sleeper(Arc<Flags>);

impl Skill for Sleeper {
    type Input = WorkerRequest;
    type Output = DraftArtifact;
    const ARTIFACT_NAME: &'static str = "draft";

    fn card(&self, public_url: &str) -> AgentCard {
        let skill = AgentSkill {
            id: "sleep".into(),
            name: "Sleep".into(),
            description: "Sleeps".into(),
            tags: vec![],
            examples: None,
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        };
        agent_runtime::agent_card("Sleeper", "test", "0", skill, public_url)
    }

    fn inference_mode(&self) -> &'static str {
        "mock"
    }

    async fn run(&self, _input: WorkerRequest) -> Result<DraftArtifact, String> {
        let _guard = DropGuard(self.0.clone());
        self.0.started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(30)).await;
        self.0.finished.store(true, Ordering::SeqCst);
        Err("unreachable in this test".into())
    }
}

async fn rpc(http: &reqwest::Client, url: &str, method: &str, params: Value) -> Value {
    http.post(format!("{url}/jsonrpc"))
        .header("A2A-Version", "1.0")
        .json(&json!({"jsonrpc": "2.0", "id": "1", "method": method, "params": params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn wait_for(flag: &AtomicBool) -> bool {
    for _ in 0..100 {
        if flag.load(Ordering::SeqCst) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn cancel_task_aborts_in_flight_work() {
    let flags = Arc::new(Flags::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = agent_runtime::router(Sleeper(flags.clone()), &url);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::new();

    let sent = rpc(
        &http,
        &url,
        "SendMessage",
        json!({
            "message": {"messageId": "m-1", "role": "ROLE_USER",
                "parts": [{"data": {"question": "A question that takes forever"}}]},
            "configuration": {"returnImmediately": true}
        }),
    )
    .await;
    let task = &sent["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_WORKING", "{sent}");
    let task_id = task["id"].as_str().unwrap().to_owned();
    assert!(wait_for(&flags.started).await, "work never started");

    let canceled = rpc(&http, &url, "CancelTask", json!({"id": task_id})).await;
    assert_eq!(
        canceled["result"]["status"]["state"], "TASK_STATE_CANCELED",
        "{canceled}"
    );

    assert!(
        wait_for(&flags.dropped).await,
        "in-flight work was not aborted"
    );
    assert!(!flags.finished.load(Ordering::SeqCst));
    let got = rpc(&http, &url, "GetTask", json!({"id": task_id})).await;
    assert_eq!(
        got["result"]["status"]["state"], "TASK_STATE_CANCELED",
        "{got}"
    );
}
