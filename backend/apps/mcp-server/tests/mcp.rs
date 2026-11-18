//! Phase 6B C1–C3 evidence: the real MCP server (rmcp, Streamable HTTP) over a
//! real socket, a real rmcp client, and a real git workspace. The only test
//! double is the check runner (`TestRunner`), which reports `runner:
//! "test-double"` so its evidence can never be mistaken for a sandbox run.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use engineering::mcp::{CapabilityClaims, McpTool, issue_token};
use engineering::roles::Role;
use futures::future::BoxFuture;
use mcp_server::{McpState, MemoryAudit, Outcome, router};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use uuid::Uuid;
use workspace::runner::RunnerError;
use workspace::{CheckOutcome, CheckRunner, Workspace, fixture};

const KEY: &[u8] = b"test-key-0123456789abcdef-0123456789";

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
                stdout_tail: "test result: ok".into(),
                stderr_tail: String::new(),
                log_sha256: "0".repeat(64),
                head_sha: ws.head_sha().await?,
                runner: self.name().into(),
            })
        })
    }
}

struct Env {
    url: String,
    audit: Arc<MemoryAudit>,
    fx: fixture::Fixture,
    workflow_id: Uuid,
    _root: tempfile::TempDir,
}

async fn start_with(audit: Arc<MemoryAudit>, runner: Option<Arc<dyn CheckRunner>>) -> Env {
    let root = tempfile::tempdir().unwrap();
    let workflow_id = Uuid::now_v7();
    let fx = fixture::create(root.path(), Uuid::now_v7(), workflow_id)
        .await
        .unwrap();
    let state = Arc::new(McpState {
        key: KEY.to_vec(),
        workspaces_root: root.path().canonicalize().unwrap(),
        audit: audit.clone(),
        runner,
        locks: Default::default(),
        source_repo: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await });
    Env {
        url: format!("http://{addr}/mcp"),
        audit,
        fx,
        workflow_id,
        _root: root,
    }
}

async fn start() -> Env {
    start_with(Arc::new(MemoryAudit::default()), Some(Arc::new(TestRunner))).await
}

impl Env {
    fn claims(&self, role: Role, tools: &[McpTool]) -> CapabilityClaims {
        let now = chrono::Utc::now().timestamp();
        CapabilityClaims {
            workflow_id: self.workflow_id,
            run_id: Uuid::now_v7(),
            role,
            agent_id: format!("{}-1", role.as_str()),
            workspace_id: self.fx.workspace.id(),
            tools: tools.to_vec(),
            scope: vec!["src".into()],
            base_sha: self.fx.base_sha.clone(),
            iat: now - 5,
            exp: now + 300,
        }
    }

    fn token(&self, role: Role, tools: &[McpTool]) -> String {
        issue_token(KEY, &self.claims(role, tools)).unwrap()
    }

    async fn client(&self, token: &str) -> RunningService<RoleClient, ClientConfig> {
        let t = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(self.url.clone())
                .auth_header(token.to_owned()),
        );
        ClientConfig::default()
            .serve(t)
            .await
            .expect("mcp handshake")
    }
}

async fn call(
    c: &RunningService<RoleClient, ClientConfig>,
    tool: &str,
    args: Value,
) -> CallToolResult {
    let Value::Object(m) = args else {
        panic!("args must be an object")
    };
    c.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(m))
        .await
        .expect("transport-level success")
}

fn body(r: &CallToolResult) -> &Value {
    r.structured_content.as_ref().expect("structured result")
}

fn err_code(r: &CallToolResult) -> &str {
    assert_eq!(r.is_error, Some(true), "{r:?}");
    body(r)["error"].as_str().unwrap()
}

#[tokio::test]
async fn c1_tools_list_has_four_tools_with_closed_schemas() {
    let env = start().await;
    let c = env
        .client(&env.token(Role::CodeReviewer, &[McpTool::Diff]))
        .await;
    let tools = c.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "repository.diff",
            "repository.read_file",
            "testing.run",
            "workspace.apply_patch"
        ]
    );
    for t in &tools {
        assert_eq!(
            t.input_schema.get("additionalProperties"),
            Some(&json!(false)),
            "{}",
            t.name
        );
        // Only the write tool is not read-only.
        assert_eq!(
            t.annotations.as_ref().and_then(|a| a.read_only_hint),
            Some(t.name != "workspace.apply_patch"),
            "{}",
            t.name
        );
    }
    let run = tools.iter().find(|t| t.name == "testing.run").unwrap();
    assert_eq!(
        run.input_schema["properties"]["check_id"]["enum"],
        json!(["cargo_fmt_check", "cargo_clippy", "cargo_test"])
    );
}

