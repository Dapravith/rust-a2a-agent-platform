//! A2A client side of the orchestrator, built on the official SDK client.
//!
//! Responsibilities: discover the Agent Card from a *configured* base URL,
//! verify it is the agent we trust, call it over JSON-RPC, handle both direct
//! `Message` and `Task` responses (polling non-terminal tasks with `GetTask`),
//! enforce a deadline, and decode + validate the returned artifact.

use std::sync::Arc;
use std::time::Duration;

use a2a::*;
use a2a_client::A2AClientFactory;
use a2a_client::agent_card::AgentCardResolver;
use a2a_client::jsonrpc::JsonRpcTransportFactory;
use contracts::{ContractError, MEDIA_TYPE_JSON, Validate, decode_artifacts, decode_parts};
use serde::de::DeserializeOwned;
use url::Url;

const MAX_ERROR_CHARS: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent unreachable: {0}")]
    Unreachable(String),
    #[error("untrusted agent card: {0}")]
    UntrustedCard(String),
    #[error("A2A protocol error: {0}")]
    Protocol(String),
    #[error("agent did not finish within {0:?}")]
    Timeout(Duration),
    #[error("agent task ended in state {state}: {message}")]
    TaskFailed { state: String, message: String },
    #[error("agent requested an unsupported interaction ({0})")]
    Unsupported(String),
    #[error("agent returned an invalid artifact: {0}")]
    InvalidArtifact(#[from] ContractError),
}

impl AgentError {
    /// A bounded, user-safe description stored with the workflow.
    pub fn safe_message(&self) -> String {
        let s = self.to_string();
        let mut out: String = s.chars().take(MAX_ERROR_CHARS).collect();
        if s.chars().count() > MAX_ERROR_CHARS {
            out.push('…');
        }
        out
    }
}

/// A trusted agent: the configured base URL plus the card name we expect.
#[derive(Debug, Clone)]
pub struct AgentEndpoint {
    pub expected_name: String,
    pub base_url: Url,
}

/// Result of one successful agent call.
#[derive(Debug)]
pub struct AgentOutcome<T> {
    pub value: T,
    pub task_id: Option<String>,
    pub context_id: Option<String>,
    pub state: String,
}

#[derive(Clone)]
pub struct A2AGateway {
    resolver: Arc<AgentCardResolver>,
    factory: Arc<A2AClientFactory>,
    timeout: Duration,
    poll_interval: Duration,
}

fn state_name(s: &TaskState) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{s:?}"))
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Verifies the card identity and keeps only JSON-RPC interfaces on the
/// configured origin, so a card cannot redirect traffic to another host.
pub fn trusted_card(card: AgentCard, endpoint: &AgentEndpoint) -> Result<AgentCard, AgentError> {
    if card.name != endpoint.expected_name {
        return Err(AgentError::UntrustedCard(format!(
            "expected agent `{}`, found `{}`",
            endpoint.expected_name, card.name
        )));
    }
    let mut card = card;
    card.supported_interfaces.retain(|i| {
        i.protocol_binding == TRANSPORT_PROTOCOL_JSONRPC
            && Url::parse(&i.url).is_ok_and(|u| same_origin(&u, &endpoint.base_url))
    });
    if card.supported_interfaces.is_empty() {
        return Err(AgentError::UntrustedCard(
            "no JSON-RPC interface on the configured agent origin".into(),
        ));
    }
    Ok(card)
}

impl A2AGateway {
    pub fn new(timeout: Duration, poll_interval: Duration) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            // Slightly above the workflow deadline so the deadline, not the
            // socket, decides and is reported as a timeout.
            .timeout(timeout + Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let card_http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        // JSON-RPC only: the defaults (JSON-RPC + REST) are disabled.
        let factory = A2AClientFactory::builder()
            .no_defaults()
            .register(Arc::new(JsonRpcTransportFactory::new(Some(http))))
            .preferred_bindings(vec![TRANSPORT_PROTOCOL_JSONRPC.to_string()])
            .build();
        Ok(Self {
            resolver: Arc::new(AgentCardResolver::new(Some(card_http))),
            factory: Arc::new(factory),
            timeout,
            poll_interval,
        })
    }

    /// Fetches and verifies the agent's card.
    pub async fn discover(&self, endpoint: &AgentEndpoint) -> Result<AgentCard, AgentError> {
        let card = self
            .resolver
            .resolve(endpoint.base_url.as_str())
            .await
            .map_err(|e| AgentError::Unreachable(e.message.clone()))?;
        trusted_card(card, endpoint)
    }

