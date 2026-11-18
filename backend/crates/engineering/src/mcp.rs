//! MCP tool contracts and per-run capability tokens (Phase 6B, C1–C3).
//!
//! Pure logic only. The MCP server (`apps/mcp-server`) is the only process
//! that touches a workspace. It calls [`verify_token`] on every request and
//! [`authorize_call`] on every tool call. The authorization source is
//! [`crate::roles`]: a token can only *narrow* what a role may do, never widen it,
//! and nothing an agent or model sends can add a tool.
//!
//! Token format: `hex(canonical claims JSON) "." hex(HMAC-SHA256(key, that JSON))`.
//! The orchestrator issues it per agent run with a short expiry.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use crate::roles::{self, PolicyError, Role, Tool, ToolRequest};

/// The MCP tools this platform exposes. Closed set; new tools are a code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum McpTool {
    #[serde(rename = "repository.read_file")]
    ReadFile,
    #[serde(rename = "repository.diff")]
    Diff,
    #[serde(rename = "testing.run")]
    RunTests,
    #[serde(rename = "workspace.apply_patch")]
    ApplyPatch,
}

impl McpTool {
    pub const ALL: [McpTool; 4] = [
        McpTool::ReadFile,
        McpTool::Diff,
        McpTool::RunTests,
        McpTool::ApplyPatch,
    ];

    pub fn name(self) -> &'static str {
        match self {
            McpTool::ReadFile => "repository.read_file",
            McpTool::Diff => "repository.diff",
            McpTool::RunTests => "testing.run",
            McpTool::ApplyPatch => "workspace.apply_patch",
        }
    }

    pub fn parse(name: &str) -> Option<McpTool> {
        McpTool::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The role-policy capability this MCP tool exercises.
    pub fn capability(self) -> Tool {
        match self {
            McpTool::ReadFile | McpTool::Diff => Tool::ReadWorkspace,
            McpTool::RunTests => Tool::RunCheck,
            McpTool::ApplyPatch => Tool::WriteWorkspace,
        }
    }

    /// MCP tools a role may ever be granted (derived from [`Role::policy`]).
    pub fn allowed_for(role: Role) -> Vec<McpTool> {
        McpTool::ALL
            .into_iter()
            .filter(|t| role.policy().tools.contains(&t.capability()))
            .collect()
    }
}

/// Maximum token lifetime the server accepts, whatever the claims say.
pub const MAX_TOKEN_TTL_SECS: i64 = 15 * 60;
/// Minimum HMAC key length.
pub const MIN_KEY_BYTES: usize = 32;

/// What one agent run may do through MCP. Issued by the orchestrator only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityClaims {
    pub workflow_id: Uuid,
    pub run_id: Uuid,
    pub role: Role,
    pub agent_id: String,
    pub workspace_id: Uuid,
    /// Granted tools; must be a subset of [`McpTool::allowed_for`] the role.
    pub tools: Vec<McpTool>,
    /// Repo-relative path prefixes from the approved work assignment.
    pub scope: Vec<String>,
    /// The commit the diff is taken against.
    pub base_sha: String,
    /// Unix seconds.
    pub iat: i64,
    pub exp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("capability key must be at least {MIN_KEY_BYTES} bytes")]
    WeakKey,
    #[error("malformed capability token")]
    Malformed,
    #[error("capability token signature is invalid")]
    BadSignature,
    #[error("capability token is expired or not yet valid")]
    Expired,
    #[error("capability token lifetime exceeds the server maximum")]
    TtlTooLong,
    #[error("capability token grants a tool its role may not use: {0}")]
    OverGranted(&'static str),
    #[error("capability token claims are invalid: {0}")]
    InvalidClaims(String),
}

type HmacSha256 = Hmac<Sha256>;

fn mac(key: &[u8]) -> Result<HmacSha256, TokenError> {
    if key.len() < MIN_KEY_BYTES {
        return Err(TokenError::WeakKey);
    }
    HmacSha256::new_from_slice(key).map_err(|_| TokenError::WeakKey)
}

fn check_claims(c: &CapabilityClaims) -> Result<(), TokenError> {
    let allowed = McpTool::allowed_for(c.role);
    if let Some(t) = c.tools.iter().find(|t| !allowed.contains(t)) {
        return Err(TokenError::OverGranted(t.name()));
    }
    crate::validate_commit_sha("base_sha", &c.base_sha)
        .map_err(|e| TokenError::InvalidClaims(e.to_string()))?;
    if c.agent_id.trim().is_empty() || c.agent_id.len() > 128 {
        return Err(TokenError::InvalidClaims("agent_id".into()));
    }
    if c.exp - c.iat > MAX_TOKEN_TTL_SECS || c.exp <= c.iat {
        return Err(TokenError::TtlTooLong);
    }
    Ok(())
}

