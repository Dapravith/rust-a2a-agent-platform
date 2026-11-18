//! Deterministic (rule-based, Mode A) agents for the other five roles, and
//! the one-process host for all six A2A agents (`/agents/<role>`).
//!
//! None of these call a model. Their outputs are honest about it (summaries,
//! `limitations`, `risks`), and every fact about code (SHAs, digests, changed
//! files, exit codes) comes from MCP tool results, which the orchestrator then
//! checks against the MCP audit.

use a2a::{AgentCard, AgentSkill};
use agent_runtime::{Skill, agent_card};
use contracts::AgentFailure;
use contracts::role::{
    DEVELOPER_AGENT_NAME, PROJECT_COORDINATOR_AGENT_NAME, PROJECT_MANAGER_AGENT_NAME,
    QA_TESTER_AGENT_NAME, QUALITY_LEAD_AGENT_NAME, ROLE_RESULT_ARTIFACT_NAME, RoleResult, RoleTask,
};
use engineering::artifacts::{
    AcceptanceCriterion, ChangedFile, CodeCandidate, CommandResult, CriterionResult,
    CriterionStatus, FileChange, QaEnvironment, QaReport, QualityAssessment, Recommendation,
    RequirementsPlan, ReviewConclusion, ReviewReport, WorkAssignment,
};
use serde_json::{Value, json};

use crate::{CodeReviewer, Mcp, PROVIDER, fail, tool_call_id};

fn card(name: &str, skill_id: &str, what: &str, public_url: &str) -> AgentCard {
    agent_card(
        name,
        &format!("{name} role agent (Phase 6B, deterministic Mode A)."),
        env!("CARGO_PKG_VERSION"),
        AgentSkill {
            id: skill_id.into(),
            name: name.into(),
            description: format!(
                "Input: RoleTask (application/json). {what} Output artifact `role_result`: \
                 {{content, tool_calls[]}}."
            ),
            tags: vec!["engineering".into()],
            examples: None,
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        },
        public_url,
    )
}

fn artifact<'a>(task: &'a RoleTask, kind: &str) -> Option<&'a Value> {
    task.artifacts
        .iter()
        .find(|a| a.kind == kind)
        .map(|a| &a.content)
}

