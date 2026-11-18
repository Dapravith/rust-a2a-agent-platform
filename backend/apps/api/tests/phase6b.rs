//! Phase 6B C5/C6 evidence: orchestrator → (A2A, official SDK) → Code
//! Reviewer role agent → (MCP, official rmcp SDK) → MCP server → real git
//! workspace, with every tool call in the Postgres audit (`eng_tool_calls`)
//! and the orchestrator verifying the agent's cited evidence against it.
//!
//! Simulated here (honestly labelled `simulation`): PM, Coordinator,
//! Developer, QA, Quality Lead. The Developer reports the fixture's real
//! commit; real patch production is M5. The reviewer is rule-based
//! (`deterministic`), not an LLM.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use api::a2a::{A2AGateway, AgentEndpoint};
use api::eng::a2a_role::RemoteRoleAgent;
use api::eng::agents::{AgentError, AgentIdentity, AgentOutput, BoxFut, RoleAgent, RoleInput};
use api::eng::approval::{self, Decision, DecisionRequest, GateKind};
use api::eng::orchestrator::Orchestrator;
use api::eng::sim::{Behavior, SimAgent};
use api::eng::store::{self, EngWorkflow};
use engineering::artifacts::ArtifactKind;
use engineering::roles::Role;
use engineering::state::{Budget, HumanRole};
use mcp_server::{McpState, Outcome, PgAudit, ToolCallRecord};
use serde_json::{Value, json};
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;
use workspace::fixture;

const KEY: &[u8] = b"phase6b-test-key-0123456789abcdef-xyz";
const REQUIREMENTS: &str =
    "Add currency validation to the campaign payload while preserving backward compatibility.";

type OutputPatch = Box<dyn Fn(&RoleInput, &mut AgentOutput) + Send + Sync>;
type Evidence = Box<dyn Fn(&RoleInput, &str) -> Vec<ToolCallRecord> + Send + Sync>;

/// A sim agent whose content is patched (scope / real SHAs for the fixture).
struct Patched {
    inner: SimAgent,
    patch: OutputPatch,
}

impl RoleAgent for Patched {
    fn identity(&self) -> AgentIdentity {
        self.inner.identity()
    }
    fn run(&self, input: RoleInput) -> BoxFut<'_, Result<AgentOutput, AgentError>> {
        Box::pin(async move {
            let mut out = self.inner.run(input.clone()).await?;
            (self.patch)(&input, &mut out);
            Ok(out)
        })
    }
}

fn sim(role: Role) -> SimAgent {
    SimAgent::new(role, &format!("sim-{}", role.as_str()), Behavior::Normal)
}

struct Stack {
    _root: tempfile::TempDir,
    wf: EngWorkflow,
    fx: fixture::Fixture,
    patch_sha256: String,
    mcp_url: String,
    reviewer_url: Url,
}

/// MCP server (Postgres audit) + Code Reviewer A2A agent on real sockets,
/// and a workflow whose workspace is a real git fixture.
async fn stack(pool: &PgPool) -> Stack {
    let wf = store::create(
        pool,
        "Currency validation",
        REQUIREMENTS,
        "bob",
        Budget::default(),
    )
    .await
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let fx = fixture::create(root.path(), wf.id, wf.id).await.unwrap();
    let patch_sha256 = fx.workspace.diff(&fx.base_sha).await.unwrap().patch_sha256;

    let state = Arc::new(McpState {
        key: KEY.to_vec(),
        workspaces_root: root.path().canonicalize().unwrap(),
        audit: Arc::new(PgAudit::new(pool.clone())),
        runner: None,
        locks: Default::default(),
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mcp_url = format!("http://{}/mcp", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, mcp_server::router(state)).await });

    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reviewer_url = Url::parse(&format!("http://{}", l.local_addr().unwrap())).unwrap();
    let app = agent_runtime::router(
        role_agents::CodeReviewer::new(mcp_url.clone()),
        reviewer_url.as_str(),
    );
    tokio::spawn(async move { axum::serve(l, app).await });

    Stack {
        _root: root,
        wf,
        fx,
        patch_sha256,
        mcp_url,
        reviewer_url,
    }
}

