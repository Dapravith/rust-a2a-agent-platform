//! Stable workflow / agent-run failure codes. Persisted in `error_code` and
//! returned by the REST API; clients and alerts key on these, never on the
//! free-text `error`. The database CHECK constraints list the same values
//! (migration `20261117000001_error_codes.sql`; a test inserts every variant).

use contracts::AgentFailureCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    AgentUnreachable,
    AgentCardInvalid,
    AgentUntrusted,
    AgentProtocolError,
    AgentTimeout,
    AgentRejected,
    AgentCanceled,
    AgentFailed,
    AgentUnsupportedInteraction,
    ArtifactInvalid,
    LlmTimeout,
    LlmRateLimited,
    LlmProviderError,
    LlmUnavailable,
    LlmRefused,
    LlmOutputTruncated,
    LlmOutputInvalid,
    PersistenceFailed,
    InternalError,
    Interrupted,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 20] = [
        Self::AgentUnreachable,
        Self::AgentCardInvalid,
        Self::AgentUntrusted,
        Self::AgentProtocolError,
        Self::AgentTimeout,
        Self::AgentRejected,
        Self::AgentCanceled,
        Self::AgentFailed,
        Self::AgentUnsupportedInteraction,
        Self::ArtifactInvalid,
        Self::LlmTimeout,
        Self::LlmRateLimited,
        Self::LlmProviderError,
        Self::LlmUnavailable,
        Self::LlmRefused,
        Self::LlmOutputTruncated,
        Self::LlmOutputInvalid,
        Self::PersistenceFailed,
        Self::InternalError,
        Self::Interrupted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentUnreachable => "agent_unreachable",
            Self::AgentCardInvalid => "agent_card_invalid",
            Self::AgentUntrusted => "agent_untrusted",
            Self::AgentProtocolError => "agent_protocol_error",
            Self::AgentTimeout => "agent_timeout",
            Self::AgentRejected => "agent_rejected",
            Self::AgentCanceled => "agent_canceled",
            Self::AgentFailed => "agent_failed",
            Self::AgentUnsupportedInteraction => "agent_unsupported_interaction",
            Self::ArtifactInvalid => "artifact_invalid",
            Self::LlmTimeout => "llm_timeout",
            Self::LlmRateLimited => "llm_rate_limited",
            Self::LlmProviderError => "llm_provider_error",
            Self::LlmUnavailable => "llm_unavailable",
            Self::LlmRefused => "llm_refused",
            Self::LlmOutputTruncated => "llm_output_truncated",
            Self::LlmOutputInvalid => "llm_output_invalid",
            Self::PersistenceFailed => "persistence_failed",
            Self::InternalError => "internal_error",
            Self::Interrupted => "interrupted",
        }
    }

    /// Maps the code an agent reported in a `failed` task. The agent only
    /// picks a label from a closed set; `internal` stays generic.
    pub fn from_agent(code: AgentFailureCode) -> Self {
        match code {
            AgentFailureCode::LlmTimeout => Self::LlmTimeout,
            AgentFailureCode::LlmRateLimited => Self::LlmRateLimited,
            AgentFailureCode::LlmProviderError => Self::LlmProviderError,
            AgentFailureCode::LlmUnavailable => Self::LlmUnavailable,
            AgentFailureCode::LlmRefused => Self::LlmRefused,
            AgentFailureCode::LlmOutputTruncated => Self::LlmOutputTruncated,
            AgentFailureCode::LlmOutputInvalid => Self::LlmOutputInvalid,
            AgentFailureCode::Internal => Self::AgentFailed,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
