# Phase 7A: deployment preflight, design and backlog

Date: 2026-11-18 · Branch `phase6-d-revised-structure` @ `3ec8e1a` (working tree = the commit Phase 6F verified; only `docs/PHASE6F.md` and this file are untracked)

**Status: design for review. No infrastructure, image, workflow or application file has been changed. Implementation (7B onward) waits on the owner decisions in §11.**

Measured values are marked MEASURED; everything else is an ESTIMATE or UNVERIFIED.

---

## 1. Phase 6F readiness audit

| Check | Result | Evidence |
|---|---|---|
| Tree matches the 6F-verified commit | PASS | `git status`: no tracked changes since `3ec8e1a` |
| Rust fmt / clippy / build / 197 tests | PASS (6F, same commit) | `PHASE6F.md` §4; not re-run here (host load avg 116–130 MEASURED); re-run is backlog item V0 |
| Astro check + build, 35 unit tests, `npm run audit` | PASS (6F, same commit) | `PHASE6F.md` §4 |
| A2A interop, MCP, provider adapter (test double), Postgres | PASS (6F) | 6F suites; no live provider key in `.env` |
| Human approval enforcement | PASS, fail-closed | No IdP → every `/api/eng` write returns 503 |
| GitHub delivery workflow | **BLOCKED** | Not built (Phase 6D) |
| Playwright e2e / axe | UNVERIFIED | Not run in 6F, not in CI |
| Rust advisory scan | UNVERIFIED | `cargo audit`/`cargo deny` not installed, not in CI |
| Owner phase order | **CONFLICT** | `PHASE6F.md` §8: "6C/6D precede Phase 7". See §11 D0 |

**Verdict: CONDITIONAL PASS for a private pilot only.** New finding during this preflight that tightens 6F's condition: the *portal itself* has no authentication, and `POST /api/workflows` (P0) is unauthenticated. 6F allowed "portal as the only ingress"; that is not safe either. Anyone who can reach the portal can read all workflows and start LLM runs (cost exposure once a real key is set). **The whole pilot, portal included, must be reachable only over a private network (VPN) until Phase 6D OIDC lands.** Public ingress is BLOCKED.

## 2. Application and infrastructure assessment

### Runnable workloads

| Binary | Default bind | Probes available | Pilot |
|---|---|---|---|
| `api` (`serve`, `migrate`) | `ORCHESTRATOR_BIND` 127.0.0.1:8080 | `/healthz` (liveness, no DB), `/api/health` (DB ping → 503) | deploy, **1 replica** |
| `worker-agent` | `WORKER_BIND` :9101 | `/healthz`, Agent Card | deploy |
| `reviewer-agent` | `REVIEWER_BIND` :9102 | `/healthz`, Agent Card | deploy |
| portal (Astro SSR, `node dist/server/entry.mjs`) | `HOST`/`PORT` 4321 | none (needs `/healthz`, A4) | deploy |
| `role-agents` | :9111 | – | **not deployed** |
| `mcp-server` | :8090, **loopback enforced** | – | **not deployed** |

Why the engineering stack (role-agents + mcp-server, `ENG_PROVIDER=a2a`) is excluded from the pilot:
1. `mcp-server` refuses non-loopback binds (`apps/mcp-server/src/main.rs:31`), so it cannot be a separate Service.
2. `testing.run` uses `DockerRunner`, which needs a Docker daemon. Mounting a container runtime socket into a pod is a host-root escape; prohibited here.
3. ADR-004 amendment (owner D3): LLM-written code runs in Docker only for dev fixture runs on the macOS host; real repos need a microVM. No such boundary exists on a K3s node.
4. Approvals are BLOCKED without an IdP, so engineering workflows cannot pass the plan gate anyway.

The pilot runs `ENG_PROVIDER=none`. Engineering pages stay readable; writes answer 503 as today.

### Deployment blockers and required (small, additive) code changes

