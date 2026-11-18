//! Agent workspaces and the check runner (Phase 6B C2/C4).
//!
//! Only the MCP server uses this crate. Facts reported to the orchestrator
//! (HEAD SHA, diff, digests, exit codes) are computed here from the real
//! repository and never taken from an agent.
//!
//! * Git always runs as an argv (never a shell) with a cleared environment,
//!   no global or system config, hooks disabled, no external diff and no textconv.
//!   A repo-local config therefore cannot run commands.
//! * Paths are re-validated here (defence in depth after `engineering::mcp`):
//!   no absolute paths, `..`, `.git`, symlinks anywhere along the path, or
//!   non-regular files. Sizes are bounded.

#[cfg(any(test, feature = "test-fixture"))]
pub mod fixture;
pub mod git;
pub mod runner;

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

pub use runner::{CheckOutcome, CheckRunner, DockerRunner, DockerRunnerConfig};

/// Largest file `read_file` returns.
pub const MAX_READ_BYTES: u64 = 256 * 1024;
/// Largest diff `diff` returns. A larger diff is refused, not truncated:
/// a reviewer must see the whole candidate or nothing.
pub const MAX_DIFF_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("workspace not found")]
    NotFound,
    #[error("path is not allowed: {0}")]
    PathRefused(String),
    #[error("file too large ({0} bytes)")]
    TooLarge(u64),
    #[error("file is not valid UTF-8 text")]
    NotText,
    #[error("base commit is not an ancestor of the workspace HEAD")]
    BaseNotAncestor,
    #[error("diff exceeds {MAX_DIFF_BYTES} bytes")]
    DiffTooLarge,
    #[error("workspace is not on the workflow's candidate branch")]
    WrongBranch,
    #[error("patch does not apply: {0}")]
    PatchRejected(String),
    #[error("git failed: {0}")]
    Git(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl WorkspaceError {
    pub fn code(&self) -> &'static str {
        match self {
            WorkspaceError::NotFound => "workspace_not_found",
            WorkspaceError::PathRefused(_) => "path_refused",
            WorkspaceError::TooLarge(_) | WorkspaceError::DiffTooLarge => "too_large",
            WorkspaceError::NotText => "not_text",
            WorkspaceError::BaseNotAncestor => "base_mismatch",
            WorkspaceError::WrongBranch => "protected_branch",
            WorkspaceError::PatchRejected(_) => "patch_rejected",
            WorkspaceError::Git(_) | WorkspaceError::Io(_) => "workspace_error",
        }
    }
}

