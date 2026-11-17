//! Phase 3: persistence, least privilege, recovery and hosted inference.
//!
//! Real Postgres (`sqlx::test`, one fresh database per test), real A2A
//! agents over HTTP, and a controlled HTTP test double for the Anthropic
//! Messages API. The restart test runs the actual `api` binary and kills it.
//! Requires `DATABASE_URL` (a local, disposable server: tests create roles).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use a2a::event::{StreamResponse, TaskStatusUpdateEvent};
use a2a::*;
use a2a_server::agent_card::agent_card_router;
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::{
    AgentExecutor, DefaultRequestHandler, ExecutorContext, InMemoryTaskStore, StaticAgentCard,
};
use agent_runtime::{LlmClient, LlmConfig, Skill};
use api::config::Config;
use api::db::{self, DbError, Failure};
use api::engine::Engine;
use api::error_code::ErrorCode;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use url::Url;

const QUESTION: &str =
    "Propose validation rules for a Rust API that creates crowdfunding campaigns.";
const SECS_5: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn bind() -> (tokio::net::TcpListener, String) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    (l, url)
}

async fn serve(app: axum::Router) -> String {
    let (l, url) = bind().await;
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

async fn worker(llm: Option<LlmClient>) -> String {
    let (l, url) = bind().await;
    let app = worker_agent::router(&url, llm);
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

async fn reviewer(llm: Option<LlmClient>) -> String {
    let (l, url) = bind().await;
    let app = reviewer_agent::router(&url, llm);
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

/// An A2A Worker that reports WORKING and never finishes.
struct Hanging;
impl AgentExecutor for Hanging {
    fn execute(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let (t, c) = ctx.task_info();
        let working = StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
            task_id: t,
            context_id: c,
            status: TaskStatus {
                state: TaskState::Working,
                message: None,
                timestamp: None,
            },
            metadata: None,
        });
        Box::pin(stream::iter([Ok(working)]).chain(stream::pending()))
    }
    fn cancel(
        &self,
        _ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        Box::pin(stream::empty())
    }
}

async fn hanging_worker() -> String {
    let (l, url) = bind().await;
    let card = worker_agent::Drafter::new(None).card(&url);
    let handler = Arc::new(DefaultRequestHandler::new(
        Hanging,
        InMemoryTaskStore::new(),
    ));
    let app = axum::Router::new()
        .nest("/jsonrpc", jsonrpc_router(handler))
        .merge(agent_card_router(Arc::new(StaticAgentCard::new(card))));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
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

async fn api(pool: PgPool, worker: &str, reviewer: &str) -> String {
    let engine = Engine::new(pool, &config(worker, reviewer, SECS_5)).unwrap();
    serve(api::app(engine, None)).await
}

async fn submit(base: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/workflows"))
        .json(&json!({ "question": QUESTION }))
        .send()
        .await
        .unwrap()
}

async fn get(base: &str, id: &str) -> Value {
    reqwest::get(format!("{base}/api/workflows/{id}"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn wait_status(base: &str, id: &str, done: &[&str]) -> Value {
    for _ in 0..400 {
        let w = get(base, id).await;
        if done.iter().any(|s| w["status"] == *s) {
            return w;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("workflow {id} never reached {done:?}");
}

async fn run(base: &str) -> Value {
    let r = submit(base).await;
    assert_eq!(r.status(), 202);
    let id = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_status(base, &id, &["completed", "failed"]).await
}

/// Inserts a failed workflow row directly (for constraint tests).
async fn insert_failed(pool: &PgPool, code: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO workflows (id, question, status, error, error_code, finished_at) \
         VALUES ($1, 'a question here', 'failed', 'x', $2, now())",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(code)
    .execute(pool)
    .await
    .map(|_| ())
}

fn sqlstate(e: &sqlx::Error) -> String {
    match e {
        sqlx::Error::Database(d) => d.code().map(|c| c.to_string()).unwrap_or_default(),
        other => format!("not a database error: {other}"),
    }
}

// ---------------------------------------------------------------------------
// P3-01: migrations and schema constraints
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn migrations_apply_and_schema_is_verified(pool: PgPool) {
    db::verify_schema(&pool).await.unwrap();
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied as usize, db::MIGRATOR.iter().count());

    // A schema this binary does not know is refused, not silently used.
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (29991231000000, 'future', true, '\\x00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let err = db::verify_schema(&pool).await.unwrap_err();
    assert!(err.contains("unknown to this binary"), "{err}");

    // A missing migration is refused with an actionable message.
    sqlx::query("DELETE FROM _sqlx_migrations")
        .execute(&pool)
        .await
        .unwrap();
    let err = db::verify_schema(&pool).await.unwrap_err();
    assert!(err.contains("run `api migrate`"), "{err}");
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn schema_constraints_reject_invalid_rows(pool: PgPool) {
    // Every application error code is accepted by the database...
    for code in ErrorCode::ALL {
        insert_failed(&pool, code.as_str()).await.unwrap();
    }
    // ...and nothing else.
    let e = insert_failed(&pool, "made_up").await.unwrap_err();
    assert_eq!(sqlstate(&e), "23514", "{e}");

    let cases: [(&str, &str); 6] = [
        // unknown status
        (
            "INSERT INTO workflows (id, question, status) VALUES (gen_random_uuid(), 'a question here', 'paused')",
            "23514",
        ),
        // failed without a code
        (
            "INSERT INTO workflows (id, question, status, error, finished_at) VALUES (gen_random_uuid(), 'a question here', 'failed', 'x', now())",
            "23514",
        ),
        // completed without verdict
        (
            "INSERT INTO workflows (id, question, status, finished_at) VALUES (gen_random_uuid(), 'a question here', 'completed', now())",
            "23514",
        ),
        // terminal without finished_at
        (
            "INSERT INTO workflows (id, question, status, verdict) VALUES (gen_random_uuid(), 'a question here', 'completed', 'pass')",
            "23514",
        ),
        // run for a workflow that does not exist
        (
            "INSERT INTO agent_runs (id, workflow_id, agent, status) VALUES (gen_random_uuid(), gen_random_uuid(), 'worker', 'running')",
            "23503",
        ),
        // unknown agent
        (
            "INSERT INTO agent_runs (id, workflow_id, agent, status) SELECT gen_random_uuid(), id, 'planner', 'running' FROM workflows LIMIT 1",
            "23514",
        ),
    ];
    for (sql, state) in cases {
        let e = sqlx::query(sql).execute(&pool).await.unwrap_err();
        assert_eq!(sqlstate(&e), state, "{sql}: {e}");
    }

    // At most one active workflow, enforced by the database.
    db::create_workflow(&pool, QUESTION, None).await.unwrap();
    assert!(matches!(
        db::create_workflow(&pool, QUESTION, None).await,
        Err(DbError::ActiveWorkflowExists)
    ));
}

// ---------------------------------------------------------------------------
// P3-02 / P3-06: repository semantics
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn transitions_happen_at_most_once(pool: PgPool) {
    let (wf, _) = db::create_workflow(&pool, QUESTION, None).await.unwrap();
    db::mark_running(&pool, wf.id).await.unwrap();
    assert!(matches!(
        db::mark_running(&pool, wf.id).await,
        Err(DbError::InvalidTransition { .. })
    ));

    let run = db::start_run(&pool, wf.id, "worker").await.unwrap();
    let content = json!({"title":"t","summary":"s","content":"c","assumptions":[]});
    let result = || db::RunResult {
        task_id: Some("task-1"),
        context_id: Some("ctx-1"),
        a2a_state: "TASK_STATE_COMPLETED",
        kind: "draft",
        content: &content,
    };
    db::complete_run(&pool, wf.id, run, result()).await.unwrap();
    // A second completion is refused and rolls back: still one artifact.
    assert!(matches!(
        db::complete_run(&pool, wf.id, run, result()).await,
        Err(DbError::InvalidTransition { .. })
    ));
    assert_eq!(db::get_artifacts(&pool, wf.id).await.unwrap().len(), 1);
    assert!(matches!(
        db::fail_run(&pool, run, ErrorCode::AgentTimeout, "late").await,
        Err(DbError::InvalidTransition { .. })
    ));
    let runs = db::get_runs(&pool, wf.id).await.unwrap();
    assert_eq!(runs[0].status, "completed");
    assert_eq!(runs[0].a2a_task_id.as_deref(), Some("task-1"));

    db::complete_workflow(&pool, wf.id, "pass").await.unwrap();
    assert!(matches!(
        db::complete_workflow(&pool, wf.id, "needs_changes").await,
        Err(DbError::InvalidTransition { .. })
    ));
    // A late failure never overwrites the recorded completion.
    let late = Failure {
        code: ErrorCode::AgentTimeout,
        stage: Some("reviewer"),
        error: "late",
    };
    assert!(matches!(
        db::fail_workflow(&pool, wf.id, late).await,
        Err(DbError::InvalidTransition { .. })
    ));
    let after = db::get_workflow(&pool, wf.id).await.unwrap().unwrap();
    assert_eq!(after.status, "completed");
    assert_eq!(after.verdict.as_deref(), Some("pass"));
    assert!(after.error_code.is_none());
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn recovery_records_code_and_stage(pool: PgPool) {
    let (wf, _) = db::create_workflow(&pool, QUESTION, None).await.unwrap();
    db::mark_running(&pool, wf.id).await.unwrap();
    db::start_run(&pool, wf.id, "worker").await.unwrap();

    assert_eq!(db::recover_interrupted(&pool).await.unwrap(), 1);
    let after = db::get_workflow(&pool, wf.id).await.unwrap().unwrap();
    assert_eq!(after.status, "failed");
    assert_eq!(after.error_code.as_deref(), Some("interrupted"));
    assert_eq!(after.failed_stage.as_deref(), Some("worker"));
    let runs = db::get_runs(&pool, wf.id).await.unwrap();
    assert_eq!(runs[0].error_code.as_deref(), Some("interrupted"));
    // Idempotent: nothing left to recover.
    assert_eq!(db::recover_interrupted(&pool).await.unwrap(), 0);
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn only_one_instance_may_hold_the_database(pool: PgPool) {
    let first = db::InstanceLock::acquire(&pool).await.unwrap();
    let err = db::InstanceLock::acquire(&pool).await.err().unwrap();
    assert!(err.contains("another orchestrator instance"), "{err}");
    drop(first);
    // The server releases the lock when the session ends.
    for _ in 0..100 {
        if db::InstanceLock::acquire(&pool).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("instance lock was not released");
}

/// Database outage at request time: stable 503 codes, no internals leaked.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn database_outage_is_reported_safely(pool: PgPool) {
    let own = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(2))
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let base = api(own.clone(), "http://127.0.0.1:1", "http://127.0.0.1:2").await;
    own.close().await;

    let r = submit(&base).await;
    assert_eq!(r.status(), 503);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unavailable");
    let text = body.to_string();
    for leak in ["sqlx", "pool", "postgres", "closed"] {
        assert!(
            !text.to_lowercase().contains(leak),
            "leaks `{leak}`: {text}"
        );
    }
    let health = reqwest::get(format!("{base}/api/health")).await.unwrap();
    assert_eq!(health.status(), 503);
    // Liveness does not depend on the database.
    let live = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(live.status(), 200);
}

/// Phase 5 regression: a database outage at the moment a failure is recorded
/// must not leave the workflow active. Before the fix the single attempt to
/// record `failed` was lost, the row stayed `running`, and every new
/// submission was refused with 409 until the API restarted.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn failure_is_recorded_after_a_transient_database_outage(pool: PgPool) {
    // One connection: while the test holds it, every query times out, i.e.
    // the database is unavailable to the engine.
    let own = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect_lazy_with((*pool.connect_options()).clone());
    // Open that connection first; connecting can be slow on a busy host.
    for attempt in 1.. {
        match own.acquire().await {
            Ok(_) => break,
            Err(e) if attempt < 15 => eprintln!("warm-up attempt {attempt}: {e}"),
            Err(e) => panic!("cannot connect: {e}"),
        }
    }
    let base = api(own.clone(), &hanging_worker().await, "http://127.0.0.1:2").await;
    let r = submit(&base).await;
    assert_eq!(r.status(), 202);
    let id = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The worker hangs; the 5 s agent deadline (+ <= 5 s CancelTask) expires
    // while the database is "down", so recording the failure hits the outage.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let outage = own.acquire().await.unwrap();
    tokio::time::sleep(Duration::from_secs(25)).await;
    drop(outage);

    let wf = wait_status(&base, &id, &["failed"]).await;
    assert_eq!(wf["error_code"], "agent_timeout");
    assert_eq!(wf["failed_stage"], "worker");
    assert_eq!(wf["runs"][0]["status"], "failed");
    // Capacity is released: the next submission is admitted, not 409.
    assert_eq!(submit(&base).await.status(), 202);
}

// ---------------------------------------------------------------------------
// P3-01: least-privilege runtime role
// ---------------------------------------------------------------------------

/// A LOGIN member of `a2a_app` (dropped at the end of the test).
struct RuntimeRole {
    admin: PgPool,
    name: String,
    pool: PgPool,
}

impl RuntimeRole {
    async fn create(admin: &PgPool) -> Self {
        let name = format!(
            "a2a_api_test_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        // Identifier is generated above (hex only), so formatting is safe here.
        let sql = format!("CREATE ROLE {name} LOGIN PASSWORD 'test-only-pw' IN ROLE a2a_app");
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(admin)
            .await
            .unwrap();
        let opts: PgConnectOptions = (*admin.connect_options())
            .clone()
            .username(&name)
            .password("test-only-pw");
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .unwrap();
        Self {
            admin: admin.clone(),
            name,
            pool,
        }
    }

    async fn drop_role(self) {
        self.pool.close().await;
        let sql = format!("DROP ROLE IF EXISTS {}", self.name);
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn runtime_role_runs_the_workflow_but_cannot_change_schema(pool: PgPool) {
    let role = RuntimeRole::create(&pool).await;
    let app_pool = role.pool.clone();

    // Everything `api serve` does at startup works with the runtime role.
    db::verify_schema(&app_pool).await.unwrap();
    let lock = db::InstanceLock::acquire(&app_pool).await.unwrap();
    db::recover_interrupted(&app_pool).await.unwrap();

    // Full A2A workflow persisted through the restricted role.
    let base = api(app_pool.clone(), &worker(None).await, &reviewer(None).await).await;
    let wf = run(&base).await;
    assert_eq!(wf["status"], "completed", "{wf}");
    assert!(wf["draft"].is_object() && wf["review"].is_object());

    // Not granted: deletes, truncation, DDL, migration history writes.
    for sql in [
        "DELETE FROM workflows",
        "TRUNCATE artifacts",
        "DROP TABLE artifacts",
        "ALTER TABLE workflows ADD COLUMN x int",
        "CREATE TABLE public.sneaky (i int)",
        "DELETE FROM _sqlx_migrations",
        "ALTER TABLE workflows DISABLE ROW LEVEL SECURITY",
    ] {
        let e = sqlx::raw_sql(sql).execute(&app_pool).await.unwrap_err();
        assert_eq!(sqlstate(&e), "42501", "`{sql}` should be denied: {e}");
    }
    drop(lock);
    role.drop_role().await;
}

// ---------------------------------------------------------------------------
// P3-04 / P3-07: hosted inference through a provider test double
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum ReviewReply {
    Valid,
    Malformed,
    Refusal,
}

/// Anthropic Messages API stand-in: drafts for the Worker prompt, reviews
/// for the Reviewer prompt (told apart by the system prompt).
async fn provider(review: ReviewReply) -> String {
    let app = axum::Router::new().route(
        "/v1/messages",
        axum::routing::post(move |axum::Json(req): axum::Json<Value>| async move {
            let reviewer = req["system"].as_str().unwrap().contains("Reviewer agent");
            let text = if !reviewer {
                json!({"title":"Campaign validation","summary":"Field rules",
                       "content":"- name: required, maximum 120 chars\n- goal: integer minor units, currency ISO 4217",
                       "assumptions":["Single currency per campaign"]}).to_string()
            } else {
                match review {
                    ReviewReply::Valid => json!({"verdict":"pass","summary":"Covers limits and currency",
                                                 "findings":[]}).to_string(),
                    ReviewReply::Malformed => "{\"verdict\": \"maybe\", \"summary\": ".into(),
                    ReviewReply::Refusal => {
                        return axum::Json(json!({"stop_reason":"refusal","content":[]}));
                    }
                }
            };
            axum::Json(json!({"stop_reason":"end_turn","content":[
                {"type":"thinking","thinking":""},{"type":"text","text":text}]}))
        }),
    );
    serve(app).await
}

fn hosted(base_url: &str) -> LlmClient {
    LlmClient::new(LlmConfig {
        api_key: "sk-test-not-a-real-key-000".into(),
        model: "claude-opus-5-5".into(),
        base_url: base_url.into(),
        timeout: Duration::from_secs(3),
        deadline: Duration::from_secs(4),
        max_tokens: 16_000,
        effort: "medium".into(),
        refusal_fallback: true,
    })
    .unwrap()
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn hosted_mode_end_to_end_is_persisted(pool: PgPool) {
    let llm = provider(ReviewReply::Valid).await;
    let (w, r) = (
        worker(Some(hosted(&llm))).await,
        reviewer(Some(hosted(&llm))).await,
    );
    let base = api(pool.clone(), &w, &r).await;
    let wf = run(&base).await;
    assert_eq!(wf["status"], "completed", "{wf}");
    assert_eq!(wf["verdict"], "pass");
    assert_eq!(wf["draft"]["title"], "Campaign validation");
    assert_eq!(wf["review"]["summary"], "Covers limits and currency");
    for r in wf["runs"].as_array().unwrap() {
        assert_eq!(r["a2a_state"], "TASK_STATE_COMPLETED");
        assert!(r["a2a_task_id"].is_string());
    }
    // Stored, not just returned.
    let id = uuid::Uuid::parse_str(wf["id"].as_str().unwrap()).unwrap();
    let kinds: Vec<String> = db::get_artifacts(&pool, id)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.kind)
        .collect();
    assert_eq!(kinds, ["draft", "review"]);
}

async fn expect_reviewer_llm_failure(pool: PgPool, reply: ReviewReply, code: &str) {
    let llm = provider(reply).await;
    let (w, r) = (
        worker(Some(hosted(&llm))).await,
        reviewer(Some(hosted(&llm))).await,
    );
    let base = api(pool, &w, &r).await;
    let wf = run(&base).await;
    assert_eq!(wf["status"], "failed", "{wf}");
    assert_eq!(wf["error_code"], code, "{wf}");
    assert_eq!(wf["failed_stage"], "reviewer", "{wf}");
    assert!(wf["verdict"].is_null() && wf["review"].is_null());
    // The Worker's draft was stored before Reviewer delegation and survives.
    assert_eq!(wf["draft"]["title"], "Campaign validation");
    assert_eq!(wf["runs"][0]["status"], "completed");
    assert_eq!(wf["runs"][1]["status"], "failed");
    assert_eq!(wf["runs"][1]["error_code"], code);
    assert!(!wf.to_string().contains("sk-test"), "API key leaked: {wf}");
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn malformed_reviewer_json_fails_with_stable_code(pool: PgPool) {
    expect_reviewer_llm_failure(pool, ReviewReply::Malformed, "llm_output_invalid").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn reviewer_refusal_fails_with_stable_code(pool: PgPool) {
    expect_reviewer_llm_failure(pool, ReviewReply::Refusal, "llm_refused").await;
}

/// The provider is down: the failure is reported, never answered by mock.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn provider_outage_never_falls_back_to_mock(pool: PgPool) {
    let (l, dead) = bind().await;
    drop(l);
    let (w, r) = (
        worker(Some(hosted(&dead))).await,
        reviewer(Some(hosted(&dead))).await,
    );
    let base = api(pool, &w, &r).await;
    let wf = run(&base).await;
    assert_eq!(wf["status"], "failed", "{wf}");
    assert_eq!(wf["error_code"], "llm_unavailable");
    assert_eq!(wf["failed_stage"], "worker");
    assert!(wf["draft"].is_null(), "no mock draft may appear: {wf}");
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn stage_and_code_recorded_for_a2a_failures(pool: PgPool) {
    let (l, dead) = bind().await;
    drop(l);
    let base = api(pool, &dead, &reviewer(None).await).await;
    let wf = run(&base).await;
    assert_eq!(wf["error_code"], "agent_unreachable");
    assert_eq!(wf["failed_stage"], "worker");
    assert_eq!(wf["runs"][0]["error_code"], "agent_unreachable");
}

// ---------------------------------------------------------------------------
// P3-03 / P3-07: real process restart
// ---------------------------------------------------------------------------

/// The `api` binary against this test's database.
struct ApiProcess {
    child: tokio::process::Child,
    base: String,
}

fn test_db_url(pool: &PgPool) -> String {
    let base = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let mut url = Url::parse(&base).unwrap();
    url.set_path(&format!(
        "/{}",
        pool.connect_options().get_database().unwrap()
    ));
    url.to_string()
}

async fn start_api(db_url: &str, worker: &str, reviewer: &str) -> ApiProcess {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_api"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("DATABASE_URL", db_url)
        .env("ORCHESTRATOR_BIND", format!("127.0.0.1:{port}"))
        .env("WORKER_URL", worker)
        .env("REVIEWER_URL", reviewer)
        .env("AGENT_TIMEOUT_SECS", "60")
        .env("AGENT_POLL_INTERVAL_MS", "50")
        .env("RUST_LOG", "warn")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..200 {
        if reqwest::get(format!("{base}/api/health"))
            .await
            .is_ok_and(|r| r.status() == 200)
        {
            return ApiProcess { child, base };
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("api process did not become ready");
}

impl ApiProcess {
    /// SIGKILL: no graceful shutdown, like a crash or OOM kill.
    async fn kill(mut self) {
        self.child.kill().await.unwrap();
    }
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn workflows_survive_a_killed_process_and_interrupted_ones_fail(pool: PgPool) {
    let db_url = test_db_url(&pool);
    let (w, r, stuck) = (
        worker(None).await,
        reviewer(None).await,
        hanging_worker().await,
    );

    // 1. Complete a workflow, then kill the process.
    let a = start_api(&db_url, &w, &r).await;
    let done = run(&a.base).await;
    assert_eq!(done["status"], "completed", "{done}");
    a.kill().await;

    // 2. Start a workflow that is mid-flight (Worker never answers); kill.
    let b = start_api(&db_url, &stuck, &r).await;
    let resp = submit(&b.base).await;
    assert_eq!(resp.status(), 202);
    let pending = resp.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_status(&b.base, &pending, &["running"]).await;
    // `running` is set just before the Worker run row is inserted: wait for
    // the run itself, so the kill lands mid-call to the agent.
    let mut mid = Value::Null;
    for _ in 0..200 {
        mid = get(&b.base, &pending).await;
        if mid["runs"][0]["status"] == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(mid["runs"][0]["status"], "running", "{mid}");
    b.kill().await;

    // 3. Restart: history intact, interrupted workflow failed, not replayed.
    let c = start_api(&db_url, &w, &r).await;
    let again = get(&c.base, done["id"].as_str().unwrap()).await;
    assert_eq!(again["status"], "completed");
    assert_eq!(again["draft"], done["draft"]);
    assert_eq!(again["review"], done["review"]);

    let failed = get(&c.base, &pending).await;
    assert_eq!(failed["status"], "failed", "{failed}");
    assert_eq!(failed["error_code"], "interrupted");
    assert_eq!(failed["failed_stage"], "worker");
    assert_eq!(failed["runs"].as_array().unwrap().len(), 1, "not replayed");
    assert_eq!(failed["runs"][0]["status"], "failed");

    // The admission slot is free again and the platform keeps working.
    let next = run(&c.base).await;
    assert_eq!(next["status"], "completed", "{next}");
    let list: Value = reqwest::get(format!("{}/api/workflows", c.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
    c.kill().await;
}

/// A second orchestrator against the same database refuses to start (it
/// would otherwise fail the first one's running workflow during recovery).
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn second_api_process_refuses_to_start(pool: PgPool) {
    let db_url = test_db_url(&pool);
    let first = start_api(&db_url, "http://127.0.0.1:1", "http://127.0.0.1:2").await;
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_api"))
        .env_clear()
        .env("DATABASE_URL", &db_url)
        .env("ORCHESTRATOR_BIND", "127.0.0.1:0")
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    let log =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(log.contains("another orchestrator instance"), "{log}");
    assert!(
        !log.contains("postgres:postgres"),
        "credentials logged: {log}"
    );
    first.kill().await;
}

/// `api serve` against an unmigrated database refuses to start.
#[sqlx::test(migrations = false)]
async fn serve_requires_migrated_schema(pool: PgPool) {
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_api"))
        .env_clear()
        .env("DATABASE_URL", test_db_url(&pool))
        .env("ORCHESTRATOR_BIND", "127.0.0.1:0")
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(log.contains("api migrate"), "{log}");

    // `api migrate` fixes it.
    let migrate = tokio::process::Command::new(env!("CARGO_BIN_EXE_api"))
        .arg("migrate")
        .env_clear()
        .env("DATABASE_URL", test_db_url(&pool))
        .output()
        .await
        .unwrap();
    assert!(
        migrate.status.success(),
        "{}",
        String::from_utf8_lossy(&migrate.stdout)
    );
    db::verify_schema(&pool).await.unwrap();
}

/// A remote database without verified TLS is refused before connecting.
#[tokio::test]
async fn remote_database_without_verified_tls_is_refused() {
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_api"))
        .env_clear()
        .env(
            "DATABASE_URL",
            "postgresql://app.ref:supersecret@db.example.com:5432/postgres?sslmode=require",
        )
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(log.contains("sslmode=verify-full"), "{log}");
    assert!(log.contains("refused to connect"), "{log}");
    assert!(!log.contains("supersecret"), "password logged: {log}");
}
