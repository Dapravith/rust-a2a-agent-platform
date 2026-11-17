//! The one hosted LLM provider (Anthropic Messages API), called with
//! `reqwest` because Rust has no official Anthropic SDK.
//!
//! Request and response bodies are typed. Output is treated as untrusted:
//! structured JSON output is requested, then the body is size-bounded,
//! parsed, decoded with unknown fields rejected, and validated against the
//! application contract.
//!
//! Retries: only when the provider certainly did not process the request
//! (connection never established, 429, 503, 529). A read timeout or another
//! 5xx is uncertain and is reported, never retried.

use std::fmt;
use std::time::Duration;

use contracts::{AgentFailure, AgentFailureCode, Validate};
use reqwest::StatusCode;
use reqwest::header::HeaderValue;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ANTHROPIC_VERSION: &str = "2026-10-21";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Largest provider response body read into memory.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_ATTEMPTS: u32 = 3;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);
const MAX_PROVIDER_MESSAGE_CHARS: usize = 300;
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM provider unreachable: {0}")]
    Transport(String),
    #[error("LLM provider did not respond within the per-attempt timeout")]
    Timeout,
    #[error("LLM provider is rate limiting or overloaded (HTTP {status}): {message}")]
    RateLimited { status: u16, message: String },
    #[error("LLM provider returned HTTP {status}: {message}")]
    Provider { status: u16, message: String },
    #[error("LLM declined the request (stop_reason=refusal)")]
    Refused,
    #[error("LLM output was truncated (stop_reason=max_tokens)")]
    Truncated,
    #[error("LLM response exceeded {0} bytes")]
    ResponseTooLarge(usize),
    #[error("LLM output was not valid for the contract: {0}")]
    InvalidOutput(String),
    #[error("LLM did not answer within the overall deadline of {0:?}")]
    Deadline(Duration),
}

impl LlmError {
    /// Stable code reported to the orchestrator.
    pub fn code(&self) -> AgentFailureCode {
        match self {
            Self::Transport(_) => AgentFailureCode::LlmUnavailable,
            Self::Timeout | Self::Deadline(_) => AgentFailureCode::LlmTimeout,
            Self::RateLimited { .. } => AgentFailureCode::LlmRateLimited,
            Self::Provider { .. } => AgentFailureCode::LlmProviderError,
            Self::Refused => AgentFailureCode::LlmRefused,
            Self::Truncated => AgentFailureCode::LlmOutputTruncated,
            Self::ResponseTooLarge(_) | Self::InvalidOutput(_) => {
                AgentFailureCode::LlmOutputInvalid
            }
        }
    }

    pub fn into_failure(self) -> AgentFailure {
        AgentFailure::new(self.code(), self.to_string())
    }
}

#[derive(Clone)]
pub struct LlmConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    /// Per HTTP attempt.
    pub timeout: Duration,
    /// Across all attempts and backoff. Keep it below the API's
    /// `AGENT_TIMEOUT_SECS` so an agent never works past the caller's deadline.
    pub deadline: Duration,
    pub max_tokens: u32,
    pub effort: String,
    /// Opt into server-side refusal fallbacks (`fallbacks: "default"`).
    pub refusal_fallback: bool,
}

/// Never print the API key.
impl fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmConfig")
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .field("deadline", &self.deadline)
            .field("max_tokens", &self.max_tokens)
            .field("effort", &self.effort)
            .field("refusal_fallback", &self.refusal_fallback)
            .finish()
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> Result<T, String> {
    match env(key) {
        Some(v) => v.parse().map_err(|_| format!("{key} has an invalid value")),
        None => Ok(default),
    }
}

