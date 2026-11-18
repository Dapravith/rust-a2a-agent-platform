//! Phase 6, Mode A (deterministic simulation): multi-agent engineering
//! workflow, human approval engine and the mandatory negative tests.
//!
//! Real Postgres (`sqlx::test`, one fresh database per test), real HTTP for
//! the API, simulated role agents (provider `simulation`, no paid model).
//! The test authenticator below is in-process test wiring; `api serve`
//! never uses it (it wires `eng::auth::Disabled`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use api::config::Config;
use api::eng::agents::RoleAgent;
use api::eng::approval::{self, Decision, DecisionRequest, GateKind};
use api::eng::auth::{AuthError, Authenticator, Principal};
use api::eng::orchestrator::Orchestrator;
use api::eng::routes::EngState;
use api::eng::sim::{Behavior, SimAgent};
use api::eng::store::{self, EngWorkflow, NewArtifact, StoreError};
use api::engine::Engine;
use axum::http::HeaderMap;
use engineering::artifacts::ArtifactKind;
use engineering::roles::Role;
use engineering::state::{Actor, Budget, Event, HumanRole, TransitionError};
use serde_json::{Value, json};
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

const REQUIREMENTS: &str =
    "Add validation to the Rust Axum POST /campaigns endpoint without changing unrelated routes.";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Bearer token -> principal, standing in for a verified IdP token.
struct TestAuth;

fn principal(sub: &str, roles: &[HumanRole]) -> Principal {
    Principal {
        subject: sub.into(),
        issuer: "test-idp".into(),
        roles: roles.to_vec(),
    }
}
fn alice() -> Principal {
    principal("alice", &[HumanRole::Approver])
}
fn carol() -> Principal {
    principal("carol", &[HumanRole::Approver])
}
fn bob() -> Principal {
    principal("bob", &[HumanRole::Requester])
}

impl Authenticator for TestAuth {
    fn configured(&self) -> bool {
        true
    }
    fn authenticate(&self, h: &HeaderMap) -> Result<Principal, AuthError> {
        let tok = h
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AuthError::Missing)?;
        match tok {
            "tok-alice" => Ok(alice()),
            "tok-carol" => Ok(carol()),
            "tok-bob" => Ok(bob()),
            _ => Err(AuthError::Invalid),
        }
    }
}

type Agents = HashMap<Role, Arc<dyn RoleAgent>>;
type Calls = HashMap<Role, Arc<std::sync::atomic::AtomicU32>>;

fn agents_with(over: Vec<SimAgent>) -> (Agents, Calls) {
    let mut team: HashMap<Role, SimAgent> = api::eng::sim::team()
        .into_iter()
        .map(|a| (a.role, a))
        .collect();
    for a in over {
        team.insert(a.role, a);
    }
    let calls = team.iter().map(|(r, a)| (*r, a.calls.clone())).collect();
    let agents = team
        .into_iter()
        .map(|(r, a)| (r, Arc::new(a) as Arc<dyn RoleAgent>))
        .collect();
    (agents, calls)
}

fn orch(pool: &PgPool, over: Vec<SimAgent>) -> Arc<Orchestrator> {
    let (agents, _) = agents_with(over);
    let mut o = Orchestrator::with_agents(pool.clone(), agents);
    o.stage_timeout = Duration::from_secs(2);
    Arc::new(o)
}

async fn submit(pool: &PgPool, budget: Budget) -> EngWorkflow {
    store::create(pool, "Campaign validation", REQUIREMENTS, "bob", budget)
        .await
        .unwrap()
}

fn req(gate: GateKind, decision: Decision, wf: &EngWorkflow) -> DecisionRequest {
    DecisionRequest {
        gate,
        decision,
        expected_version: wf.version,
        artifact_hash: wf.pending_decision_hash.clone(),
        reviewed_commit_sha: if gate == GateKind::Delivery {
            wf.candidate_commit_sha.clone()
        } else {
            None
        },
        comment: matches!(decision, Decision::RequestChanges | Decision::Reject)
            .then(|| "please change".to_owned()),
    }
}