/// One workspace directory: `<root>/<workspace_id>`, a git work tree.
#[derive(Debug, Clone)]
pub struct Workspace {
    id: uuid::Uuid,
    dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub bytes: u64,
    pub sha256: String,
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DiffResult {
    pub base_sha: String,
    pub head_sha: String,
    pub changed_files: Vec<String>,
    pub patch: String,
    pub patch_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PatchResult {
    pub base_sha: String,
    pub previous_head_sha: String,
    pub head_sha: String,
    pub branch: String,
    pub changed_files: Vec<String>,
    /// SHA-256 of `git diff <base>..<head>`: the whole candidate, as a reviewer will see it.
    pub patch_sha256: String,
}

/// Committer identity for agent commits (no user config is read).
const AGENT_IDENT: [&str; 4] = [
    "-c",
    "user.name=a2a-developer-agent",
    "-c",
    "user.email=developer-agent@a2a.invalid",
];

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

impl Workspace {
    /// Opens `<root>/<id>`. The id is a UUID, so it cannot traverse.
    pub fn open(root: &Path, id: uuid::Uuid) -> Result<Workspace, WorkspaceError> {
        let dir = root.join(id.to_string());
        let meta = std::fs::symlink_metadata(&dir).map_err(|_| WorkspaceError::NotFound)?;
        if !meta.is_dir() || !dir.join(".git").exists() {
            return Err(WorkspaceError::NotFound);
        }
        Ok(Workspace {
            id,
            dir: dir.canonicalize()?,
        })
    }

    pub fn id(&self) -> uuid::Uuid {
        self.id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub async fn head_sha(&self) -> Result<String, WorkspaceError> {
        let out = git::run(&self.dir, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
        let sha = String::from_utf8_lossy(&out).trim().to_owned();
        engineering::validate_commit_sha("head", &sha)
            .map_err(|e| WorkspaceError::Git(e.to_string()))?;
        Ok(sha)
    }

    /// Resolves a repo-relative path without following symlinks.
    fn resolve(&self, rel: &str) -> Result<PathBuf, WorkspaceError> {
        let refused = || WorkspaceError::PathRefused(rel.to_owned());
        let p = Path::new(rel);
        if rel.is_empty() || rel.contains('\0') || rel.contains('\\') {
            return Err(refused());
        }
        let mut cur = self.dir.clone();
        for c in p.components() {
            match c {
                Component::Normal(seg) if seg != ".git" => {
                    cur.push(seg);
                    let m = std::fs::symlink_metadata(&cur).map_err(|_| refused())?;
                    if m.file_type().is_symlink() {
                        return Err(refused());
                    }
                }
                _ => return Err(refused()),
            }
        }
        Ok(cur)
    }

    pub async fn read_file(&self, rel: &str) -> Result<FileContent, WorkspaceError> {
        let path = self.resolve(rel)?;
        let meta = std::fs::symlink_metadata(&path)?;
        if !meta.is_file() {
            return Err(WorkspaceError::PathRefused(rel.to_owned()));
        }
        if meta.len() > MAX_READ_BYTES {
            return Err(WorkspaceError::TooLarge(meta.len()));
        }
        let bytes = tokio::fs::read(&path).await?;
        let sha256 = sha256_hex(&bytes);
        let content = String::from_utf8(bytes).map_err(|_| WorkspaceError::NotText)?;
        Ok(FileContent {
            path: rel.to_owned(),
            bytes: meta.len(),
            content,
            sha256,
            head_sha: self.head_sha().await?,
        })
    }

    /// The exact candidate: `git diff <base>..HEAD`. `base` must be a full SHA
    /// and an ancestor of HEAD (otherwise the reviewer would see the wrong change).
    pub async fn diff(&self, base: &str) -> Result<DiffResult, WorkspaceError> {
        engineering::validate_commit_sha("base_sha", base)
            .map_err(|e| WorkspaceError::Git(e.to_string()))?;
        let head = self.head_sha().await?;
        if !git::is_ancestor(&self.dir, base, &head).await? {
            return Err(WorkspaceError::BaseNotAncestor);
        }
        let range = format!("{base}..{head}");
        let patch = git::run(
            &self.dir,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--full-index",
                "--binary",
                &range,
            ],
        )
        .await?;
        if patch.len() > MAX_DIFF_BYTES {
            return Err(WorkspaceError::DiffTooLarge);
        }
        let names = git::run(
            &self.dir,
            &["diff", "--name-only", "-z", "--no-renames", &range],
        )
        .await?;
        let changed_files = names
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        Ok(DiffResult {
            base_sha: base.to_owned(),
            head_sha: head,
            changed_files,
            patch_sha256: sha256_hex(&patch),
            patch: String::from_utf8_lossy(&patch).into_owned(),
        })
    }
}

impl Workspace {
    async fn current_branch(&self) -> Result<Option<String>, WorkspaceError> {
        match git::run(&self.dir, &["symbolic-ref", "-q", "--short", "HEAD"]).await {
            Ok(b) => Ok(Some(String::from_utf8_lossy(&b).trim().to_owned())),
            Err(_) => Ok(None), // detached HEAD
        }
    }

    async fn reset(&self) {
        let _ = git::run(&self.dir, &["reset", "-q", "--hard", "HEAD"]).await;
    }

    /// Applies a git-format patch and commits it on `branch` (the workflow's
    /// candidate branch). Preconditions: HEAD is `base` (then the branch is
    /// created) or HEAD is already on `branch` and descends from `base`. After
    /// applying, the *staged* result is re-checked: only `allowed` paths, only
    /// regular non-executable files. Any failure leaves the tree at HEAD.
    ///
    /// Not idempotent: callers must not retry it automatically.
    pub async fn apply_patch(
        &self,
        base: &str,
        branch: &str,
        patch: &str,
        message: &str,
        allowed: &[String],
    ) -> Result<PatchResult, WorkspaceError> {
        engineering::validate_commit_sha("base_sha", base)
            .map_err(|e| WorkspaceError::Git(e.to_string()))?;
        let head = self.head_sha().await?;
        match self.current_branch().await?.as_deref() {
            Some(b) if b == branch => {
                if !git::is_ancestor(&self.dir, base, &head).await? {
                    return Err(WorkspaceError::BaseNotAncestor);
                }
            }
            _ if head == base => {
                git::run(&self.dir, &["checkout", "-q", "-b", branch]).await?;
            }
            _ => return Err(WorkspaceError::WrongBranch),
        }
        let result = async {
            git::run_stdin(
                &self.dir,
                &["apply", "--check", "--index", "-"],
                patch.as_bytes(),
            )
            .await
            .map_err(|e| WorkspaceError::PatchRejected(e.to_string()))?;
            git::run_stdin(&self.dir, &["apply", "--index", "-"], patch.as_bytes())
                .await
                .map_err(|e| WorkspaceError::PatchRejected(e.to_string()))?;
            // Re-check what git actually staged.
            let raw = git::run(
                &self.dir,
                &["diff", "--cached", "--raw", "--no-renames", "-z"],
            )
            .await?;
            let mut staged = Vec::new();
            let mut fields = raw.split(|b| *b == 0).filter(|s| !s.is_empty());
            while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
                let meta = String::from_utf8_lossy(meta);
                let path = String::from_utf8_lossy(path).into_owned();
                // ":<old mode> <new mode> <old sha> <new sha> <status>"
                let new_mode = meta.split_whitespace().nth(1).unwrap_or_default();
                if !(new_mode == "100644" || new_mode == "000000") {
                    return Err(WorkspaceError::PatchRejected(format!(
                        "`{path}` has mode {new_mode}"
                    )));
                }
                if !engineering::roles::path_in_scope(&path, allowed) {
                    return Err(WorkspaceError::PathRefused(path));
                }
                staged.push(path);
            }
            if staged.is_empty() {
                return Err(WorkspaceError::PatchRejected(
                    "patch changes nothing".into(),
                ));
            }
            let mut a = AGENT_IDENT.to_vec();
            a.extend([
                "commit",
                "-q",
                "--no-gpg-sign",
                "--no-verify",
                "-m",
                message,
            ]);
            git::run(&self.dir, &a).await?;
            Ok(staged)
        }
        .await;
        let changed_files = match result {
            Ok(f) => f,
            Err(e) => {
                self.reset().await;
                return Err(e);
            }
        };
        let new_head = self.head_sha().await?;
        let d = self.diff(base).await?;
        Ok(PatchResult {
            base_sha: base.to_owned(),
            previous_head_sha: head,
            head_sha: new_head,
            branch: branch.to_owned(),
            changed_files,
            patch_sha256: d.patch_sha256,
        })
    }
}

#[cfg(test)]
mod tests;
