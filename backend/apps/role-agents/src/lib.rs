//! Engineering role agents over A2A (Phase 6B C5).
//!
//! Each role agent is an A2A server (official SDK, via `agent_runtime`). It
//! receives a [`RoleTask`] and returns a [`RoleResult`]. All repository access
//! goes through the configured MCP server with the per-run token from the task.
//! The agent holds no workspace, credentials or policy of its own.
//!
//! All six roles run in **deterministic (rule-based) mode** (Mode A): the
//! Code Reviewer here, the other five in [`roles`]. Their findings and facts
//! come from real MCP tool results. Phase 6C puts real providers behind the
//! same contract.

use a2a::{AgentCard, AgentSkill};
use agent_runtime::{Skill, agent_card};
use contracts::role::{
    CODE_REVIEWER_AGENT_NAME, CODE_REVIEWER_SKILL_ID, ROLE_RESULT_ARTIFACT_NAME, RoleResult,
    RoleTask,
};
use contracts::{AgentFailure, AgentFailureCode};
use engineering::artifacts::{
    FindingCategory, FindingSeverity, ReviewConclusion, ReviewFinding, ReviewReport,
};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::Value;

pub mod roles;

pub use roles::{AgentsConfig, agent_path, router};

pub const DEFAULT_PORT: u16 = 9111;
pub const PROVIDER: &str = "deterministic";

/// A thin MCP client bound to one task's token.
pub struct Mcp {
    client: rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>,
}

pub(crate) fn fail(msg: impl Into<String>) -> AgentFailure {
    AgentFailure::new(AgentFailureCode::Internal, msg)
}

impl Mcp {
    pub async fn connect(url: &str, token: &str) -> Result<Mcp, AgentFailure> {
        let t = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url.to_owned())
                .auth_header(token.to_owned()),
        );
        let client = ClientConfig::default()
            .serve(t)
            .await
            .map_err(|e| fail(format!("mcp connect failed: {e}")))?;
        Ok(Mcp { client })
    }

    /// Calls a tool; a tool error (refusal/failure) becomes an agent failure.
    pub async fn call(&self, tool: &'static str, args: Value) -> Result<Value, AgentFailure> {
        let Value::Object(m) = args else {
            return Err(fail("tool arguments must be an object"));
        };
        let r = self
            .client
            .call_tool(CallToolRequestParams::new(tool).with_arguments(m))
            .await
            .map_err(|e| fail(format!("mcp `{tool}` failed: {e}")))?;
        let body = r.structured_content.unwrap_or(Value::Null);
        if r.is_error == Some(true) {
            let code = body["error"].as_str().unwrap_or("unknown");
            return Err(fail(format!("mcp `{tool}` refused: {code}")));
        }
        Ok(body)
    }

    pub async fn close(self) {
        let _ = self.client.cancel().await;
    }
}

pub(crate) fn tool_call_id(v: &Value) -> Result<uuid::Uuid, AgentFailure> {
    v["tool_call_id"]
        .as_str()
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| fail("mcp result has no valid tool_call_id"))
}

