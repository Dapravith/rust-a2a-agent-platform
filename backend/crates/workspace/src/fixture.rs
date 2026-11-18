//! Test fixture (feature `test-fixture`, never enabled by a binary): a real
//! git repository built from `backend/fixtures/sample-crate`, with a base
//! commit on `main` and one candidate commit on `agent/<workflow>/candidate`.

use std::path::{Path, PathBuf};

use crate::{Workspace, WorkspaceError, git};

pub struct Fixture {
    pub workspace: Workspace,
    pub base_sha: String,
    pub candidate_sha: String,
}

pub fn source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sample-crate")
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dst = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dst)?;
        } else {
            std::fs::copy(e.path(), dst)?;
        }
    }
    Ok(())
}

const ID: [&str; 4] = [
    "-c",
    "user.name=fixture",
    "-c",
    "user.email=fixture@invalid",
];

async fn commit(dir: &Path, msg: &str) -> Result<(), WorkspaceError> {
    git::run(dir, &["add", "-A"]).await?;
    let mut a = ID.to_vec();
    a.extend(["commit", "-q", "--no-gpg-sign", "-m", msg]);
    git::run(dir, &a).await?;
    Ok(())
}

/// The candidate change the fixture's "developer" made: currency validation
/// that keeps existing callers working (new variant, old one unchanged).
pub const CANDIDATE_LIB: &str = include_str!("fixture_candidate_lib.rs");

pub async fn create(
    root: &Path,
    id: uuid::Uuid,
    workflow_id: uuid::Uuid,
) -> Result<Fixture, WorkspaceError> {
    let dir = root.join(id.to_string());
    copy_dir(&source_dir(), &dir)?;
    git::run(&dir, &["init", "-q", "-b", "main"]).await?;
    commit(&dir, "base").await?;
    let ws = Workspace::open(root, id)?;
    let base_sha = ws.head_sha().await?;
    let branch = engineering::roles::candidate_branch(workflow_id);
    git::run(&dir, &["checkout", "-q", "-b", &branch]).await?;
    std::fs::write(dir.join("src/lib.rs"), CANDIDATE_LIB)?;
    commit(&dir, "candidate: validate currency").await?;
    let candidate_sha = ws.head_sha().await?;
    Ok(Fixture {
        workspace: ws,
        base_sha,
        candidate_sha,
    })
}

/// Only the base commit on `main`: the starting point for a Developer run.
pub async fn create_base(
    root: &Path,
    id: uuid::Uuid,
) -> Result<(Workspace, String), WorkspaceError> {
    let dir = root.join(id.to_string());
    copy_dir(&source_dir(), &dir)?;
    git::run(&dir, &["init", "-q", "-b", "main"]).await?;
    commit(&dir, "base").await?;
    let ws = Workspace::open(root, id)?;
    let base = ws.head_sha().await?;
    Ok((ws, base))
}

/// The fixture's candidate change as a git-format patch against the base
/// (what a Developer agent would submit through `workspace.apply_patch`).
pub async fn candidate_patch() -> Result<String, WorkspaceError> {
    let tmp = std::env::temp_dir().join(format!("a2a-fixture-{}", uuid::Uuid::now_v7()));
    let wf = uuid::Uuid::now_v7();
    let f = create(&tmp, uuid::Uuid::now_v7(), wf).await?;
    let d = f.workspace.diff(&f.base_sha).await?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(d.patch)
}