    /// Sends one message and returns the decoded, validated artifact.
    pub async fn call<T: DeserializeOwned + Validate>(
        &self,
        endpoint: &AgentEndpoint,
        payload: Part,
        artifact_name: &str,
    ) -> Result<AgentOutcome<T>, AgentError> {
        let card = self.discover(endpoint).await?;
        let client = self
            .factory
            .create_from_card(&card)
            .await
            .map_err(|e| AgentError::Protocol(e.message.clone()))?;

        let request = SendMessageRequest {
            message: Message::new(Role::User, vec![payload]),
            configuration: Some(SendMessageConfiguration {
                accepted_output_modes: Some(vec![MEDIA_TYPE_JSON.into()]),
                task_push_notification_config: None,
                history_length: Some(0),
                return_immediately: None,
            }),
            metadata: None,
            tenant: None,
        };

        let work = async {
            match client.send_message(&request).await.map_err(map_rpc_error)? {
                SendMessageResponse::Message(m) => Ok(AgentOutcome {
                    value: decode_parts(&m.parts)?,
                    task_id: m.task_id.clone(),
                    context_id: m.context_id.clone(),
                    state: "message".into(),
                }),
                SendMessageResponse::Task(mut task) => {
                    // Poll until the task leaves submitted/working.
                    while matches!(task.status.state, TaskState::Submitted | TaskState::Working) {
                        tokio::time::sleep(self.poll_interval).await;
                        task = client
                            .get_task(&GetTaskRequest {
                                id: task.id.clone(),
                                history_length: Some(0),
                                tenant: None,
                            })
                            .await
                            .map_err(map_rpc_error)?;
                    }
                    finish_task(task, artifact_name)
                }
            }
        };

        match tokio::time::timeout(self.timeout, work).await {
            Ok(result) => result,
            Err(_) => Err(AgentError::Timeout(self.timeout)),
        }
    }
}

fn map_rpc_error(e: A2AError) -> AgentError {
    // Transport failures surface from the SDK as internal errors.
    AgentError::Protocol(format!("{} (code {})", e.message, e.code))
}

fn finish_task<T: DeserializeOwned + Validate>(
    task: Task,
    artifact_name: &str,
) -> Result<AgentOutcome<T>, AgentError> {
    let state = state_name(&task.status.state);
    match task.status.state {
        TaskState::Completed => Ok(AgentOutcome {
            value: decode_artifacts(task.artifacts.as_deref().unwrap_or_default(), artifact_name)?,
            task_id: Some(task.id),
            context_id: Some(task.context_id),
            state,
        }),
        TaskState::Failed | TaskState::Rejected | TaskState::Canceled => {
            let message = task
                .status
                .message
                .as_ref()
                .and_then(Message::text)
                .unwrap_or("no reason given")
                .chars()
                .take(MAX_ERROR_CHARS)
                .collect();
            Err(AgentError::TaskFailed { state, message })
        }
        // input-required, auth-required, unspecified: not part of this workflow.
        _ => Err(AgentError::Unsupported(state)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn card(name: &str, urls: &[(&str, &str)]) -> AgentCard {
        AgentCard {
            name: name.into(),
            description: "d".into(),
            version: "1".into(),
            provider: None,
            capabilities: AgentCapabilities::default(),
            skills: vec![],
            default_input_modes: vec![],
            default_output_modes: vec![],
            supported_interfaces: urls
                .iter()
                .map(|(u, p)| AgentInterface::new(*u, *p))
                .collect(),
            security_schemes: None,
            security_requirements: None,
            documentation_url: None,
            icon_url: None,
            signatures: None,
        }
    }

    fn endpoint() -> AgentEndpoint {
        AgentEndpoint {
            expected_name: "Draft Worker".into(),
            base_url: Url::parse("http://127.0.0.1:9101").unwrap(),
        }
    }

    #[test]
    fn rejects_wrong_name() {
        let c = card(
            "Evil",
            &[("http://127.0.0.1:9101/jsonrpc", TRANSPORT_PROTOCOL_JSONRPC)],
        );
        assert!(matches!(
            trusted_card(c, &endpoint()),
            Err(AgentError::UntrustedCard(_))
        ));
    }

    #[test]
    fn drops_foreign_origins_and_other_bindings() {
        let c = card(
            "Draft Worker",
            &[
                (
                    "http://attacker.example/jsonrpc",
                    TRANSPORT_PROTOCOL_JSONRPC,
                ),
                ("http://127.0.0.1:9101/rest", TRANSPORT_PROTOCOL_HTTP_JSON),
                ("http://127.0.0.1:9101/jsonrpc", TRANSPORT_PROTOCOL_JSONRPC),
            ],
        );
        let t = trusted_card(c, &endpoint()).unwrap();
        assert_eq!(t.supported_interfaces.len(), 1);
        assert_eq!(
            t.supported_interfaces[0].url,
            "http://127.0.0.1:9101/jsonrpc"
        );
    }

    #[test]
    fn rejects_card_with_only_foreign_interfaces() {
        let c = card(
            "Draft Worker",
            &[("http://127.0.0.1:9999/jsonrpc", TRANSPORT_PROTOCOL_JSONRPC)],
        );
        assert!(trusted_card(c, &endpoint()).is_err());
    }
}
