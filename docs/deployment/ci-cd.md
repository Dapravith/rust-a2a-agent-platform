# CI/CD

Two workflows. Neither deploys: deployment is a reviewed GitOps change reconciled by Argo CD (Phase 7D).

## `ci.yml` — every PR and every push to `main`

Token: `contents: read` only. No repository secrets are used, so PRs from forks get nothing privileged. Nothing is pushed.

| Job | Checks |
|---|---|
| `backend` | `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace --locked` (unit, Postgres integration, A2A interop, MCP, provider test double, SIGTERM) |
| `frontend` | `npm ci`, `npm run audit` (allowlist with expiry), unit tests, `astro check` + build, `check-secrets.sh` |
| `supply-chain` | `cargo deny check` (`backend/deny.toml`: advisories, licenses, sources), gitleaks over full history (`.gitleaksignore`: reviewed false positives only), `trivy config` HIGH/CRITICAL |
| `e2e` | Playwright + axe against the real API and both agents (mock inference) on a scratch DB |
| `images` | Builds all four images for `linux/amd64`; Trivy fails on fixable CRITICAL, reports HIGH |

Actions are pinned by commit SHA (tag in a trailing comment). trivy and gitleaks are release binaries verified against SHA-256 values in `scripts/ci-install-scanners.sh`, not wrapper actions. Rust is pinned to `1.100.0`.

## `release.yml` — publish after CI succeeds on `main`

Triggered by `workflow_run` of CI on a `push` to `main` with conclusion `success`, and only when the repository variable **`PUBLISH_IMAGES=true`**. It checks out the exact commit CI tested (`head_sha`); a failing job anywhere in CI blocks publication.

Per image:
1. Build `linux/amd64` and push `ghcr.io/<owner>/a2a-<name>:sha-<commit>` (human-readable tag; deploys reference the digest).
2. Trivy scan of the pushed **digest** (fails on fixable CRITICAL).
3. `actions/attest-build-provenance` (SLSA provenance, pushed to the registry).
4. Release record: image, digest, commit, workflow run, build time, provenance URL, HIGH/CRITICAL counts.

The `record` job merges these into one `release-manifest` artifact, the input for a GitOps promotion PR (added in 7D).

Permissions in `publish` only: `packages: write`, `id-token: write`, `attestations: write`.

## Enabling publication (owner action)

1. Settings → Variables: `PUBLISH_IMAGES=true`, and `PORTAL_HOSTS=<comma-separated hostnames of every environment>`.
2. After the first publish, set each GHCR package's visibility and link it to this repository.

Verify an image before use:

```sh
gh attestation verify oci://ghcr.io/<owner>/a2a-api@sha256:<digest> --repo <owner>/<repo>
```

## Known limits

- Trivy sees Debian and npm packages in the images, not the Rust crates inside the binaries; those are covered by `cargo deny`. A clean scan is not proof the image is secure.
- PRs opened with `GITHUB_TOKEN` do not trigger workflows; the 7D GitOps PR needs a GitHub App token.
- `RUSTSEC-2026-0543` (chrono unmaintained, not a vulnerability) is ignored with a review date in `deny.toml`.
