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
    // A commit that is not an ancestor of HEAD.
    let (_r2, other) = fx().await;
    assert!(f.workspace.diff(&other.candidate_sha).await.is_err());
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