impl LlmConfig {
    /// `INFERENCE_MODE=mock` (default) → `None`; `anthropic` → validated
    /// config. There is no automatic fallback between the two.
    pub fn from_env() -> Result<Option<Self>, String> {
        let mode = env("INFERENCE_MODE").unwrap_or_else(|| "mock".into());
        match mode.as_str() {
            "mock" => {
                if env("ANTHROPIC_API_KEY").is_some() {
                    tracing::warn!(
                        "ANTHROPIC_API_KEY is set but INFERENCE_MODE=mock; the key is not used"
                    );
                }
                Ok(None)
            }
            "anthropic" => {
                let cfg = Self {
                    api_key: env("ANTHROPIC_API_KEY")
                        .ok_or("INFERENCE_MODE=anthropic requires ANTHROPIC_API_KEY")?,
                    model: env("ANTHROPIC_MODEL").unwrap_or_else(|| DEFAULT_MODEL.into()),
                    base_url: env("ANTHROPIC_BASE_URL")
                        .unwrap_or_else(|| "https://api.anthropic.com".into()),
                    timeout: Duration::from_secs(env_parse("LLM_TIMEOUT_SECS", 120)?),
                    deadline: Duration::from_secs(env_parse("LLM_DEADLINE_SECS", 200)?),
                    max_tokens: env_parse("LLM_MAX_TOKENS", 16_000)?,
                    effort: env("LLM_EFFORT").unwrap_or_else(|| "medium".into()),
                    refusal_fallback: env("ANTHROPIC_REFUSAL_FALLBACK").is_none_or(|v| v != "off"),
                };
                cfg.validate()?;
                Ok(Some(cfg))
            }
            other => Err(format!(
                "INFERENCE_MODE must be `mock` or `anthropic`, got `{other}`"
            )),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.timeout.is_zero() || self.deadline < self.timeout {
            return Err("LLM_TIMEOUT_SECS must be > 0 and not exceed LLM_DEADLINE_SECS".into());
        }
        if !(1..=128_000).contains(&self.max_tokens) {
            return Err("LLM_MAX_TOKENS must be between 1 and 128000".into());
        }
        if !EFFORTS.contains(&self.effort.as_str()) {
            return Err(format!("LLM_EFFORT must be one of {EFFORTS:?}"));
        }
        if self.model.trim().is_empty() {
            return Err("ANTHROPIC_MODEL must not be empty".into());
        }
        let url =
            url::Url::parse(&self.base_url).map_err(|_| "ANTHROPIC_BASE_URL is not a valid URL")?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        // The API key travels in a header: never in clear text off-host.
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err("ANTHROPIC_BASE_URL must use https (http only for loopback)".into());
        }
        HeaderValue::from_str(&self.api_key)
            .map_err(|_| "ANTHROPIC_API_KEY contains invalid characters")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Typed wire contracts (Messages API)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MessagesRequest<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub system: &'a str,
    pub messages: [InputMessage<'a>; 1],
    pub output_config: OutputConfig<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallbacks: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct InputMessage<'a> {
    pub role: &'static str,
    pub content: &'a str,
}

#[derive(Debug, Serialize)]
pub struct OutputConfig<'a> {
    pub effort: &'a str,
    pub format: OutputFormat<'a>,
}

#[derive(Debug, Serialize)]
pub struct OutputFormat<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub schema: &'a Value,
}

/// The fields of a Messages response this client relies on. Unknown fields
/// and block types are ignored (e.g. `thinking`, `fallback`).
#[derive(Debug, Deserialize)]
pub struct MessagesResponse {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    message: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    cfg: LlmConfig,
    api_key: HeaderValue,
}

