//! Shared runtime for the Worker and Reviewer agents.
//!
//! - [`server`]: wraps one [`Skill`] in the official `a2a-rs` server
//!   (executor, Agent Card, JSON-RPC router, serve loop).
//! - [`llm`]: the single hosted LLM provider client (Anthropic Messages API).
//!
//! Agents never call each other; the API owns the sequence.

pub mod llm;
pub mod server;

pub use llm::{Generated, LlmClient, LlmConfig, LlmError, UsageBlock};
pub use server::{ServeConfig, Skill, agent_card, init_tracing, router, serve, shutdown_signal};
