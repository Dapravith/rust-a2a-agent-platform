//! Phase 6B evidence (B1–B5, C5, C6): the whole engineering workflow over real
//! protocols.
//!
//! orchestrator ─A2A (official SDK)─► six role agents (one process, one Agent
//! Card each) ─MCP (official rmcp)─► MCP server ─► workspace cloned by the MCP
//! server from a source repo, with the candidate committed via
//! `workspace.apply_patch` and every tool call in Postgres (`eng_tool_calls`).
//!
//! The role agents are **deterministic** (rule-based, provider
//! `deterministic`), not models: the Developer applies the fixture's predefined
//! patch. QA runs checks through `testing.run`; the default runner here is a
//! labelled test double (`runner: "test-double"`). The `#[ignore]`d
//! `live_sandbox_*` test uses the real Docker sandbox.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use api::a2a::{A2AGateway, AgentEndpoint};
use api::eng::a2a_role::RemoteRoleAgent;
use api::eng::agents::{AgentError, AgentIdentity, AgentOutput, BoxFut, RoleAgent, RoleInput};
use api::eng::approval::{self, Decision, DecisionRequest, GateKind};
use api::eng::mcp_control::McpControl;
use api::eng::orchestrator::Orchestrator;
use api::eng::store::{self, EngWorkflow};
use engineering::artifacts::ArtifactKind;
use engineering::roles::Role;
use engineering::state::{Budget, HumanRole};
use futures::future::BoxFuture;
use mcp_server::{McpState, Outcome, PgAudit, ToolCallRecord};
use serde_json::{Value, json};
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;
use workspace::runner::RunnerError;
use workspace::{CheckOutcome, CheckRunner, Workspace, fixture};

const KEY: &[u8] = b"phase6b-test-key-0123456789abcdef-xyz";
const REQUIREMENTS: &str =
    "Add currency validation to the campaign payload while preserving backward compatibility.";

/// Labelled test double for the sandbox: reports `runner: "test-double"`, so
/// its output can never pass for a sandbox run.
struct TestRunner;
impl CheckRunner for TestRunner {
    fn name(&self) -> &'static str {
        "test-double"
    }
    fn run<'a>(
        &'a self,
        ws: &'a Workspace,
        id: &'a str,
    ) -> BoxFuture<'a, Result<CheckOutcome, RunnerError>> {
        Box::pin(async move {
            let argv = engineering::roles::check_argv(id)
                .ok_or_else(|| RunnerError::NotAllowed(id.into()))?;
            Ok(CheckOutcome {
                check_id: id.into(),
                argv: argv.iter().map(|s| (*s).to_owned()).collect(),
                exit_code: Some(0),
                timed_out: false,
                duration_ms: 1,
                stdout_tail: "test result: ok (test double)".into(),
                stderr_tail: String::new(),
                log_sha256: "0".repeat(64),
                head_sha: ws.head_sha().await?,
                runner: self.name().into(),
            })
        })
    }
}

struct Stack {
    _dirs: (tempfile::TempDir, tempfile::TempDir),
    wf: EngWorkflow,
    base: String,
    agents_url: Url,
    mcp_origin: Url,
}