fn remote_reviewer(s: &Stack) -> Arc<dyn RoleAgent> {
    Arc::new(
        RemoteRoleAgent::new(
            Role::CodeReviewer,
            AgentIdentity {
                agent_id: "a2a-code-reviewer-1".into(),
                provider: role_agents::PROVIDER.into(),
                model: None,
            },
            AgentEndpoint {
                expected_name: contracts::role::CODE_REVIEWER_AGENT_NAME.into(),
                base_url: s.reviewer_url.clone(),
            },
            A2AGateway::new(Duration::from_secs(20), Duration::from_millis(50)).unwrap(),
            KEY.to_vec(),
            Duration::from_secs(300),
        )
        .unwrap(),
    )
}

fn team(s: &Stack, reviewer: Arc<dyn RoleAgent>) -> HashMap<Role, Arc<dyn RoleAgent>> {
    let (cand, base, psha) = (
        s.fx.candidate_sha.clone(),
        s.fx.base_sha.clone(),
        s.patch_sha256.clone(),
    );
    let mut m: HashMap<Role, Arc<dyn RoleAgent>> = HashMap::new();
    m.insert(
        Role::ProjectManager,
        Arc::new(Patched {
            inner: sim(Role::ProjectManager),
            patch: Box::new(|_, o| o.content["scope"] = json!(["src"])),
        }),
    );
    m.insert(
        Role::ProjectCoordinator,
        Arc::new(sim(Role::ProjectCoordinator)),
    );
    m.insert(
        Role::Developer,
        Arc::new(Patched {
            inner: sim(Role::Developer),
            patch: Box::new(move |_, o| {
                for t in &mut o.tool_requests {
                    if t.tool == "write_workspace" {
                        t.target = Some("src/lib.rs".into());
                    }
                }
                let c = &mut o.content;
                c["commit_sha"] = json!(cand);
                c["base_commit_sha"] = json!(base);
                c["changed_files"] = json!([{"path": "src/lib.rs", "change": "modified"}]);
                c["patch_sha256"] = json!(psha);
            }),
        }),
    );
    m.insert(Role::CodeReviewer, reviewer);
    m.insert(Role::QaTester, Arc::new(sim(Role::QaTester)));
    m.insert(Role::QualityLead, Arc::new(sim(Role::QualityLead)));
    m
}

