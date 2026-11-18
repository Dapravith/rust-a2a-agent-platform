//! Hardened git invocation: argv only, cleared environment, no user/system
//! config, no hooks, no pager, no prompts, bounded runtime.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::WorkspaceError;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.external=",
            "-c",
            "protocol.allow=never",
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
}

/// Runs git and returns stdout; a non-zero exit is an error.
pub async fn run(dir: &Path, args: &[&str]) -> Result<Vec<u8>, WorkspaceError> {
    let out = tokio::time::timeout(GIT_TIMEOUT, command(dir, args).output())
        .await
        .map_err(|_| WorkspaceError::Git("timed out".into()))??;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        return Err(WorkspaceError::Git(msg.chars().take(500).collect()));
    }
    Ok(out.stdout)
}

pub async fn is_ancestor(dir: &Path, base: &str, head: &str) -> Result<bool, WorkspaceError> {
    let st = tokio::time::timeout(
        GIT_TIMEOUT,
        command(dir, &["merge-base", "--is-ancestor", base, head]).status(),
    )
    .await
    .map_err(|_| WorkspaceError::Git("timed out".into()))??;
    match st.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(WorkspaceError::Git(
            "merge-base failed (unknown commit?)".into(),
        )),
    }
}

/// Runs git with `input` on stdin; a non-zero exit is an error.
pub async fn run_stdin(dir: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>, WorkspaceError> {
    let mut c = command(dir, args);
    c.stdin(Stdio::piped());
    let work = async {
        let mut child = c.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input).await?;
            drop(stdin);
        }
        child.wait_with_output().await
    };
    let out = tokio::time::timeout(GIT_TIMEOUT, work)
        .await
        .map_err(|_| WorkspaceError::Git("timed out".into()))??;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        return Err(WorkspaceError::Git(msg.chars().take(500).collect()));
    }
    Ok(out.stdout)
}
