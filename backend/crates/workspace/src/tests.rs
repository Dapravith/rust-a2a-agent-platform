#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use super::*;
use crate::fixture;

async fn fx() -> (tempfile::TempDir, fixture::Fixture) {
    let root = tempfile::tempdir().unwrap();
    let f = fixture::create(root.path(), uuid::Uuid::now_v7(), uuid::Uuid::now_v7())
        .await
        .unwrap();
    (root, f)
}

#[tokio::test]
async fn open_refuses_missing_workspace() {
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        Workspace::open(root.path(), uuid::Uuid::now_v7()),
        Err(WorkspaceError::NotFound)
    ));
}

#[tokio::test]
async fn read_file_returns_content_bound_to_head() {
    let (_root, f) = fx().await;
    let c = f.workspace.read_file("src/lib.rs").await.unwrap();
    assert!(c.content.contains("UnsupportedCurrency"));
    assert_eq!(c.head_sha, f.candidate_sha);
    assert_eq!(c.sha256, sha256_hex(c.content.as_bytes()));
}

#[tokio::test]
async fn read_file_refuses_escapes_symlinks_and_special_files() {
    let (_root, f) = fx().await;
    let dir = f.workspace.dir().to_path_buf();
    // Symlink to a file outside the workspace, and a symlinked directory.
    std::os::unix::fs::symlink("/etc/hosts", dir.join("src/hosts")).unwrap();
    std::os::unix::fs::symlink("/etc", dir.join("etc")).unwrap();
    for p in [
        "../x",
        "/etc/passwd",
        ".git/config",
        "src/../../x",
        "src/hosts",
        "etc/hosts",
        "src",
        "./src/lib.rs",
        "src\\lib.rs",
        "",
    ] {
        assert!(
            matches!(
                f.workspace.read_file(p).await,
                Err(WorkspaceError::PathRefused(_))
            ),
            "{p}"
        );
    }
}

#[tokio::test]
async fn read_file_bounds_size_and_requires_text() {
    let (_root, f) = fx().await;
    let dir = f.workspace.dir();
    std::fs::write(dir.join("big.txt"), vec![b'a'; MAX_READ_BYTES as usize + 1]).unwrap();
    std::fs::write(dir.join("bin.dat"), [0xff, 0xfe, 0x00]).unwrap();
    assert!(matches!(
        f.workspace.read_file("big.txt").await,
        Err(WorkspaceError::TooLarge(_))
    ));
    assert!(matches!(
        f.workspace.read_file("bin.dat").await,
        Err(WorkspaceError::NotText)
    ));
}

#[tokio::test]
async fn diff_is_the_exact_candidate() {
    let (_root, f) = fx().await;
    let d = f.workspace.diff(&f.base_sha).await.unwrap();
    assert_eq!(d.base_sha, f.base_sha);
    assert_eq!(d.head_sha, f.candidate_sha);
    assert_eq!(d.changed_files, vec!["src/lib.rs".to_owned()]);
    assert!(d.patch.contains("+    UnsupportedCurrency,"));
    assert_eq!(d.patch_sha256, sha256_hex(d.patch.as_bytes()));
    // Deterministic.
    assert_eq!(f.workspace.diff(&f.base_sha).await.unwrap(), d);
}

#[tokio::test]
async fn diff_refuses_wrong_or_abbreviated_base() {
    let (_root, f) = fx().await;
    assert!(f.workspace.diff(&f.base_sha[..12]).await.is_err());
    assert!(f.workspace.diff(&"0".repeat(40)).await.is_err());
    // A commit that exists in this repository but is not an ancestor of HEAD.
    // It is created in the same repository with `commit-tree`, which leaves
    // HEAD unchanged. (A second fixture is not usable here: identical content,
    // author, message and a same-second timestamp give it the *same* SHAs.)
    let tree = git::run(f.workspace.dir(), &["rev-parse", "HEAD^{tree}"])
        .await
        .unwrap();
    let tree = String::from_utf8(tree).unwrap().trim().to_owned();
    let side = git::run(
        f.workspace.dir(),
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@invalid",
            "commit-tree",
            &tree,
            "-m",
            "unrelated root commit",
        ],
    )
    .await
    .unwrap();
    let side = String::from_utf8(side).unwrap().trim().to_owned();
    assert_ne!(side, f.base_sha);
    assert_ne!(side, f.candidate_sha);
    assert_eq!(f.workspace.head_sha().await.unwrap(), f.candidate_sha);
    assert!(matches!(
        f.workspace.diff(&side).await,
        Err(WorkspaceError::BaseNotAncestor)
    ));
}

