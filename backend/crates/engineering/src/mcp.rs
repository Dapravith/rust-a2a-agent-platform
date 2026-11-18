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
}

impl McpTool {
    pub const ALL: [McpTool; 3] = [McpTool::ReadFile, McpTool::Diff, McpTool::RunTests];

    pub fn name(self) -> &'static str {
        match self {
            McpTool::ReadFile => "repository.read_file",
            McpTool::Diff => "repository.diff",
            McpTool::RunTests => "testing.run",
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
    ReadFile { path: String },
    Diff,
    RunTests { check_id: String },
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
            vec![ReadFile, Diff, RunTests]
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
        for t in [
            "workspace.apply_patch",
            "shell",
            "git.push",
            "approve_delivery",
        ] {
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
}