impl LlmClient {
    pub fn new(cfg: LlmConfig) -> Result<Self, String> {
        cfg.validate()?;
        let mut api_key = HeaderValue::from_str(&cfg.api_key)
            .map_err(|_| "ANTHROPIC_API_KEY contains invalid characters")?;
        // Redacted from reqwest/hyper debug output.
        api_key.set_sensitive(true);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10).min(cfg.timeout))
            .timeout(cfg.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self { http, cfg, api_key })
    }

    /// Reads [`LlmConfig::from_env`]; `None` means mock mode.
    pub fn from_env() -> Result<Option<Self>, String> {
        LlmConfig::from_env()?.map(Self::new).transpose()
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    /// The typed request body (exposed for tests).
    pub fn request<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
        schema: &'a Value,
    ) -> MessagesRequest<'a> {
        MessagesRequest {
            model: &self.cfg.model,
            max_tokens: self.cfg.max_tokens,
            system,
            messages: [InputMessage {
                role: "user",
                content: user,
            }],
            output_config: OutputConfig {
                effort: &self.cfg.effort,
                format: OutputFormat {
                    kind: "json_schema",
                    schema,
                },
            },
            fallbacks: self.cfg.refusal_fallback.then_some("default"),
        }
    }

    /// One structured-output request, bounded by [`LlmConfig::deadline`].
    pub async fn generate<T: DeserializeOwned + Validate>(
        &self,
        system: &str,
        user: &str,
        schema: Value,
    ) -> Result<T, LlmError> {
        tokio::time::timeout(
            self.cfg.deadline,
            self.generate_with_retries(system, user, &schema),
        )
        .await
        .unwrap_or(Err(LlmError::Deadline(self.cfg.deadline)))
    }

    /// Removes the API key from any text that may leave this process.
    fn scrub(&self, text: &str) -> String {
        if self.cfg.api_key.len() >= 8 {
            text.replace(&self.cfg.api_key, "<redacted>")
        } else {
            text.to_owned()
        }
    }

    fn transport_error(&self, e: reqwest::Error) -> LlmError {
        if e.is_timeout() {
            LlmError::Timeout
        } else {
            // `without_url` keeps request details out of the message.
            LlmError::Transport(self.scrub(&e.without_url().to_string()))
        }
    }

    async fn generate_with_retries<T: DeserializeOwned + Validate>(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
    ) -> Result<T, LlmError> {
        let body = self.request(system, user, schema);
        let url = format!("{}/v1/messages", self.cfg.base_url.trim_end_matches('/'));

        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut req = self
                .http
                .post(&url)
                .header("x-api-key", self.api_key.clone())
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body);
            if self.cfg.refusal_fallback {
                req = req.header("anthropic-beta", FALLBACK_BETA);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                // Connection never established: the request was not sent.
                Err(e) if e.is_connect() && attempt < MAX_ATTEMPTS => {
                    tracing::warn!(attempt, "LLM connection failed, retrying");
                    tokio::time::sleep(backoff(attempt, None)).await;
                    continue;
                }
                Err(e) => return Err(self.transport_error(e)),
            };
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let bytes = read_limited(resp, MAX_RESPONSE_BYTES)
                .await
                .map_err(|e| match e {
                    ReadError::TooLarge => LlmError::ResponseTooLarge(MAX_RESPONSE_BYTES),
                    ReadError::Http(e) => self.transport_error(e),
                })?;

            if status.is_success() {
                let parsed: MessagesResponse = serde_json::from_slice(&bytes).map_err(|e| {
                    LlmError::InvalidOutput(format!("malformed response body: {e}"))
                })?;
                return parse_message_response(parsed);
            }

            let message = self.provider_message(&bytes);
            if not_processed(status) {
                if attempt < MAX_ATTEMPTS {
                    tracing::warn!(status = status.as_u16(), attempt, "LLM retryable status");
                    tokio::time::sleep(backoff(attempt, retry_after)).await;
                    continue;
                }
                return Err(LlmError::RateLimited {
                    status: status.as_u16(),
                    message,
                });
            }
            return Err(LlmError::Provider {
                status: status.as_u16(),
                message,
            });
        }
    }

    fn provider_message(&self, body: &[u8]) -> String {
        let raw = serde_json::from_slice::<ErrorEnvelope>(body)
            .map(|e| e.error.message)
            .unwrap_or_else(|_| "unknown error".into());
        self.scrub(&raw)
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_PROVIDER_MESSAGE_CHARS)
            .collect()
    }
}

/// Statuses that mean the provider did not run the request.
fn not_processed(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 503 | 529)
}

fn backoff(attempt: u32, retry_after: Option<Duration>) -> Duration {
    let base = Duration::from_millis(500 * u64::from(attempt));
    retry_after.map_or(base, |r| r.min(MAX_RETRY_AFTER).max(base))
}

enum ReadError {
    TooLarge,
    Http(reqwest::Error),
}

