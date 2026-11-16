//! Startup configuration, validated before anything else runs.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use url::Url;

#[derive(Clone)]
pub struct Config {
    pub bind: String,
    pub database_url: String,
    pub db_max_connections: u32,
    pub worker_url: Url,
    pub reviewer_url: Url,
    pub agent_timeout: Duration,
    pub poll_interval: Duration,
    pub frontend_dir: Option<PathBuf>,
}

/// Never print the database URL: it contains the password.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("database_url", &"<redacted>")
            .field("db_max_connections", &self.db_max_connections)
            .field("worker_url", &self.worker_url.as_str())
            .field("reviewer_url", &self.reviewer_url.as_str())
            .field("agent_timeout", &self.agent_timeout)
            .field("poll_interval", &self.poll_interval)
            .field("frontend_dir", &self.frontend_dir)
            .finish()
    }
}

fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn parse<T: std::str::FromStr>(key: &str, default: T) -> Result<T, String> {
    match var(key) {
        Some(v) => v.parse().map_err(|_| format!("{key} has an invalid value")),
        None => Ok(default),
    }
}

/// Agent base URLs must be plain http(s) origins without credentials.
pub fn parse_agent_url(key: &str, raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| format!("{key} is not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{key} must use http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{key} must not embed credentials"));
    }
    if url.host_str().is_none() {
        return Err(format!("{key} must include a host"));
    }
    Ok(url)
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let database_url = var("DATABASE_URL").ok_or("DATABASE_URL is required")?;
        if !(database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")) {
            return Err("DATABASE_URL must be a postgres:// URL".into());
        }
        let agent_timeout = Duration::from_secs(parse("AGENT_TIMEOUT_SECS", 240u64)?);
        let poll_interval = Duration::from_millis(parse("AGENT_POLL_INTERVAL_MS", 500u64)?);
        if agent_timeout.is_zero() || poll_interval.is_zero() {
            return Err("AGENT_TIMEOUT_SECS and AGENT_POLL_INTERVAL_MS must be > 0".into());
        }
        let frontend_dir = var("FRONTEND_DIR").map(PathBuf::from);
        if let Some(dir) = &frontend_dir
            && !dir.join("index.html").is_file()
        {
            return Err(format!(
                "FRONTEND_DIR {} has no index.html (run `npm run build`)",
                dir.display()
            ));
        }
        Ok(Self {
            bind: var("ORCHESTRATOR_BIND").unwrap_or_else(|| "127.0.0.1:8080".into()),
            database_url,
            db_max_connections: parse("DB_MAX_CONNECTIONS", 5u32)?,
            worker_url: parse_agent_url(
                "WORKER_URL",
                &var("WORKER_URL").unwrap_or_else(|| "http://127.0.0.1:9101".into()),
            )?,
            reviewer_url: parse_agent_url(
                "REVIEWER_URL",
                &var("REVIEWER_URL").unwrap_or_else(|| "http://127.0.0.1:9102".into()),
            )?,
            agent_timeout,
            poll_interval,
            frontend_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_url_rules() {
        assert!(parse_agent_url("X", "http://127.0.0.1:9101").is_ok());
        assert!(parse_agent_url("X", "ftp://host").is_err());
        assert!(parse_agent_url("X", "http://user:pw@host").is_err());
        assert!(parse_agent_url("X", "not a url").is_err());
    }
}
