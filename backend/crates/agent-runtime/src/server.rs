//! A2A server plumbing shared by both agents, built on the official SDK.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use a2a::event::{StreamResponse, TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
use a2a::*;
use a2a_server::agent_card::agent_card_router;
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::{
    AgentExecutor, DefaultRequestHandler, ExecutorContext, InMemoryTaskStore, StaticAgentCard,
};
use contracts::{MEDIA_TYPE_JSON, Validate, data_part, decode_parts};
use futures::StreamExt;
use futures::future::{AbortHandle, Abortable};
use futures::stream::{self, BoxStream};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// One agent capability: a validated JSON input in, one JSON artifact out.
pub trait Skill: Send + Sync + 'static {
    type Input: DeserializeOwned + Validate + Send;
    type Output: Serialize + Send;

    /// Name of the artifact carrying [`Self::Output`].
    const ARTIFACT_NAME: &'static str;

    /// The Agent Card published at `/.well-known/agent-card.json`.
    fn card(&self, public_url: &str) -> AgentCard;

    /// Short label for logs (`mock` or `anthropic`).
    fn inference_mode(&self) -> &'static str;

    /// Does the work. Errors become an A2A `failed` task with this message.
    fn run(&self, input: Self::Input) -> impl Future<Output = Result<Self::Output, String>> + Send;
}

/// Builds a standards-compliant Agent Card advertising only JSON-RPC.
pub fn agent_card(
    name: &str,
    description: &str,
    version: &str,
    skill: AgentSkill,
    public_url: &str,
) -> AgentCard {
    let base = public_url.trim_end_matches('/');
    AgentCard {
        name: name.into(),
        description: description.into(),
        version: version.into(),
        provider: None,
        capabilities: AgentCapabilities {
            streaming: Some(false),
            push_notifications: Some(false),
            extensions: None,
            extended_agent_card: Some(false),
        },
        skills: vec![skill],
        default_input_modes: vec![MEDIA_TYPE_JSON.into()],
        default_output_modes: vec![MEDIA_TYPE_JSON.into()],
        supported_interfaces: vec![AgentInterface::new(
            format!("{base}/jsonrpc"),
            TRANSPORT_PROTOCOL_JSONRPC,
        )],
        security_schemes: None,
        security_requirements: None,
        documentation_url: None,
        icon_url: None,
        signatures: None,
    }
}

fn status(
    task_id: &str,
    context_id: &str,
    state: TaskState,
    text: Option<String>,
) -> StreamResponse {
    StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
        task_id: task_id.into(),
        context_id: context_id.into(),
        status: TaskStatus {
            state,
            message: text.map(|t| Message::new(Role::Agent, vec![Part::text(t)])),
            timestamp: None,
        },
        metadata: None,
    })
}

fn artifact(task_id: &str, context_id: &str, name: &str, part: Part) -> StreamResponse {
    StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
        task_id: task_id.into(),
        context_id: context_id.into(),
        artifact: Artifact {
            artifact_id: new_artifact_id(),
            name: Some(name.into()),
            description: None,
            parts: vec![part],
            metadata: None,
            extensions: None,
        },
        append: Some(false),
        last_chunk: Some(true),
        metadata: None,
    })
}

/// Adapts a [`Skill`] to the SDK's [`AgentExecutor`].
///
/// The SDK runs each execution in a detached task and its `CancelTask` only
/// calls [`AgentExecutor::cancel`]; it does not stop that task. So the
/// executor keeps an abort handle per running task and fires it on cancel,
/// which drops the in-flight work (e.g. an LLM request).
struct SkillExecutor<S> {
    skill: Arc<S>,
    running: Arc<Mutex<HashMap<String, AbortHandle>>>,
}

impl<S> SkillExecutor<S> {
    fn new(skill: S) -> Self {
        Self {
            skill: Arc::new(skill),
            running: Arc::default(),
        }
    }
}

/// Poison only means another thread panicked mid-update of a plain map; the
/// map itself is still usable, so recover instead of panicking.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn run_skill<S: Skill>(
    skill: Arc<S>,
    parts: Vec<Part>,
    task_id: String,
    context_id: String,
) -> Vec<StreamResponse> {
    let outcome = match decode_parts::<S::Input>(&parts) {
        // Bad input is the caller's fault: reject without doing work.
        Err(e) => Err((TaskState::Rejected, format!("invalid input: {e}"))),
        Ok(input) => match skill.run(input).await {
            Ok(output) => data_part(&output).map_err(|e| (TaskState::Failed, e.to_string())),
            Err(e) => Err((TaskState::Failed, e)),
        },
    };
    match outcome {
        Ok(part) => {
            tracing::info!(task_id = %task_id, outcome = "completed", "task finished");
            vec![
                artifact(&task_id, &context_id, S::ARTIFACT_NAME, part),
                status(&task_id, &context_id, TaskState::Completed, None),
            ]
        }
        Err((state, reason)) => {
            tracing::warn!(task_id = %task_id, outcome = ?state, %reason, "task not completed");
            vec![status(&task_id, &context_id, state, Some(reason))]
        }
    }
}

