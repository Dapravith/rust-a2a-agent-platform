//! Deterministic simulated role agents (test Mode A).
//!
//! Every run is recorded with provider `simulation`, so simulated output can
//! never be mistaken for a real provider's. `api serve` never wires these;
//! only tests (and an explicit future demo mode) construct them.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use engineering::artifacts::ArtifactKind;
use engineering::roles::{Role, ToolRequest, candidate_branch};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::agents::{
    AgentError, AgentErrorKind, AgentIdentity, AgentOutput, BoxFut, RoleAgent, RoleInput, Usage,
};

pub const PROVIDER: &str = "simulation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Behavior {
    Normal,
    /// Output that does not match the role's contract.
    Malformed,
    /// Reports use of an extra tool (e.g. `git_push`).
    RequestTool(String),
    /// `Unavailable` for the first `n` calls, then normal.
    FailTimes(u32),
    /// Never returns (exercises the stage deadline).
    Hang,
    /// Reviewer/QA/QL report on a different commit than the candidate.
    WrongCommit,
    /// Developer reports a commit on a protected branch.
    ProtectedBranch,
    /// Reviewer reports a high-severity finding.
    BlockingFinding,
    /// QA reports a failing check.
    FailingChecks,
}

pub struct SimAgent {
    pub role: Role,
    pub agent_id: String,
    pub behavior: Behavior,
    pub calls: Arc<AtomicU32>,
}

impl SimAgent {
    pub fn new(role: Role, agent_id: &str, behavior: Behavior) -> Self {
        SimAgent {
            role,
            agent_id: agent_id.into(),
            behavior,
            calls: Arc::new(AtomicU32::new(0)),
        }
    }
}

pub fn sha_of(seed: &str) -> String {
    let h = hex::encode(Sha256::digest(seed.as_bytes()));
    h[..40].to_owned()
}

fn sha256_of(seed: &str) -> String {
    hex::encode(Sha256::digest(seed.as_bytes()))
}

pub fn base_commit(input: &RoleInput) -> String {
    input
        .base_commit_sha
        .clone()
        .unwrap_or_else(|| sha_of(&format!("{}:base", input.workflow_id)))
}

impl RoleAgent for SimAgent {
    fn identity(&self) -> AgentIdentity {
        AgentIdentity {
            agent_id: self.agent_id.clone(),
            provider: PROVIDER.into(),
            model: None,
        }
    }

    fn run(&self, input: RoleInput) -> BoxFut<'_, Result<AgentOutput, AgentError>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            match &self.behavior {
                Behavior::Hang => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
                Behavior::FailTimes(k) if n <= *k => {
                    return Err(AgentError {
                        kind: AgentErrorKind::Unavailable,
                        message: format!("simulated outage (call {n})"),
                    });
                }
                Behavior::Malformed => {
                    return Ok(out(
                        json!({"note": "not a contract", "reasoning": "hidden"}),
                        vec![],
                    ));
                }
                _ => {}
            }
            let mut o = self.produce(&input);
            if let Behavior::RequestTool(t) = &self.behavior {
                o.tool_requests.push(ToolRequest {
                    tool: t.clone(),
                    target: None,
                });
            }
            Ok(o)
        })
    }
}

fn out(content: Value, tools: Vec<ToolRequest>) -> AgentOutput {
    AgentOutput {
        content,
        tool_requests: tools,
        a2a_task_id: None,
        usage: Usage::default(),
        tool_calls: vec![],
    }
}

fn tool(t: &str, target: Option<&str>) -> ToolRequest {
    ToolRequest {
        tool: t.into(),
        target: target.map(Into::into),
    }
}

const FILE: &str = "apps/api/src/routes/campaigns.rs";

