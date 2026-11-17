//! Phase 6, Mode B: provider contract tests for the Anthropic Messages API
//! adapter, against a controlled local HTTP test double of `/v1/messages`.
//! Real orchestrator, real Postgres; no paid model is called.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_runtime::{LlmClient, LlmConfig};
use api::eng::agents::RoleAgent;
use api::eng::approval::{self, Decision, DecisionRequest, GateKind};
use api::eng::auth::Principal;
use api::eng::orchestrator::Orchestrator;
use api::eng::providers::anthropic::{AnthropicRoleAgent, PROVIDER};
use api::eng::sim;
use api::eng::store::{self, EngWorkflow};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use engineering::artifacts::ArtifactKind;
use engineering::roles::Role;
use engineering::state::{Budget, HumanRole};
use serde_json::{Value, json};
use sqlx::PgPool;

const KEY: &str = "sk-stub-provider-0123456789";
const REQUIREMENTS: &str =
    "Add validation to the Rust Axum POST /campaigns endpoint without changing unrelated routes.";
const BASE: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";

type Reply = Arc<dyn Fn(usize) -> Response + Send + Sync>;

struct Stub {
    base: String,
    count: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Value>>>,
}

async fn stub(reply: Reply) -> Stub {
    stub_delayed(reply, Duration::ZERO).await
}

async fn stub_delayed(reply: Reply, delay: Duration) -> Stub {
    let count = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let (c, b) = (count.clone(), bodies.clone());
    let app = axum::Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap, body: String| {
            let (reply, c, b) = (reply.clone(), c.clone(), b.clone());
            async move {
                assert_eq!(headers["x-api-key"], KEY);
                assert_eq!(headers["anthropic-version"], "2026-10-21");
                b.lock()
                    .unwrap()
                    .push(serde_json::from_str::<Value>(&body).unwrap());
                let n = c.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                reply(n)
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    Stub {
        base,
        count,
        bodies,
    }
}

fn client(base: &str, timeout: Duration) -> LlmClient {
    LlmClient::new(LlmConfig {
        api_key: KEY.into(),
        model: "claude-opus-5-5".into(),
        base_url: base.into(),
        timeout,
        deadline: timeout * 3,
        max_tokens: 16_000,
        effort: "medium".into(),
        refusal_fallback: true,
    })
    .unwrap()
}

fn message(value: &Value) -> Response {
    let body = json!({
        "stop_reason": "end_turn",
        "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": value.to_string()}],
        "usage": {"input_tokens": 1234, "output_tokens": 567}
    });
    (StatusCode::OK, axum::Json(body)).into_response()
}

fn status(code: u16, msg: &str) -> Response {
    (
        StatusCode::from_u16(code).unwrap(),
        axum::Json(json!({"type": "error", "error": {"type": "x", "message": msg}})),
    )
        .into_response()
}

fn plan() -> Value {
    json!({
        "summary": "Validate campaign payloads.",
        "deliverables": ["Validation for POST /campaigns"],
        "acceptance_criteria": [{"id": "AC-1", "description": "Invalid payloads get 422."}],
        "dependencies": [], "risks": ["Currency handling"], "steps": ["Implement", "Test"],
        "scope": ["apps/api/src/routes"]
    })
}

fn approver() -> Principal {
    Principal {
        subject: "alice".into(),
        issuer: "test-idp".into(),
        roles: vec![HumanRole::Approver],
    }
}

/// Simulated team with `real` replacing some roles.
fn orch(pool: &PgPool, real: Vec<(Role, Arc<dyn RoleAgent>)>) -> Orchestrator {
    let mut agents: HashMap<Role, Arc<dyn RoleAgent>> = sim::team()
        .into_iter()
        .map(|a| (a.role, Arc::new(a) as Arc<dyn RoleAgent>))
        .collect();
    for (r, a) in real {
        agents.insert(r, a);
    }
    let mut o = Orchestrator::with_agents(pool.clone(), agents);
    o.stage_timeout = Duration::from_secs(20);
    o.base_commit_sha = Some(BASE.into());
    o
}

fn real(role: Role, s: &Stub, timeout: Duration) -> (Role, Arc<dyn RoleAgent>) {
    (
        role,
        Arc::new(
            AnthropicRoleAgent::new(
                role,
                &format!("anthropic-{}", role.as_str()),
                client(&s.base, timeout),
            )
            .unwrap(),
        ),
    )
}

async fn submit(pool: &PgPool) -> EngWorkflow {
    store::create(
        pool,
        "Campaign validation",
        REQUIREMENTS,
        "bob",
        Budget::default(),
    )
    .await
    .unwrap()
}

async fn approve_plan(pool: &PgPool, wf: &EngWorkflow) {
    let req = DecisionRequest {
        gate: GateKind::Plan,
        decision: Decision::Approve,
        expected_version: wf.version,
        artifact_hash: wf.pending_decision_hash.clone(),
        reviewed_commit_sha: None,
        comment: None,
    };
    approval::decide(
        pool,
        &approver(),
        wf.id,
        &uuid::Uuid::new_v4().to_string(),
        &req,
    )
    .await
    .unwrap();
}