| ID | Finding | Location | Impact | Fix |
|---|---|---|---|---|
| A1 | Every binary waits only for `ctrl_c()` (SIGINT). Kubernetes sends SIGTERM; a PID-1 process without a SIGTERM handler ignores it → killed after the grace period, in-flight workflows marked `interrupted` on next start | `api/src/main.rs`, `agent-runtime/src/server.rs:serve`, `role-agents`, `mcp-server` | Every deploy/rollback interrupts work | Shared `shutdown_signal()` (SIGTERM ∪ SIGINT) |
| A2 | Agents log plain text; only `api` honours `LOG_FORMAT=json` | `agent-runtime::init_tracing` | Loki parsing | Honour `LOG_FORMAT` |
| A3 | No `/metrics` anywhere | all services | No app metrics | 7E: Prometheus endpoint on `api` (route template + status class labels only) |
| A4 | Portal has no health route; `/` calls the API, so probing it couples portal health to the API | `frontend/src/pages` | Probes | `src/pages/healthz.ts` returning 200 |
| A5 | `PORTAL_HOSTS` (Astro `allowedDomains`, CSRF origin check) is **build-time** | `frontend/astro.config.mjs` | One image per hostname set breaks "promote the same digest" | Build once with every environment's hostname in the list (needs hostnames, D4) |
| A6 | `InstanceLock` (`pg_try_advisory_lock`) allows one API process per DB; a second exits | `api/src/db.rs:157` | RollingUpdate would crash-loop the new pod | `replicas: 1`, `strategy: Recreate`; Supabase **direct or session pooler only** (never transaction pooler) |
| A7 | An older `api` refuses to start against a DB with migrations it does not know | `api/src/db.rs:143` | Image rollback across a migration fails closed (CrashLoop) | Rollback procedure §10; migrations stay additive |
| A8 | Remote DB requires `sslmode=verify-full` + `sslrootcert` | `api/src/config.rs:73` | Supabase CA file must be mounted | ConfigMap (CA cert is public) |

None of these change REST, A2A, MCP contracts, migrations or approval semantics.

### Existing CI gaps
`ci.yml` pins actions by tag (`@v4`, `@stable`, `@v2`), not SHA; no Docker build, image scan, Rust advisory scan, gitleaks, provenance or publishing; no e2e.

### Local tooling (MEASURED, this Mac)
Docker 29.9.2 (arm64, buildx with `linux/amd64` emulation), kubectl, helm, kind, gitleaks, gh present. **Missing:** kustomize (kubectl has `-k`), trivy, terraform/tofu, ansible, kubeconform, actionlint, hadolint, cargo-audit/deny, cosign. Host: 8 CPU, 16 GiB, load avg ~120–130. Local amd64 builds under QEMU will be slow; CI (ubuntu amd64) is the authoritative amd64 build.

### Target infrastructure
**None known.** No VPS, provider, region, DNS domain or Supabase plan is recorded in the repo. Terraform, Ansible execution, K3s install and backup design depend on D1–D3.

## 3. High-level deployment design

```
operator ──VPN (WireGuard/Tailscale)──▶ node:443 Traefik ──▶ portal ──▶ api ──▶ worker-agent ─┐
                                        node:6443 (VPN only)          │     └─▶ reviewer-agent ┤
                                                                      ▼                        ▼
                                                     Supabase Postgres (TLS verify-full)   LLM provider :443
GitHub Actions ─▶ GHCR (digest) ─▶ GitOps PR ─▶ human review ─▶ merge ─▶ Argo CD (pull) ─▶ K3s
```

- One K3s node, private pilot. Not highly available. Namespaces are not a security boundary between environments; staging/production on the same node would not be isolated (§7).
- Only the portal is routed by Ingress; api and agents are ClusterIP only. Firewall exposes 443/6443 only on the VPN interface; SSH restricted to VPN / admin source.
- Supabase stays external; no in-cluster Postgres.

## 4. Low-level deployment design

**Namespaces:** `a2a-dev` (pilot), later `a2a-staging`, `a2a-prod`; `argocd`, `sealed-secrets` (kube-system by default), `monitoring`. App namespaces labelled `pod-security.kubernetes.io/enforce=restricted`.

