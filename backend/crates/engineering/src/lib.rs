//! Phase 6 domain model for human-supervised multi-agent software engineering.
//!
//! Pure logic only (no I/O): the orchestrator in `apps/api` persists and
//! enforces what is decided here, so every rule is unit-testable and the same
//! rule is applied on every path.
//!
//! * [`roles`]: application-level role policies (not A2A primitives) and the
//!   tool allowlist. Agent tool requests are untrusted until authorized here.
//! * [`state`]: the workflow state machine. Human gates can only be passed by
//!   an [`state::Actor::Human`]; agents can never reach an approved state.
//! * [`artifacts`]: typed, versioned handoff contracts with integrity hashes.
//! * [`gates`]: mandatory quality gates checked before a delivery approval.

pub mod artifacts;
pub mod gates;
pub mod mcp;
pub mod roles;
pub mod state;

/// Validation failure of an agent- or human-supplied payload.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("payload does not match the `{kind}` schema: {reason}")]
    Schema { kind: &'static str, reason: String },
    #[error("invalid field `{field}`: {reason}")]
    Invalid { field: &'static str, reason: String },
}

pub(crate) fn invalid(field: &'static str, reason: impl Into<String>) -> ContractError {
    ContractError::Invalid {
        field,
        reason: reason.into(),
    }
}

/// Bounded, printable text (newlines/tabs allowed).
pub(crate) fn check_text(
    field: &'static str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<(), ContractError> {
    if value.trim().chars().count() < min {
        return Err(invalid(
            field,
            format!("must contain at least {min} characters"),
        ));
    }
    if value.chars().count() > max {
        return Err(invalid(field, format!("must be at most {max} characters")));
    }
    if value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
    {
        return Err(invalid(field, "must not contain control characters"));
    }
    Ok(())
}

pub(crate) fn check_len<T>(
    field: &'static str,
    items: &[T],
    min: usize,
    max: usize,
) -> Result<(), ContractError> {
    if items.len() < min || items.len() > max {
        return Err(invalid(field, format!("must have {min}..={max} items")));
    }
    Ok(())
}

/// A full, lowercase 40-hex Git object id (abbreviated SHAs are ambiguous
/// and are refused: approvals bind to an exact candidate).
pub fn validate_commit_sha(field: &'static str, sha: &str) -> Result<(), ContractError> {
    if sha.len() == 40
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(invalid(
            field,
            "must be a full 40-character lowercase hex commit SHA",
        ))
    }
}

/// A lowercase 64-hex SHA-256 digest.
pub fn validate_sha256(field: &'static str, h: &str) -> Result<(), ContractError> {
    if h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(invalid(field, "must be a lowercase hex SHA-256 digest"))
    }
}