/// Rule-based review of one unified diff. Pure, so it is unit-testable.
pub fn review_diff(diff: &Value, scope: &[String]) -> ReviewReport {
    let head = diff["head_sha"].as_str().unwrap_or_default().to_owned();
    let patch = diff["patch"].as_str().unwrap_or_default();
    let files: Vec<String> = diff["changed_files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let added: Vec<&str> = patch
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .collect();
    let removed = patch
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .count();
    let mut findings = Vec::new();
    let mut add = |sev, cat, file: &str, title: &str, detail: String| {
        findings.push(ReviewFinding {
            id: format!("R-{}", findings.len() + 1),
            severity: sev,
            category: cat,
            file: file.to_owned(),
            line: None,
            title: title.to_owned(),
            detail,
        });
    };
    if files.is_empty() {
        add(
            FindingSeverity::High,
            FindingCategory::Correctness,
            "(none)",
            "Candidate changes nothing",
            "The diff against the assigned base is empty.".into(),
        );
    }
    for f in files
        .iter()
        .filter(|f| !engineering::roles::path_in_scope(f, scope))
    {
        add(
            FindingSeverity::Critical,
            FindingCategory::Security,
            f,
            "Change outside the approved scope",
            format!("`{f}` is not under {scope:?}."),
        );
    }
    let touches_src = files.iter().any(|f| f.ends_with(".rs"));
    let adds_test = added.iter().any(|l| l.contains("#[test]"));
    if touches_src && !adds_test {
        add(
            FindingSeverity::Medium,
            FindingCategory::Testing,
            &files[0],
            "No test added for the change",
            "Rust sources changed but the diff adds no #[test].".into(),
        );
    }
    if let Some(l) = added.iter().find(|l| l.contains(".unwrap()")) {
        add(
            FindingSeverity::Low,
            FindingCategory::Maintainability,
            &files[0],
            "New unwrap()",
            format!(
                "Added line uses unwrap(): `{}`",
                l.trim_start_matches('+').trim()
            ),
        );
    }
    let blocking = findings.iter().any(|f| f.severity >= FindingSeverity::High);
    ReviewReport {
        commit_sha: head,
        summary: format!(
            "Rule-based review (deterministic, no model) of the exact candidate diff from MCP \
             repository.diff: {} file(s), +{} / -{} lines, patch sha256 {}.",
            files.len(),
            added.len(),
            removed,
            diff["patch_sha256"].as_str().unwrap_or("?")
        ),
        conclusion: if blocking {
            ReviewConclusion::BlockingFindings
        } else {
            ReviewConclusion::NoBlockingFindings
        },
        findings,
    }
}

pub struct CodeReviewer {
    mcp_url: String,
}

impl CodeReviewer {
    pub fn new(mcp_url: impl Into<String>) -> Self {
        CodeReviewer {
            mcp_url: mcp_url.into(),
        }
    }

    async fn review(&self, task: &RoleTask) -> Result<RoleResult, AgentFailure> {
        if task.role != "code_reviewer" {
            return Err(fail("this agent only serves the code_reviewer role"));
        }
        let candidate = task
            .candidate_commit_sha
            .as_deref()
            .ok_or_else(|| fail("no candidate commit to review"))?;
        let token = task
            .mcp_token
            .as_deref()
            .ok_or_else(|| fail("no MCP token in the task"))?;
        let mcp = Mcp::connect(&self.mcp_url, token).await?;
        let result = async {
            let diff = mcp.call("repository.diff", serde_json::json!({})).await?;
            if diff["head_sha"].as_str() != Some(candidate) {
                return Err(fail("workspace HEAD is not the candidate commit"));
            }
            let id = tool_call_id(&diff)?;
            let report = review_diff(&diff, &task.scope);
            let content = serde_json::to_value(&report).map_err(|e| fail(e.to_string()))?;
            Ok(RoleResult {
                content,
                tool_calls: vec![id],
            })
        }
        .await;
        mcp.close().await;
        result
    }
}

impl Skill for CodeReviewer {
    type Input = RoleTask;
    type Output = RoleResult;
    const ARTIFACT_NAME: &'static str = ROLE_RESULT_ARTIFACT_NAME;

    fn card(&self, public_url: &str) -> AgentCard {
        let skill = AgentSkill {
            id: CODE_REVIEWER_SKILL_ID.into(),
            name: "Review candidate".into(),
            description: "Input: RoleTask (application/json). Reviews the exact candidate diff \
                obtained via MCP repository.diff. Output artifact `role_result`: \
                {content: review_report, tool_calls[]}. Read-only: cannot modify the candidate."
                .into(),
            tags: vec!["code-review".into(), "engineering".into()],
            examples: None,
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        };
        agent_card(
            CODE_REVIEWER_AGENT_NAME,
            "Engineering Code Reviewer role agent (Phase 6B).",
            env!("CARGO_PKG_VERSION"),
            skill,
            public_url,
        )
    }

    fn inference_mode(&self) -> &'static str {
        PROVIDER
    }

    async fn run(&self, input: RoleTask) -> Result<RoleResult, AgentFailure> {
        tracing::info!(task = %input, "code review started");
        self.review(&input).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use engineering::artifacts::Contract;
    use serde_json::json;

    fn diff(files: &[&str], patch: &str) -> Value {
        json!({"head_sha": "a".repeat(40), "changed_files": files, "patch": patch, "patch_sha256": "b".repeat(64)})
    }

    #[test]
    fn clean_tested_change_has_no_blocking_findings() {
        let r = review_diff(
            &diff(
                &["src/lib.rs"],
                "+++ b/src/lib.rs\n+    #[test]\n+    fn t() {}\n-old\n",
            ),
            &["src".into()],
        );
        assert_eq!(r.conclusion, ReviewConclusion::NoBlockingFindings);
        assert!(r.findings.is_empty());
        r.validate().unwrap();
    }

    #[test]
    fn out_of_scope_and_empty_diffs_block() {
        let r = review_diff(&diff(&["Cargo.toml"], "+x\n"), &["src".into()]);
        assert_eq!(r.conclusion, ReviewConclusion::BlockingFindings);
        r.validate().unwrap();
        let r = review_diff(&diff(&[], ""), &["src".into()]);
        assert_eq!(r.conclusion, ReviewConclusion::BlockingFindings);
        r.validate().unwrap();
    }

    #[test]
    fn untested_change_and_unwrap_are_reported_but_not_blocking() {
        let r = review_diff(
            &diff(&["src/lib.rs"], "+let x = y.unwrap();\n"),
            &["src".into()],
        );
        let sev: Vec<_> = r.findings.iter().map(|f| f.severity).collect();
        assert_eq!(sev, [FindingSeverity::Medium, FindingSeverity::Low]);
        assert_eq!(r.conclusion, ReviewConclusion::NoBlockingFindings);
        r.validate().unwrap();
    }
}