**Per workload:** Deployment, ClusterIP Service, dedicated ServiceAccount with `automountServiceAccountToken: false` (no workload calls the Kubernetes API, so no Role/RoleBinding), `runAsNonRoot`, `readOnlyRootFilesystem`, `allowPrivilegeEscalation: false`, `capabilities.drop: [ALL]`, `seccompProfile: RuntimeDefault`, `emptyDir` `/tmp` only where needed.

| Workload | Port | Startup / readiness / liveness | Requests → limits (ESTIMATE, to be measured in 7B) |
|---|---|---|---|
| api | 8080 | startup+liveness `/healthz`, readiness `/api/health` (DB) | 50m/64Mi → 500m/256Mi |
| worker, reviewer | 9101/9102 | `/healthz` (all three) | 20m/32Mi → 250m/128Mi |
| portal | 4321 | `/healthz` (A4) | 50m/96Mi → 500m/256Mi |

Readiness of the api reflects the DB only. It does not depend on agents; agent outages surface as failed workflows and in `/api/agents`, avoiding a startup-order coupling.

**Migrations:** a `Job` running `api migrate` with `DATABASE_MIGRATION_URL` (schema-owner role, separate Secret from the runtime role). Dev: Argo CD `PreSync` hook. Staging/prod: same hook, but only reached through a manually approved sync. `api serve` already verifies the schema and refuses to start otherwise.

**NetworkPolicy** (K3s enforces NetworkPolicy by default via its embedded controller; verify on the node): default-deny ingress+egress per namespace, then: Traefik → portal:4321; portal → api:8080; api → worker:9101, reviewer:9102; all → kube-dns:53; api → Supabase:5432; worker/reviewer → LLM provider:443. Limitation: vanilla NetworkPolicy is IP-only, so Supabase/provider egress is `0.0.0.0/0` on that port (or pinned CIDRs that can drift). FQDN egress needs Cilium; not proposed for the pilot.

**Images** (one multi-target `backend/Dockerfile`, one `frontend/Dockerfile`): Rust builder on a digest-pinned `rust:<1.100>-bookworm`, `--locked --release`; runtime `gcr.io/distroless/cc-debian12:nonroot` by digest (rustls everywhere, no OpenSSL; verify with `ldd`). Portal: build on `node:24.21.0-bookworm-slim`, runtime distroless Node 24 if a published variant exists at implementation, else `node:24.21.0-bookworm-slim` with non-root user; production deps only. `linux/amd64` published; arm64 only for local use.

**Kustomize:** `base/{portal,api,agents}` + `overlays/{dev,staging,production}`. Overlays set only namespace, `images:` digests, hostnames, replica/resource deltas and `INFERENCE_MODE`.

## 5. Threat model and trust boundaries

| Boundary | Threat | Control |
|---|---|---|
| Internet → node | Unauthenticated portal/API use, LLM cost abuse, data read (S3) | No public ingress; VPN only; firewall default-deny |
| Operator → K8s API / Argo CD / SSH | Credential theft, lateral admin | 6443 and Argo UI on VPN only; SSH keys only, no root login; Argo local admin disabled after SSO or kept with rotated password; kubeconfig mode 600 |
| CI → registry/GitOps | Malicious PR obtains write tokens | `pull_request` jobs read-only, no secrets; publish + GitOps PR only on `push` to `main`; SHA-pinned actions; `GITHUB_TOKEN` minimal scopes per job |
| GitOps repo → cluster | Unreviewed manifest change reaches prod | Branch protection + CODEOWNERS on `overlays/production`, `argocd/`; Argo `AppProject` restricts repo, namespaces, cluster-scoped kinds |
| Pod → pod | Compromised agent pivots | NetworkPolicy allow-list; no SA tokens; restricted PSA |
| Pod → host | Container escape | Non-root, no caps, seccomp, RO rootfs; **no code execution workloads on the node** |
| App → Supabase | MITM, credential leak | `verify-full`; runtime role vs migration role vs (future) MCP role in separate Secrets |
| Secrets in Git | Plaintext leak | Sealed Secrets only; gitleaks + `check-secrets.sh` in CI |
| Logs | Key/prompt leakage | Existing redaction (`Config` Debug, `RedactingTaskStore`); no prompt bodies at info; review in 7E |