#[tokio::test]
async fn repo_local_config_cannot_execute_commands() {
    let (_root, f) = fx().await;
    let dir = f.workspace.dir();
    let marker = dir.join("pwned");
    let m = marker.display();
    let cfg = format!(
        "\n[diff]\n\texternal = touch {m}\n[core]\n\tpager = touch {m}\n\tfsmonitor = touch {m}\n\
         [diff \"x\"]\n\ttextconv = touch {m}\n[core]\n\thooksPath = .\n"
    );
    let mut c = std::fs::read_to_string(dir.join(".git/config")).unwrap();
    c.push_str(&cfg);
    std::fs::write(dir.join(".git/config"), c).unwrap();
    std::fs::write(dir.join(".gitattributes"), "*.rs diff=x\n").unwrap();
    f.workspace.diff(&f.base_sha).await.unwrap();
    f.workspace.read_file("src/lib.rs").await.unwrap();
    assert!(!marker.exists(), "repo-local git config executed a command");
}

fn pinned() -> DockerRunnerConfig {
    DockerRunnerConfig {
        image: format!("rust@sha256:{}", "a".repeat(64)),
        timeout: Duration::from_secs(60),
        memory: "1g".into(),
        cpus: "1".into(),
        pids: 128,
    }
}

#[test]
fn docker_image_must_be_pinned_by_digest() {
    for img in [
        "rust:1.94",
        "rust:latest",
        "rust@sha256:abc",
        "rust;rm -rf /@sha256:x",
    ] {
        let mut c = pinned();
        c.image = img.into();
        assert!(DockerRunner::new(c).is_err(), "{img}");
    }
    assert!(DockerRunner::new(pinned()).is_ok());
}

#[test]
fn docker_args_enforce_the_boundary() {
    let r = DockerRunner::new(pinned()).unwrap();
    let argv = engineering::roles::check_argv("cargo_test").unwrap();
    let a = r
        .docker_args(std::path::Path::new("/ws/abc"), "a2a-check-1", argv)
        .unwrap();
    let s = a.join(" ");
    for must in [
        "--network none",
        "--read-only",
        "--cap-drop ALL",
        "--security-opt no-new-privileges",
        "--user 65534:65534",
        "--pids-limit 128",
        "--memory 1g",
        "--memory-swap 1g",
        "type=bind,src=/ws/abc,dst=/workspace,readonly",
        "--entrypoint cargo",
    ] {
        assert!(s.contains(must), "missing `{must}` in {s}");
    }
    assert!(s.ends_with("test --offline"), "{s}");
    assert!(!s.contains("docker.sock") && !s.contains("--privileged"));
    // A path that would inject extra mount options is refused.
    assert!(
        r.docker_args(std::path::Path::new("/ws/a,dst=/etc"), "c", argv)
            .is_err()
    );
}

/// Live boundary test (C4). Needs a working Docker daemon and a digest-pinned
/// Rust image: `A2A_SANDBOX_IMAGE=rust@sha256:... cargo test -p workspace -- --ignored`.
#[tokio::test]
#[ignore = "needs Docker and A2A_SANDBOX_IMAGE"]
async fn live_sandbox_runs_checks_without_network() {
    let image = std::env::var("A2A_SANDBOX_IMAGE").expect("A2A_SANDBOX_IMAGE");
    let mut cfg = pinned();
    cfg.image = image;
    cfg.timeout = Duration::from_secs(300);
    let r = DockerRunner::new(cfg).unwrap();
    let (_root, f) = fx().await;
    let out = r.run(&f.workspace, "cargo_test").await.unwrap();
    assert_eq!(
        out.exit_code,
        Some(0),
        "{}\n{}",
        out.stdout_tail,
        out.stderr_tail
    );
    assert!(out.stdout_tail.contains("rejects_unknown_currency ... ok"));
    assert_eq!(out.head_sha, f.candidate_sha);
    assert!(r.run(&f.workspace, "curl").await.is_err());
}

// ---------------------------------------------------------------------------
// workspace.apply_patch (B1)
// ---------------------------------------------------------------------------

async fn base_ws() -> (tempfile::TempDir, Workspace, String) {
    let root = tempfile::tempdir().unwrap();
    let (ws, base) = fixture::create_base(root.path(), uuid::Uuid::now_v7())
        .await
        .unwrap();
    (root, ws, base)
}

fn scope() -> Vec<String> {
    vec!["src".into()]
}

async fn clean(ws: &Workspace) -> bool {
    git::run(ws.dir(), &["status", "--porcelain"])
        .await
        .unwrap()
        .is_empty()
}