async fn decide(
    pool: &PgPool,
    who: &Principal,
    wf: &EngWorkflow,
    gate: GateKind,
    d: Decision,
) -> Result<(), StoreError> {
    approval::decide(
        pool,
        who,
        wf.id,
        &Uuid::new_v4().to_string(),
        &req(gate, d, wf),
    )
    .await
    .map(|_| ())
}

/// Submit, plan, approve plan, run to the delivery gate.
async fn to_delivery_gate(pool: &PgPool, o: &Orchestrator) -> EngWorkflow {
    let wf = submit(pool, Budget::default()).await;
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(wf.status, "awaiting_plan_approval");
    decide(pool, &alice(), &wf, GateKind::Plan, Decision::Approve)
        .await
        .unwrap();
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(wf.status, "awaiting_delivery_approval", "{:?}", wf.error);
    wf
}

fn kinds(arts: &[store::EngArtifact]) -> Vec<&str> {
    arts.iter().map(|a| a.kind.as_str()).collect()
}

async fn expect_failed(o: &Orchestrator, pool: &PgPool, code: &str) -> EngWorkflow {
    let wf = submit(pool, Budget::default()).await;
    let mut wf = o.advance(wf.id).await.unwrap();
    if wf.status == "awaiting_plan_approval" {
        decide(pool, &alice(), &wf, GateKind::Plan, Decision::Approve)
            .await
            .unwrap();
        wf = o.advance(wf.id).await.unwrap();
    }
    assert_eq!(wf.status, "failed");
    assert_eq!(wf.error_code.as_deref(), Some(code), "{:?}", wf.error);
    wf
}

fn p0_config() -> Config {
    Config {
        bind: "127.0.0.1:0".into(),
        database_url: "postgres://unused".into(),
        db_max_connections: 5,
        worker_url: Url::parse("http://127.0.0.1:2").unwrap(),
        reviewer_url: Url::parse("http://127.0.0.1:3").unwrap(),
        agent_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(50),
        frontend_dir: None,
    }
}

