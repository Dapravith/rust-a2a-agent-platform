# Phase 6D + 6E: Phase 6C audit, design and corrective backlog

Date: 2026-11-18 · Branch `phase6-c/real-multi-provider-collaboration` @ `449603e` + 1 uncommitted edit · Rust 1.100.0 · Node 24.21.0 · Postgres 17 (private test container `a2a-6b-testdb`, `127.0.0.1:54339`)

**Verdict: Phase 6C FAILS its mandatory gates (not started; blocked by unfinished Phase 6B items B2–B7 and by missing provider credentials). Following the 6D/6E instruction ("if mandatory Phase 6C gates fail, stop and provide a prioritized corrective backlog"), no Phase 6D or 6E code was written.** This document is the audit (§1), the design that the backlog implements (§2–§9), the backlog (§10) and the test plan (§11). Quality gates D and E are both **NOT STARTED** (§12).

---

## 1. Phase 6C technical audit (Step 1)

Verification ran on a detached worktree of `HEAD` (`449603e`) in a scratch directory with its own `CARGO_TARGET_DIR`, so the uncommitted edit in the live checkout was left untouched.

| # | Check | Result | Evidence (this session) |
|---|---|---|---|
| 1 | Git status / structure | **FAIL (working tree)** / PASS (HEAD) | One uncommitted edit, `apps/api/src/eng/providers/anthropic.rs:189`: `replace(a"</", …)` — a stray `a`. `cargo check -p api` on the working tree: `E0425 cannot find value a`, the `api` crate does not compile. Looks like an accidental keystroke; not reverted by this audit (owner's file) |
| 2 | PRD / ADRs | PASS | `A2A_PRD.md` §9 (human gates, IdP required, delivery boundary); ADR-003, ADR-004 |
| 3 | Rust workspace builds (HEAD) | see §1a | `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace` |
| 4 | Astro checks (HEAD) | PASS | `astro check`: 55 files, 0 errors, 0 warnings, 0 hints. `npm test`: 31/31. Playwright not run |
| 5 | A2A interoperability | see §1a | `worker-agent/tests/a2a.rs`, `reviewer-agent/tests/a2a.rs`, `api/tests/phase6b.rs` |
| 6 | MCP tool authorization | see §1a | `mcp-server/tests/mcp.rs` (incl. B1 `workspace.apply_patch`) |
| 7 | Provider adapter contracts | PARTIAL | Only `eng/providers/anthropic.rs` exists, tested against an HTTP test double (`phase6_provider.rs`). No provider registry, no Codex / Gemini / Grok / Claude Code adapter |
| 8 | ≥ 2 actual provider integrations | **FAIL / BLOCKED** | No provider API key in env or `.env` (only `ANTHROPIC_BASE_URL` is set; no `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `XAI_API_KEY`). Blocker D1 in `PHASE6C.md` §4 is still open. No live provider call has ever run in this repo |
| 9 | Isolated engineering workflow | **PARTIAL** | Only the Code Reviewer is an A2A role agent. PM, Coordinator, Developer, QA, QL are in-process simulation (`eng/sim.rs`). `workspace.apply_patch` exists (B1) but no Developer agent calls it; workspace mapping is still `workspace_id = workflow_id` (`eng/a2a_role.rs:13`, B2 open); the MCP token is still in the A2A message (`RoleTask.mcp_token`, B5 open) |
| 10 | Persistence and audit | PASS (by design review + tests in §1a) | `eng_approvals`, `eng_audit_events`, `eng_artifacts`, `eng_tool_calls` append-only via triggers (incl. TRUNCATE) and grants; one decision per `(workflow_id, workflow_version)` |

### 1a. Regression results on HEAD

Environment: `CARGO_TARGET_DIR` in scratch, `DATABASE_URL=postgres://…@127.0.0.1:54339/postgres` (private container), host load average ~100.

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS (finished, no warnings) |
| `cargo test --workspace` | **PASS: 184 passed, 0 failed, 1 ignored** (`live_sandbox_runs_checks_without_network`, needs Docker opt-in) |

Per suite: agent-runtime 4+1+11 · api unit 8 · `phase3` 17/17 (40 s) · `phase6` 21/21 · `phase6_provider` 9/9 (HTTP test double, not a live provider) · `phase6b` 6/6 (Code Reviewer over real A2A → MCP) · `workflow` 25/25 · contracts 8 · engineering 32 · `mcp-server/tests/mcp.rs` 12/12 (incl. B1 `apply_patch`) · reviewer-agent 2 + A2A 5/5 · worker-agent 3 + A2A 5/5 · role-agents 3 · workspace 12 (+1 ignored).

So checks 3, 5, 6 and 10 PASS on HEAD. The committed foundation is green; what fails is **scope** (rows 7–9: no second provider, no live provider, 5 of 6 roles simulated), plus the uncommitted compile error in the working tree.

### 1b. What Phase 6C claims vs. what exists

`docs/PHASE6C.md` (committed in `449603e`) already reports **"Phase 6C: NOT STARTED … Phase 6D readiness: FAIL"**. This audit confirms it independently: the repository's own 6C gate lists ≥ 2 live providers as BLOCKED, code changes in a restricted workspace by an agent as BLOCKED, and the Astro provider view as FAIL. Nothing since then changes those rows: the only later code is B1 (`apply_patch`) and the broken uncommitted edit.

### 1c. Existing assets Phase 6D can reuse (verified by reading the code)

| Need | Existing asset | Gap |
|---|---|---|
| Server-side identity seam | `eng/auth.rs` `Authenticator` trait, `Principal{subject, issuer, roles}`; request bodies have no identity field (`deny_unknown_fields`) | Only `Disabled` in production; no OIDC, no sessions, no logout, no CSRF token for the API |
| Roles | `state::HumanRole { Requester, Approver }` | 2 roles, global; spec needs 8 roles, project-scoped |
| Plan + delivery approvals | `eng/approval.rs`: row lock, version check, plan-hash / manifest-hash / candidate-SHA binding, gates must pass, idempotency key + request hash, comment required for changes/reject | No merge-authorization gate; no project scope; no auth-context reference |
| Append-only approvals/audit | migration `20261117000003` triggers + grants + RLS | Fine; extend with new tables only |
| State machine | `crates/engineering::state` (12 states, actor rules, budgets) | No `awaiting_merge_authorization` / `merged`; PR lifecycle states absent (they belong in a separate delivery lifecycle, §5) |
| CI | `.github/workflows/ci.yml`: fmt, clippy, tests (Rust); npm audit, unit, build, secret scan (frontend) | Job names are the check names the platform will require; no SAST/dependency scan for Rust (`cargo audit`/`cargo deny`) yet |
| Portal | Astro SSR (`output: "server"`), browser never calls Rust, `checkOrigin: true`, strict CSP | No auth pages, no approval inbox (B6 also open) |

---

## 2. Gap analysis (6D/6E vs. today)

| Area | Required | Today | Gap size |
|---|---|---|---|
| OIDC login / logout / sessions | Auth Code + PKCE, server-side validation, revocation | none | Large |
| RBAC | 8 roles, project-scoped, multi-role | 2 global roles | Medium |
| Approval types | plan, delivery, merge | plan, delivery | Small (merge gate) |
| Approval integrity | role, auth context, manifest hash, SHA, version | all except auth-context ref and project | Small |
| Delivery candidate manifest | immutable, includes CI + PR identity | manifest hash over review/QA/assessment only | Medium |
| GitHub App | JWT, installation tokens, allowlist | none | Large |
| PR lifecycle | 9 states + 6 side states | none | Large |
| Webhooks | HMAC, dedup, reconcile | none | Medium |
| CI evidence | commit-bound, source-verified | none (QA evidence is sandbox-only) | Medium |
| Branch protection validation | read + enforce policy | none | Medium |
| Merge | disabled-by-default, SHA-pinned | none | Medium |
| Portal | inbox, plan review, PR dashboard, delivery review, timeline | read-only workflow pages; B6 open | Large |
| **Prerequisite** | real agents produce a real candidate | simulation for 5/6 roles; no live provider | **Blocking** |

---

## 3. Threat model and trust boundaries

```
                  ┌──────────────── TB1: internet / browser (untrusted) ───────────────┐
 Human ──browser──►  Astro SSR portal  (session cookie __Host-a2a_sid, CSRF token)      │
                  └──────────┬──────────────────────────────────────────────────────────┘
                             │ server-side fetch, cookie forwarded, private network
            ┌────────────────▼──────────────── TB2: trusted Rust API (modular monolith) ─┐
            │ auth (OIDC RP, sessions) ─ rbac ─ approval engine ─ orchestrator ─ delivery │
            │                   │                         │              │  (GitHub App) │
            └───────┬───────────┼─────────────────────────┼──────────────┼───────────────┘
                    │ SQL       │ OIDC (TLS, JWKS)        │ A2A JSON-RPC │ HTTPS, installation token
               Postgres    IdP (TB3, trusted     ┌────────▼───────┐  ┌──▼──────────────────┐
             (TB2 data)    for identity only)    │ role agents    │  │ GitHub (TB5):        │
                                                 │ + LLM providers│  │ authoritative for    │
                                                 │ (TB4 untrusted │  │ repo, PR, CI, review │
                                                 │ output) ─MCP──►│  │ webhooks = hints only│
                                                 │ mcp-server     │  └─────────────────────┘
                                                 └────────────────┘
```

| ID | Threat (STRIDE) | Boundary | Control |
|---|---|---|---|
| T1 | Spoofed human (forged header, body field, LLM text claiming "admin") | TB1→TB2 | Identity only from a server-side session created after OIDC ID-token validation (iss, aud, signature via JWKS, exp, nonce); no identity field in any body; roles from `project_memberships`, never from claims or prompts |
| T2 | Session theft / fixation | TB1 | `__Host-` cookie, `HttpOnly; Secure; SameSite=Lax; Path=/`; random 256-bit id, only SHA-256 stored; rotate on login; idle + absolute expiry; server-side revoke on logout |
| T3 | CSRF on decisions | TB1 | Astro `checkOrigin`, plus a per-session CSRF token (double-submit, header `X-CSRF-Token`) checked by Rust on every mutation; SameSite=Lax |
| T4 | Agent self-approval / impersonation | TB4→TB2 | `Actor::Agent` can never satisfy `Required::Human`; agents have no session; MCP exposes no approval/merge tool; human and agent ids live in different tables and namespaces |
| T5 | Stale / conflicting approval | TB2 | Row lock, `expected_version`, UNIQUE `(workflow_id, workflow_version)`, idempotency key + request hash |
| T6 | Candidate swapped after approval | TB2/TB5 | Approval binds manifest hash + head SHA; any PR `synchronize` → new manifest, old approvals stale; merge uses `sha=` precondition |
| T7 | Forged / replayed webhook | TB5→TB2 | HMAC-SHA256 (`X-Hub-Signature-256`, constant-time), 1 MiB cap, delivery-id dedup, repo allowlist, then **reconcile through the API** (payload never final) |
| T8 | Lying CI evidence (agent says "CI passed") | TB4 | CI evidence only from GitHub Checks API under the app token, matched on repo id + head SHA + check name + `app.id` of the expected source |
| T9 | Over-privileged automation | TB2→TB5 | GitHub App with minimal permissions; installation token scoped to one repo id; app is not a bypass actor; repo allowlist in config + DB |
| T10 | Protected-branch push / force-push | TB5 | Pushes only to `agent/<workflow>/<n>`; refuse any ref matching the protected set; GitHub ruleset as independent control |
| T11 | Secret leakage (tokens, keys) | all | Private key from file with 0600 check, never logged; tokens wrapped in a redacting type; no credential in A2A messages or prompts; secret scan in CI |
| T12 | Prompt injection triggering delivery ops | TB4 | Delivery module is not reachable from agents; all privileged calls take a `HumanSession`, not text |
| T13 | GitHub unavailable / uncertain write | TB5 | Typed errors; no blind retry of non-idempotent writes (re-read state first); workflow pauses in a blocked state |
| T14 | Audit tampering | TB2 | Append-only triggers + grants (existing), new tables follow the same pattern |

---

## 4. High-level design (HLD)

All new code lives in the existing `api` crate as modules (modular monolith); one new pure crate is not needed — policy types go into `crates/engineering`.

| Module | Responsibility |
|---|---|
| `api::identity` (new) | OIDC relying party (`/api/v1/auth/login`, `/callback`, `/logout`, `/me`), sessions, CSRF; implements the existing `Authenticator` trait (`OidcSession`) so `eng/approval.rs` is unchanged at the seam |
| `api::rbac` (new) | Project memberships, permission check `authorize(principal, project, Operation, resource_state) -> Result<(), Denied>`; one table-driven matrix |
| `eng::approval` (extend) | Adds `GateKind::Merge`; records auth-context ref and project |
| `api::delivery` (new) | GitHub App client (narrow `reqwest` adapter), webhook receiver, reconciler, candidate manifest, merge-eligibility, controlled merge (feature-flagged) |
| `engineering::delivery` (new, pure) | PR lifecycle state machine and merge-eligibility rules (unit-testable, no I/O) |
| Portal | `/login`, `/approvals`, `/workflows/[id]/plan`, `/workflows/[id]/delivery`, `/workflows/[id]/pr`, `/workflows/[id]/timeline` |

Flow: human → portal → Rust (session) → orchestrator drives agents over A2A/MCP → candidate committed to `agent/<wf>/<n>` in the **workspace**; `delivery` pushes that exact commit to GitHub, opens a draft PR, then waits for webhooks → reconcile → evidence → manifest → human delivery decision → separate human merge authorization → (flag on) controlled merge → confirm from GitHub.

---

## 5. Low-level design (LLD)

### 5.1 Identity

- Crate: `openidconnect` (Rust; Authorization Code + PKCE S256, nonce, JWKS discovery). Exact version pinned at implementation time.
- Rust API is a **confidential** client (BFF). Client secret stays in the API env. Astro proxies `/auth/*` to Rust and forwards `Set-Cookie`.
- `login`: create `oidc_login_attempts` row (state, PKCE verifier, nonce, return path allow-listed to same origin, 10 min TTL) → 302 to IdP.
- `callback`: single-use state; exchange code; validate ID token (iss, aud == client id, `azp` if multiple aud, signature, exp, iat skew ≤ 60 s, nonce); upsert `users (issuer, subject)`; **ignore email/name for authorization**; create session; rotate id.
- Session: `user_sessions(id_hash, user_id, csrf_token_hash, created_at, last_seen_at, idle_expires_at, absolute_expires_at, revoked_at, auth_time, amr, id_token_sid)`. Idle 30 min, absolute 8 h. Logout revokes server-side and calls the IdP end-session endpoint when advertised; back-channel logout (`sid`) revokes when the IdP supports it.
- Approval decisions additionally require `auth_time` within 12 h (configurable); otherwise `403 reauth_required`.

### 5.2 RBAC

Roles (`human_role` enum): `viewer, developer, code_reviewer, qa_tester, project_manager, project_coordinator, approver, administrator`. Stored in `project_memberships (user_id, project_id, role)`; administrators assign them; IdP group mapping is optional and only *suggests* memberships (an admin confirms). Mapping to the existing engine: `HumanRole::Approver` ⇐ `approver`; `HumanRole::Requester` ⇐ any non-viewer.

| Operation | Allowed human roles (project-scoped) | Extra conditions |
|---|---|---|
| `view_workflow` | any membership | — |
| `submit_requirements` | developer, project_manager, project_coordinator, approver, administrator | — |
| `decide_plan` | approver | state `awaiting_plan_approval`; version + plan hash match |
| `decide_delivery` | approver | state `awaiting_delivery_approval`; manifest hash + SHA match; gates pass; **not the workflow requester** (separation of duties, configurable) |
| `authorize_merge` | approver | state `awaiting_merge_authorization`; different decision record; eligibility fresh |
| `execute_merge` | approver | `DELIVERY_MERGE_ENABLED=true` and repo policy verified |
| `request_pr_creation` | approver, project_coordinator | plan approved, candidate exists, repo allowlisted |
| `cancel` | requester (own) or approver | non-terminal |
| `manage_memberships` | administrator | cannot grant self `approver` on a project where they requested the workflow |
| `record_ci_evidence` | **nobody** (reconciler only) | — |

Administrator does **not** imply approver.

### 5.3 Approval state machine (application states, not A2A task states)

```
draft ─► awaiting_plan_approval ─approve─► approved_for_execution ─► executing ─► reviewing ─► testing
             │ request_changes ─► draft (bounded)        │ reject ─► rejected
             ▼
awaiting_delivery_approval ◄─ (manifest built, all gates + CI pass)
    │ approve ─► awaiting_merge_authorization ─authorize─► merge_authorized ─(flag on, eligible)─► merged
    │ request_changes ─► changes_requested ─► executing (bounded)        │ deny ─► approved_for_delivery (manual merge path)
    │ reject ─► rejected                                                  │ ineligible ─► merge_blocked
    └ candidate changed (PR synchronize) ─► reviewing   (all approvals for old manifest are stale)
any non-terminal ─cancel─► cancelled ; system error ─► failed
```

New states: `awaiting_merge_authorization`, `merge_authorized`, `merge_blocked`, `merged`. `approved_for_delivery` remains a terminal-for-6D state for the manual-merge default.

### 5.4 PR lifecycle (separate column `pull_request_links.lifecycle`)

`candidate_prepared → draft_pr_created → ci_running → review_in_progress → ready_for_human_review → awaiting_delivery_approval → approved_for_delivery → awaiting_merge_authorization → merged`, side states `changes_requested, ci_failed, review_blocked, merge_blocked, rejected, cancelled`. Transitions are a pure function in `engineering::delivery::next(lifecycle, GithubFact) -> Result<Lifecycle>`, where `GithubFact` is always derived from a **reconciled API read**, never from a webhook payload. PR closed without merge → `cancelled` (never `merged`).

### 5.5 GitHub client

Narrow `reqwest` adapter (`api::delivery::github`), typed `GithubError { Unauthorized, NotFound, Conflict, Validation, RateLimited{reset}, Unavailable, Uncertain }`. Calls used: `GET /app/installations/{id}`, `POST /app/installations/{id}/access_tokens` (with `repository_ids:[id]` and narrowed `permissions`), `GET /repos/{o}/{r}`, `GET /repos/{o}/{r}/branches/{b}/protection` and `GET /repos/{o}/{r}/rules/branches/{b}`, `POST /repos/{o}/{r}/git/refs` (feature branch), `POST /repos/{o}/{r}/pulls` (`draft: true`), `GET /repos/{o}/{r}/pulls/{n}`, `GET /repos/{o}/{r}/pulls/{n}/reviews`, `GET /repos/{o}/{r}/commits/{sha}/check-runs`, `PUT /repos/{o}/{r}/pulls/{n}/merge` with `sha`. App JWT: RS256, `iat = now-60`, `exp ≤ now+9 min`, `iss = app id`. Pushing the candidate commit: `git push` over HTTPS with the installation token in a credential helper env (never in argv or logs), refspec `<sha>:refs/heads/agent/<wf>/<n>`, no `--force`; refused locally if the ref matches a protected pattern.

### 5.6 Webhooks

`POST /api/v1/github/webhooks`: body limit 1 MiB; verify `X-Hub-Signature-256` (HMAC-SHA256, constant-time) before parsing; require `X-GitHub-Event ∈ {pull_request, pull_request_review, check_run, check_suite}` and `X-GitHub-Delivery`; insert into `github_webhook_deliveries` (UNIQUE delivery id → duplicate = 200, no-op); repository id must be allowlisted; enqueue a reconcile for `(repo, pr)` and return 202. The reconciler re-reads PR, reviews and check runs, so out-of-order events converge.

---

## 6. Authentication and RBAC design summary

See §5.1–5.2. Production default stays `Disabled` until `OIDC_ISSUER_URL`, `OIDC_CLIENT_ID`, `OIDC_CLIENT_SECRET`, `OIDC_REDIRECT_URL` are configured; the API refuses to start if only some are set. Tests use a local OIDC test issuer (in-process, RSA key generated per test) so that signature, issuer, audience, expiry and nonce checks run against real JWTs, not a stubbed `Authenticator`.

## 7. GitHub App and CI design

**App permissions (minimum):** Metadata: read; Contents: read & write (feature branches; GitHub cannot scope this per branch, so protection rules are the independent control); Pull requests: read & write; Checks: read; Commit statuses: read; Administration: read (only to read branch protection — if the owner prefers not to grant it, merge stays disabled and protection is reported UNVERIFIED). No Workflows permission (agents cannot change `.github/workflows`). **Events:** `pull_request`, `pull_request_review`, `check_run`, `check_suite`.

**Required checks** (names = existing CI job names, plus additions): `backend` (fmt, clippy, tests, build), `frontend` (audit, unit, build, secret scan), new `security` job (`cargo deny check advisories`, gitleaks or the existing `scripts/check-secrets.sh`). Evidence is accepted only when `check_run.app.slug == "github-actions"`, `head_sha == candidate SHA`, repository id matches, and `status == completed`; conclusions `success` pass; `skipped`/`neutral` pass only if the policy marks the check optional; anything else (`failure, cancelled, timed_out, action_required, stale`, pending, missing) blocks.

**Branch protection policy check** (before merge eligibility; result persisted): PR required; required status checks ⊇ policy list with `strict` (up to date); required approving reviews ≥ policy (default 1) with `dismiss_stale_reviews`; no bypass actors including the app; force-push and deletion disabled; protected patterns cover `main` and `release/*`. If any read fails → `protection_verified = false` → merge disabled.

## 8. Database schema changes (new migration `20261119000001_identity_delivery.sql`)

All new tables append-only unless marked; UUID v7 ids; `timestamptz`; RLS + least-privilege grants like `…000003`.

| Table | Key columns | Notes |
|---|---|---|
| `users` | id, issuer, subject, display_name, created_at | UNIQUE (issuer, subject); mutable display name only |
| `projects` | id, slug, name | existing workflows get a default project |
| `project_memberships` | user_id, project_id, role, granted_by, granted_at, revoked_at | UNIQUE active (user, project, role); revocation is an insert of `revoked_at` via a new row (append-only history) |
| `user_sessions` | id_hash, user_id, csrf_hash, auth_time, idle/absolute expiry, revoked_at | mutable (last_seen, revoked) — the one updatable table |
| `oidc_login_attempts` | state_hash, pkce_verifier (encrypted), nonce, expires_at, consumed_at | single use |
| `eng_approvals` (alter) | + `project_id`, + `session_ref`, + `approver_user_id`, gate adds `merge`, decision adds `authorize`/`deny` | existing CHECKs extended |
| `github_installations` | installation_id, account, app_id, repository_ids allowlist | no tokens stored |
| `pull_request_links` | workflow_id, repo_id, owner/name, number, head_ref, head_sha, base_ref, lifecycle, last_reconciled_at | UNIQUE (workflow_id), UNIQUE (repo_id, number); lifecycle mutable via state function only |
| `delivery_candidates` | id, workflow_id, head_sha, base_sha, pr_link_id, manifest jsonb, manifest_hash, superseded_at | immutable except `superseded_at` |
| `ci_check_evidence` | candidate_id, repo_id, pr_number, head_sha, check_name, source_app, check_run_id, status, conclusion, verified_at | UNIQUE (candidate_id, check_run_id, verified_at) |
| `github_webhook_deliveries` | delivery_id, event, repo_id, received_at, signature_ok, processed_at | UNIQUE delivery_id |
| `merge_attempts` | workflow_id, approval_id, expected_sha, result, merge_commit_sha, github_response_code | append-only |
| `eng_audit_events` | reuse | new actions: `login`, `logout`, `pr_created`, `ci_reconciled`, `candidate_superseded`, `merge_authorized`, `merge_executed`, … |

## 9. API contracts (`/api/v1`, all JSON, errors as `{"error":{code,message}}`)

| Method + path | Auth / role | Idempotency | Notes |
|---|---|---|---|
| `GET /auth/login`, `GET /auth/callback`, `POST /auth/logout`, `GET /auth/me` | none / session | — | `me` returns user + memberships + csrf token |
| `GET /approvals?status=pending` | session, any membership | — | inbox |
| `GET /approvals/{id}` | session | — | details + evidence links |
| `POST /eng/workflows/{id}/decisions` (existing, extended) | session + CSRF; role per gate | `Idempotency-Key` required | gate `plan`/`delivery`/`merge`; 409 `stale_version`, 409 `candidate_changed`, 422 `gates_failed`, 403 `forbidden`, 403 `reauth_required` |
| `GET /eng/workflows/{id}/approvals` | session | — | history |
| `GET /github/connections` | session, administrator/approver | — | allowlisted repos |
| `POST /eng/workflows/{id}/pull-request` | session + CSRF, approver/coordinator | `Idempotency-Key`; second call returns existing link | creates branch + draft PR |
| `GET /eng/workflows/{id}/pull-request` | session | — | PR + lifecycle + last reconcile |
| `GET /eng/workflows/{id}/ci-evidence` | session | — | normalized checks for current candidate |
| `GET /eng/workflows/{id}/delivery-candidate` | session | — | candidate + manifest + hash |
| `POST /eng/workflows/{id}/merge-eligibility` | session + CSRF, approver | safe to repeat | reconciles first, returns reasons |
| `POST /eng/workflows/{id}/merge` | session + CSRF, approver, flag on | `Idempotency-Key`; uncertain result → re-read PR, never resend | 409 `head_changed`, 422 `not_eligible`, 503 `merge_disabled` |
| `POST /github/webhooks` | HMAC | delivery-id dedup | 202 |

The existing `decisions` endpoint is extended rather than adding per-gate endpoints (no redundant routes).

---

## 10. Prioritized backlog

### 10a. Corrective backlog (must pass before any 6D/6E code) — owner order: 6B → 6C → 6D → 6E

| P | ID | Item | Done when |
|---|---|---|---|
| P0 | X1 | Fix or discard the uncommitted edit at `eng/providers/anthropic.rs:189` (`replace(a"</"` → `replace("</"`) | `cargo check --workspace` on the working tree exits 0 |
| P1 | B2 | Workspace creation from an approved base by the MCP side (stored mapping, no `workspace_id = workflow_id`) | orchestrator never names a path |
| P1 | B4 | Candidate SHA and patch digest from MCP audit rows, never from the agent | test: a lying developer is rejected |
| P1 | B5 | Remove the MCP token from stored A2A history | test: `GetTask` history has no token |
| P1 | B3 | Developer and QA as A2A role agents (QA via `testing.run`), then PM/Coordinator/QL | `phase6b` E2E reaches the delivery gate with a real patch |
| P1 | B7 | State-machine delta and contract v2 | unit tests per transition |
| P2 | B6 | Read-only portal pages for `/api/eng` | astro check, unit, Playwright |
| P2 | D1/D2 | **Owner:** dedicated API keys for ≥ 2 vendors in `.env`, and a spend cap | keys present; cap recorded |
| P2 | 6C | Second provider adapter + registry; live run on the fixture crate with two vendors | 6C gate in `PHASE6C.md` §6 PASS |

### 10b. Phase 6D/6E backlog (after 10a)

| M | Items | Depends on | Owner input |
|---|---|---|---|
| M2 | `identity` module, migration (users, projects, memberships, sessions, login attempts), local test issuer, CSRF, `OidcSession` authenticator | 10a | **IdP choice** (issuer URL, client registration) |
| M3 | 8-role RBAC matrix, merge gate, project scope + `session_ref` on approvals, separation of duties, re-auth window | M2, B7 | SoD on/off default |
| M4 | Portal: login, approval inbox, plan review, delivery review, timeline; confirmation dialogs; stale warnings | M3, B6 | — |
| M5 | GitHub App client (JWT, installation token, allowlist), read-only sync, protection reader | M3 | **GitHub App + disposable test repo**, private key path |
| M6 | Feature branch push + draft PR (idempotent) | M5, B3 | **Explicit authorization for writes to the named test repo** |
| M7 | Webhook receiver, dedup, reconciler, CI evidence | M6 | webhook secret, reachable URL (tunnel) |
| M8 | Immutable candidate manifest incl. CI + PR identity; supersede on new head | M7 | — |
| M9 | Merge eligibility + human merge authorization | M8 | — |
| M10 | Controlled merge behind `DELIVERY_MERGE_ENABLED=false` | M9 | **Explicit authorization for a live merge in the test repo** |
| M11 | E2E: main path + 10 alternate paths (§11) | M10 | spend cap for live agents |
| M12 | Security audit of privileged paths | M11 | — |

---

## 11. Test strategy and acceptance criteria

Layers: (A) pure unit tests for RBAC matrix, approval/PR state machines, eligibility rules, HMAC; (B) sqlx integration tests on the private DB for transactions, idempotency, append-only, stale rejection; (C) HTTP tests against the real router with a local OIDC issuer issuing real signed JWTs and a local GitHub API double (`wiremock`-style) for error paths; (D) live tests against a disposable GitHub repository, opt-in (`#[ignore]`, env-gated), only after explicit authorization; (E) Playwright for the portal, run against the real API with seeded sessions.

Negative tests (spec §15) map to layers: 1–8, 25–27 → C; 9–11, 24, 26 → B/C; 12–15, 17, 20–23 → C (GitHub double) and D for 19/22/23; 16 → C (double returns 503 / timeout); 18–19 → A + D. **Authorization controls are verified only at layer C or above, never by frontend state.**

E2E acceptance (spec §16): the main path reaches `merged` (or `approved_for_delivery` with merge disabled) with every decision in `eng_approvals`, every transition in `eng_audit_events`, PR and checks matching the candidate SHA; each alternate path ends in the state listed in §5.3/5.4.

---

## 12. Quality gates

### Gate D — Human governance: **NOT STARTED**

| Criterion | Status | Evidence / limitation |
|---|---|---|
| OIDC authentication works | NOT STARTED | `serve` wires `Disabled`; no IdP chosen |
| Server-side RBAC enforced | PARTIAL (pre-existing) | `state::next` actor rules + `HumanRole{Requester, Approver}` (phase6 tests); no project scope, 2 of 8 roles |
| Human and AI identities separated | PASS (pre-existing logic) | `Actor::{Human, Agent, System}`; agents never satisfy `Required::Human` |
| Plan approval works | PASS (logic, test authenticator) | `phase6.rs`; not with a real IdP |
| Delivery approval works | PASS (logic, test authenticator) | as above; candidate is simulated |
| Unauthorized approvals fail | PASS (logic) | phase6 N-tests; no real sessions |
| Decisions persisted, audit recorded | PASS (pre-existing) | append-only `eng_approvals`, `eng_audit_events` |
| Version conflicts detected | PASS (pre-existing) | UNIQUE `(workflow_id, workflow_version)` + `expected_version` |
| Stale candidate approvals invalidated | PARTIAL | hash/SHA binding exists; no GitHub-driven candidate change |
| Astro approval UI functional | NOT STARTED | B6 open |
| Agents cannot bypass human authorization | PASS (logic) | no approval tool in MCP; state machine actor rules |

### Gate E — GitHub delivery: **NOT STARTED** (every criterion). Live merge execution: **BLOCKED** (no app, no test repo, no authorization).

### Final verdict: **FAIL** — prerequisites (Phase 6B B2–B7, Phase 6C live providers) are not met; Phase 6D/6E implementation was correctly not started.

## 13. Delivery readiness today

- **Safe:** running the deterministic workflow to the delivery gate with simulated roles; MCP read/diff/test/apply_patch on the fixture crate.
- **Disabled:** every human decision in `serve` (no IdP); any GitHub interaction (none exists); merge.
- **Requires human authorization (and does not exist yet):** plan/delivery decisions with a real identity; any write to a GitHub repository.

## 14. Next phase

Phase 7 is not started. The smallest justified next task is not Phase 7 but corrective item **X1** (the one-character compile fix), then **B2** (stored workspace mapping), which unblocks B3/B4 and therefore everything in 6C–6E.
