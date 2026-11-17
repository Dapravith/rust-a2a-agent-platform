//! The one hosted LLM provider (Anthropic Messages API), called with
//! `reqwest` because Rust has no official Anthropic SDK.
//!
//! Output is treated as untrusted: structured JSON output is requested, then
//! parsed, decoded with unknown fields rejected, and validated.

use std::time::Duration;

use contracts::Validate;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const ANTHROPIC_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM request failed: {0}")]
    Transport(String),
    #[error("LLM provider returned HTTP {status}: {message}")]
    Provider { status: u16, message: String },
    #[error("LLM declined the request (stop_reason={0})")]
    Declined(String),
    #[error("LLM output was not valid for the contract: {0}")]
    InvalidOutput(String),
    #[error("LLM did not answer within the overall deadline of {0:?}")]
    Deadline(Duration),
}

#[derive(Debug, Clone)]
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

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> Result<T, String> {
    match std::env::var(key) {
        Ok(v) => v.parse().map_err(|_| format!("{key} has an invalid value")),
        Err(_) => Ok(default),
    }
}

impl LlmConfig {
    /// `INFERENCE_MODE=mock` (default) → `None`; `anthropic` → validated config.
    pub fn from_env() -> Result<Option<Self>, String> {
        let mode = std::env::var("INFERENCE_MODE").unwrap_or_else(|_| "mock".into());
        match mode.as_str() {
            "mock" => Ok(None),
            "anthropic" => {
                let timeout = Duration::from_secs(env_parse("LLM_TIMEOUT_SECS", 120)?);
                let deadline = Duration::from_secs(env_parse("LLM_DEADLINE_SECS", 200)?);
                if timeout.is_zero() || deadline < timeout {
                    return Err(
                        "LLM_TIMEOUT_SECS must be > 0 and not exceed LLM_DEADLINE_SECS".into(),
                    );
                }
                Ok(Some(Self {
                    api_key: std::env::var("ANTHROPIC_API_KEY")
                        .ok()
                        .filter(|k| !k.trim().is_empty())
                        .ok_or("INFERENCE_MODE=anthropic requires ANTHROPIC_API_KEY")?,
                    model: std::env::var("ANTHROPIC_MODEL")
                        .unwrap_or_else(|_| "claude-opus-5-5".into()),
                    base_url: std::env::var("ANTHROPIC_BASE_URL")
                        .unwrap_or_else(|_| "https://api.anthropic.com".into()),
                    timeout,
                    deadline,
                    max_tokens: env_parse("LLM_MAX_TOKENS", 16_000)?,
                    effort: std::env::var("LLM_EFFORT").unwrap_or_else(|_| "medium".into()),
                    refusal_fallback: std::env::var("ANTHROPIC_REFUSAL_FALLBACK")
                        .map(|v| v != "off")
                        .unwrap_or(true),
                }))
            }
            other => Err(format!(
                "INFERENCE_MODE must be `mock` or `anthropic`, got `{other}`"
            )),
        }
    }
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    cfg: LlmConfig,
}