async fn serve_api(
    pool: &PgPool,
    orchestrator: Option<Arc<Orchestrator>>,
    auth: Arc<dyn Authenticator>,
) -> String {
    let engine = Engine::new(pool.clone(), &p0_config()).unwrap();
    let eng = EngState {
        pool: pool.clone(),
        orchestrator,
        auth,
        budget: Budget::default(),
    };
    let app = api::app_with(engine, eng, None);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

async fn wait_eng(base: &str, id: &str, status: &str) -> Value {
    for _ in 0..300 {
        let d: Value = reqwest::get(format!("{base}/api/eng/workflows/{id}"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if d["workflow"]["status"] == status {
            return d;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("workflow {id} never reached {status}");
}

fn post(base: &str, path: &str, token: Option<&str>) -> reqwest::RequestBuilder {
    let r = reqwest::Client::new().post(format!("{base}{path}"));
    match token {
        Some(t) => r.bearer_auth(t),
        None => r,
    }
}

// ---------------------------------------------------------------------------
// Required end-to-end flows
// ---------------------------------------------------------------------------

/// §12: PM -> human plan approval -> Coordinator -> Developer -> Reviewer ->
/// QA -> Quality Lead -> pause -> human approves; identity persisted. Over HTTP.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn approved_flow_over_http(pool: PgPool) {
    let base = serve_api(&pool, Some(orch(&pool, vec![])), Arc::new(TestAuth)).await;
    let r = post(&base, "/api/eng/workflows", Some("tok-bob"))
        .json(&json!({"title": "Campaign validation", "requirements": REQUIREMENTS}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let id = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let d = wait_eng(&base, &id, "awaiting_plan_approval").await;
    assert_eq!(d["awaiting_decision"], "plan");
    let wf = &d["workflow"];
    let r = post(
        &base,
        &format!("/api/eng/workflows/{id}/decisions"),
        Some("tok-alice"),
    )
    .header("idempotency-key", "plan-1")
    .json(
        &json!({"gate": "plan", "decision": "approve", "expected_version": wf["version"],
                      "artifact_hash": wf["pending_decision_hash"]}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 201, "{}", r.text().await.unwrap());

    let d = wait_eng(&base, &id, "awaiting_delivery_approval").await;
    let roles: Vec<&str> = d["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        [
            "project_manager",
            "project_coordinator",
            "developer",
            "code_reviewer",
            "qa_tester",
            "quality_lead"
        ]
    );
    assert!(
        d["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["provider"] == "simulation" && r["status"] == "completed")
    );
    assert!(
        d["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["passed"] == true),
        "{}",
        d["gates"]
    );
    let wf = d["workflow"].clone();
    let sha = wf["candidate_commit_sha"].as_str().unwrap().to_owned();
    let manifest = wf["pending_decision_hash"].as_str().unwrap().to_owned();

    let r = post(
        &base,
        &format!("/api/eng/workflows/{id}/decisions"),
        Some("tok-alice"),
    )
    .header("idempotency-key", "delivery-1")
    .json(
        &json!({"gate": "delivery", "decision": "approve", "expected_version": wf["version"],
                      "artifact_hash": manifest, "reviewed_commit_sha": sha, "comment": "LGTM"}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 201);
    let d = wait_eng(&base, &id, "approved_for_delivery").await;
    let a = &d["approvals"][1];
    assert_eq!(a["approver_id"], "alice");
    assert_eq!(a["approver_role"], "approver");
    assert_eq!(a["identity_issuer"], "test-idp");
    assert_eq!(a["reviewed_commit_sha"], sha.as_str());
    assert_eq!(a["artifact_hash"], manifest.as_str());
    assert_eq!(d["workflow"]["candidate_commit_sha"], sha.as_str());
    // Every artifact carries id, producer, schema version and an integrity hash.
    for art in d["artifacts"].as_array().unwrap() {
        assert_eq!(art["content_hash"].as_str().unwrap().len(), 64);
        assert_eq!(art["schema_version"], 1);
    }
    let actions: Vec<&str> = d["audit"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(actions.contains(&"plan_approved") && actions.contains(&"delivery_approved"));
    // Who made each human decision is in the audit trail, as a human.
    assert!(
        d["audit"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "delivery_approved"
                && e["actor_kind"] == "human"
                && e["actor_id"] == "alice")
    );
}

/// §12: changes requested -> new candidate -> old decision refused -> re-approval.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn changes_requested_flow_requires_reapproval(pool: PgPool) {
    let o = orch(&pool, vec![]);
    let first = to_delivery_gate(&pool, &o).await;
    decide(
        &pool,
        &alice(),
        &first,
        GateKind::Delivery,
        Decision::RequestChanges,
    )
    .await
    .unwrap();
    let wf = store::get(&pool, first.id).await.unwrap();
    assert_eq!(wf.status, "changes_requested");
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(wf.status, "awaiting_delivery_approval");
    assert_eq!(wf.delivery_revisions, 1);
    assert_ne!(
        wf.candidate_commit_sha, first.candidate_commit_sha,
        "a new candidate was produced"
    );
    assert_ne!(wf.pending_decision_hash, first.pending_decision_hash);

    // Approving what was reviewed earlier is refused: stale version.
    let stale = approval::decide(
        &pool,
        &alice(),
        wf.id,
        "old",
        &req(GateKind::Delivery, Decision::Approve, &first),
    )
    .await;
    assert!(matches!(stale, Err(StoreError::Stale { .. })), "{stale:?}");
    // Current version but the old candidate / manifest: refused.
    let mut forged = req(GateKind::Delivery, Decision::Approve, &wf);
    forged.reviewed_commit_sha = first.candidate_commit_sha.clone();
    assert!(matches!(
        approval::decide(&pool, &alice(), wf.id, "f1", &forged).await,
        Err(StoreError::Mismatch(_))
    ));
    let mut forged = req(GateKind::Delivery, Decision::Approve, &wf);
    forged.artifact_hash = first.pending_decision_hash.clone();
    assert!(matches!(
        approval::decide(&pool, &alice(), wf.id, "f2", &forged).await,
        Err(StoreError::Mismatch(_))
    ));

    decide(&pool, &alice(), &wf, GateKind::Delivery, Decision::Approve)
        .await
        .unwrap();
    let done = store::get(&pool, wf.id).await.unwrap();
    assert_eq!(done.status, "approved_for_delivery");
    assert_eq!(done.candidate_commit_sha, wf.candidate_commit_sha);
    let arts = store::artifacts(&pool, wf.id).await.unwrap();
    assert_eq!(
        kinds(&arts)
            .iter()
            .filter(|k| **k == "code_candidate")
            .count(),
        2
    );
    assert_eq!(
        kinds(&arts)
            .iter()
            .filter(|k| **k == "delivery_manifest")
            .count(),
        2
    );
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn delivery_can_be_rejected(pool: PgPool) {
    let o = orch(&pool, vec![]);
    let wf = to_delivery_gate(&pool, &o).await;
    decide(&pool, &alice(), &wf, GateKind::Delivery, Decision::Reject)
        .await
        .unwrap();
    assert_eq!(store::get(&pool, wf.id).await.unwrap().status, "rejected");
}

// ---------------------------------------------------------------------------
// §13 mandatory negative tests
// ---------------------------------------------------------------------------

/// Unauthorized user attempts approval; browser-supplied identity; no IdP.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn unauthorized_user_cannot_approve(pool: PgPool) {
    let o = orch(&pool, vec![]);
    let wf = submit(&pool, Budget::default()).await;
    let wf = o.advance(wf.id).await.unwrap();
    let base = serve_api(&pool, Some(o.clone()), Arc::new(TestAuth)).await;
    let path = format!("/api/eng/workflows/{}/decisions", wf.id);
    let body = json!({"gate": "plan", "decision": "approve", "expected_version": wf.version, "artifact_hash": wf.pending_decision_hash});
    let send = |tok: Option<&'static str>, body: Value| {
        post(&base, &path, tok)
            .header("idempotency-key", Uuid::new_v4().to_string())
            .json(&body)
            .send()
    };

    assert_eq!(send(None, body.clone()).await.unwrap().status(), 401);
    assert_eq!(
        send(Some("tok-forged"), body.clone())
            .await
            .unwrap()
            .status(),
        401
    );
    let r = send(Some("tok-bob"), body.clone()).await.unwrap();
    assert_eq!(r.status(), 403, "requester is not an approver");
    let mut spoof = body.clone();
    spoof["approver_id"] = json!("alice");
    assert_eq!(
        send(Some("tok-bob"), spoof).await.unwrap().status(),
        422,
        "identity cannot come from the body"
    );
    assert_eq!(
        store::get(&pool, wf.id).await.unwrap().status,
        "awaiting_plan_approval"
    );
    assert!(store::approvals(&pool, wf.id).await.unwrap().is_empty());

    // Default production wiring: no identity provider -> BLOCKED (503).
    let blocked = serve_api(&pool, None, Arc::new(api::eng::auth::Disabled)).await;
    let r = post(&blocked, &path, Some("tok-alice"))
        .header("idempotency-key", "k")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "auth_unavailable"
    );
    let s: Value = reqwest::get(format!("{blocked}/api/eng/status"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s, json!({"approvals_enabled": false, "providers": []}));
}

/// AI agent attempts to approve delivery (directly, and by requesting a tool).
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn ai_agent_cannot_approve_delivery(pool: PgPool) {
    let o = orch(&pool, vec![]);
    let wf = to_delivery_gate(&pool, &o).await;
    for role in Role::ALL {
        let mut tx = pool.begin().await.unwrap();
        let locked = store::lock(&mut tx, wf.id).await.unwrap();
        let actor = Actor::Agent {
            role,
            agent_id: format!("sim-{}", role.as_str()),
        };
        let r = store::transition(
            &mut tx,
            &locked,
            Event::DeliveryApproved,
            &actor,
            Default::default(),
            json!({}),
        )
        .await;
        assert!(
            matches!(
                r,
                Err(StoreError::Transition(TransitionError::Unauthorized { .. }))
            ),
            "{role:?}: {r:?}"
        );
    }
    let mut tx = pool.begin().await.unwrap();
    let locked = store::lock(&mut tx, wf.id).await.unwrap();
    let r = store::transition(
        &mut tx,
        &locked,
        Event::DeliveryApproved,
        &Actor::System,
        Default::default(),
        json!({}),
    )
    .await;
    assert!(matches!(
        r,
        Err(StoreError::Transition(TransitionError::Unauthorized { .. }))
    ));
    drop(tx);
    assert_eq!(
        store::get(&pool, wf.id).await.unwrap().status,
        "awaiting_delivery_approval"
    );

    // A Quality Lead that asks for an approval tool fails the workflow.
    let o = orch(
        &pool,
        vec![SimAgent::new(
            Role::QualityLead,
            "sim-ql",
            Behavior::RequestTool("approve_delivery".into()),
        )],
    );
    expect_failed(&o, &pool, "tool_not_permitted").await;
}

/// Reviewer attempts to review (approve) its own patch.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn reviewer_cannot_review_own_patch(pool: PgPool) {
    let o = orch(
        &pool,
        vec![
            SimAgent::new(Role::Developer, "same-agent", Behavior::Normal),
            SimAgent::new(Role::CodeReviewer, "same-agent", Behavior::Normal),
        ],
    );
    let wf = expect_failed(&o, &pool, "policy_violation").await;
    let arts = store::artifacts(&pool, wf.id).await.unwrap();
    assert!(!kinds(&arts).contains(&"review_report"));
}

/// Candidate changes after approval: no new artifacts; stored history immutable even for the owner.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn candidate_and_history_are_immutable_after_approval(pool: PgPool) {
    let o = orch(&pool, vec![]);
    let wf = to_delivery_gate(&pool, &o).await;
    decide(&pool, &alice(), &wf, GateKind::Delivery, Decision::Approve)
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let locked = store::lock(&mut tx, wf.id).await.unwrap();
    let content = json!({});
    let r = store::insert_artifact(
        &mut tx,
        &locked,
        NewArtifact {
            kind: ArtifactKind::CodeCandidate,
            run_id: None,
            producer_kind: "system",
            producer_id: "x",
            provider: None,
            model: None,
            content: &content,
            commit_sha: None,
        },
    )
    .await;
    assert!(matches!(
        r,
        Err(StoreError::Transition(TransitionError::Terminal(_)))
    ));
    drop(tx);

    // The test connects as the table owner: the trigger binds it too.
    for sql in [
        "UPDATE eng_artifacts SET content = '{}'::jsonb WHERE workflow_id = $1",
        "DELETE FROM eng_approvals WHERE workflow_id = $1",
        "UPDATE eng_audit_events SET actor_id = 'mallory' WHERE workflow_id = $1",
        "DELETE FROM eng_audit_events WHERE workflow_id = $1",
    ] {
        let e = sqlx::query(sql)
            .bind(wf.id)
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("append-only"), "{sql}: {e}");
    }
    assert!(
        sqlx::query("TRUNCATE eng_audit_events CASCADE")
            .execute(&pool)
            .await
            .is_err()
    );
    // Runtime role has no UPDATE/DELETE on the append-only tables at all.
    let (upd, del): (bool, bool) = sqlx::query_as(
        "SELECT has_table_privilege('a2a_app', 'eng_audit_events', 'UPDATE'), \
                has_table_privilege('a2a_app', 'eng_approvals', 'DELETE')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!upd && !del);
}

/// Provider becomes unavailable: bounded retry, then failure, never mock success.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn provider_unavailable_fails_without_fallback(pool: PgPool) {
    let o = orch(
        &pool,
        vec![SimAgent::new(
            Role::Developer,
            "sim-dev",
            Behavior::FailTimes(99),
        )],
    );
    let wf = expect_failed(&o, &pool, "agent_unavailable").await;
    let runs = store::runs(&pool, wf.id).await.unwrap();
    let dev: Vec<_> = runs.iter().filter(|r| r.role == "developer").collect();
    assert_eq!(dev.len(), 2, "max_attempts = 2");
    assert!(
        dev.iter()
            .all(|r| r.status == "failed" && r.error_code.as_deref() == Some("agent_unavailable"))
    );
    assert!(!kinds(&store::artifacts(&pool, wf.id).await.unwrap()).contains(&"code_candidate"));

    // A transient outage is recovered by the bounded retry.
    let o = orch(
        &pool,
        vec![SimAgent::new(
            Role::Developer,
            "sim-dev",
            Behavior::FailTimes(1),
        )],
    );
    let wf = to_delivery_gate(&pool, &o).await;
    let dev: Vec<_> = store::runs(&pool, wf.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.role == "developer")
        .collect();
    assert_eq!(
        dev.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(),
        ["failed", "completed"]
    );
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn malformed_output_is_rejected(pool: PgPool) {
    let o = orch(
        &pool,
        vec![SimAgent::new(
            Role::ProjectManager,
            "sim-pm",
            Behavior::Malformed,
        )],
    );
    let wf = expect_failed(&o, &pool, "artifact_invalid").await;
    assert!(store::artifacts(&pool, wf.id).await.unwrap().is_empty());
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn unauthorized_tool_request_fails_the_stage(pool: PgPool) {
    let o = orch(
        &pool,
        vec![SimAgent::new(
            Role::Developer,
            "sim-dev",
            Behavior::RequestTool("git_push".into()),
        )],
    );
    let wf = expect_failed(&o, &pool, "tool_not_permitted").await;
    let dev = store::runs(&pool, wf.id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.role == "developer")
        .unwrap();
    assert!(dev.error.unwrap().contains("git_push"));
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn execution_budget_is_enforced(pool: PgPool) {
    let o = orch(&pool, vec![]);
    let wf = submit(
        &pool,
        Budget {
            max_agent_runs: 3,
            ..Budget::default()
        },
    )
    .await;
    let wf = o.advance(wf.id).await.unwrap();
    decide(&pool, &alice(), &wf, GateKind::Plan, Decision::Approve)
        .await
        .unwrap();
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(wf.status, "failed");
    assert_eq!(wf.error_code.as_deref(), Some("budget_exceeded"));
    assert_eq!(wf.agent_runs, 3);
}

/// Orchestrator restarts during human approval: the gate survives; an
/// in-flight stage fails as interrupted (no replay).
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn restart_preserves_human_gate_and_fails_inflight_stage(pool: PgPool) {
    let wf = to_delivery_gate(&pool, &orch(&pool, vec![])).await;
    assert_eq!(store::recover_interrupted(&pool).await.unwrap(), 0);
    let after = store::get(&pool, wf.id).await.unwrap();
    assert_eq!(
        (
            after.status.as_str(),
            after.version,
            &after.pending_decision_hash
        ),
        (
            "awaiting_delivery_approval",
            wf.version,
            &wf.pending_decision_hash
        )
    );
    // A fresh orchestrator (new process) accepts the decision.
    let _fresh = orch(&pool, vec![]);
    decide(
        &pool,
        &alice(),
        &after,
        GateKind::Delivery,
        Decision::Approve,
    )
    .await
    .unwrap();

    // Crash while the developer runs.
    let mut slow = Orchestrator::with_agents(
        pool.clone(),
        agents_with(vec![SimAgent::new(
            Role::Developer,
            "sim-dev",
            Behavior::Hang,
        )])
        .0,
    );
    slow.stage_timeout = Duration::from_secs(600);
    let slow = Arc::new(slow);
    let wf = submit(&pool, Budget::default()).await;
    let wf = slow.advance(wf.id).await.unwrap();
    decide(&pool, &alice(), &wf, GateKind::Plan, Decision::Approve)
        .await
        .unwrap();
    let task = tokio::spawn({
        let s = slow.clone();
        async move { s.advance(wf.id).await }
    });
    for _ in 0..200 {
        if store::runs(&pool, wf.id)
            .await
            .unwrap()
            .iter()
            .any(|r| r.role == "developer" && r.status == "running")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    task.abort(); // the process "dies"
    assert_eq!(store::recover_interrupted(&pool).await.unwrap(), 1);
    let wf = store::get(&pool, wf.id).await.unwrap();
    assert_eq!(
        (wf.status.as_str(), wf.error_code.as_deref()),
        ("failed", Some("interrupted"))
    );
    let dev = store::runs(&pool, wf.id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.role == "developer")
        .unwrap();
    assert_eq!(dev.error_code.as_deref(), Some("interrupted"));
}

/// Two approvers submit conflicting decisions at the same time: exactly one wins.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn conflicting_approvers_one_wins(pool: PgPool) {
    let wf = to_delivery_gate(&pool, &orch(&pool, vec![])).await;
    let (al, ca) = (alice(), carol());
    let (a, c) = tokio::join!(
        decide(&pool, &al, &wf, GateKind::Delivery, Decision::Approve),
        decide(&pool, &ca, &wf, GateKind::Delivery, Decision::Reject),
    );
    assert_eq!(
        [a.is_ok(), c.is_ok()].iter().filter(|x| **x).count(),
        1,
        "{a:?} / {c:?}"
    );
    let loser = if a.is_ok() { c } else { a };
    assert!(
        matches!(
            loser,
            Err(StoreError::Stale { .. } | StoreError::Transition(_))
        ),
        "{loser:?}"
    );
    assert_eq!(
        store::approvals(&pool, wf.id)
            .await
            .unwrap()
            .iter()
            .filter(|x| x.gate == "delivery")
            .count(),
        1
    );
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn qa_for_another_commit_is_rejected(pool: PgPool) {
    let o = orch(
        &pool,
        vec![SimAgent::new(
            Role::QaTester,
            "sim-qa",
            Behavior::WrongCommit,
        )],
    );
    let wf = expect_failed(&o, &pool, "candidate_mismatch").await;
    assert!(!kinds(&store::artifacts(&pool, wf.id).await.unwrap()).contains(&"qa_report"));
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn protected_branch_is_refused(pool: PgPool) {
    let o = orch(
        &pool,
        vec![SimAgent::new(
            Role::Developer,
            "sim-dev",
            Behavior::ProtectedBranch,
        )],
    );
    let wf = expect_failed(&o, &pool, "policy_violation").await;
    assert!(wf.error.unwrap().contains("protected"));
}

/// A retry / second loop must not execute a stage twice.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn concurrent_advance_does_not_duplicate_execution(pool: PgPool) {
    let (agents, calls) = agents_with(vec![]);
    let o = Arc::new(Orchestrator::with_agents(pool.clone(), agents));
    let wf = submit(&pool, Budget::default()).await;
    let (x, y) = tokio::join!(o.advance(wf.id), o.advance(wf.id));
    x.unwrap();
    y.unwrap();
    let wf = store::get(&pool, wf.id).await.unwrap();
    decide(&pool, &alice(), &wf, GateKind::Plan, Decision::Approve)
        .await
        .unwrap();
    let (x, y, z) = tokio::join!(o.advance(wf.id), o.advance(wf.id), o.advance(wf.id));
    for r in [x, y, z] {
        r.unwrap();
    }
    assert_eq!(
        store::get(&pool, wf.id).await.unwrap().status,
        "awaiting_delivery_approval"
    );
    for role in Role::ALL {
        assert_eq!(
            calls[&role].load(Ordering::SeqCst),
            1,
            "{role:?} executed more than once"
        );
    }
    let runs = store::runs(&pool, wf.id).await.unwrap();
    assert_eq!(runs.len(), 6);
    assert!(runs.iter().all(|r| r.status == "completed"));
}

/// Forged or stale approval requests; idempotent replay.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn forged_stale_and_replayed_decisions(pool: PgPool) {
    let o = orch(&pool, vec![]);
    let wf = o
        .advance(submit(&pool, Budget::default()).await.id)
        .await
        .unwrap();
    let mut forged = req(GateKind::Plan, Decision::Approve, &wf);
    forged.artifact_hash = Some("0".repeat(64));
    assert!(matches!(
        approval::decide(&pool, &alice(), wf.id, "k1", &forged).await,
        Err(StoreError::Mismatch(_))
    ));
    let mut stale = req(GateKind::Plan, Decision::Approve, &wf);
    stale.expected_version -= 1;
    assert!(matches!(
        approval::decide(&pool, &alice(), wf.id, "k2", &stale).await,
        Err(StoreError::Stale { .. })
    ));
    let wrong_gate = req(GateKind::Delivery, Decision::Approve, &wf);
    assert!(matches!(
        approval::decide(&pool, &alice(), wf.id, "k3", &wrong_gate).await,
        Err(StoreError::Transition(_))
    ));

    let good = req(GateKind::Plan, Decision::Approve, &wf);
    let (first, replayed) = approval::decide(&pool, &alice(), wf.id, "k4", &good)
        .await
        .unwrap();
    assert!(!replayed);
    let (again, replayed) = approval::decide(&pool, &alice(), wf.id, "k4", &good)
        .await
        .unwrap();
    assert!(replayed);
    assert_eq!(first.id, again.id);
    let mut other = good.clone();
    other.decision = Decision::RequestChanges;
    other.comment = Some("x".into());
    assert!(matches!(
        approval::decide(&pool, &alice(), wf.id, "k4", &other).await,
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(store::approvals(&pool, wf.id).await.unwrap().len(), 1);
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn stage_timeout_is_enforced(pool: PgPool) {
    let mut o = Orchestrator::with_agents(
        pool.clone(),
        agents_with(vec![SimAgent::new(
            Role::CodeReviewer,
            "sim-rev",
            Behavior::Hang,
        )])
        .0,
    );
    o.stage_timeout = Duration::from_millis(300);
    let wf = expect_failed(&o, &pool, "agent_timeout").await;
    let rev: Vec<_> = store::runs(&pool, wf.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.role == "code_reviewer")
        .collect();
    assert_eq!(rev.len(), 2);
}

/// Failed QA or blocking review findings can never become an approved delivery.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn failed_evidence_cannot_be_approved(pool: PgPool) {
    for (role, behavior) in [
        (Role::QaTester, Behavior::FailingChecks),
        (Role::CodeReviewer, Behavior::BlockingFinding),
    ] {
        let o = orch(&pool, vec![SimAgent::new(role, "sim-x", behavior)]);
        let wf = to_delivery_gate(&pool, &o).await;
        let r = decide(&pool, &alice(), &wf, GateKind::Delivery, Decision::Approve).await;
        assert!(matches!(r, Err(StoreError::GatesFailed(_))), "{r:?}");
        assert_eq!(
            store::get(&pool, wf.id).await.unwrap().status,
            "awaiting_delivery_approval"
        );
        decide(&pool, &alice(), &wf, GateKind::Delivery, Decision::Reject)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn plan_changes_loop_and_cancel(pool: PgPool) {
    let o = orch(&pool, vec![]);
    let wf = o
        .advance(submit(&pool, Budget::default()).await.id)
        .await
        .unwrap();
    decide(
        &pool,
        &alice(),
        &wf,
        GateKind::Plan,
        Decision::RequestChanges,
    )
    .await
    .unwrap();
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(
        (wf.status.as_str(), wf.plan_revisions),
        ("awaiting_plan_approval", 1)
    );
    // Requester cancels own workflow; an agent cannot.
    let cancel = DecisionRequest {
        gate: GateKind::Workflow,
        decision: Decision::Cancel,
        expected_version: wf.version,
        artifact_hash: None,
        reviewed_commit_sha: None,
        comment: None,
    };
    approval::decide(&pool, &bob(), wf.id, "c1", &cancel)
        .await
        .unwrap();
    assert_eq!(store::get(&pool, wf.id).await.unwrap().status, "cancelled");
}

/// Closed set of failure codes matches the DB CHECK constraint.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn every_error_code_is_accepted_by_the_schema(pool: PgPool) {
    for code in store::EngErrorCode::ALL {
        let wf = submit(&pool, Budget::default()).await;
        store::fail_workflow(&pool, wf.id, code, "x").await.unwrap();
        assert_eq!(
            store::get(&pool, wf.id)
                .await
                .unwrap()
                .error_code
                .as_deref(),
            Some(code.as_str())
        );
    }
}

/// Engineering responses (requirements, approvals, audit) must never be
/// cached, on success or error, under both mount points.
#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn eng_responses_are_not_cacheable(pool: PgPool) {
    let base = serve_api(&pool, None, Arc::new(api::eng::auth::Disabled)).await;
    let missing = Uuid::now_v7();
    for (method, path, status) in [
        ("GET", "/api/eng/status".to_owned(), 200),
        ("GET", "/api/v1/eng/workflows".to_owned(), 200),
        ("GET", format!("/api/eng/workflows/{missing}"), 404),
        ("POST", "/api/eng/workflows".to_owned(), 503),
    ] {
        let client = reqwest::Client::new();
        let req = match method {
            "POST" => client
                .post(format!("{base}{path}"))
                .json(&json!({"title": "t", "requirements": REQUIREMENTS})),
            _ => client.get(format!("{base}{path}")),
        };
        let r = req.send().await.unwrap();
        assert_eq!(r.status(), status, "{method} {path}");
        assert_eq!(
            r.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "{method} {path}"
        );
    }
}