Alert correlation: an unknown source IP alone is not an incident; correlate with auth/audit events (PRD §5).

## 6. Resource sizing and cost

All values are ESTIMATES until 7B measures image size and idle/peak RSS with `docker stats`.

| Component | RAM (est.) | Notes |
|---|---:|---|
| OS + K3s server (+ Traefik, CoreDNS, metrics-server) | 0.8–1.2 GiB | check against current K3s requirements |
| Pilot app (4 pods) | 0.2–0.4 GiB | P0 allows one active workflow at a time |
| Argo CD (non-HA, no Dex if unused) | 0.5–1.0 GiB | |
| Sealed Secrets controller | <0.1 GiB | |
| Prometheus + Grafana + Loki (single binary) + Alloy | 1.0–2.0 GiB | retention-dependent |

Profiles:
- **Minimal:** 2 vCPU / 4 GiB / 40 GB SSD. K3s + app + Argo CD + Sealed Secrets; metrics/logs shipped to an external hosted backend (owner to choose and verify plan/pricing) or only `kubectl top` + K3s logs.
- **Recommended self-hosted:** 4 vCPU / 8 GiB / 80 GB SSD with the full monitoring stack on the node.

Network: low (LLM calls of a few KB–100 KB each; image pulls ~tens of MB per release, ESTIMATE). Cost: not estimated; depends on the provider chosen in D1. No free-tier or pricing claims are made.

## 7. Environment strategy

| Env | Status | Sync | Inference | Data |
|---|---|---|---|---|
| dev | pilot target | Argo auto-sync, prune on, self-heal on | `mock` default; real key only with the spend cap (owner D2 from Phase 6C) | synthetic; separate Supabase project or schema |
| staging | template, UNVERIFIED | manual sync | real provider, staging key | separate Supabase project, no prod data |
| production | template, UNVERIFIED | manual sync, prune off | prod key | prod Supabase |

Staging/production on the same single node as dev is not isolated and is not claimed. Each needs its own Supabase project and credentials; real isolation means separate nodes/clusters.

## 8. Repository structure (to create)

```
backend/Dockerfile                  frontend/Dockerfile      (+ .dockerignore each)
.github/workflows/ci.yml            (hardened)  release.yml (main only: build, scan, push, attest, GitOps PR)
infrastructure/
  terraform/        (only if D1 has a supported provider)
  ansible/          inventory.example.yml, site.yml, roles/{base,ssh,firewall,k3s}
  kubernetes/base/{portal,api,agents}   overlays/{dev,staging,production}
  argocd/{projects,applications}        (bootstrap: install pinned Argo CD manifest once)
  observability/    (values/config for the chosen profile)
  scripts/          validate.sh (kustomize build | kubeconform), promote.sh (copy digest between overlays)
docs/deployment/    architecture, local-containers, ci-cd, k3s-setup, gitops, secrets, monitoring, backup-restore, rollback, security
```

GitOps lives in this repo for the pilot (simplest; protected paths via CODEOWNERS). A separate GitOps repo gives stronger access separation and is the upgrade path.

## 9. Implementation backlog