/// Reads at most `limit` bytes of body; larger bodies are rejected without
/// buffering them.
async fn read_limited(mut resp: reqwest::Response, limit: usize) -> Result<Vec<u8>, ReadError> {
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        return Err(ReadError::TooLarge);
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(ReadError::Http)? {
        if buf.len() + chunk.len() > limit {
            return Err(ReadError::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Extracts and validates the structured JSON text of a Messages response.
pub fn parse_message_response<T: DeserializeOwned + Validate>(
    resp: MessagesResponse,
) -> Result<T, LlmError> {
    match resp.stop_reason.as_deref() {
        Some("refusal") => return Err(LlmError::Refused),
        Some("max_tokens") => return Err(LlmError::Truncated),
        _ => {}
    }
    let text: String = resp
        .content
        .into_iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text),
            ContentBlock::Other => None,
        })
        .collect();
    if text.trim().is_empty() {
        return Err(LlmError::InvalidOutput("response had no text block".into()));
    }
    let decoded: T =
        serde_json::from_str(&text).map_err(|e| LlmError::InvalidOutput(e.to_string()))?;
    decoded
        .validate()
        .map_err(|e| LlmError::InvalidOutput(e.to_string()))?;
    Ok(decoded)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use contracts::{ReviewArtifact, Verdict};
    use serde_json::json;

    pub(crate) fn config(base_url: &str) -> LlmConfig {
        LlmConfig {
            api_key: "test-key-0123456789".into(),
            model: DEFAULT_MODEL.into(),
            base_url: base_url.into(),
            timeout: Duration::from_secs(5),
            deadline: Duration::from_secs(10),
            max_tokens: 16_000,
            effort: "medium".into(),
            refusal_fallback: true,
        }
    }

    fn resp(v: Value) -> MessagesResponse {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn parses_structured_response_and_rejects_bad_ones() {
        let ok = resp(json!({
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "{\"verdict\":\"pass\",\"summary\":\"Fine\",\"findings\":[]}"}
            ]
        }));
        let r: ReviewArtifact = parse_message_response(ok).unwrap();
        assert_eq!(r.verdict, Verdict::Pass);

        let refusal = resp(json!({"stop_reason": "refusal", "content": []}));
        assert!(matches!(
            parse_message_response::<ReviewArtifact>(refusal),
            Err(LlmError::Refused)
        ));
        let truncated = resp(json!({"stop_reason": "max_tokens", "content": [
            {"type":"text","text":"{\"verdict\":"}]}));
        assert!(matches!(
            parse_message_response::<ReviewArtifact>(truncated),
            Err(LlmError::Truncated)
        ));
        let garbage = resp(
            json!({"stop_reason": "end_turn", "content": [{"type":"text","text":"not json"}]}),
        );
        assert!(matches!(
            parse_message_response::<ReviewArtifact>(garbage),
            Err(LlmError::InvalidOutput(_))
        ));
        let bad_verdict = resp(
            json!({"stop_reason": "end_turn", "content": [{"type":"text",
            "text":"{\"verdict\":\"needs_changes\",\"summary\":\"x\",\"findings\":[]}"}]}),
        );
        assert!(matches!(
            parse_message_response::<ReviewArtifact>(bad_verdict),
            Err(LlmError::InvalidOutput(_))
        ));
        let extra_field = resp(
            json!({"stop_reason": "end_turn", "content": [{"type":"text",
            "text":"{\"verdict\":\"pass\",\"summary\":\"x\",\"findings\":[],\"run\":\"rm -rf /\"}"}]}),
        );
        assert!(matches!(
            parse_message_response::<ReviewArtifact>(extra_field),
            Err(LlmError::InvalidOutput(_))
        ));
    }

    #[test]
    fn request_body_shape() {
        let c = LlmClient::new(config("http://127.0.0.1:1")).unwrap();
        let schema = json!({"type": "object"});
        let b = serde_json::to_value(c.request("sys", "user", &schema)).unwrap();
        assert_eq!(b["model"], DEFAULT_MODEL);
        assert_eq!(b["messages"][0]["role"], "user");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(b["output_config"]["effort"], "medium");
        assert_eq!(b["fallbacks"], "default");
        for absent in ["thinking", "temperature", "top_p", "top_k"] {
            assert!(b.get(absent).is_none(), "{absent} must not be sent");
        }
        assert!(!b.to_string().contains("test-key"));
    }

    #[test]
    fn config_validation_and_redaction() {
        let mut c = config("https://api.anthropic.com");
        assert!(c.validate().is_ok());
        assert!(!format!("{c:?}").contains("test-key"));
        c.base_url = "http://api.example.com".into();
        assert!(c.validate().is_err(), "plain http off-host must be refused");
        let mut c = config("https://api.anthropic.com");
        c.effort = "turbo".into();
        assert!(c.validate().is_err());
        let mut c = config("https://api.anthropic.com");
        c.max_tokens = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(LlmError::Timeout.code().as_str(), "llm_timeout");
        assert_eq!(
            LlmError::RateLimited {
                status: 429,
                message: String::new()
            }
            .code()
            .as_str(),
            "llm_rate_limited"
        );
        assert_eq!(LlmError::Refused.code().as_str(), "llm_refused");
        assert_eq!(
            LlmError::ResponseTooLarge(1).code().as_str(),
            "llm_output_invalid"
        );
        assert_eq!(backoff(1, Some(Duration::from_secs(99))), MAX_RETRY_AFTER);
        assert_eq!(backoff(2, None), Duration::from_millis(1000));
    }
}