fn orchestrator(pool: &PgPool, s: &Stack, reviewer: Arc<dyn RoleAgent>) -> Orchestrator {
    let mut o = Orchestrator::with_agents(pool.clone(), team(s, reviewer));
    o.stage_timeout = Duration::from_secs(30);
    o.base_commit_sha = Some(s.fx.base_sha.clone());
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

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn c5_reviewer_reviews_the_exact_candidate_over_a2a_and_mcp(pool: PgPool) {
    let s = stack(&pool).await;
    let o = orchestrator(&pool, &s, remote_reviewer(&s));
    let wf = run_to_end(&pool, &o, &s.wf).await;
    assert_eq!(
        wf.status, "awaiting_delivery_approval",
        "{:?} {:?}",
        wf.error_code, wf.error
    );
    assert_eq!(
        wf.candidate_commit_sha.as_deref(),
        Some(s.fx.candidate_sha.as_str())
    );

    // The review artifact: produced by the A2A agent, about the exact candidate,
    // citing the real patch digest it got from MCP.
    let arts = store::artifacts(&pool, wf.id).await.unwrap();
    let review = arts
        .iter()
        .find(|a| a.kind == ArtifactKind::ReviewReport.as_str())
        .unwrap();
    assert_eq!(review.producer_id, "a2a-code-reviewer-1");
    assert_eq!(review.provider.as_deref(), Some("deterministic"));
    assert_eq!(review.content["commit_sha"], json!(s.fx.candidate_sha));
    assert_eq!(review.content["conclusion"], json!("no_blocking_findings"));
    assert!(
        review.content["summary"]
            .as_str()
            .unwrap()
            .contains(&s.patch_sha256)
    );

    // The reviewer run went over A2A (task id recorded).
    let (run_id, task_id): (Uuid, Option<String>) = sqlx::query_as(
        "SELECT id, a2a_task_id FROM eng_agent_runs WHERE workflow_id = $1 AND role = 'code_reviewer' AND status = 'completed'",
    )
    .bind(wf.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(task_id.is_some_and(|t| !t.is_empty()));

    // The MCP audit has the diff call for exactly that run, against the candidate.
    let calls: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, tool, outcome, head_sha FROM eng_tool_calls WHERE run_id = $1 ORDER BY at",
    )
    .bind(run_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let (call_id, tool, outcome, head) = &calls[0];
    assert_eq!(
        (tool.as_str(), outcome.as_str()),
        ("repository.diff", "allowed")
    );
    assert_eq!(head.as_deref(), Some(s.fx.candidate_sha.as_str()));

    // The orchestrator's audit event correlates artifact ↔ run ↔ tool call.
    let detail: Value = sqlx::query_scalar(
        "SELECT detail FROM eng_audit_events WHERE workflow_id = $1 AND action = 'artifact_recorded' \
         AND detail->>'kind' = 'review_report'",
    )
    .bind(wf.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(detail["run_id"], json!(run_id));
    assert_eq!(detail["tool_calls"], json!([call_id]));
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn c5_reviewer_fails_honestly_when_mcp_refuses(pool: PgPool) {
    let s = stack(&pool).await;
    // The workspace disappears (or was never created): the agent cannot review.
    std::fs::remove_dir_all(s.fx.workspace.dir()).unwrap();
    let o = orchestrator(&pool, &s, remote_reviewer(&s));
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
            .all(|a| a.kind != ArtifactKind::ReviewReport.as_str())
    );
    let failed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM eng_tool_calls WHERE workflow_id = $1 AND outcome = 'failed' AND error_code = 'workspace_not_found'",
    )
    .bind(wf.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failed, 1);
}

/// A reviewer that returns a valid-looking review and cites whatever
/// evidence `evidence` fabricates for the run (it can write audit rows the
/// way a compromised component might, but cannot make them match).
struct LyingReviewer {
    pool: PgPool,
    candidate: String,
    evidence: Evidence,
    cite_extra: Option<Uuid>,
}

impl RoleAgent for LyingReviewer {
    fn identity(&self) -> AgentIdentity {
        AgentIdentity {
            agent_id: "liar-1".into(),
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
            let mut ids = vec![];
            for r in (self.evidence)(&input, &self.candidate) {
                ids.push(r.id);
                mcp_server::AuditSink::record(&audit, r).await.unwrap();
            }
            ids.extend(self.cite_extra);
            Ok(AgentOutput {
                content: json!({"commit_sha": self.candidate, "summary": "Looks good.",
                                "conclusion": "no_blocking_findings", "findings": []}),
                tool_requests: vec![],
                a2a_task_id: None,
                usage: Default::default(),
                tool_calls: ids,
            })
        })
    }
}

fn row(input: &RoleInput, tool: &str, outcome: Outcome, head: &str) -> ToolCallRecord {
    let allowed = outcome == Outcome::Allowed;
    ToolCallRecord {
        id: Uuid::now_v7(),
        workflow_id: Some(input.workflow_id),
        run_id: Some(input.run_id),
        role: Some("code_reviewer".into()),
        agent_id: Some("liar-1".into()),
        workspace_id: Some(input.workflow_id),
        tool: tool.into(),
        target: None,
        arguments_sha256: None,
        outcome,
        error_code: (!allowed).then(|| "tool_not_granted".into()),
        head_sha: Some(head.into()),
        exit_code: None,
        result_sha256: allowed.then(|| "c".repeat(64)),
        duration_ms: 1,
    }
}

async fn expect_evidence_failure(pool: &PgPool, liar: LyingReviewer, why: &str) {
    let s = stack(pool).await;
    let liar = LyingReviewer {
        candidate: s.fx.candidate_sha.clone(),
        ..liar
    };
    let o = orchestrator(pool, &s, Arc::new(liar));
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
        arts.iter()
            .all(|a| a.kind != ArtifactKind::ReviewReport.as_str()),
        "{why}"
    );
}