async fn stack_with(
    pool: &PgPool,
    runner: Arc<dyn CheckRunner>,
    developer_patch: Option<String>,
) -> Stack {
    let wf = store::create(
        pool,
        "Currency validation",
        REQUIREMENTS,
        "bob",
        Budget::default(),
    )
    .await
    .unwrap();
    let src_root = tempfile::tempdir().unwrap();
    let (src, base) = fixture::create_base(src_root.path(), Uuid::now_v7())
        .await
        .unwrap();
    let ws_root = tempfile::tempdir().unwrap();
    let state = Arc::new(McpState {
        key: KEY.to_vec(),
        workspaces_root: ws_root.path().canonicalize().unwrap(),
        audit: Arc::new(PgAudit::new(pool.clone())),
        runner: Some(runner),
        locks: Default::default(),
        source_repo: Some(src.dir().to_path_buf()),
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mcp_origin = Url::parse(&format!("http://{}", l.local_addr().unwrap())).unwrap();
    tokio::spawn(async move { axum::serve(l, mcp_server::router(state)).await });

    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agents_url = Url::parse(&format!("http://{}", l.local_addr().unwrap())).unwrap();
    let cfg = role_agents::AgentsConfig {
        mcp_url: mcp_origin.join("/mcp").unwrap().to_string(),
        developer_patch,
        pm_scope: vec!["src".into()],
    };
    let app = role_agents::router(&cfg, agents_url.as_str());
    tokio::spawn(async move { axum::serve(l, app).await });
    Stack {
        _dirs: (src_root, ws_root),
        wf,
        base,
        agents_url,
        mcp_origin,
    }
}

async fn stack(pool: &PgPool) -> Stack {
    let patch = fixture::candidate_patch().await.unwrap();
    stack_with(pool, Arc::new(TestRunner), Some(patch)).await
}

fn remote(s: &Stack, role: Role) -> Arc<dyn RoleAgent> {
    let base = s
        .agents_url
        .join(&role_agents::agent_path(role.as_str()))
        .unwrap();
    Arc::new(
        RemoteRoleAgent::new(
            role,
            AgentIdentity {
                agent_id: format!("a2a-{}-1", role.as_str()),
                provider: role_agents::PROVIDER.into(),
                model: None,
            },
            AgentEndpoint {
                expected_name: contracts::role::agent_name(role.as_str()).unwrap().into(),
                base_url: base,
            },
            A2AGateway::new(Duration::from_secs(30), Duration::from_millis(50)).unwrap(),
            KEY.to_vec(),
            Duration::from_secs(300),
        )
        .unwrap(),
    )
}

fn orchestrator(pool: &PgPool, s: &Stack, over: Vec<(Role, Arc<dyn RoleAgent>)>) -> Orchestrator {
    let mut agents: HashMap<Role, Arc<dyn RoleAgent>> =
        Role::ALL.into_iter().map(|r| (r, remote(s, r))).collect();
    agents.extend(over);
    let mut o = Orchestrator::with_agents(pool.clone(), agents);
    o.stage_timeout = Duration::from_secs(60);
    o.base_commit_sha = Some(s.base.clone());
    o.mcp_control = Some(Arc::new(
        McpControl::new(s.mcp_origin.clone(), KEY.to_vec()).unwrap(),
    ));
    o
}

async fn approve_plan(pool: &PgPool, wf: &EngWorkflow) {
    let who = api::eng::auth::Principal {
        subject: "alice".into(),
        issuer: "test-idp".into(),
        roles: vec![HumanRole::Approver],
    };
    approval::decide(
        pool,
        &who,
        wf.id,
        &Uuid::new_v4().to_string(),
        &DecisionRequest {
            gate: GateKind::Plan,
            decision: Decision::Approve,
            expected_version: wf.version,
            artifact_hash: wf.pending_decision_hash.clone(),
            reviewed_commit_sha: None,
            comment: None,
        },
    )
    .await
    .unwrap();
}

async fn run_to_end(pool: &PgPool, o: &Orchestrator, wf: &EngWorkflow) -> EngWorkflow {
    let wf = o.advance(wf.id).await.unwrap();
    assert_eq!(wf.status, "awaiting_plan_approval", "{:?}", wf.error);
    approve_plan(pool, &wf).await;
    o.advance(wf.id).await.unwrap()
}

type CallRow = (
    Uuid,
    String,
    String,
    Option<String>,
    Option<i32>,
    Option<String>,
);

async fn calls_for(pool: &PgPool, wf: Uuid, role: &str) -> Vec<CallRow> {
    sqlx::query_as(
        "SELECT id, tool, outcome, head_sha, exit_code, patch_sha256 FROM eng_tool_calls \
         WHERE workflow_id = $1 AND role = $2 ORDER BY at, id",
    )
    .bind(wf)
    .bind(role)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn art(arts: &[store::EngArtifact], k: ArtifactKind) -> &store::EngArtifact {
    arts.iter()
        .find(|a| a.kind == k.as_str())
        .unwrap_or_else(|| panic!("no {k:?}"))
}

// ---------------------------------------------------------------------------
// Positive end-to-end
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn six_role_agents_over_a2a_and_mcp_reach_the_delivery_gate(pool: PgPool) {
    let s = stack(&pool).await;
    let o = orchestrator(&pool, &s, vec![]);
    let wf = run_to_end(&pool, &o, &s.wf).await;
    assert_eq!(
        wf.status, "awaiting_delivery_approval",
        "{:?} {:?}",
        wf.error_code, wf.error
    );

    // Every stage ran over A2A (task id recorded) as its own agent.
    let runs: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT role, agent_id, a2a_task_id, provider FROM eng_agent_runs \
         WHERE workflow_id = $1 AND status = 'completed' ORDER BY started_at",
    )
    .bind(wf.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let roles: Vec<&str> = runs.iter().map(|r| r.0.as_str()).collect();
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
    for (role, agent, task, provider) in &runs {
        assert!(task.as_deref().is_some_and(|t| !t.is_empty()), "{role}");
        assert_eq!(agent, &format!("a2a-{role}-1"));
        assert_eq!(provider, "deterministic");
    }

    // B2: the MCP server created the workspace; the orchestrator recorded it.
    let (ws_id, ws_base): (Uuid, String) =
        sqlx::query_as("SELECT workspace_id, base_sha FROM eng_workspaces WHERE workflow_id = $1")
            .bind(wf.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(ws_id, wf.id);
    assert_eq!(ws_base, s.base);

    // B1/B4: the candidate is the commit MCP made, with MCP's digest.
    let dev = calls_for(&pool, wf.id, "developer").await;
    assert_eq!(dev.len(), 1, "{dev:?}");
    let (_, tool, outcome, head, _, psha) = &dev[0];
    assert_eq!(
        (tool.as_str(), outcome.as_str()),
        ("workspace.apply_patch", "allowed")
    );
    let candidate = wf.candidate_commit_sha.clone().unwrap();
    assert_eq!(head.as_deref(), Some(candidate.as_str()));
    let arts = store::artifacts(&pool, wf.id).await.unwrap();
    let cand = art(&arts, ArtifactKind::CodeCandidate);
    assert_eq!(cand.content["patch_sha256"].as_str(), psha.as_deref());
    assert_eq!(cand.content["base_commit_sha"], json!(s.base));
    assert_eq!(
        cand.content["changed_files"],
        json!([{"path": "src/lib.rs", "change": "modified"}])
    );

    // The reviewer diffed exactly that patch.
    let rev = calls_for(&pool, wf.id, "code_reviewer").await;
    assert!(rev.iter().any(|c| c.1 == "repository.diff"
        && c.5 == *psha
        && c.3.as_deref() == Some(candidate.as_str())));
    let review = art(&arts, ArtifactKind::ReviewReport);
    assert_eq!(review.content["conclusion"], json!("no_blocking_findings"));

    // QA ran the assignment's check through MCP, on the candidate.
    let qa = calls_for(&pool, wf.id, "qa_tester").await;
    assert_eq!(qa.len(), 1, "{qa:?}");
    assert_eq!((qa[0].1.as_str(), qa[0].4), ("testing.run", Some(0)));
    assert_eq!(qa[0].3.as_deref(), Some(candidate.as_str()));
    let qa_report = art(&arts, ArtifactKind::QaReport);
    assert_eq!(
        qa_report.content["results"][0]["check_id"],
        json!("cargo_test")
    );
    assert!(
        qa_report.content["environment"]["sandbox"]
            .as_str()
            .unwrap()
            .starts_with("test-double")
    );

    // Roles without MCP tools made no tool calls.
    for role in ["project_manager", "project_coordinator", "quality_lead"] {
        assert!(calls_for(&pool, wf.id, role).await.is_empty(), "{role}");
    }
    let ql = art(&arts, ArtifactKind::QualityAssessment);
    assert_eq!(ql.content["recommendation"], json!("ready"));
    // Advice, not approval: the workflow waits for a human.
    assert!(wf.pending_decision_hash.is_some());

    // B6: the read API the portal uses exposes the same evidence chain.
    let state = api::eng::routes::EngState {
        pool: pool.clone(),
        orchestrator: None,
        auth: Arc::new(api::eng::auth::Disabled),
        budget: Budget::default(),
    };
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, api::eng::routes::router(state)).await });
    let detail: Value = reqwest::get(format!("{base}/workflows/{}", wf.id))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["workspace_id"], json!(ws_id));
    let tools: Vec<&str> = detail["tool_calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["tool"].as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"workspace.apply_patch") && tools.contains(&"testing.run"),
        "{tools:?}"
    );
    assert!(
        detail["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c.get("arguments").is_none())
    );
    assert_eq!(detail["awaiting_decision"], json!("delivery"));
    if let Ok(path) = std::env::var("PHASE6B_DUMP_DETAIL") {
        std::fs::write(path, serde_json::to_vec_pretty(&detail).unwrap()).unwrap();
    }
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn b5_mcp_tokens_are_not_kept_in_agent_task_history(pool: PgPool) {
    let s = stack(&pool).await;
    let o = orchestrator(&pool, &s, vec![]);
    let wf = run_to_end(&pool, &o, &s.wf).await;
    assert_eq!(wf.status, "awaiting_delivery_approval", "{:?}", wf.error);
    let tasks: Vec<(String, String)> = sqlx::query_as(
        "SELECT role, a2a_task_id FROM eng_agent_runs WHERE workflow_id = $1 \
         AND role IN ('developer','code_reviewer','qa_tester') AND status = 'completed'",
    )
    .bind(wf.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(tasks.len(), 3);
    for (role, task_id) in tasks {
        let url = s.agents_url.join(&role_agents::agent_path(&role)).unwrap();
        let card = a2a_client::agent_card::AgentCardResolver::new(None)
            .resolve(url.as_str())
            .await
            .unwrap();
        let client = a2a_client::A2AClientFactory::builder()
            .build()
            .create_from_card(&card)
            .await
            .unwrap();
        let t = client
            .get_task(&a2a::GetTaskRequest {
                id: task_id,
                history_length: None,
                tenant: None,
            })
            .await
            .unwrap();
        let dump = serde_json::to_string(&t).unwrap();
        assert!(
            dump.contains("\"run_id\""),
            "{role}: history should still hold the task: {dump}"
        );
        assert!(
            !dump.contains("mcp_token"),
            "{role}: token leaked into task history"
        );
    }
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn developer_without_a_patch_fails_honestly(pool: PgPool) {
    let s = stack_with(&pool, Arc::new(TestRunner), None).await;
    let o = orchestrator(&pool, &s, vec![]);
    let wf = run_to_end(&pool, &o, &s.wf).await;
    assert_eq!(wf.status, "failed");
    assert_eq!(
        wf.error_code.as_deref(),
        Some("agent_failed"),
        "{:?}",
        wf.error
    );
    let arts = store::artifacts(&pool, wf.id).await.unwrap();
    assert!(
        arts.iter()
            .all(|a| a.kind != ArtifactKind::CodeCandidate.as_str())
    );
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn mcp_backed_stage_without_mcp_control_fails_before_running(pool: PgPool) {
    let s = stack(&pool).await;
    let mut o = orchestrator(&pool, &s, vec![]);
    o.mcp_control = None;
    let wf = run_to_end(&pool, &o, &s.wf).await;
    assert_eq!(wf.status, "failed");
    assert_eq!(
        wf.error_code.as_deref(),
        Some("provider_unconfigured"),
        "{:?}",
        wf.error
    );
}

/// Live: QA runs `cargo test` for real in the Docker sandbox (ADR-004, dev fixture only).
/// `A2A_SANDBOX_IMAGE=rust@sha256:... cargo test -p api --test phase6b -- --ignored live_sandbox`
#[sqlx::test(migrator = "api::db::MIGRATOR")]
#[ignore = "needs Docker and A2A_SANDBOX_IMAGE"]
async fn live_sandbox_qa_runs_real_tests_on_the_candidate(pool: PgPool) {
    let image = std::env::var("A2A_SANDBOX_IMAGE").expect("A2A_SANDBOX_IMAGE");
    let runner = workspace::DockerRunner::new(workspace::DockerRunnerConfig {
        image,
        timeout: Duration::from_secs(420),
        memory: "2g".into(),
        cpus: "2".into(),
        pids: 256,
    })
    .unwrap();
    let patch = fixture::candidate_patch().await.unwrap();
    let s = stack_with(&pool, Arc::new(runner), Some(patch)).await;
    let mut o = orchestrator(&pool, &s, vec![]);
    o.stage_timeout = Duration::from_secs(480);
    let wf = run_to_end(&pool, &o, &s.wf).await;
    assert_eq!(
        wf.status, "awaiting_delivery_approval",
        "{:?} {:?}",
        wf.error_code, wf.error
    );
    let arts = store::artifacts(&pool, wf.id).await.unwrap();
    let qa = art(&arts, ArtifactKind::QaReport);
    assert_eq!(qa.content["results"][0]["exit_code"], json!(0));
    let out = qa.content["results"][0]["output_tail"].as_str().unwrap();
    assert!(out.contains("rejects_unknown_currency ... ok"), "{out}");
    assert!(
        qa.content["environment"]["sandbox"]
            .as_str()
            .unwrap()
            .starts_with("docker")
    );
    println!("LIVE QA EVIDENCE: {}", qa.content);
}

// ---------------------------------------------------------------------------
// Negative: forged or inconsistent evidence (C5, B4)
// ---------------------------------------------------------------------------

type Evidence = Box<dyn Fn(&RoleInput, &str) -> Vec<ToolCallRecord> + Send + Sync>;
type Content = Box<dyn Fn(&RoleInput) -> Value + Send + Sync>;

/// A stage adapter that returns `content` and cites whatever audit rows
/// `evidence` fabricates for its run. It writes those rows directly (as a
/// compromised component might), so only the orchestrator's cross-checks stand
/// between it and a recorded artifact.
struct Liar {
    pool: PgPool,
    role: Role,
    content: Content,
    evidence: Evidence,
    cite_extra: Option<Uuid>,
}

impl RoleAgent for Liar {
    fn identity(&self) -> AgentIdentity {
        AgentIdentity {
            agent_id: format!("liar-{}", self.role.as_str()),
            provider: "test-double".into(),
            model: None,
        }
    }
    fn requires_tool_evidence(&self) -> bool {
        true
    }
    fn run(&self, input: RoleInput) -> BoxFut<'_, Result<AgentOutput, AgentError>> {
        Box::pin(async move {
            let audit = PgAudit::new(self.pool.clone());
            let cand = input.candidate_commit_sha.clone().unwrap_or_default();
            let mut ids = vec![];
            for r in (self.evidence)(&input, &cand) {
                ids.push(r.id);
                mcp_server::AuditSink::record(&audit, r).await.unwrap();
            }
            ids.extend(self.cite_extra);
            Ok(AgentOutput {
                content: (self.content)(&input),
                tool_requests: vec![],
                a2a_task_id: None,
                usage: Default::default(),
                tool_calls: ids,
            })
        })
    }
}

fn row(input: &RoleInput, role: Role, tool: &str, outcome: Outcome, head: &str) -> ToolCallRecord {
    let allowed = outcome == Outcome::Allowed;
    ToolCallRecord {
        id: Uuid::now_v7(),
        workflow_id: Some(input.workflow_id),
        run_id: Some(input.run_id),
        role: Some(role.as_str().into()),
        agent_id: Some(format!("liar-{}", role.as_str())),
        workspace_id: input.workspace_id,
        tool: tool.into(),
        target: Some("src/lib.rs".into()),
        arguments_sha256: None,
        outcome,
        error_code: (!allowed).then(|| "tool_not_granted".into()),
        head_sha: Some(head.into()),
        patch_sha256: Some("d".repeat(64)),
        exit_code: None,
        result_sha256: allowed.then(|| "c".repeat(64)),
        duration_ms: 1,
    }
}

fn review_content(i: &RoleInput) -> Value {
    json!({"commit_sha": i.candidate_commit_sha, "summary": "Looks good.",
           "conclusion": "no_blocking_findings", "findings": []})
}

fn liar_reviewer(
    pool: &PgPool,
    f: impl Fn(&RoleInput, &str) -> Vec<ToolCallRecord> + Send + Sync + 'static,
) -> Liar {
    Liar {
        pool: pool.clone(),
        role: Role::CodeReviewer,
        content: Box::new(review_content),
        evidence: Box::new(f),
        cite_extra: None,
    }
}

async fn expect_evidence_failure(pool: &PgPool, liar: Liar, why: &str) {
    let s = stack(pool).await;
    let role = liar.role;
    let o = orchestrator(pool, &s, vec![(role, Arc::new(liar))]);
    let wf = run_to_end(pool, &o, &s.wf).await;
    assert_eq!(wf.status, "failed", "{why}");
    assert_eq!(
        wf.error_code.as_deref(),
        Some("tool_evidence_invalid"),
        "{why}: {:?}",
        wf.error
    );
    let arts = store::artifacts(pool, wf.id).await.unwrap();
    assert!(
        arts.iter().all(|a| a.kind != role.produces().as_str()),
        "{why}"
    );
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn c5_fabricated_missing_refused_foreign_or_stale_review_evidence_is_rejected(pool: PgPool) {
    let mut l = liar_reviewer(&pool, |_, _| vec![]);
    l.cite_extra = Some(Uuid::now_v7());
    expect_evidence_failure(&pool, l, "id not in audit").await;
    expect_evidence_failure(&pool, liar_reviewer(&pool, |_, _| vec![]), "no evidence").await;
    expect_evidence_failure(
        &pool,
        liar_reviewer(&pool, |i, c| {
            vec![row(
                i,
                Role::CodeReviewer,
                "repository.read_file",
                Outcome::Allowed,
                c,
            )]
        }),
        "read but never diffed",
    )
    .await;
    expect_evidence_failure(
        &pool,
        liar_reviewer(&pool, |i, c| {
            vec![row(
                i,
                Role::CodeReviewer,
                "repository.diff",
                Outcome::Refused,
                c,
            )]
        }),
        "refused call",
    )
    .await;
    expect_evidence_failure(
        &pool,
        liar_reviewer(&pool, |i, c| {
            let mut r = row(
                i,
                Role::CodeReviewer,
                "repository.diff",
                Outcome::Allowed,
                c,
            );
            r.run_id = Some(Uuid::now_v7());
            vec![r]
        }),
        "another run",
    )
    .await;
    expect_evidence_failure(
        &pool,
        liar_reviewer(&pool, |i, _| {
            vec![row(
                i,
                Role::CodeReviewer,
                "repository.diff",
                Outcome::Allowed,
                &"e".repeat(40),
            )]
        }),
        "diff of another commit",
    )
    .await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn b4_review_of_a_different_patch_is_rejected(pool: PgPool) {
    // Right commit, wrong patch digest (`d…d` is not the candidate's).
    expect_evidence_failure(
        &pool,
        liar_reviewer(&pool, |i, c| {
            vec![row(
                i,
                Role::CodeReviewer,
                "repository.diff",
                Outcome::Allowed,
                c,
            )]
        }),
        "diff digest differs from candidate",
    )
    .await;
}

fn candidate_content(i: &RoleInput, commit: &str, psha: &str) -> Value {
    let base = i
        .artifact(ArtifactKind::WorkAssignment)
        .map(|a| a["base_commit_sha"].clone())
        .unwrap_or_default();
    json!({"base_commit_sha": base, "commit_sha": commit,
           "branch": engineering::roles::candidate_branch(i.workflow_id),
           "changed_files": [{"path": "src/lib.rs", "change": "modified"}],
           "patch_sha256": psha, "checks": [], "limitations": []})
}

fn liar_developer(
    pool: &PgPool,
    commit: String,
    psha: String,
    f: impl Fn(&RoleInput, &str) -> Vec<ToolCallRecord> + Send + Sync + 'static,
) -> Liar {
    Liar {
        pool: pool.clone(),
        role: Role::Developer,
        content: Box::new(move |i| candidate_content(i, &commit, &psha)),
        evidence: Box::new(f),
        cite_extra: None,
    }
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn b4_developer_cannot_report_facts_mcp_did_not_produce(pool: PgPool) {
    let audited = "a".repeat(40);
    let applied = |head: String| {
        move |i: &RoleInput, _: &str| {
            vec![row(
                i,
                Role::Developer,
                "workspace.apply_patch",
                Outcome::Allowed,
                &head,
            )]
        }
    };
    expect_evidence_failure(
        &pool,
        liar_developer(
            &pool,
            "b".repeat(40),
            "d".repeat(64),
            applied(audited.clone()),
        ),
        "commit differs from audited apply_patch",
    )
    .await;
    expect_evidence_failure(
        &pool,
        liar_developer(
            &pool,
            audited.clone(),
            "f".repeat(64),
            applied(audited.clone()),
        ),
        "digest differs",
    )
    .await;
    let a = audited.clone();
    expect_evidence_failure(
        &pool,
        liar_developer(&pool, audited.clone(), "d".repeat(64), move |i, _| {
            let mut r = row(
                i,
                Role::Developer,
                "workspace.apply_patch",
                Outcome::Allowed,
                &a,
            );
            r.target = Some("src/lib.rs,src/secret_backdoor.rs".into());
            vec![r]
        }),
        "hides a file the patch touched",
    )
    .await;
    expect_evidence_failure(
        &pool,
        liar_developer(&pool, audited, "d".repeat(64), |_, _| vec![]),
        "no patch applied",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Audit table (C6) and workspace mapping / control plane (B2)
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn c6_audit_and_workspace_tables_are_append_only_and_least_privilege(pool: PgPool) {
    let s = stack(&pool).await;
    let o = orchestrator(&pool, &s, vec![]);
    run_to_end(&pool, &o, &s.wf).await;
    for stmt in [
        "UPDATE eng_tool_calls SET outcome = 'refused'",
        "DELETE FROM eng_tool_calls",
        "TRUNCATE eng_tool_calls",
        "UPDATE eng_workspaces SET base_sha = repeat('0', 40)",
        "DELETE FROM eng_workspaces",
    ] {
        let e = sqlx::query(stmt).execute(&pool).await.unwrap_err();
        assert!(e.to_string().contains("append-only"), "{stmt}: {e}");
    }
    let privs: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT table_name::text, grantee::text, privilege_type::text \
         FROM information_schema.role_table_grants \
         WHERE table_name IN ('eng_tool_calls', 'eng_workspaces') \
         AND grantee IN ('a2a_mcp', 'a2a_app') ORDER BY 1, 2, 3",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let p: Vec<(&str, &str, &str)> = privs
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    assert_eq!(
        p,
        [
            ("eng_tool_calls", "a2a_app", "SELECT"),
            ("eng_tool_calls", "a2a_mcp", "INSERT"),
            ("eng_workspaces", "a2a_app", "INSERT"),
            ("eng_workspaces", "a2a_app", "SELECT"),
        ]
    );
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn b2_control_endpoint_refuses_agent_tokens_and_unknown_bases(pool: PgPool) {
    let s = stack(&pool).await;
    let http = reqwest::Client::new();
    let url = s.mcp_origin.join("/control/workspaces").unwrap();
    let now = chrono::Utc::now().timestamp();
    let agent_tok = engineering::mcp::issue_token(
        KEY,
        &engineering::mcp::CapabilityClaims {
            workflow_id: s.wf.id,
            run_id: Uuid::now_v7(),
            role: Role::Developer,
            agent_id: "dev".into(),
            workspace_id: Uuid::now_v7(),
            tools: vec![],
            scope: vec!["src".into()],
            base_sha: s.base.clone(),
            iat: now,
            exp: now + 60,
        },
    )
    .unwrap();
    for tok in [None, Some("garbage".to_owned()), Some(agent_tok)] {
        let mut r = http.post(url.clone());
        if let Some(t) = &tok {
            r = r.bearer_auth(t);
        }
        assert_eq!(r.send().await.unwrap().status(), 401, "{tok:?}");
    }
    let refused: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM eng_tool_calls WHERE tool = '(transport)' \
         AND error_code LIKE 'control_token_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refused, 3);
    // A valid control token for a base the source does not have: refused.
    let ctl = McpControl::new(s.mcp_origin.clone(), KEY.to_vec()).unwrap();
    assert!(
        ctl.create_workspace(s.wf.id, &"0".repeat(40))
            .await
            .is_err()
    );
    assert!(ctl.create_workspace(s.wf.id, &s.base).await.is_ok());
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn b7_every_workflow_status_is_accepted_by_the_schema(pool: PgPool) {
    use engineering::state::Status;
    for st in Status::ALL {
        let wf = store::create(&pool, "t", REQUIREMENTS, "bob", Budget::default())
            .await
            .unwrap();
        let terminal = st.is_terminal();
        sqlx::query(
            "UPDATE eng_workflows SET status = $2, \
             finished_at = CASE WHEN $3 THEN now() ELSE NULL END, \
             error_code = CASE WHEN $2 = 'failed' THEN 'internal_error' ELSE NULL END, \
             candidate_commit_sha = CASE WHEN $2 = 'approved_for_delivery' THEN repeat('a', 40) ELSE NULL END \
             WHERE id = $1",
        )
        .bind(wf.id)
        .bind(st.as_str())
        .bind(terminal)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("{}: {e}", st.as_str()));
    }
}
