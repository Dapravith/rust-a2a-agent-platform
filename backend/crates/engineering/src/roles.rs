//! Role policies. Roles are application policy, not A2A protocol concepts:
//! the A2A layer only carries messages, tasks and artifacts; *what* an agent
//! in a role may produce or request is decided here.
//!
//! No role has a tool that pushes, merges, approves, deploys, changes policy,
//! edits audit history or reads credentials: those capabilities do not exist
//! in [`Tool`], so they cannot be granted by configuration either.

use serde::{Deserialize, Serialize};

use crate::artifacts::ArtifactKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    ProjectManager,
    ProjectCoordinator,
    Developer,
    CodeReviewer,
    QaTester,
    QualityLead,
}

impl Role {
    pub const ALL: [Role; 6] = [
        Role::ProjectManager,
        Role::ProjectCoordinator,
        Role::Developer,
        Role::CodeReviewer,
        Role::QaTester,
        Role::QualityLead,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::ProjectManager => "project_manager",
            Role::ProjectCoordinator => "project_coordinator",
            Role::Developer => "developer",
            Role::CodeReviewer => "code_reviewer",
            Role::QaTester => "qa_tester",
            Role::QualityLead => "quality_lead",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// The one artifact kind this role may hand off.
    pub fn produces(self) -> ArtifactKind {
        match self {
            Role::ProjectManager => ArtifactKind::RequirementsPlan,
            Role::ProjectCoordinator => ArtifactKind::WorkAssignment,
            Role::Developer => ArtifactKind::CodeCandidate,
            Role::CodeReviewer => ArtifactKind::ReviewReport,
            Role::QaTester => ArtifactKind::QaReport,
            Role::QualityLead => ArtifactKind::QualityAssessment,
        }
    }

    pub fn policy(self) -> RolePolicy {
        use Tool::*;
        let tools: &'static [Tool] = match self {
            Role::ProjectManager | Role::ProjectCoordinator | Role::QualityLead => &[ReadArtifacts],
            Role::Developer => &[
                ReadArtifacts,
                ReadWorkspace,
                WriteWorkspace,
                RunCheck,
                CommitToCandidateBranch,
            ],
            Role::CodeReviewer => &[ReadArtifacts, ReadWorkspace],
            Role::QaTester => &[ReadArtifacts, ReadWorkspace, RunCheck],
        };
        RolePolicy {
            role: self,
            tools,
            produces: self.produces(),
        }
    }
}

/// Every capability an agent can request. Deliberately small and closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    /// Read artifacts of the same workflow.
    ReadArtifacts,
    /// Read the isolated workspace of the candidate.
    ReadWorkspace,
    /// Write files inside the isolated workspace (scoped paths only).
    WriteWorkspace,
    /// Run one command from the check allowlist ([`ALLOWED_CHECKS`]).
    RunCheck,
    /// Commit to the workflow's own `agent/<workflow-id>/...` branch, locally.
    CommitToCandidateBranch,
}

/// Commands an agent may run, by id. The orchestrator maps an id to a fixed
/// argv; an agent never supplies a shell string.
pub const ALLOWED_CHECKS: &[(&str, &[&str])] = &[
    (
        "cargo_fmt_check",
        &["cargo", "fmt", "--all", "--", "--check"],
    ),
    (
        "cargo_clippy",
        &[
            "cargo",
            "clippy",
            "--all-targets",
            "--offline",
            "--",
            "-D",
            "warnings",
        ],
    ),
    ("cargo_test", &["cargo", "test", "--offline"]),
];

pub fn check_argv(id: &str) -> Option<&'static [&'static str]> {
    ALLOWED_CHECKS
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, v)| *v)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RolePolicy {
    pub role: Role,
    pub tools: &'static [Tool],
    pub produces: ArtifactKind,
}

/// A tool request as an agent reports it (untrusted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequest {
    /// Free-form tool name from the agent; mapped to [`Tool`] or refused.
    pub tool: String,
    /// For `run_check`: the check id. For workspace tools: the path.
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("role `{role}` is not permitted to use tool `{tool}`")]
    ToolNotPermitted { role: &'static str, tool: String },
    #[error("check `{0}` is not on the allowlist")]
    CheckNotAllowed(String),
    #[error("path `{0}` is outside the authorized scope")]
    PathOutOfScope(String),
    #[error("branch `{0}` is protected or outside the workflow's candidate namespace")]
    ProtectedBranch(String),
    #[error("the reviewer must be a different agent from the developer of the candidate")]
    SelfReview,
}