| ID | Phase | Item | Needs authorization? |
|---|---|---|---|
| V0 | 7A | Re-run Rust/Astro baselines on a private DB (memory: `:54339`, scratch target dir) | no |
| A1, A2, A4 | 7B | SIGTERM handling, `LOG_FORMAT` for agents, portal `/healthz` (+ tests) | no |
| B1 | 7B | Dockerfiles + `.dockerignore`; local build arm64; amd64 build; run with `--read-only --cap-drop ALL --user` and probe; compose smoke of portal→api→agents against local Postgres | no |
| B2 | 7B | CI hardening: SHA-pinned actions, cargo-deny, gitleaks, Trivy fs/config/image, image build on PR (no push) | no (repo file) |
| B3 | 7B | `release.yml`: push to GHCR by digest on `main`, `actions/attest-build-provenance`, release metadata JSON, GitOps PR to `overlays/dev` | **yes**: first live publish; needs GitHub App or PAT for PRs that trigger CI (PRs opened with `GITHUB_TOKEN` do not run workflows) |
| C1 | 7C | Kustomize base/overlays, NetworkPolicies, PSA labels; `kubectl kustomize` + kubeconform + Trivy config | no |
| C2 | 7C | Local cluster test (kind, or k3d/K3s in Docker) of manifests, policies, probes | no |
| C3 | 7C | Terraform (provider per D1): `validate`/`plan` only | apply: **yes** |
| C4 | 7C | Ansible roles; `--syntax-check`, `ansible-lint`, run against a local VM/container | remote run: **yes** |
| C5 | 7C | K3s install config: pinned version, `--secrets-encryption`, `--write-kubeconfig-mode 600`, API on VPN address, `--cluster-init` for `etcd-snapshot` | install: **yes** |
| D1 | 7D | Argo CD AppProject (repo, namespaces, no cluster-scoped kinds for apps), Applications per overlay; test in local cluster | remote install: **yes** |
| D2 | 7D | Sealed Secrets controller; sealing script; key backup procedure | live secrets: **yes** |
| E1 | 7E | `/metrics` on api (A3), alert rules, dashboards; profile per D3 | no (code) |
| E2 | 7E | Backup: K3s etcd snapshots off-node, sealed-secrets key offline, Supabase backup per plan + logical dump; restore test into a disposable Postgres | live: **yes** |
| F1 | 7F | Test matrix A–G on local cluster; repeat on remote when authorized | remote: **yes** |
| — | docs | `docs/deployment/*` for implemented parts only | no |

## 10. Validation and rollback strategy

**Validation (offline, no authorization needed):** `cargo`/`npm` suites; `docker build` both arches; container run under the target security settings; `kubectl kustomize overlays/*` → kubeconform (strict) + Trivy config; NetworkPolicy tests in kind with a policy-enforcing CNI (deny portal→agent, deny agent→api, allow the listed paths); Argo CD in the local cluster syncing from a local branch to prove auto-sync, drift self-heal and manual-sync gating; `actionlint` on workflows.

**Rollback:**
1. Revert the digest change in the overlay via a GitOps PR (no rebuild); Argo reconciles.
2. If the target image predates an applied migration, the api **refuses to start** (A7). Then either forward-fix with a new image, or restore the DB to a pre-migration backup (data loss window) before rolling back. Migrations are not reversed by image rollback.
3. In-flight workflows: with A1, the api stops accepting work on SIGTERM; anything still running is marked `interrupted` by `recover_interrupted` on start (existing behaviour). Approval records are append-only and untouched by rollback.
4. Argo prune: on for dev, off for staging/production (manual prune after review).

## 11. Decisions needed from the owner

| ID | Decision | Recommendation |
|---|---|---|
| D0 | Phase order: 6F records that 6C/6D precede Phase 7. Start Phase 7 now? | Proceed with offline 7B–7D for the private dev pilot only (no public ingress, no approvals, no code execution); keep 6D as a prerequisite for any public or approval-enabled deployment |
| D1 | Target host/provider and region (determines Terraform provider, sizing, cost) | Any provider with an official Terraform provider; 4 vCPU / 8 GiB if self-hosting monitoring |
| D2 | Private access method | WireGuard or Tailscale; no public 80/443 |
| D3 | Observability profile | Minimal profile first; self-host the full stack only on ≥ 8 GiB |
| D4 | Hostnames per environment (A5 build-time list) and TLS source | Private DNS names; cert-manager DNS-01 if the DNS provider is supported, else an internal CA |
| D5 | Secrets mechanism | Sealed Secrets (no external secret manager required; Argo-compatible). Lost controller key = every SealedSecret must be re-created from source values |
| D6 | GitOps location | Same repo with CODEOWNERS on protected paths for the pilot |
| D7 | Supabase plan / projects for dev (backup and PITR capability depends on plan) | Separate dev project; confirm backup features in the dashboard |