impl SimAgent {
    fn produce(&self, input: &RoleInput) -> AgentOutput {
        let criteria: Vec<String> = input
            .artifact(ArtifactKind::RequirementsPlan)
            .and_then(|p| p["acceptance_criteria"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let candidate = input.candidate_commit_sha.clone().unwrap_or_default();
        let reported = if self.behavior == Behavior::WrongCommit {
            sha_of("some-other-commit")
        } else {
            candidate.clone()
        };
        match self.role {
            Role::ProjectManager => out(
                json!({
                    "summary": "Add input validation to the campaign-creation endpoint without changing unrelated routes.",
                    "deliverables": ["Validation for POST /campaigns", "Unit tests for each rule"],
                    "acceptance_criteria": [
                        {"id": "AC-1", "description": "Invalid campaign payloads are rejected with 422 and a field error."},
                        {"id": "AC-2", "description": "No other route's behaviour changes (existing tests pass)."}
                    ],
                    "dependencies": [],
                    "risks": ["Over-strict rules could reject valid currencies."],
                    "steps": ["Define rules", "Implement validation", "Add tests"],
                    "scope": ["apps/api/src/routes"]
                }),
                vec![tool("read_artifacts", None)],
            ),
            Role::ProjectCoordinator => out(
                json!({
                    "summary": format!("Implement plan revision {}", input.revision),
                    "scope": input.scope,
                    "acceptance_criteria": criteria,
                    "required_checks": ["cargo_test"],
                    "base_commit_sha": base_commit(input),
                }),
                vec![tool("read_artifacts", None)],
            ),
            Role::Developer => {
                let commit = sha_of(&format!(
                    "{}:candidate:{}",
                    input.workflow_id, input.revision
                ));
                let branch = if self.behavior == Behavior::ProtectedBranch {
                    "main".to_owned()
                } else {
                    candidate_branch(input.workflow_id)
                };
                out(
                    json!({
                        "base_commit_sha": base_commit(input),
                        "commit_sha": commit,
                        "branch": branch,
                        "changed_files": [{"path": FILE, "change": "modified"}],
                        "patch_sha256": sha256_of(&format!("patch:{commit}")),
                        "checks": [{"check_id": "cargo_test", "exit_code": 0, "duration_ms": 1, "output_tail": "test result: ok (simulated)"}],
                        "limitations": ["Simulated change: no real repository was modified."]
                    }),
                    vec![
                        tool("read_artifacts", None),
                        tool("write_workspace", Some(FILE)),
                        tool("run_check", Some("cargo_test")),
                        tool("commit_to_candidate_branch", None),
                    ],
                )
            }
            Role::CodeReviewer => {
                let findings = if self.behavior == Behavior::BlockingFinding {
                    json!([{"id": "F1", "severity": "high", "category": "security", "file": FILE, "line": 12,
                            "title": "Unbounded goal amount", "detail": "No upper limit on goal; overflow risk."}])
                } else {
                    json!([])
                };
                let conclusion = if self.behavior == Behavior::BlockingFinding {
                    "blocking_findings"
                } else {
                    "no_blocking_findings"
                };
                out(
                    json!({"commit_sha": reported, "summary": "Reviewed the exact candidate diff.", "conclusion": conclusion, "findings": findings}),
                    vec![tool("read_workspace", None)],
                )
            }
            Role::QaTester => {
                let exit = if self.behavior == Behavior::FailingChecks {
                    101
                } else {
                    0
                };
                let status = if exit == 0 { "verified" } else { "failed" };
                out(
                    json!({
                        "commit_sha": reported,
                        "environment": {"os": "simulated", "toolchain": "simulated", "sandbox": "none (simulation)"},
                        "results": [{"check_id": "cargo_test", "exit_code": exit, "duration_ms": 1, "output_tail": "simulated"}],
                        "criteria": criteria.iter().map(|c| json!({"criterion_id": c, "status": status, "evidence": "simulated check"})).collect::<Vec<_>>()
                    }),
                    vec![tool("run_check", Some("cargo_test"))],
                )
            }
            Role::QualityLead => {
                let qa_ok = input
                    .artifact(ArtifactKind::QaReport)
                    .and_then(|q| q["results"].as_array())
                    .is_some_and(|r| r.iter().all(|x| x["exit_code"] == 0));
                let rec = if qa_ok { "ready" } else { "changes_required" };
                out(
                    json!({"commit_sha": reported, "recommendation": rec, "summary": "Evidence evaluated (simulated).", "open_issues": []}),
                    vec![tool("read_artifacts", None)],
                )
            }
        }
    }
}

/// A full set of normal simulated agents with distinct identities.
pub fn team() -> Vec<SimAgent> {
    Role::ALL
        .into_iter()
        .map(|r| SimAgent::new(r, &format!("sim-{}", r.as_str()), Behavior::Normal))
        .collect()
}
