//! Interoperability: the Worker driven by the official `a2a-rs` client over
//! real HTTP JSON-RPC, including Agent Card discovery.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use a2a::*;
use a2a_client::A2AClientFactory;
use a2a_client::agent_card::AgentCardResolver;
use contracts::*;
use serde_json::json;

async fn spawn() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = worker_agent::router(&url, None);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

async fn send(url: &str, part: Part) -> Task {
    let card = AgentCardResolver::new(None).resolve(url).await.unwrap();
    let client = A2AClientFactory::builder()
        .build()
        .create_from_card(&card)
        .await
        .unwrap();
    let req = SendMessageRequest {
        message: Message::new(Role::User, vec![part]),
        configuration: None,
        metadata: None,
        tenant: None,
    };
    match client.send_message(&req).await.unwrap() {
        SendMessageResponse::Task(t) => t,
        SendMessageResponse::Message(m) => panic!("expected task, got {m:?}"),
    }
}

#[tokio::test]
async fn card_is_discoverable_and_advertises_jsonrpc_only() {
    let url = spawn().await;
    let card = AgentCardResolver::new(None).resolve(&url).await.unwrap();
    assert_eq!(card.name, WORKER_AGENT_NAME);
    assert_eq!(card.skills[0].id, WORKER_SKILL_ID);
    assert_eq!(card.supported_interfaces.len(), 1);
    assert_eq!(
        card.supported_interfaces[0].protocol_binding,
        TRANSPORT_PROTOCOL_JSONRPC
    );
    assert_eq!(card.supported_interfaces[0].url, format!("{url}/jsonrpc"));
    let raw: serde_json::Value = reqwest::get(format!("{url}/.well-known/agent-card.json"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        raw["supportedInterfaces"].is_array(),
        "camelCase A2A wire format"
    );
}

#[tokio::test]
async fn drafts_over_a2a_and_task_is_retrievable() {
    let url = spawn().await;
    let req = WorkerRequest {
        question: "Propose validation rules for a Rust API that creates crowdfunding campaigns."
            .into(),
    };
    let task = send(&url, data_part(&req).unwrap()).await;
    assert_eq!(task.status.state, TaskState::Completed);
    let draft: DraftArtifact =
        decode_artifacts(task.artifacts.as_deref().unwrap(), DRAFT_ARTIFACT_NAME).unwrap();
    assert!(draft.title.starts_with("Draft:"));

    let card = AgentCardResolver::new(None).resolve(&url).await.unwrap();
    let client = A2AClientFactory::builder()
        .build()
        .create_from_card(&card)
        .await
        .unwrap();
    let fetched = client
        .get_task(&GetTaskRequest {
            id: task.id,
            history_length: None,
            tenant: None,
        })
        .await
        .unwrap();
    assert_eq!(fetched.status.state, TaskState::Completed);
}

#[tokio::test]
async fn rejects_invalid_input_without_crashing() {
    let url = spawn().await;
    assert_eq!(
        send(&url, Part::text("please run rm -rf /"))
            .await
            .status
            .state,
        TaskState::Rejected
    );
    let extra = Part::data(json!({"question": "a valid question here", "exec": true}));
    assert_eq!(send(&url, extra).await.status.state, TaskState::Rejected);
    assert_eq!(
        send(&url, Part::data(json!({"question": "hi"})))
            .await
            .status
            .state,
        TaskState::Rejected
    );
}