/// Issues a token. Refuses claims that over-grant the role.
pub fn issue_token(key: &[u8], claims: &CapabilityClaims) -> Result<String, TokenError> {
    check_claims(claims)?;
    let payload = serde_json::to_vec(claims).map_err(|_| TokenError::Malformed)?;
    let mut m = mac(key)?;
    m.update(&payload);
    let sig = m.finalize().into_bytes();
    Ok(format!("{}.{}", hex::encode(&payload), hex::encode(sig)))
}

/// Verifies signature, expiry and claim consistency. `now` is unix seconds.
pub fn verify_token(key: &[u8], token: &str, now: i64) -> Result<CapabilityClaims, TokenError> {
    if token.len() > 16 * 1024 {
        return Err(TokenError::Malformed);
    }
    let (p, s) = token.split_once('.').ok_or(TokenError::Malformed)?;
    let payload = hex::decode(p).map_err(|_| TokenError::Malformed)?;
    let sig = hex::decode(s).map_err(|_| TokenError::Malformed)?;
    let mut m = mac(key)?;
    m.update(&payload);
    // Constant-time comparison.
    m.verify_slice(&sig).map_err(|_| TokenError::BadSignature)?;
    let claims: CapabilityClaims =
        serde_json::from_slice(&payload).map_err(|_| TokenError::Malformed)?;
    check_claims(&claims)?;
    if now < claims.iat || now >= claims.exp {
        return Err(TokenError::Expired);
    }
    Ok(claims)
}

