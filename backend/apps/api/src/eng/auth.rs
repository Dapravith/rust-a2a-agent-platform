//! Human identity for engineering workflows.
//!
//! The approver identity always comes from an [`Authenticator`] that verifies
//! a credential server-side; request bodies have no identity field at all
//! (`deny_unknown_fields`), so a browser cannot assert who it is.
//!
//! No identity provider is integrated yet. The only production
//! implementation is [`Disabled`], which refuses every request: submissions
//! and approvals are unavailable (reported as BLOCKED) until an IdP-backed
//! authenticator (e.g. OIDC JWT verification against the issuer's JWKS) is
//! configured. Tests inject their own authenticator in-process.

use axum::http::HeaderMap;
use engineering::state::{Actor, HumanRole};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Stable subject id from the identity provider.
    pub subject: String,
    /// Issuer that vouched for `subject`.
    pub issuer: String,
    /// Roles from the IdP's claims / group mapping, never from the request.
    pub roles: Vec<HumanRole>,
}

impl Principal {
    pub fn actor(&self) -> Actor {
        Actor::Human {
            id: self.subject.clone(),
            roles: self.roles.clone(),
        }
    }
    pub fn has(&self, r: HumanRole) -> bool {
        self.roles.contains(&r)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("no identity provider is configured; human approvals are disabled")]
    NotConfigured,
    #[error("authentication required")]
    Missing,
    #[error("invalid credentials")]
    Invalid,
}

pub trait Authenticator: Send + Sync {
    fn configured(&self) -> bool;
    fn authenticate(&self, headers: &HeaderMap) -> Result<Principal, AuthError>;
}

/// Default: no IdP, so nobody can submit or decide.
pub struct Disabled;

impl Authenticator for Disabled {
    fn configured(&self) -> bool {
        false
    }
    fn authenticate(&self, _: &HeaderMap) -> Result<Principal, AuthError> {
        Err(AuthError::NotConfigured)
    }
}