/// Authorizes one tool request for `role` within `scope` (path prefixes
/// granted by the human-approved work assignment).
pub fn authorize(role: Role, req: &ToolRequest, scope: &[String]) -> Result<Tool, PolicyError> {
    let not_permitted = || PolicyError::ToolNotPermitted {
        role: role.as_str(),
        tool: req.tool.clone(),
    };
    let tool: Tool = serde_json::from_value(serde_json::Value::String(req.tool.clone()))
        .map_err(|_| not_permitted())?;
    if !role.policy().tools.contains(&tool) {
        return Err(not_permitted());
    }
    match tool {
        Tool::RunCheck => {
            let id = req.target.as_deref().unwrap_or_default();
            check_argv(id).ok_or_else(|| PolicyError::CheckNotAllowed(id.to_owned()))?;
        }
        Tool::WriteWorkspace => {
            let path = req.target.as_deref().unwrap_or_default();
            if !path_in_scope(path, scope) {
                return Err(PolicyError::PathOutOfScope(path.to_owned()));
            }
        }
        Tool::ReadWorkspace | Tool::ReadArtifacts | Tool::CommitToCandidateBranch => {}
    }
    Ok(tool)
}

/// Relative, normalized path under one of the scope prefixes.
pub fn path_in_scope(path: &str, scope: &[String]) -> bool {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        || path.split('/').next() == Some(".git")
    {
        return false;
    }
    scope.iter().any(|prefix| {
        let p = prefix.trim_end_matches('/');
        path == p || path.starts_with(&format!("{p}/"))
    })
}

/// Agents may only write the workflow's own candidate namespace.
pub fn candidate_branch(workflow_id: uuid::Uuid) -> String {
    format!("agent/{workflow_id}/candidate")
}

pub fn authorize_branch(workflow_id: uuid::Uuid, branch: &str) -> Result<(), PolicyError> {
    if branch == candidate_branch(workflow_id) {
        Ok(())
    } else {
        Err(PolicyError::ProtectedBranch(branch.to_owned()))
    }
}

/// Separation of duties: the agent that reviews (or tests) a candidate must
/// not be the agent instance that produced it.
pub fn ensure_independent(developer_agent: &str, reviewer_agent: &str) -> Result<(), PolicyError> {
    if developer_agent.eq_ignore_ascii_case(reviewer_agent) {
        Err(PolicyError::SelfReview)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn req(tool: &str, target: Option<&str>) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            target: target.map(Into::into),
        }
    }

    #[test]
    fn no_role_can_request_privileged_tools() {
        for role in Role::ALL {
            for t in [
                "git_push",
                "merge",
                "approve_delivery",
                "deploy",
                "read_secret",
                "shell",
                "edit_audit",
            ] {
                assert!(matches!(
                    authorize(role, &req(t, None), &[]),
                    Err(PolicyError::ToolNotPermitted { .. })
                ));
            }
        }
    }

    #[test]
    fn reviewer_cannot_write_or_commit() {
        let scope = vec!["src".to_owned()];
        assert!(
            authorize(
                Role::CodeReviewer,
                &req("write_workspace", Some("src/a.rs")),
                &scope
            )
            .is_err()
        );
        assert!(
            authorize(
                Role::CodeReviewer,
                &req("commit_to_candidate_branch", None),
                &scope
            )
            .is_err()
        );
        assert!(authorize(Role::CodeReviewer, &req("read_workspace", None), &scope).is_ok());
    }

    #[test]
    fn developer_writes_only_inside_scope() {
        let scope = vec!["apps/api/src/routes".to_owned()];
        let ok = |p| authorize(Role::Developer, &req("write_workspace", Some(p)), &scope);
        assert!(ok("apps/api/src/routes/x.rs").is_ok());
        for bad in [
            "apps/api/src/db.rs",
            "../etc/passwd",
            "/etc/passwd",
            "apps/api/src/routes/../../db.rs",
            ".git/config",
            "apps/api/src/routesX/a.rs",
        ] {
            assert_eq!(
                ok(bad),
                Err(PolicyError::PathOutOfScope(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn checks_are_allowlisted_by_id() {
        assert!(authorize(Role::QaTester, &req("run_check", Some("cargo_test")), &[]).is_ok());
        assert_eq!(
            authorize(Role::QaTester, &req("run_check", Some("rm -rf /")), &[]),
            Err(PolicyError::CheckNotAllowed("rm -rf /".into()))
        );
    }

    #[test]
    fn protected_branches_are_refused() {
        let id = uuid::Uuid::nil();
        assert!(authorize_branch(id, &candidate_branch(id)).is_ok());
        for b in [
            "main",
            "refs/heads/main",
            "release/1",
            "agent/other/candidate",
        ] {
            assert!(authorize_branch(id, b).is_err(), "{b}");
        }
    }

    #[test]
    fn self_review_is_refused() {
        assert_eq!(
            ensure_independent("dev-1", "DEV-1"),
            Err(PolicyError::SelfReview)
        );
        assert!(ensure_independent("dev-1", "rev-1").is_ok());
    }
}