#[tokio::test]
async fn apply_patch_commits_on_the_candidate_branch() {
    let (_r, ws, base) = base_ws().await;
    let branch = engineering::roles::candidate_branch(uuid::Uuid::now_v7());
    let patch = fixture::candidate_patch().await.unwrap();
    let r = ws
        .apply_patch(&base, &branch, &patch, "validate currency", &scope())
        .await
        .unwrap();
    assert_eq!(r.previous_head_sha, base);
    assert_ne!(r.head_sha, base);
    assert_eq!(r.changed_files, vec!["src/lib.rs".to_owned()]);
    assert_eq!(r.branch, branch);
    let d = ws.diff(&base).await.unwrap();
    assert_eq!(
        (d.head_sha.as_str(), d.patch_sha256.as_str()),
        (r.head_sha.as_str(), r.patch_sha256.as_str())
    );
    assert!(
        ws.read_file("src/lib.rs")
            .await
            .unwrap()
            .content
            .contains("UnsupportedCurrency")
    );
    assert!(clean(&ws).await);
    // A follow-up patch on the same branch builds on the candidate.
    let more = "diff --git a/src/extra.rs b/src/extra.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/extra.rs\n@@ -0,0 +1 @@\n+// extra\n";
    let r2 = ws
        .apply_patch(&base, &branch, more, "add file", &scope())
        .await
        .unwrap();
    assert_eq!(r2.previous_head_sha, r.head_sha);
    assert_eq!(ws.diff(&base).await.unwrap().changed_files.len(), 2);
}

#[tokio::test]
async fn apply_patch_failures_leave_the_tree_untouched() {
    let (_r, ws, base) = base_ws().await;
    let branch = engineering::roles::candidate_branch(uuid::Uuid::now_v7());
    // Applies cleanly but is out of scope (and build.rs would run at `cargo build`).
    let outside = "diff --git a/build.rs b/build.rs\nnew file mode 100644\n--- /dev/null\n+++ b/build.rs\n@@ -0,0 +1 @@\n+fn main() {}\n";
    let not_applying = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-this line does not exist\n+x\n";
    // A symlink smuggled past the pure parser: the staged-state check must catch it.
    let symlink = "diff --git a/src/hosts b/src/hosts\nnew file mode 120000\n--- /dev/null\n+++ b/src/hosts\n@@ -0,0 +1 @@\n+/etc/hosts\n\\ No newline at end of file\n";
    let exec = "diff --git a/src/run.sh b/src/run.sh\nnew file mode 100755\n--- /dev/null\n+++ b/src/run.sh\n@@ -0,0 +1 @@\n+echo hi\n";
    for (p, code) in [
        (outside, "path_refused"),
        (not_applying, "patch_rejected"),
        (symlink, "patch_rejected"),
        (exec, "patch_rejected"),
        ("garbage", "patch_rejected"),
    ] {
        let e = ws
            .apply_patch(&base, &branch, p, "x", &scope())
            .await
            .unwrap_err();
        assert_eq!(e.code(), code, "{e}");
        assert!(clean(&ws).await, "tree dirty after {code}");
        assert_eq!(ws.head_sha().await.unwrap(), base);
    }
    assert!(!ws.dir().join("src/hosts").exists());
}

#[tokio::test]
async fn apply_patch_refuses_other_branches_and_bases() {
    // Workspace already on another workflow's candidate branch, HEAD != base.
    let (_r, f) = fx().await;
    let patch = fixture::candidate_patch().await.unwrap();
    let mine = engineering::roles::candidate_branch(uuid::Uuid::now_v7());
    let e = f
        .workspace
        .apply_patch(&f.base_sha, &mine, &patch, "x", &scope())
        .await
        .unwrap_err();
    assert_eq!(e.code(), "protected_branch");
    // Base that HEAD does not descend from.
    let (_r2, other) = fx().await;
    let e = f
        .workspace
        .apply_patch(&other.base_sha, &mine, &patch, "x", &scope())
        .await
        .unwrap_err();
    assert_eq!(e.code(), "protected_branch");
    assert_eq!(f.workspace.head_sha().await.unwrap(), f.candidate_sha);
}

// ---------------------------------------------------------------------------
// Workspace creation (B2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_from_source_checks_out_the_base_without_a_remote() {
    let src_root = tempfile::tempdir().unwrap();
    let (src, base) = fixture::create_base(src_root.path(), uuid::Uuid::now_v7())
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::create_from(root.path(), src.dir(), &base)
        .await
        .unwrap();
    assert_eq!(ws.head_sha().await.unwrap(), base);
    assert!(git::run(ws.dir(), &["remote"]).await.unwrap().is_empty());
    // The developer can then branch and commit from it.
    let branch = engineering::roles::candidate_branch(uuid::Uuid::now_v7());
    let patch = fixture::candidate_patch().await.unwrap();
    ws.apply_patch(&base, &branch, &patch, "x", &scope())
        .await
        .unwrap();
    // The source is untouched.
    assert_eq!(src.head_sha().await.unwrap(), base);
}

#[tokio::test]
async fn create_from_refuses_unknown_or_short_bases_and_leaves_nothing_behind() {
    let src_root = tempfile::tempdir().unwrap();
    let (src, base) = fixture::create_base(src_root.path(), uuid::Uuid::now_v7())
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    assert!(
        Workspace::create_from(root.path(), src.dir(), &"0".repeat(40))
            .await
            .is_err()
    );
    assert!(
        Workspace::create_from(root.path(), src.dir(), &base[..12])
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
