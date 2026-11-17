//! Startup configuration, validated before anything else runs.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
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

/// A database URL must be `postgres://`, and any non-loopback host must use
/// `sslmode=verify-full` (certificate chain *and* hostname verified).
/// `require`/`prefer` only encrypt and would accept an impostor server.
/// For Supabase, add `sslrootcert=<path to the project's CA certificate>`.
pub fn validate_database_url(key: &str, raw: &str) -> Result<(), String> {
    if !(raw.starts_with("postgres://") || raw.starts_with("postgresql://")) {
        return Err(format!("{key} must be a postgres:// URL"));
    }
    // Never echo the URL: it contains the password.
    let opts: PgConnectOptions = raw
        .parse()
        .map_err(|_| format!("{key} could not be parsed"))?;
    let host = opts
        .get_host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let local = host.starts_with('/')
        || host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !local && !matches!(opts.get_ssl_mode(), PgSslMode::VerifyFull) {
        return Err(format!(
            "{key} points to a remote host without verified TLS; refused to connect. Add `sslmode=verify-full` (and `sslrootcert=` for \
             Supabase) so the server certificate and hostname are verified"
        ));
    }
    Ok(())
}

/// The URL `api migrate` uses: `DATABASE_MIGRATION_URL` (schema owner) or,
/// for local development, `DATABASE_URL`.
pub fn migration_database_url() -> Result<String, String> {
    let (key, url) = match var("DATABASE_MIGRATION_URL") {
        Some(u) => ("DATABASE_MIGRATION_URL", u),
        None => (
            "DATABASE_URL",
            var("DATABASE_URL").ok_or("DATABASE_MIGRATION_URL or DATABASE_URL is required")?,
        ),
    };
    validate_database_url(key, &url)?;
    Ok(url)
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let database_url = var("DATABASE_URL").ok_or("DATABASE_URL is required")?;
        validate_database_url("DATABASE_URL", &database_url)?;
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
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn database_url_requires_verified_tls_off_host() {
        let ok = [
            "postgres://u:p@127.0.0.1:54329/a2a",
            "postgres://u:p@localhost/a2a",
            "postgres://u:p@[::1]:5432/a2a",
            "postgresql://u:p@db.example.com:5432/postgres?sslmode=verify-full&sslrootcert=/etc/ssl/db-ca.crt",
        ];
        for url in ok {
            assert!(validate_database_url("X", url).is_ok(), "{url}");
        }
        let bad = [
            "postgres://u:p@db.example.com:5432/postgres",
            "postgres://u:p@db.example.com:5432/postgres?sslmode=require",
            "postgres://u:p@db.example.com:5432/postgres?sslmode=verify-ca",
            "postgres://u:p@10.0.0.5/a2a?sslmode=prefer",
            "mysql://u:p@localhost/a2a",
        ];
        for url in bad {
            let err = validate_database_url("X", url).unwrap_err();
            assert!(
                !err.contains(":p@"),
                "error must not echo credentials: {err}"
            );
        }
    }

    #[test]
    fn agent_url_rules() {
        assert!(parse_agent_url("X", "http://127.0.0.1:9101").is_ok());
        assert!(parse_agent_url("X", "ftp://host").is_err());
        assert!(parse_agent_url("X", "http://user:pw@host").is_err());
        assert!(parse_agent_url("X", "not a url").is_err());
    }
}