#[tokio::test]
async fn c2_reviewer_reads_and_diffs_the_exact_candidate() {
    let env = start().await;
    let claims = env.claims(Role::CodeReviewer, &[McpTool::ReadFile, McpTool::Diff]);
    let c = env.client(&issue_token(KEY, &claims).unwrap()).await;

    let r = call(&c, "repository.diff", json!({})).await;
    assert_eq!(r.is_error, Some(false), "{r:?}");
    let d = body(&r);
    assert_eq!(d["base_sha"], json!(env.fx.base_sha));
    assert_eq!(d["head_sha"], json!(env.fx.candidate_sha));
    assert_eq!(d["changed_files"], json!(["src/lib.rs"]));
    assert!(
        d["patch"]
            .as_str()
            .unwrap()
            .contains("+    UnsupportedCurrency,")
    );
    let diff_call_id = d["tool_call_id"].as_str().unwrap().to_owned();

    let r = call(&c, "repository.read_file", json!({"path": "src/lib.rs"})).await;
    assert_eq!(body(&r)["head_sha"], json!(env.fx.candidate_sha));

    let rows = env.audit.rows();
    assert_eq!(rows.len(), 2);
    let diff_row = rows
        .iter()
        .find(|r| r.id.to_string() == diff_call_id)
        .expect("audit row for diff");
    assert_eq!(diff_row.outcome, Outcome::Allowed);
    assert_eq!(diff_row.run_id, Some(claims.run_id));
    assert_eq!(diff_row.role.as_deref(), Some("code_reviewer"));
    assert_eq!(
        diff_row.head_sha.as_deref(),
        Some(env.fx.candidate_sha.as_str())
    );
    assert!(diff_row.result_sha256.is_some());
}

#[tokio::test]
async fn c3_requests_without_a_valid_token_are_refused_at_the_edge_and_audited() {
    let env = start().await;
    let http = reqwest::Client::new();
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}});

    let mut expired = env.claims(Role::CodeReviewer, &[McpTool::Diff]);
    expired.iat -= 1000;
    expired.exp -= 1000;
    let mut forged = env.token(Role::CodeReviewer, &[McpTool::Diff]);
    forged.replace_range(forged.len() - 4.., "0000");
    let cases = [
        (None, "token_missing"),
        (Some("not-a-token".to_owned()), "token_invalid"),
        (Some(forged), "token_bad_signature"),
        (Some(issue_token(KEY, &expired).unwrap()), "token_expired"),
        (
            Some(
                issue_token(
                    b"another-key-0123456789abcdef-01234",
                    &env.claims(Role::Developer, &[]),
                )
                .unwrap(),
            ),
            "token_bad_signature",
        ),
    ];
    for (tok, code) in &cases {
        let mut req = http
            .post(&env.url)
            .header("accept", "application/json, text/event-stream")
            .json(&init);
        if let Some(t) = tok {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status(), 401, "{code}");
        assert_eq!(resp.json::<Value>().await.unwrap()["error"], json!(code));
    }
    let rows = env.audit.rows();
    assert_eq!(rows.len(), cases.len());
    assert!(
        rows.iter().all(|r| r.outcome == Outcome::Refused
            && r.tool == "(transport)"
            && r.run_id.is_none())
    );
}

#[tokio::test]
async fn c3_role_and_token_limits_are_enforced_before_execution() {
    let env = start().await;
    // Reviewer: read-only. Its token cannot even be issued with testing.run.
    assert!(issue_token(KEY, &env.claims(Role::CodeReviewer, &[McpTool::RunTests])).is_err());
    let rev = env
        .client(&env.token(Role::CodeReviewer, &[McpTool::ReadFile, McpTool::Diff]))
        .await;
    assert_eq!(
        err_code(&call(&rev, "testing.run", json!({"check_id": "cargo_test"})).await),
        "tool_not_granted"
    );
    for t in ["shell.exec", "git.push", "approve_delivery", "secrets.read"] {
        assert_eq!(
            err_code(&call(&rev, t, json!({})).await),
            "unknown_tool",
            "{t}"
        );
    }
    for p in [
        "../../etc/passwd",
        "/etc/passwd",
        ".git/config",
        "Cargo.toml",
        "src/../Cargo.toml",
    ] {
        assert_eq!(
            err_code(&call(&rev, "repository.read_file", json!({"path": p})).await),
            "path_out_of_scope",
            "{p}"
        );
    }
    // A diff cannot be redirected to another base by arguments.
    assert_eq!(
        err_code(&call(&rev, "repository.diff", json!({"base_sha": "0".repeat(40)})).await),
        "invalid_arguments"
    );

    // A token narrower than the role is honoured: QA with only read access cannot run tests.
    let qa_ro = env
        .client(&env.token(Role::QaTester, &[McpTool::ReadFile]))
        .await;
    assert_eq!(
        err_code(&call(&qa_ro, "testing.run", json!({"check_id": "cargo_test"})).await),
        "tool_not_granted"
    );

    // QA: only allowlisted check ids, no argv.
    let qa = env
        .client(&env.token(Role::QaTester, &[McpTool::RunTests]))
        .await;
    for bad in [json!({"check_id": "rm -rf /"}), json!({"check_id": "bash"})] {
        assert_eq!(
            err_code(&call(&qa, "testing.run", bad).await),
            "check_not_allowed"
        );
    }
    assert_eq!(
        err_code(
            &call(
                &qa,
                "testing.run",
                json!({"check_id": "cargo_test", "argv": ["sh", "-c", "id"]})
            )
            .await
        ),
        "invalid_arguments"
    );

    let rows = env.audit.rows();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|r| r.outcome == Outcome::Refused
        && r.error_code.is_some()
        && r.result_sha256.is_none()));
    assert!(rows.iter().all(|r| r.run_id.is_some()));
}