const T: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn real_provider_plans_and_records_model_and_usage(pool: PgPool) {
    let s = stub(Arc::new(|_| message(&plan()))).await;
    let o = orch(&pool, vec![real(Role::ProjectManager, &s, T)]);
    let wf = o.advance(submit(&pool).await.id).await.unwrap();
    assert_eq!(wf.status, "awaiting_plan_approval", "{:?}", wf.error);

    let run = &store::runs(&pool, wf.id).await.unwrap()[0];
    assert_eq!(
        (run.provider.as_str(), run.model.as_deref()),
        (PROVIDER, Some("claude-opus-5-5"))
    );
    assert_eq!(
        (run.input_tokens, run.output_tokens),
        (Some(1234), Some(567))
    );
    let art = &store::artifacts(&pool, wf.id).await.unwrap()[0];
    assert_eq!(
        (art.provider.as_deref(), art.model.as_deref()),
        (Some(PROVIDER), Some("claude-opus-5-5"))
    );
    assert_eq!(art.content["scope"], json!(["apps/api/src/routes"]));

    // Wire contract: structured output, closed schema, untrusted data delimited.
    let body = &s.bodies.lock().unwrap()[0];
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(
        body["output_config"]["format"]["schema"]["additionalProperties"],
        false
    );
    assert!(body["system"].as_str().unwrap().contains("Project Manager"));
    let user = body["messages"][0]["content"].as_str().unwrap();
    assert!(user.starts_with("<workflow_data>") && user.contains("POST /campaigns"));
    assert!(
        body.get("tools").is_none(),
        "no tools are offered to an LLM-API role"
    );
    assert!(!body.to_string().contains(KEY));
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn authentication_failure_is_not_retried_or_faked(pool: PgPool) {
    let s = stub(Arc::new(|_| {
        status(401, &format!("invalid x-api-key {KEY}"))
    }))
    .await;
    let o = orch(&pool, vec![real(Role::ProjectManager, &s, T)]);
    let wf = o.advance(submit(&pool).await.id).await.unwrap();
    assert_eq!(
        (wf.status.as_str(), wf.error_code.as_deref()),
        ("failed", Some("agent_failed"))
    );
    assert_eq!(s.count.load(Ordering::SeqCst), 1, "401 must not be retried");
    let runs = store::runs(&pool, wf.id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].provider, PROVIDER, "no fallback to another adapter");
    assert!(
        !runs[0].error.as_deref().unwrap().contains(KEY),
        "key redacted"
    );
    assert!(
        store::artifacts(&pool, wf.id).await.unwrap().is_empty(),
        "no fabricated plan"
    );
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn provider_outage_is_retried_within_bounds_then_fails(pool: PgPool) {
    let s = stub(Arc::new(|_| status(529, "overloaded"))).await;
    let o = orch(&pool, vec![real(Role::ProjectManager, &s, T)]);
    let wf = o.advance(submit(&pool).await.id).await.unwrap();
    assert_eq!(
        (wf.status.as_str(), wf.error_code.as_deref()),
        ("failed", Some("agent_unavailable"))
    );
    // 3 client attempts per run x 2 orchestrator attempts.
    assert_eq!(s.count.load(Ordering::SeqCst), 6);
    assert!(
        store::runs(&pool, wf.id)
            .await
            .unwrap()
            .iter()
            .all(|r| r.error_code.as_deref() == Some("agent_unavailable"))
    );
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn provider_timeout_fails_the_stage(pool: PgPool) {
    let s = stub_delayed(Arc::new(|_| message(&plan())), Duration::from_millis(1500)).await;
    let o = orch(
        &pool,
        vec![real(Role::ProjectManager, &s, Duration::from_millis(500))],
    );
    let wf = o.advance(submit(&pool).await.id).await.unwrap();
    assert_eq!(wf.status, "failed");
    let runs = store::runs(&pool, wf.id).await.unwrap();
    assert_eq!(runs.len(), 2);
    assert!(
        runs[0].error.as_deref().unwrap().contains("llm_timeout"),
        "{:?}",
        runs[0].error
    );
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn invalid_responses_are_rejected(pool: PgPool) {
    let cases: Vec<(Reply, &str)> = vec![
        (
            Arc::new(|_| {
                (StatusCode::OK, axum::Json(json!({"stop_reason":"end_turn","content":[{"type":"text","text":"not json"}]}))).into_response()
            }),
            "agent_failed",
        ),
        (
            Arc::new(|_| {
                (
                    StatusCode::OK,
                    axum::Json(json!({"stop_reason":"refusal","content":[]})),
                )
                    .into_response()
            }),
            "agent_failed",
        ),
        (
            Arc::new(|_| {
                (StatusCode::OK, axum::Json(json!({"stop_reason":"max_tokens","content":[{"type":"text","text":"{\"summ"}]}))).into_response()
            }),
            "agent_failed",
        ),
        // Valid JSON that breaks the contract: caught by the orchestrator.
        (
            Arc::new(|_| {
                let mut p = plan();
                p["approved"] = json!(true);
                message(&p)
            }),
            "artifact_invalid",
        ),
        (
            Arc::new(|_| {
                let mut p = plan();
                p["scope"] = json!(["/etc"]);
                message(&p)
            }),
            "artifact_invalid",
        ),
    ];
    for (reply, code) in cases {
        let s = stub(reply).await;
        let o = orch(&pool, vec![real(Role::ProjectManager, &s, T)]);
        let wf = o.advance(submit(&pool).await.id).await.unwrap();
        assert_eq!(
            (wf.status.as_str(), wf.error_code.as_deref()),
            ("failed", Some(code)),
            "{:?}",
            wf.error
        );
        assert!(store::artifacts(&pool, wf.id).await.unwrap().is_empty());
    }
}

#[test]
fn execution_roles_are_unsupported() {
    let c = client("http://127.0.0.1:1", T);
    for role in [Role::Developer, Role::QaTester] {
        assert!(
            AnthropicRoleAgent::new(role, "x", c.clone()).is_err(),
            "{role:?}"
        );
    }
}

/// Coordinator: base commit comes from configuration, never from the model;
/// a scope wider than the approved plan is a policy violation.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn coordinator_cannot_invent_base_or_widen_scope(pool: PgPool) {
    let assignment = |scope: &str| {
        json!({"summary": "Do it", "scope": [scope], "acceptance_criteria": ["AC-1", "AC-2"],
        "required_checks": ["cargo_test"], "base_commit_sha": "ffffffffffffffffffffffffffffffffffffffff"})
    };
    let s = stub(Arc::new(move |_| {
        message(&assignment("apps/api/src/routes"))
    }))
    .await;
    let o = orch(&pool, vec![real(Role::ProjectCoordinator, &s, T)]);
    let wf = o.advance(submit(&pool).await.id).await.unwrap();
    approve_plan(&pool, &wf).await;
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(wf.status, "awaiting_delivery_approval", "{:?}", wf.error);
    let arts = store::artifacts(&pool, wf.id).await.unwrap();
    let a = store::latest(&arts, ArtifactKind::WorkAssignment).unwrap();
    assert_eq!(
        a.content["base_commit_sha"], BASE,
        "configured base, not the model's"
    );
    assert_eq!(a.provider.as_deref(), Some(PROVIDER));

    let s = stub(Arc::new(move |_| message(&assignment("apps/api/src/db")))).await;
    let o = orch(&pool, vec![real(Role::ProjectCoordinator, &s, T)]);
    let wf = o.advance(submit(&pool).await.id).await.unwrap();
    approve_plan(&pool, &wf).await;
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(
        (wf.status.as_str(), wf.error_code.as_deref()),
        ("failed", Some("policy_violation"))
    );

    // No base commit configured: refused before any provider call.
    let s = stub(Arc::new(move |_| {
        message(&assignment("apps/api/src/routes"))
    }))
    .await;
    let mut o = orch(&pool, vec![real(Role::ProjectCoordinator, &s, T)]);
    o.base_commit_sha = None;
    let wf = o.advance(submit(&pool).await.id).await.unwrap();
    approve_plan(&pool, &wf).await;
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(wf.error_code.as_deref(), Some("agent_failed"));
    assert_eq!(s.count.load(Ordering::SeqCst), 0);
}

/// The reviewer needs the exact diff; without a workspace it refuses
/// instead of "reviewing" metadata.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn reviewer_without_a_real_diff_refuses(pool: PgPool) {
    let s = stub(Arc::new(|_| status(500, "must not be called"))).await;
    let o = orch(&pool, vec![real(Role::CodeReviewer, &s, T)]);
    let wf = o.advance(submit(&pool).await.id).await.unwrap();
    approve_plan(&pool, &wf).await;
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(wf.error_code.as_deref(), Some("agent_failed"));
    assert!(wf.error.unwrap().contains("diff"));
    assert_eq!(s.count.load(Ordering::SeqCst), 0);
}

/// Quality Lead: real provider assessment bound to the orchestrator's commit.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn quality_lead_assessment_is_bound_to_the_candidate(pool: PgPool) {
    let s = stub(Arc::new(|_| {
        message(
            &json!({"recommendation": "ready", "summary": "Evidence is complete.",
        "open_issues": [], "commit_sha": "0000000000000000000000000000000000000000"}),
        )
    }))
    .await;
    let o = orch(&pool, vec![real(Role::QualityLead, &s, T)]);
    let wf = o.advance(submit(&pool).await.id).await.unwrap();
    approve_plan(&pool, &wf).await;
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(wf.status, "awaiting_delivery_approval", "{:?}", wf.error);
    let arts = store::artifacts(&pool, wf.id).await.unwrap();
    let qa = store::latest(&arts, ArtifactKind::QualityAssessment).unwrap();
    assert_eq!(qa.commit_sha, wf.candidate_commit_sha);
    assert_eq!(
        qa.content["commit_sha"].as_str(),
        wf.candidate_commit_sha.as_deref()
    );
    // The QL saw the QA report and the review as data.
    let user = s.bodies.lock().unwrap()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(user.contains("qa_report") && user.contains("review_report"));
}
