//! Phase 6: human-supervised multi-agent engineering workflows.
//!
//! Additive to the P0 Worker -> Reviewer workflow (`crate::engine`), which is
//! untouched. Pure rules live in the `engineering` crate; this module
//! persists them ([`store`]), runs agent stages ([`orchestrator`]), records
//! human decisions ([`approval`]) and exposes `/api/eng` ([`routes`]).

pub mod agents;
pub mod approval;
pub mod auth;
pub mod orchestrator;
pub mod providers;
pub mod routes;
pub mod sim;
pub mod store;