#[tokio::test]
async fn symlink_inside_scope_is_refused_by_the_workspace_layer() {
    let env = start().await;
    std::os::unix::fs::symlink("/etc/hosts", env.fx.workspace.dir().join("src/hosts")).unwrap();
    let rev = env
        .client(&env.token(Role::CodeReviewer, &[McpTool::ReadFile]))
        .await;
    let r = call(&rev, "repository.read_file", json!({"path": "src/hosts"})).await;
    assert_eq!(err_code(&r), "path_refused");
    assert_eq!(env.audit.rows()[0].outcome, Outcome::Failed);
}

#[tokio::test]
async fn workspace_comes_from_the_token_only() {
    let env = start().await;
    let mut c = env.claims(Role::CodeReviewer, &[McpTool::Diff]);
    c.workspace_id = Uuid::now_v7(); // a workspace that does not exist (or belongs to another run)
    let rev = env.client(&issue_token(KEY, &c).unwrap()).await;
    assert_eq!(
        err_code(&call(&rev, "repository.diff", json!({})).await),
        "workspace_not_found"
    );
}

#[tokio::test]
async fn qa_run_evidence_comes_from_the_runner_and_is_audited() {
    let env = start().await;
    let qa = env
        .client(&env.token(Role::QaTester, &[McpTool::RunTests]))
        .await;
    let r = call(&qa, "testing.run", json!({"check_id": "cargo_test"})).await;
    assert_eq!(r.is_error, Some(false), "{r:?}");
    let b = body(&r);
    assert_eq!(b["runner"], json!("test-double"));
    assert_eq!(b["argv"], json!(["cargo", "test", "--offline"]));
    assert_eq!(b["head_sha"], json!(env.fx.candidate_sha));
    let row = &env.audit.rows()[0];
    assert_eq!(
        (row.outcome, row.exit_code, row.target.as_deref()),
        (Outcome::Allowed, Some(0), Some("cargo_test"))
    );
}

#[tokio::test]
async fn testing_run_without_a_sandbox_fails_and_never_runs_on_the_host() {
    let env = start_with(Arc::new(MemoryAudit::default()), None).await;
    let qa = env
        .client(&env.token(Role::QaTester, &[McpTool::RunTests]))
        .await;
    assert_eq!(
        err_code(&call(&qa, "testing.run", json!({"check_id": "cargo_test"})).await),
        "sandbox_unavailable"
    );
    assert_eq!(env.audit.rows()[0].outcome, Outcome::Failed);
}

#[tokio::test]
async fn results_are_withheld_when_the_audit_cannot_be_written() {
    let env = start_with(Arc::new(MemoryAudit::failing()), Some(Arc::new(TestRunner))).await;
    let rev = env
        .client(&env.token(Role::CodeReviewer, &[McpTool::Diff]))
        .await;
    let r = rev
        .call_tool(CallToolRequestParams::new("repository.diff").with_arguments(Default::default()))
        .await;
    assert!(r.is_err(), "an unaudited result was returned: {r:?}");
}

// ---------------------------------------------------------------------------
// workspace.apply_patch (Phase 6B B1)
// ---------------------------------------------------------------------------

/// A server whose workspace holds only the base commit (Developer starting point).
async fn start_base() -> (Env, String) {
    let env = start().await;
    let root = env._root.path().to_path_buf();
    let id = Uuid::now_v7();
    let (_ws, base) = fixture::create_base(&root, id).await.unwrap();
    // Point the env's fixture at the new base-only workspace.
    let mut env = env;
    env.fx = fixture::Fixture {
        workspace: Workspace::open(&root, id).unwrap(),
        base_sha: base.clone(),
        candidate_sha: String::new(),
    };
    (env, base)
}