fn liar(
    pool: &PgPool,
    f: impl Fn(&RoleInput, &str) -> Vec<ToolCallRecord> + Send + Sync + 'static,
) -> LyingReviewer {
    LyingReviewer {
        pool: pool.clone(),
        candidate: String::new(),
        evidence: Box::new(f),
        cite_extra: None,
    }
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn c5_fabricated_tool_call_id_is_rejected(pool: PgPool) {
    let mut l = liar(&pool, |_, _| vec![]);
    l.cite_extra = Some(Uuid::now_v7());
    expect_evidence_failure(&pool, l, "id not in audit").await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn c5_review_without_an_audited_diff_is_rejected(pool: PgPool) {
    expect_evidence_failure(&pool, liar(&pool, |_, _| vec![]), "no evidence at all").await;
    expect_evidence_failure(
        &pool,
        liar(&pool, |i, c| {
            vec![row(i, "repository.read_file", Outcome::Allowed, c)]
        }),
        "read but never diffed",
    )
    .await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn c5_refused_foreign_or_stale_evidence_is_rejected(pool: PgPool) {
    expect_evidence_failure(
        &pool,
        liar(&pool, |i, c| {
            vec![row(i, "repository.diff", Outcome::Refused, c)]
        }),
        "refused call",
    )
    .await;
    expect_evidence_failure(
        &pool,
        liar(&pool, |i, c| {
            let mut r = row(i, "repository.diff", Outcome::Allowed, c);
            r.run_id = Some(Uuid::now_v7());
            vec![r]
        }),
        "another run",
    )
    .await;
    expect_evidence_failure(
        &pool,
        liar(&pool, |i, _| {
            vec![row(i, "repository.diff", Outcome::Allowed, &"e".repeat(40))]
        }),
        "diff of another commit",
    )
    .await;
}

#[sqlx::test(migrator = "api::db::MIGRATOR")]
async fn c6_tool_call_audit_is_append_only_and_least_privilege(pool: PgPool) {
    let s = stack(&pool).await;
    let o = orchestrator(&pool, &s, remote_reviewer(&s));
    run_to_end(&pool, &o, &s.wf).await;
    // Even the table owner cannot rewrite history.
    for stmt in [
        "UPDATE eng_tool_calls SET outcome = 'refused'",
        "DELETE FROM eng_tool_calls",
        "TRUNCATE eng_tool_calls",
    ] {
        let e = sqlx::query(stmt).execute(&pool).await.unwrap_err();
        assert!(e.to_string().contains("append-only"), "{stmt}: {e}");
    }
    // a2a_mcp: INSERT only. a2a_app: SELECT only.
    let privs: Vec<(String, String)> = sqlx::query_as(
        "SELECT grantee::text, privilege_type::text FROM information_schema.role_table_grants \
         WHERE table_name = 'eng_tool_calls' AND grantee IN ('a2a_mcp', 'a2a_app') ORDER BY 1, 2",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        privs,
        [
            ("a2a_app".into(), "SELECT".into()),
            ("a2a_mcp".into(), "INSERT".into())
        ]
    );
    // The CHECKs refuse an inconsistent row (allowed with an error code).
    let bad = sqlx::query(
        "INSERT INTO eng_tool_calls (id, run_id, tool, outcome, error_code, result_sha256) \
         VALUES ($1, $1, 'repository.diff', 'allowed', 'x', NULL)",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert!(bad.is_err());
    let _ = s.mcp_url;
}
