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
    assert!(draft.title.starts_with("Mock draft:"));
    assert!(draft.mock, "mock drafts must be flagged");

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

/// The advertised JSON-RPC URL must also work with a trailing slash
/// (found by the official A2A TCK, whose client posts to `<url>/`).
#[tokio::test]
async fn jsonrpc_endpoint_accepts_trailing_slash() {
    let url = spawn().await;
    let body = json!({"jsonrpc": "2.0", "id": "1", "method": "GetTask",
        "params": {"id": "does-not-exist"}});
    for path in ["/jsonrpc", "/jsonrpc/"] {
        let resp: serde_json::Value = reqwest::Client::new()
            .post(format!("{url}{path}"))
            .header("A2A-Version", "1.0")
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], -32001, "{path}: {resp}");
    }
}

/// Capabilities the card declares `false` are refused with
/// UnsupportedOperationError (-32004), not silently served (found by the TCK).
#[tokio::test]
async fn undeclared_capabilities_are_refused() {
    let url = spawn().await;
    let card = AgentCardResolver::new(None).resolve(&url).await.unwrap();
    assert_eq!(card.capabilities.streaming, Some(false));
    assert_eq!(card.capabilities.extended_agent_card, Some(false));
    let message = json!({"messageId": "m", "role": "ROLE_USER", "parts": [{"text": "hi"}]});
    for (method, params) in [
        ("SendStreamingMessage", json!({"message": message})),
        ("GetExtendedAgentCard", json!({})),
    ] {
        let resp = reqwest::Client::new()
            .post(format!("{url}/jsonrpc"))
            .header("A2A-Version", "1.0")
            .json(&json!({"jsonrpc": "2.0", "id": "1", "method": method, "params": params}))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], -32004, "{method}: {body}");
    }
}