#[tokio::test]
async fn b1_developer_patch_is_applied_committed_and_audited() {
    let (env, base) = start_base().await;
    let claims = env.claims(Role::Developer, &McpTool::allowed_for(Role::Developer));
    let dev = env.client(&issue_token(KEY, &claims).unwrap()).await;
    let patch = fixture::candidate_patch().await.unwrap();
    let r = call(
        &dev,
        "workspace.apply_patch",
        json!({"patch": patch, "message": "Validate currency"}),
    )
    .await;
    assert_eq!(r.is_error, Some(false), "{r:?}");
    let b = body(&r);
    assert_eq!(b["previous_head_sha"], json!(base));
    assert_eq!(
        b["branch"],
        json!(engineering::roles::candidate_branch(env.workflow_id))
    );
    assert_eq!(b["changed_files"], json!(["src/lib.rs"]));
    let head = b["head_sha"].as_str().unwrap().to_owned();
    assert_ne!(head, base);
    // The same token's diff now shows exactly that candidate.
    let d = call(&dev, "repository.diff", json!({})).await;
    assert_eq!(body(&d)["head_sha"], json!(head));
    assert_eq!(body(&d)["patch_sha256"], b["patch_sha256"]);
    let row = env
        .audit
        .rows()
        .into_iter()
        .find(|r| r.tool == "workspace.apply_patch")
        .unwrap();
    assert_eq!(row.outcome, Outcome::Allowed);
    assert_eq!(row.run_id, Some(claims.run_id));
    assert_eq!(row.head_sha.as_deref(), Some(head.as_str()));
    assert_eq!(row.target.as_deref(), Some("src/lib.rs"));
}

#[tokio::test]
async fn b1_writes_are_refused_for_other_roles_and_out_of_scope_paths() {
    let (env, base) = start_base().await;
    let patch = fixture::candidate_patch().await.unwrap();
    for role in [Role::CodeReviewer, Role::QaTester] {
        let c = env
            .client(&env.token(role, &McpTool::allowed_for(role)))
            .await;
        assert_eq!(
            err_code(
                &call(
                    &c,
                    "workspace.apply_patch",
                    json!({"patch": patch, "message": "x"})
                )
                .await
            ),
            "tool_not_granted",
            "{role:?}"
        );
    }
    let dev = env
        .client(&env.token(Role::Developer, &McpTool::allowed_for(Role::Developer)))
        .await;
    let outside = "diff --git a/build.rs b/build.rs\nnew file mode 100644\n--- /dev/null\n+++ b/build.rs\n@@ -0,0 +1 @@\n+fn main() {}\n";
    assert_eq!(
        err_code(
            &call(
                &dev,
                "workspace.apply_patch",
                json!({"patch": outside, "message": "x"})
            )
            .await
        ),
        "path_out_of_scope"
    );
    let hook = outside.replace("build.rs", ".git/hooks/pre-commit");
    assert!(
        call(
            &dev,
            "workspace.apply_patch",
            json!({"patch": hook, "message": "x"})
        )
        .await
        .is_error
            == Some(true)
    );
    // Nothing was written: HEAD is still the base and the tree is clean.
    assert_eq!(env.fx.workspace.head_sha().await.unwrap(), base);
    assert!(!env.fx.workspace.dir().join("build.rs").exists());
    let rows = env.audit.rows();
    assert!(
        rows.iter().all(|r| r.outcome == Outcome::Refused),
        "{rows:?}"
    );
}

#[tokio::test]
async fn b1_concurrent_patches_are_serialized_per_workspace() {
    let (env, _base) = start_base().await;
    let tok = env.token(Role::Developer, &McpTool::allowed_for(Role::Developer));
    let (a, b) = (env.client(&tok).await, env.client(&tok).await);
    let p = |n: &str| {
        format!(
            "diff --git a/src/{n}.rs b/src/{n}.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/{n}.rs\n@@ -0,0 +1 @@\n+// {n}\n"
        )
    };
    let (ra, rb) = tokio::join!(
        call(
            &a,
            "workspace.apply_patch",
            json!({"patch": p("a"), "message": "a"})
        ),
        call(
            &b,
            "workspace.apply_patch",
            json!({"patch": p("b"), "message": "b"})
        ),
    );
    assert_eq!(
        (ra.is_error, rb.is_error),
        (Some(false), Some(false)),
        "{ra:?} {rb:?}"
    );
    // Both landed, one after the other (a linear history, no lost update).
    let heads = [
        body(&ra)["previous_head_sha"].clone(),
        body(&rb)["previous_head_sha"].clone(),
    ];
    let news = [body(&ra)["head_sha"].clone(), body(&rb)["head_sha"].clone()];
    assert!(
        heads.contains(&news[0]) || heads.contains(&news[1]),
        "not serialized: {heads:?} {news:?}"
    );
    let d = call(&a, "repository.diff", json!({})).await;
    assert_eq!(body(&d)["changed_files"], json!(["src/a.rs", "src/b.rs"]));
}
