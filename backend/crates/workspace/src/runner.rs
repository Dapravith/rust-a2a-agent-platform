//! Check execution boundary (ADR-004).
//!
//! A check is an allowlisted id that maps to a fixed argv
//! (`engineering::roles::ALLOWED_CHECKS`). It runs in a disposable container
//! with no network, a read-only root and a read-only workspace mount, no
//! capabilities, a non-root user, and CPU/memory/PID/time limits. Output is
//! capped (the tail is kept) and the full streams are hashed.
//!
//! ADR-004 scope: this boundary is accepted for deterministic test fixtures.
//! It is **not** accepted for running LLM-generated code; that needs a
//! microVM or disposable VM and stays BLOCKED.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::Workspace;

/// Bytes of each stream kept in the outcome (the tail).
pub const OUTPUT_TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CheckOutcome {
    pub check_id: String,
    pub argv: Vec<String>,
    /// `None` when the process was killed (timeout) or had no exit code.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    /// `sha256(sha256(full stdout) || sha256(full stderr))`: binds the tails to
    /// the complete streams without storing them.
    pub log_sha256: String,
    pub head_sha: String,
    /// Which boundary ran it (`docker`, or a test double's name).
    pub runner: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("check `{0}` is not on the allowlist")]
    NotAllowed(String),
    #[error("sandbox unavailable: {0}")]
    Unavailable(String),
    #[error(transparent)]
    Workspace(#[from] crate::WorkspaceError),
}

impl RunnerError {
    pub fn code(&self) -> &'static str {
        match self {
            RunnerError::NotAllowed(_) => "check_not_allowed",
            RunnerError::Unavailable(_) => "sandbox_unavailable",
            RunnerError::Workspace(e) => e.code(),
        }
    }
}

pub trait CheckRunner: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn run<'a>(
        &'a self,
        ws: &'a Workspace,
        check_id: &'a str,
    ) -> BoxFuture<'a, Result<CheckOutcome, RunnerError>>;
}

#[derive(Debug, Clone)]
pub struct DockerRunnerConfig {
    /// Must be pinned by digest (`name@sha256:...`).
    pub image: String,
    pub timeout: Duration,
    pub memory: String,
    pub cpus: String,
    pub pids: u32,
}

impl DockerRunnerConfig {
    pub fn validate(&self) -> Result<(), RunnerError> {
        let ok_chars = self
            .image
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "./:_@-".contains(c));
        let pinned = self
            .image
            .split_once("@sha256:")
            .is_some_and(|(n, d)| !n.is_empty() && d.len() == 64);
        if !ok_chars || !pinned {
            return Err(RunnerError::Unavailable(
                "sandbox image must be pinned by digest (name@sha256:<64 hex>)".into(),
            ));
        }
        Ok(())
    }
}

pub struct DockerRunner {
    cfg: DockerRunnerConfig,
}

impl DockerRunner {
    pub fn new(cfg: DockerRunnerConfig) -> Result<Self, RunnerError> {
        cfg.validate()?;
        Ok(Self { cfg })
    }

    /// The full `docker` argv for one check (public for review and tests).
    pub fn docker_args(
        &self,
        workspace_dir: &Path,
        container: &str,
        argv: &[&str],
    ) -> Result<Vec<String>, RunnerError> {
        let dir = workspace_dir
            .to_str()
            .filter(|d| !d.contains(',') && !d.contains('=') && !d.contains('\n'))
            .ok_or_else(|| RunnerError::Unavailable("workspace path not mountable".into()))?;
        let mut a: Vec<String> = [
            "run",
            "--rm",
            "--name",
            container,
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            "65534:65534",
            "--ipc",
            "none",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        a.extend([
            "--pids-limit".into(),
            self.cfg.pids.to_string(),
            "--memory".into(),
            self.cfg.memory.clone(),
            "--memory-swap".into(),
            self.cfg.memory.clone(),
            "--cpus".into(),
            self.cfg.cpus.clone(),
            "--tmpfs".into(),
            "/tmp:rw,exec,nosuid,size=1g".into(),
            "--mount".into(),
            format!("type=bind,src={dir},dst=/workspace,readonly"),
            "--workdir".into(),
            "/workspace".into(),
            "-e".into(),
            "HOME=/tmp".into(),
            "-e".into(),
            "CARGO_TARGET_DIR=/tmp/target".into(),
            "-e".into(),
            "CARGO_NET_OFFLINE=true".into(),
            "--entrypoint".into(),
            argv[0].to_owned(),
            self.cfg.image.clone(),
        ]);
        a.extend(argv[1..].iter().map(|s| (*s).to_owned()));
        Ok(a)
    }
}

async fn capture<R: AsyncRead + Unpin>(mut r: R, hasher: &mut Sha256) -> Vec<u8> {
    let mut tail = Vec::new();
    let mut buf = [0u8; 8192];
    while let Ok(n) = r.read(&mut buf).await {
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        tail.extend_from_slice(&buf[..n]);
        if tail.len() > OUTPUT_TAIL_BYTES * 2 {
            tail.drain(..tail.len() - OUTPUT_TAIL_BYTES);
        }
    }
    if tail.len() > OUTPUT_TAIL_BYTES {
        tail.drain(..tail.len() - OUTPUT_TAIL_BYTES);
    }
    tail
}

impl CheckRunner for DockerRunner {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn run<'a>(
        &'a self,
        ws: &'a Workspace,
        check_id: &'a str,
    ) -> BoxFuture<'a, Result<CheckOutcome, RunnerError>> {
        Box::pin(async move {
            let argv = engineering::roles::check_argv(check_id)
                .ok_or_else(|| RunnerError::NotAllowed(check_id.to_owned()))?;
            let head_sha = ws.head_sha().await?;
            let container = format!("a2a-check-{}", uuid::Uuid::now_v7());
            let args = self.docker_args(ws.dir(), &container, argv)?;
            let started = Instant::now();
            let mut child = Command::new("docker")
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| RunnerError::Unavailable(format!("docker: {e}")))?;
            let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
                return Err(RunnerError::Unavailable("no pipes".into()));
            };
            let mut h_out = Sha256::new();
            let mut h_err = Sha256::new();
            let work = async {
                let (o, e) = tokio::join!(capture(out, &mut h_out), capture(err, &mut h_err));
                let st = child.wait().await;
                (o, e, st)
            };
            let (stdout, stderr, exit_code, timed_out) =
                match tokio::time::timeout(self.cfg.timeout, work).await {
                    Ok((o, e, st)) => (o, e, st.ok().and_then(|s| s.code()), false),
                    Err(_) => {
                        // Kill the container itself, not only the docker client.
                        let _ = Command::new("docker")
                            .args(["kill", &container])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status()
                            .await;
                        (Vec::new(), Vec::new(), None, true)
                    }
                };
            let mut h = Sha256::new();
            h.update(h_out.finalize());
            h.update(h_err.finalize());
            Ok(CheckOutcome {
                check_id: check_id.to_owned(),
                argv: argv.iter().map(|s| (*s).to_owned()).collect(),
                exit_code,
                timed_out,
                duration_ms: started.elapsed().as_millis() as u64,
                stdout_tail: String::from_utf8_lossy(&stdout).into_owned(),
                stderr_tail: String::from_utf8_lossy(&stderr).into_owned(),
                log_sha256: hex::encode(h.finalize()),
                head_sha,
                runner: self.name().to_owned(),
            })
        })
    }
}