impl LlmClient {
    pub fn new(cfg: LlmConfig) -> Result<Self, String> {
        url::Url::parse(&cfg.base_url).map_err(|_| "ANTHROPIC_BASE_URL is not a valid URL")?;
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self { http, cfg })
    }

    /// Reads [`LlmConfig::from_env`]; `None` means mock mode.
    pub fn from_env() -> Result<Option<Self>, String> {
        LlmConfig::from_env()?.map(Self::new).transpose()
    }

    /// Builds the request body (exposed for tests).
    pub fn request_body(&self, system: &str, user: &str, schema: Value) -> Value {
        let mut body = json!({
            "model": self.cfg.model,
            "max_tokens": self.cfg.max_tokens,
            "system": system,
            "messages": [{"role": "user", "content": user}],
            "output_config": {
                "effort": self.cfg.effort,
                "format": {"type": "json_schema", "schema": schema}
            }
        });
        if self.cfg.refusal_fallback {
            body["fallbacks"] = json!("default");
        }
        body
    }

    /// One structured-output request; retries 429/5xx/connect errors twice,
    /// all within [`LlmConfig::deadline`].
    pub async fn generate<T: DeserializeOwned + Validate>(
        &self,
        system: &str,
        user: &str,
        schema: Value,
    ) -> Result<T, LlmError> {
        tokio::time::timeout(
            self.cfg.deadline,
            self.generate_with_retries(system, user, schema),
        )
        .await
        .unwrap_or(Err(LlmError::Deadline(self.cfg.deadline)))
    }

    async fn generate_with_retries<T: DeserializeOwned + Validate>(
        &self,
        system: &str,
        user: &str,
        schema: Value,
    ) -> Result<T, LlmError> {
        let body = self.request_body(system, user, schema);
        let url = format!("{}/v1/messages", self.cfg.base_url.trim_end_matches('/'));

        let mut attempt = 0u32;
        let resp = loop {
            attempt += 1;
            let mut req = self
                .http
                .post(&url)
                .header("x-api-key", &self.cfg.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body);
            if self.cfg.refusal_fallback {
                req = req.header("anthropic-beta", FALLBACK_BETA);
            }
            match req.send().await {
                Ok(r)
                    if (r.status().as_u16() == 429 || r.status().is_server_error())
                        && attempt < 3 =>
                {
                    tracing::warn!(
                        status = r.status().as_u16(),
                        attempt,
                        "LLM retryable status"
                    );
                }
                Ok(r) => break r,
                Err(e) if (e.is_connect() || e.is_timeout()) && attempt < 3 => {
                    tracing::warn!(attempt, "LLM transport error, retrying");
                }
                // `without_url` keeps error text free of request details.
                Err(e) => return Err(LlmError::Transport(e.without_url().to_string())),
            }
            tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
        };

        let status = resp.status();
        let value: Value = resp
            .json()
            .await
            .map_err(|e| LlmError::Transport(e.without_url().to_string()))?;
        if !status.is_success() {
            let message = value["error"]["message"]
                .as_str()
                .unwrap_or("unknown error")
                .chars()
                .take(300)
                .collect();
            return Err(LlmError::Provider {
                status: status.as_u16(),
                message,
            });
        }
        parse_message_response(&value)
    }
}

/// Extracts and validates the structured JSON text of a Messages response.
pub fn parse_message_response<T: DeserializeOwned + Validate>(
    value: &Value,
) -> Result<T, LlmError> {
    let stop = value["stop_reason"].as_str().unwrap_or("");
    if stop == "refusal" || stop == "max_tokens" {
        return Err(LlmError::Declined(stop.to_string()));
    }
    let text = value["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<String>();
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

    pub(crate) fn config(base_url: &str) -> LlmConfig {
        LlmConfig {
            api_key: "test-key".into(),
            model: "claude-opus-5-5".into(),
            base_url: base_url.into(),
            timeout: Duration::from_secs(5),
            deadline: Duration::from_secs(10),
            max_tokens: 16_000,
            effort: "medium".into(),
            refusal_fallback: true,
        }
    }

    #[test]
    fn parses_structured_response_and_rejects_bad_ones() {
        let ok = json!({
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "{\"verdict\":\"pass\",\"summary\":\"Fine\",\"findings\":[]}"}
            ]
        });
        let r: ReviewArtifact = parse_message_response(&ok).unwrap();
        assert_eq!(r.verdict, Verdict::Pass);

        let refusal = json!({"stop_reason": "refusal", "content": []});
        assert!(matches!(
            parse_message_response::<ReviewArtifact>(&refusal),
            Err(LlmError::Declined(_))
        ));
        let garbage =
            json!({"stop_reason": "end_turn", "content": [{"type":"text","text":"not json"}]});
        assert!(matches!(
            parse_message_response::<ReviewArtifact>(&garbage),
            Err(LlmError::InvalidOutput(_))
        ));
        let bad_verdict = json!({"stop_reason": "end_turn", "content": [{"type":"text",
            "text":"{\"verdict\":\"needs_changes\",\"summary\":\"x\",\"findings\":[]}"}]});
        assert!(matches!(
            parse_message_response::<ReviewArtifact>(&bad_verdict),
            Err(LlmError::InvalidOutput(_))
        ));
    }

    #[test]
    fn request_body_shape() {
        let c = LlmClient::new(config("http://127.0.0.1:1")).unwrap();
        let b = c.request_body("sys", "user", json!({"type": "object"}));
        assert_eq!(b["model"], "claude-opus-5-5");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(b["fallbacks"], "default");
        assert!(b.get("thinking").is_none());
        assert!(!b.to_string().contains("test-key"));
    }
}