impl<S: Skill> AgentExecutor for SkillExecutor<S> {
    fn execute(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let (task_id, context_id) = ctx.task_info();
        let parts = ctx.message.map(|m| m.parts).unwrap_or_default();
        let working = status(&task_id, &context_id, TaskState::Working, None);
        let (abort, registration) = AbortHandle::new_pair();
        lock(&self.running).insert(task_id.to_string(), abort);
        let running = self.running.clone();
        let key = task_id.to_string();
        let work = Abortable::new(
            run_skill(self.skill.clone(), parts, task_id, context_id),
            registration,
        );
        let work = async move {
            let result = work.await;
            lock(&running).remove(&key);
            // Aborted: the cancel path already reported the terminal state.
            result.unwrap_or_default()
        };
        Box::pin(
            stream::iter([Ok(working)]).chain(
                stream::once(work).flat_map(|events| stream::iter(events.into_iter().map(Ok))),
            ),
        )
    }

    fn cancel(&self, ctx: ExecutorContext) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let (task_id, context_id) = ctx.task_info();
        if let Some(abort) = lock(&self.running).remove(task_id.as_str()) {
            abort.abort();
            tracing::info!(task_id = %task_id, "task canceled; in-flight work aborted");
        }
        let ev = status(&task_id, &context_id, TaskState::Canceled, None);
        Box::pin(stream::once(async move { Ok(ev) }))
    }
}

/// Largest JSON-RPC request an agent accepts. The biggest valid payload (a
/// review request: 4k question + 20k draft + metadata) is well below this.
pub const MAX_AGENT_BODY_BYTES: usize = 256 * 1024;

/// The complete HTTP surface of one agent: JSON-RPC, Agent Card, health.
pub fn router<S: Skill>(skill: S, public_url: &str) -> axum::Router {
    let card = skill.card(public_url);
    // Enforce what the card advertises: the SDK only refuses streaming,
    // push and extended-card calls for capabilities explicitly `Some(false)`
    // in the handler, independent of the published card.
    let capabilities = card.capabilities.clone();
    let card = Arc::new(StaticAgentCard::new(card));
    let handler = Arc::new(
        DefaultRequestHandler::new(SkillExecutor::new(skill), InMemoryTaskStore::new())
            .with_capabilities(capabilities),
    );
    let rpc = jsonrpc_router(handler);
    let rpc_slash = rpc.clone();
    axum::Router::new()
        .nest("/jsonrpc", rpc)
        // Same endpoint with a trailing slash: clients that join the
        // advertised URL with "/" (e.g. the official A2A TCK's httpx client)
        // otherwise get an empty 404. `nest` does not match that form.
        .route(
            "/jsonrpc/",
            axum::routing::post(move |mut req: axum::extract::Request| async move {
                *req.uri_mut() = axum::http::Uri::from_static("/");
                tower::ServiceExt::oneshot(rpc_slash, req).await
            }),
        )
        .merge(agent_card_router(card))
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        // Not `DefaultBodyLimit`: the SDK's `jsonrpc_router` sets its own
        // (10 MiB) and the innermost one wins. This layer limits the body
        // stream itself, so it holds for every nested route.
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            MAX_AGENT_BODY_BYTES,
        ))
}

/// Bind address and public URL, read from `<PREFIX>_BIND` / `<PREFIX>_PUBLIC_URL`.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub bind: String,
    pub public_url: String,
}

impl ServeConfig {
    pub fn from_env(prefix: &str, default_port: u16) -> Result<Self, String> {
        let bind = std::env::var(format!("{prefix}_BIND"))
            .unwrap_or_else(|_| format!("127.0.0.1:{default_port}"));
        let public_url = std::env::var(format!("{prefix}_PUBLIC_URL"))
            .unwrap_or_else(|_| format!("http://127.0.0.1:{default_port}"));
        url::Url::parse(&public_url)
            .map_err(|_| format!("{prefix}_PUBLIC_URL is not a valid URL"))?;
        Ok(Self { bind, public_url })
    }
}

pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
}

/// Serves one agent until Ctrl+C.
pub async fn serve<S: Skill>(skill: S, cfg: ServeConfig) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await
        .map_err(|e| format!("cannot bind {}: {e}", cfg.bind))?;
    let name = skill.card(&cfg.public_url).name;
    tracing::info!(agent = %name, bind = %cfg.bind, public_url = %cfg.public_url,
        inference = skill.inference_mode(), "A2A agent listening");
    axum::serve(listener, router(skill, &cfg.public_url))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| format!("server error: {e}"))
}