fn decode<T: serde::de::DeserializeOwned>(task: &RoleTask, kind: &str) -> Result<T, AgentFailure> {
    let v = artifact(task, kind).ok_or_else(|| fail(format!("no `{kind}` in the task")))?;
    // Artifacts arrive through A2A, where numbers are doubles (see `normalize_numbers`).
    serde_json::from_value(contracts::role::normalize_numbers(v.clone()))
        .map_err(|e| fail(format!("bad `{kind}`: {e}")))
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, AgentFailure> {
    serde_json::to_value(v).map_err(|e| fail(e.to_string()))
}

fn expect_role(task: &RoleTask, role: &str) -> Result<(), AgentFailure> {
    if task.role == role {
        Ok(())
    } else {
        Err(fail(format!("this agent only serves the {role} role")))
    }
}

fn clip(s: &str, n: usize) -> String {
    let t = s.trim();
    let c: String = t.chars().take(n).collect();
    if t.chars().count() > n {
        format!("{c}…")
    } else {
        c
    }
}

fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

// ---------------------------------------------------------------------------
// Project Manager
// ---------------------------------------------------------------------------

pub struct ProjectManager {
    /// Path prefixes the planner proposes (the human approves or rejects them).
    pub scope: Vec<String>,
}

impl Skill for ProjectManager {
    type Input = RoleTask;
    type Output = RoleResult;
    const ARTIFACT_NAME: &'static str = ROLE_RESULT_ARTIFACT_NAME;
    fn card(&self, url: &str) -> AgentCard {
        card(
            PROJECT_MANAGER_AGENT_NAME,
            "plan-requirements",
            "Produces a requirements_plan.",
            url,
        )
    }
    fn inference_mode(&self) -> &'static str {
        PROVIDER
    }
    async fn run(&self, task: RoleTask) -> Result<RoleResult, AgentFailure> {
        expect_role(&task, "project_manager")?;
        let req = task.requirements.trim();
        if req.is_empty() {
            return Err(fail("empty requirements"));
        }
        let plan = RequirementsPlan {
            summary: clip(req, 300),
            deliverables: vec![format!("Change requested: {}", clip(req, 400))],
            acceptance_criteria: vec![
                AcceptanceCriterion {
                    id: "AC-1".into(),
                    description: clip(req, 900),
                },
                AcceptanceCriterion {
                    id: "AC-2".into(),
                    description: "Existing behaviour and tests keep passing (backward compatible)."
                        .into(),
                },
            ],
            dependencies: vec![],
            non_goals: vec![
                "Changes outside the approved scope.".into(),
                "Merging, deploying or approving the change (humans decide).".into(),
            ],
            risks: vec![
                "Deterministic planner: the plan restates the request; no model analysed it."
                    .into(),
            ],
            steps: vec![
                "Implement the change inside the approved scope.".into(),
                "Add or update tests for the change.".into(),
                "Run the required checks on the candidate.".into(),
            ],
            scope: self.scope.clone(),
        };
        Ok(RoleResult {
            content: to_value(&plan)?,
            tool_calls: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// Project Coordinator
// ---------------------------------------------------------------------------

pub struct ProjectCoordinator;

impl Skill for ProjectCoordinator {
    type Input = RoleTask;
    type Output = RoleResult;
    const ARTIFACT_NAME: &'static str = ROLE_RESULT_ARTIFACT_NAME;
    fn card(&self, url: &str) -> AgentCard {
        card(
            PROJECT_COORDINATOR_AGENT_NAME,
            "assign-work",
            "Produces a work_assignment from the approved plan.",
            url,
        )
    }
    fn inference_mode(&self) -> &'static str {
        PROVIDER
    }
    async fn run(&self, task: RoleTask) -> Result<RoleResult, AgentFailure> {
        expect_role(&task, "project_coordinator")?;
        let plan: RequirementsPlan = decode(&task, "requirements_plan")?;
        let base = task
            .base_commit_sha
            .clone()
            .ok_or_else(|| fail("no base commit supplied by the orchestrator"))?;
        let a = WorkAssignment {
            summary: format!("Implement the approved plan: {}", clip(&plan.summary, 300)),
            scope: plan.scope.clone(),
            acceptance_criteria: plan
                .acceptance_criteria
                .iter()
                .map(|c| c.id.clone())
                .collect(),
            required_checks: vec!["cargo_test".into()],
            base_commit_sha: base,
        };
        Ok(RoleResult {
            content: to_value(&a)?,
            tool_calls: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// Developer
// ---------------------------------------------------------------------------

/// Applies a patch configured at startup (in tests: the fixture's change)
/// through `workspace.apply_patch`. It writes nothing itself: the MCP server
/// applies, checks and commits, and its result is the only source of the facts
/// reported here.
pub struct Developer {
    pub mcp_url: String,
    pub patch: Option<String>,
}

fn change_kinds(patch: &str, files: &[String]) -> Vec<ChangedFile> {
    files
        .iter()
        .map(|f| {
            let header = format!("diff --git a/{f} b/{f}");
            let section: Vec<&str> = patch
                .split("\ndiff --git ")
                .find(|s| s.starts_with(&header) || format!("diff --git {s}").starts_with(&header))
                .map(|s| s.lines().take(4).collect())
                .unwrap_or_default();
            let change = if section.iter().any(|l| l.starts_with("new file mode")) {
                FileChange::Added
            } else if section.iter().any(|l| l.starts_with("deleted file mode")) {
                FileChange::Deleted
            } else {
                FileChange::Modified
            };
            ChangedFile {
                path: f.clone(),
                change,
            }
        })
        .collect()
}

impl Skill for Developer {
    type Input = RoleTask;
    type Output = RoleResult;
    const ARTIFACT_NAME: &'static str = ROLE_RESULT_ARTIFACT_NAME;
    fn card(&self, url: &str) -> AgentCard {
        card(
            DEVELOPER_AGENT_NAME,
            "implement-change",
            "Commits a candidate via MCP workspace.apply_patch; produces a code_candidate.",
            url,
        )
    }
    fn inference_mode(&self) -> &'static str {
        PROVIDER
    }
    async fn run(&self, task: RoleTask) -> Result<RoleResult, AgentFailure> {
        expect_role(&task, "developer")?;
        let patch = self
            .patch
            .as_deref()
            .ok_or_else(|| fail("no patch is configured for the deterministic developer"))?;
        let assignment: WorkAssignment = decode(&task, "work_assignment")?;
        let token = task
            .mcp_token
            .as_deref()
            .ok_or_else(|| fail("no MCP token in the task"))?;
        let mcp = Mcp::connect(&self.mcp_url, token).await?;
        let result = async {
            let r = mcp
                .call(
                    "workspace.apply_patch",
                    json!({"patch": patch, "message": clip(&assignment.summary, 180)}),
                )
                .await?;
            let id = tool_call_id(&r)?;
            let s = |k: &str| {
                r[k].as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| fail(format!("MCP result lacks `{k}`")))
            };
            let files: Vec<String> = r["changed_files"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let c = CodeCandidate {
                base_commit_sha: s("base_sha")?,
                commit_sha: s("head_sha")?,
                branch: s("branch")?,
                changed_files: change_kinds(patch, &files),
                patch_sha256: s("patch_sha256")?,
                checks: vec![],
                limitations: vec![
                    "Deterministic developer: applied a predefined patch; no model wrote it."
                        .into(),
                    "Checks are run by QA in the sandbox, not by the developer.".into(),
                ],
            };
            Ok(RoleResult {
                content: to_value(&c)?,
                tool_calls: vec![id],
            })
        }
        .await;
        mcp.close().await;
        result
    }
}

// ---------------------------------------------------------------------------
// QA Tester
// ---------------------------------------------------------------------------

pub struct QaTester {
    pub mcp_url: String,
}

impl Skill for QaTester {
    type Input = RoleTask;
    type Output = RoleResult;
    const ARTIFACT_NAME: &'static str = ROLE_RESULT_ARTIFACT_NAME;
    fn card(&self, url: &str) -> AgentCard {
        card(
            QA_TESTER_AGENT_NAME,
            "test-candidate",
            "Runs the assignment's allowlisted checks via MCP testing.run; produces a qa_report.",
            url,
        )
    }
    fn inference_mode(&self) -> &'static str {
        PROVIDER
    }
    async fn run(&self, task: RoleTask) -> Result<RoleResult, AgentFailure> {
        expect_role(&task, "qa_tester")?;
        let candidate = task
            .candidate_commit_sha
            .clone()
            .ok_or_else(|| fail("no candidate commit to test"))?;
        let assignment: WorkAssignment = decode(&task, "work_assignment")?;
        let token = task
            .mcp_token
            .as_deref()
            .ok_or_else(|| fail("no MCP token in the task"))?;
        let mcp = Mcp::connect(&self.mcp_url, token).await?;
        let result = async {
            let mut results = Vec::new();
            let mut ids = Vec::new();
            let mut evidence = Vec::new();
            let mut runner = String::from("unknown");
            for check in &assignment.required_checks {
                let r = mcp.call("testing.run", json!({"check_id": check})).await?;
                if r["head_sha"].as_str() != Some(candidate.as_str()) {
                    return Err(fail("check ran against a commit other than the candidate"));
                }
                let id = tool_call_id(&r)?;
                runner = r["runner"].as_str().unwrap_or("unknown").to_owned();
                // A killed or timed-out check has no exit code: it failed.
                let exit = r["exit_code"].as_i64().map_or(-1, |c| c as i32);
                let out = format!(
                    "{}\n{}",
                    r["stdout_tail"].as_str().unwrap_or_default(),
                    r["stderr_tail"].as_str().unwrap_or_default()
                );
                results.push(CommandResult {
                    check_id: check.clone(),
                    exit_code: exit,
                    duration_ms: r["duration_ms"].as_u64().unwrap_or(0),
                    output_tail: tail(&out, 7_000),
                });
                evidence.push(format!(
                    "{check}: exit {exit} (tool call {id}, runner {runner}, log sha256 {})",
                    r["log_sha256"].as_str().unwrap_or("?")
                ));
                ids.push(id);
            }
            let all_ok = !results.is_empty() && results.iter().all(|r| r.exit_code == 0);
            let report = QaReport {
                commit_sha: candidate.clone(),
                environment: QaEnvironment {
                    os: "linux (sandbox container)".into(),
                    toolchain: "rust toolchain of the digest-pinned sandbox image".into(),
                    sandbox: format!("{runner}: no network, read-only workspace, non-root"),
                },
                results,
                criteria: assignment
                    .acceptance_criteria
                    .iter()
                    .map(|c| CriterionResult {
                        criterion_id: c.clone(),
                        status: if all_ok {
                            CriterionStatus::Verified
                        } else {
                            CriterionStatus::Failed
                        },
                        evidence: format!(
                            "Deterministic QA: inferred from the required checks only. {}",
                            evidence.join("; ")
                        ),
                    })
                    .collect(),
            };
            Ok(RoleResult {
                content: to_value(&report)?,
                tool_calls: ids,
            })
        }
        .await;
        mcp.close().await;
        result
    }
}

// ---------------------------------------------------------------------------
// Quality Lead
// ---------------------------------------------------------------------------

pub struct QualityLead;

impl Skill for QualityLead {
    type Input = RoleTask;
    type Output = RoleResult;
    const ARTIFACT_NAME: &'static str = ROLE_RESULT_ARTIFACT_NAME;
    fn card(&self, url: &str) -> AgentCard {
        card(
            QUALITY_LEAD_AGENT_NAME,
            "assess-evidence",
            "Summarises the review and QA evidence; produces a quality_assessment (advice, never an approval).",
            url,
        )
    }
    fn inference_mode(&self) -> &'static str {
        PROVIDER
    }
    async fn run(&self, task: RoleTask) -> Result<RoleResult, AgentFailure> {
        expect_role(&task, "quality_lead")?;
        let candidate = task
            .candidate_commit_sha
            .clone()
            .ok_or_else(|| fail("no candidate commit to assess"))?;
        let review: ReviewReport = decode(&task, "review_report")?;
        let qa: QaReport = decode(&task, "qa_report")?;
        if review.commit_sha != candidate || qa.commit_sha != candidate {
            return Err(fail("evidence is not about the candidate commit"));
        }
        let mut issues: Vec<String> = review
            .findings
            .iter()
            .map(|f| format!("{:?} {}: {}", f.severity, f.file, f.title))
            .collect();
        issues.extend(
            qa.results
                .iter()
                .filter(|r| r.exit_code != 0)
                .map(|r| format!("check {} failed (exit {})", r.check_id, r.exit_code)),
        );
        let recommendation = if review.conclusion == ReviewConclusion::BlockingFindings {
            Recommendation::Blocked
        } else if qa.all_passed() {
            Recommendation::Ready
        } else {
            Recommendation::ChangesRequired
        };
        let a = QualityAssessment {
            commit_sha: candidate,
            recommendation,
            summary: format!(
                "Rule-based assessment (no model): review `{:?}` with {} finding(s); QA {} of {} check(s) passed. \
                 This is advice for the human approver, not an approval.",
                review.conclusion,
                review.findings.len(),
                qa.results.iter().filter(|r| r.exit_code == 0).count(),
                qa.results.len()
            ),
            open_issues: issues.into_iter().take(30).collect(),
        };
        Ok(RoleResult {
            content: to_value(&a)?,
            tool_calls: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// Hosting
// ---------------------------------------------------------------------------

pub struct AgentsConfig {
    /// The only MCP server these agents call.
    pub mcp_url: String,
    pub developer_patch: Option<String>,
    pub pm_scope: Vec<String>,
}

/// URL path of a role's agent: `/agents/<role with dashes>`.
pub fn agent_path(role: &str) -> String {
    format!("/agents/{}", role.replace('_', "-"))
}

/// All six role agents on one listener, each with its own Agent Card at
/// `<public_base>/agents/<role>/.well-known/agent-card.json`.
pub fn router(cfg: &AgentsConfig, public_base: &str) -> axum::Router {
    let b = public_base.trim_end_matches('/');
    let at = |role: &str| format!("{b}{}", agent_path(role));
    axum::Router::new()
        .nest(
            &agent_path("project_manager"),
            agent_runtime::router(
                ProjectManager {
                    scope: cfg.pm_scope.clone(),
                },
                &at("project_manager"),
            ),
        )
        .nest(
            &agent_path("project_coordinator"),
            agent_runtime::router(ProjectCoordinator, &at("project_coordinator")),
        )
        .nest(
            &agent_path("developer"),
            agent_runtime::router(
                Developer {
                    mcp_url: cfg.mcp_url.clone(),
                    patch: cfg.developer_patch.clone(),
                },
                &at("developer"),
            ),
        )
        .nest(
            &agent_path("code_reviewer"),
            agent_runtime::router(CodeReviewer::new(cfg.mcp_url.clone()), &at("code_reviewer")),
        )
        .nest(
            &agent_path("qa_tester"),
            agent_runtime::router(
                QaTester {
                    mcp_url: cfg.mcp_url.clone(),
                },
                &at("qa_tester"),
            ),
        )
        .nest(
            &agent_path("quality_lead"),
            agent_runtime::router(QualityLead, &at("quality_lead")),
        )
        .route("/healthz", axum::routing::get(|| async { "ok" }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn change_kinds_follow_the_patch_headers() {
        let p = "diff --git a/src/a.rs b/src/a.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/a.rs\n\
                 diff --git a/src/b.rs b/src/b.rs\nindex 1..2 100644\n--- a/src/b.rs\n+++ b/src/b.rs\n\
                 diff --git a/src/c.rs b/src/c.rs\ndeleted file mode 100644\n--- a/src/c.rs\n+++ /dev/null\n";
        let k: Vec<_> = change_kinds(
            p,
            &["src/a.rs".into(), "src/b.rs".into(), "src/c.rs".into()],
        )
        .into_iter()
        .map(|c| c.change)
        .collect();
        assert_eq!(
            k,
            [FileChange::Added, FileChange::Modified, FileChange::Deleted]
        );
    }

    #[test]
    fn agent_paths_are_kebab_case() {
        assert_eq!(agent_path("code_reviewer"), "/agents/code-reviewer");
    }
}