/// A validated, authorized tool call, ready for execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizedCall {
    ReadFile {
        path: String,
    },
    Diff,
    RunTests {
        check_id: String,
    },
    /// A git-format patch whose every path is inside the scope, committed on
    /// the workflow's candidate branch.
    ApplyPatch {
        patch: String,
        message: String,
        paths: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    #[error("tool `{0}` is not granted by this capability token")]
    NotGranted(&'static str),
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
}

impl CallError {
    /// Stable code for audit rows and tool error results.
    pub fn code(&self) -> &'static str {
        match self {
            CallError::UnknownTool(_) => "unknown_tool",
            CallError::NotGranted(_) => "tool_not_granted",
            CallError::Policy(PolicyError::ToolNotPermitted { .. }) => "tool_not_permitted",
            CallError::Policy(PolicyError::CheckNotAllowed(_)) => "check_not_allowed",
            CallError::Policy(PolicyError::PathOutOfScope(_)) => "path_out_of_scope",
            CallError::Policy(_) => "policy_violation",
            CallError::InvalidArguments(_) => "invalid_arguments",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    check_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyArgs {
    patch: String,
    message: String,
}

/// Largest patch `workspace.apply_patch` accepts.
pub const MAX_PATCH_BYTES: usize = 512 * 1024;

/// Paths a git-format patch touches. Refuses anything that is not a plain
/// text change of regular files: binary patches, symlink or submodule modes,
/// mode changes, renames/copies, quoted (escaped) paths, and non-git diffs.
/// The workspace re-checks the staged result after applying (defence in depth).
pub fn patch_paths(patch: &str) -> Result<Vec<String>, CallError> {
    let bad = |m: &str| CallError::InvalidArguments(format!("patch refused: {m}"));
    if patch.len() > MAX_PATCH_BYTES {
        return Err(bad("too large"));
    }
    let mut paths: Vec<String> = Vec::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if rest.contains('"') {
                return Err(bad("quoted paths"));
            }
            let (a, b) = rest
                .split_once(" b/")
                .ok_or_else(|| bad("malformed diff header"))?;
            let a = a
                .strip_prefix("a/")
                .ok_or_else(|| bad("malformed diff header"))?;
            if a != b || b.contains(' ') {
                return Err(bad("renames, copies or paths with spaces"));
            }
            paths.push(b.to_owned());
        } else if line.starts_with("GIT binary patch") || line.starts_with("Binary files ") {
            return Err(bad("binary content"));
        } else if line.starts_with("rename ")
            || line.starts_with("copy ")
            || line.starts_with("similarity index")
        {
            return Err(bad("renames or copies"));
        } else if line.starts_with("old mode ") || line.starts_with("new mode ") {
            return Err(bad("mode changes"));
        } else if let Some(m) = line
            .strip_prefix("new file mode ")
            .or_else(|| line.strip_prefix("deleted file mode "))
        {
            if m.trim() != "100644" {
                return Err(bad("only regular, non-executable files"));
            }
        } else if let Some(p) = line
            .strip_prefix("--- a/")
            .or_else(|| line.strip_prefix("+++ b/"))
        {
            // Must agree with the header (git apply trusts these lines).
            if paths.last().map(String::as_str) != Some(p.trim_end()) {
                return Err(bad("file header disagrees with diff header"));
            }
        }
    }
    if paths.is_empty() {
        return Err(bad("not a git-format patch (no `diff --git` header)"));
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn args<T: for<'de> Deserialize<'de>>(v: &serde_json::Value) -> Result<T, CallError> {
    serde_json::from_value(v.clone()).map_err(|e| CallError::InvalidArguments(e.to_string()))
}

/// Authorizes one call (tool name and raw JSON arguments, both untrusted)
/// against verified claims. Order: known tool → granted by token → role policy
/// (same function the orchestrator uses) → argument validation → path scope.
pub fn authorize_call(
    claims: &CapabilityClaims,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> Result<AuthorizedCall, CallError> {
    let tool = McpTool::parse(tool_name).ok_or_else(|| CallError::UnknownTool(tool_name.into()))?;
    if !claims.tools.contains(&tool) {
        return Err(CallError::NotGranted(tool.name()));
    }
    let cap_name = serde_json::to_value(tool.capability())
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    match tool {
        McpTool::ReadFile => {
            let a: ReadFileArgs = args(arguments)?;
            roles::authorize(
                claims.role,
                &ToolRequest {
                    tool: cap_name,
                    target: Some(a.path.clone()),
                },
                &claims.scope,
            )?;
            // Reads are scoped too: an agent sees only the assignment's paths.
            if !roles::path_in_scope(&a.path, &claims.scope) {
                return Err(PolicyError::PathOutOfScope(a.path).into());
            }
            Ok(AuthorizedCall::ReadFile { path: a.path })
        }
        McpTool::Diff => {
            let _: DiffArgs = args(arguments)?;
            roles::authorize(
                claims.role,
                &ToolRequest {
                    tool: cap_name,
                    target: None,
                },
                &claims.scope,
            )?;
            Ok(AuthorizedCall::Diff)
        }
        McpTool::RunTests => {
            let a: RunArgs = args(arguments)?;
            roles::authorize(
                claims.role,
                &ToolRequest {
                    tool: cap_name,
                    target: Some(a.check_id.clone()),
                },
                &claims.scope,
            )?;
            Ok(AuthorizedCall::RunTests {
                check_id: a.check_id,
            })
        }
        McpTool::ApplyPatch => {
            let a: ApplyArgs = args(arguments)?;
            let msg = a.message.trim();
            if msg.is_empty() || msg.chars().count() > 200 || msg.chars().any(char::is_control) {
                return Err(CallError::InvalidArguments(
                    "message must be 1..=200 printable characters on one line".into(),
                ));
            }
            let paths = patch_paths(&a.patch)?;
            // Same rule as everywhere else: every written path goes through the role policy.
            for p in &paths {
                roles::authorize(
                    claims.role,
                    &ToolRequest {
                        tool: cap_name.clone(),
                        target: Some(p.clone()),
                    },
                    &claims.scope,
                )?;
            }
            roles::authorize(
                claims.role,
                &ToolRequest {
                    tool: "commit_to_candidate_branch".into(),
                    target: None,
                },
                &claims.scope,
            )?;
            Ok(AuthorizedCall::ApplyPatch {
                patch: a.patch,
                message: msg.to_owned(),
                paths,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn claims(role: Role, tools: Vec<McpTool>) -> CapabilityClaims {
        CapabilityClaims {
            workflow_id: Uuid::nil(),
            run_id: Uuid::nil(),
            role,
            agent_id: "agent-1".into(),
            workspace_id: Uuid::nil(),
            tools,
            scope: vec!["src".into()],
            base_sha: SHA.into(),
            iat: 1000,
            exp: 1300,
        }
    }

    #[test]
    fn tool_matrix_follows_role_policy() {
        use McpTool::*;
        assert_eq!(
            McpTool::allowed_for(Role::Developer),
            vec![ReadFile, Diff, RunTests, ApplyPatch]
        );
        assert_eq!(
            McpTool::allowed_for(Role::CodeReviewer),
            vec![ReadFile, Diff]
        );
        assert_eq!(
            McpTool::allowed_for(Role::QaTester),
            vec![ReadFile, Diff, RunTests]
        );
        for r in [
            Role::ProjectManager,
            Role::ProjectCoordinator,
            Role::QualityLead,
        ] {
            assert!(McpTool::allowed_for(r).is_empty(), "{r:?}");
        }
    }

    #[test]
    fn token_roundtrip_and_tamper() {
        let c = claims(Role::CodeReviewer, vec![McpTool::Diff]);
        let t = issue_token(KEY, &c).unwrap();
        assert_eq!(verify_token(KEY, &t, 1100).unwrap(), c);
        // Wrong key, expired, not yet valid.
        assert_eq!(
            verify_token(b"ffffffffffffffffffffffffffffffff", &t, 1100),
            Err(TokenError::BadSignature)
        );
        assert_eq!(verify_token(KEY, &t, 1300), Err(TokenError::Expired));
        assert_eq!(verify_token(KEY, &t, 999), Err(TokenError::Expired));
        // Payload swapped for a widened one, old signature kept.
        let mut wide = c.clone();
        wide.tools.push(McpTool::RunTests);
        let forged = format!(
            "{}.{}",
            hex::encode(serde_json::to_vec(&wide).unwrap()),
            t.split_once('.').unwrap().1
        );
        assert_eq!(
            verify_token(KEY, &forged, 1100),
            Err(TokenError::BadSignature)
        );
        assert_eq!(
            verify_token(KEY, "garbage", 1100),
            Err(TokenError::Malformed)
        );
        assert_eq!(verify_token(KEY, "zz.zz", 1100), Err(TokenError::Malformed));
    }

    #[test]
    fn tokens_cannot_over_grant_or_live_long() {
        let c = claims(Role::CodeReviewer, vec![McpTool::RunTests]);
        assert_eq!(
            issue_token(KEY, &c),
            Err(TokenError::OverGranted("testing.run"))
        );
        let c = claims(Role::QualityLead, vec![McpTool::ReadFile]);
        assert!(matches!(
            issue_token(KEY, &c),
            Err(TokenError::OverGranted(_))
        ));
        let mut c = claims(Role::Developer, vec![]);
        c.exp = c.iat + MAX_TOKEN_TTL_SECS + 1;
        assert_eq!(issue_token(KEY, &c), Err(TokenError::TtlTooLong));
        assert_eq!(
            issue_token(b"short", &claims(Role::Developer, vec![])),
            Err(TokenError::WeakKey)
        );
    }

    #[test]
    fn calls_are_authorized_against_token_and_role() {
        let rev = claims(Role::CodeReviewer, vec![McpTool::ReadFile, McpTool::Diff]);
        assert_eq!(
            authorize_call(&rev, "repository.read_file", &json!({"path": "src/lib.rs"})),
            Ok(AuthorizedCall::ReadFile {
                path: "src/lib.rs".into()
            })
        );
        assert_eq!(
            authorize_call(&rev, "repository.diff", &json!({})),
            Ok(AuthorizedCall::Diff)
        );
        // Reviewer: no tests, no unknown tools.
        assert_eq!(
            authorize_call(&rev, "testing.run", &json!({"check_id": "cargo_test"}))
                .unwrap_err()
                .code(),
            "tool_not_granted"
        );
        // Reviewer: never a write, even with a well-formed patch.
        assert_eq!(
            authorize_call(
                &rev,
                "workspace.apply_patch",
                &json!({"patch": PATCH, "message": "x"})
            )
            .unwrap_err()
            .code(),
            "tool_not_granted"
        );
        for t in ["shell", "git.push", "approve_delivery"] {
            assert_eq!(
                authorize_call(&rev, t, &json!({})).unwrap_err().code(),
                "unknown_tool"
            );
        }
        // Read scope, traversal, .git, absolute.
        for p in [
            "Cargo.toml",
            "../secret",
            "/etc/passwd",
            ".git/config",
            "src/../Cargo.toml",
        ] {
            assert_eq!(
                authorize_call(&rev, "repository.read_file", &json!({"path": p}))
                    .unwrap_err()
                    .code(),
                "path_out_of_scope",
                "{p}"
            );
        }
        // Extra / missing arguments are refused, not ignored.
        assert_eq!(
            authorize_call(&rev, "repository.diff", &json!({"base": "HEAD~5"}))
                .unwrap_err()
                .code(),
            "invalid_arguments"
        );
        assert_eq!(
            authorize_call(&rev, "repository.read_file", &json!({}))
                .unwrap_err()
                .code(),
            "invalid_arguments"
        );
    }

    #[test]
    fn qa_runs_only_allowlisted_checks() {
        let qa = claims(Role::QaTester, vec![McpTool::RunTests]);
        assert_eq!(
            authorize_call(&qa, "testing.run", &json!({"check_id": "cargo_test"})),
            Ok(AuthorizedCall::RunTests {
                check_id: "cargo_test".into()
            })
        );
        for bad in ["rm -rf /", "cargo test; curl x", "bash"] {
            assert_eq!(
                authorize_call(&qa, "testing.run", &json!({"check_id": bad}))
                    .unwrap_err()
                    .code(),
                "check_not_allowed"
            );
        }
        assert_eq!(
            authorize_call(
                &qa,
                "testing.run",
                &json!({"check_id": "cargo_test", "argv": ["sh"]})
            )
            .unwrap_err()
            .code(),
            "invalid_arguments"
        );
    }

    const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 1..2 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-a\n+b\n";

    #[test]
    fn developer_patch_is_authorized_per_path() {
        let dev = claims(Role::Developer, McpTool::allowed_for(Role::Developer));
        assert_eq!(
            authorize_call(
                &dev,
                "workspace.apply_patch",
                &json!({"patch": PATCH, "message": "fix"})
            ),
            Ok(AuthorizedCall::ApplyPatch {
                patch: PATCH.into(),
                message: "fix".into(),
                paths: vec!["src/lib.rs".into()],
            })
        );
        let outside = PATCH.replace("src/lib.rs", "Cargo.toml");
        assert_eq!(
            authorize_call(
                &dev,
                "workspace.apply_patch",
                &json!({"patch": outside, "message": "x"})
            )
            .unwrap_err()
            .code(),
            "path_out_of_scope"
        );
        let git_dir = PATCH.replace("src/lib.rs", ".git/hooks/pre-commit");
        assert!(
            authorize_call(
                &dev,
                "workspace.apply_patch",
                &json!({"patch": git_dir, "message": "x"})
            )
            .is_err()
        );
        for m in ["", "a\nb", &"x".repeat(201)] {
            assert_eq!(
                authorize_call(
                    &dev,
                    "workspace.apply_patch",
                    &json!({"patch": PATCH, "message": m})
                )
                .unwrap_err()
                .code(),
                "invalid_arguments"
            );
        }
        // A QA token can never carry the write tool.
        assert!(issue_token(KEY, &claims(Role::QaTester, vec![McpTool::ApplyPatch])).is_err());
    }

    #[test]
    fn patch_parser_refuses_everything_but_plain_text_edits() {
        assert_eq!(patch_paths(PATCH).unwrap(), vec!["src/lib.rs".to_owned()]);
        let new_file = "diff --git a/src/n.rs b/src/n.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/n.rs\n@@ -0,0 +1 @@\n+x\n";
        assert_eq!(patch_paths(new_file).unwrap(), vec!["src/n.rs".to_owned()]);
        for (bad, why) in [
            (
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-a\n+b\n",
                "plain diff",
            ),
            (
                "diff --git a/src/l b/src/l\nnew file mode 120000\n",
                "symlink",
            ),
            (
                "diff --git a/src/m b/src/m\nnew file mode 160000\n",
                "submodule",
            ),
            (
                "diff --git a/src/x b/src/x\nnew file mode 100755\n",
                "executable",
            ),
            (
                "diff --git a/src/x b/src/x\nold mode 100644\nnew mode 100755\n",
                "chmod",
            ),
            (
                "diff --git a/src/a b/src/b\nrename from src/a\nrename to src/b\n",
                "rename",
            ),
            ("diff --git a/src/x b/src/x\nGIT binary patch\n", "binary"),
            ("diff --git \"a/src/x\" \"b/src/x\"\n", "quoted"),
            (
                "diff --git a/src/x b/src/x\n--- a/src/x\n+++ b/Cargo.toml\n",
                "header mismatch",
            ),
        ] {
            assert!(patch_paths(bad).is_err(), "{why}");
        }
        assert!(patch_paths(&"x".repeat(MAX_PATCH_BYTES + 1)).is_err());
    }
}
