# Phase 5: Full-stack QA, security hardening, production readiness (verification record)

Date: 2026-11-17 · Branch: `phase4/frontend` (HEAD `3e94306` + uncommitted Phase 5 changes) ·
Rust 1.100.0 · Node 24.21.0 (`.nvmrc`) · Playwright 1.64.1 (Chromium) · Postgres 17 (local Docker).

**Host conditions (important for every timing below):** load average 130–200 on 8 cores for the whole
session (other workloads), Docker CLI unresponsive (`docker ps` timed out; the Postgres port worked).
Latencies are a lower bound on quality, not a baseline for a healthy host.

## 1. Phase 4 readiness audit

| Check | Result | Evidence |
|---|---|---|
| Working tree builds | **FAIL (blocker in the working tree, not in HEAD)** | Uncommitted `backend/Cargo.toml` adds a root `[package] backend` with `[[bin]]` paths `src/bin/{api,worker-agent,reviewer-agent}.rs` that do not exist. `cargo build`/`check` fails for the whole workspace: `can't find bin 'api' at path …/backend/src/bin/api.rs`. It also uses `edition = "2021"` against the workspace's 2024 and breaks `scripts/dev.sh` / `scripts/e2e-stack.sh` (they build from this manifest). **Resolved:** removed at the owner's request (manifest and lockfile restored to HEAD); the working tree now passes fmt, clippy, build and 86/86 tests |
| HEAD `cargo fmt --check` | PASS | |
| HEAD `cargo clippy --workspace --all-targets --locked -D warnings` | PASS | |
| HEAD `cargo build --workspace --locked` | PASS | |
| HEAD `cargo test --workspace` | PASS 85/85 on re-run; first run 37 passed, 1 failed (`database_outage_is_reported_safely`: `PoolTimedOut` building its own 2 s pool on the saturated host; cargo then stopped, so later binaries never ran) | §7 |
| Frontend `npm ci && npm test && npm run build` | PASS on the pinned Node 24.21.0 (28/28, `astro check` 0/0/0). **On the shell default Node 22.20.0, 4 unit tests are "cancelled" while the summary says `fail 0`, and `npm run build` exits 1** (Astro requires ≥ 22.23.4) | Environment issue; CI uses `.nvmrc` |

Verdict: the committed Phase 4 state passes its mandatory checks, so Phase 5 proceeded against
HEAD in an isolated `git worktree` (shared `target/`), and verified changes were copied back.
The working-tree manifest blocker was then removed at the owner's request, and all gates were re-run in the working tree itself (§7).

## 2. Architecture assessment

The HLD in [ARCHITECTURE.md](ARCHITECTURE.md) matches the code: Browser → Astro SSR (`:4321`) →
Rust API (`:8080`) → Worker (`:9101`) / Reviewer (`:9102`) over the official SDK (JSON-RPC only), API → Postgres.
The browser never reaches Rust or the agents; the agents never reach each other or the database. All
three services bind to `127.0.0.1` by default (verified with `lsof`).

Strengths verified in code and tests: single-active-workflow enforced by a partial unique index
(`migrations/20261116000001_init.sql:21`), guarded `UPDATE … WHERE status = …` transitions
(`apps/api/src/db.rs`), no transaction held across an A2A call, Agent Card identity + same-origin
interface filtering (`apps/api/src/a2a.rs:138`), deadline + best-effort `CancelTask`, process-wide
advisory lock before restart recovery (`db.rs:157`), least-privilege runtime role and RLS
(`migrations/20261117000002_runtime_role.sql`), closed set of failure codes enforced by CHECK constraints.

Findings (architecture/maintainability), ranked:

| Sev | Finding | Location |
|---|---|---|
| HIGH (resolved) | Working tree did not build (see §1); root package removed | `backend/Cargo.toml` (uncommitted) |
| MEDIUM | A failure that could not be persisted was never retried, so a brief DB outage at that moment left the workflow `queued`/`running` and the single-active index refused every later submission (409) until a restart. **Fixed** (§6) | `apps/api/src/engine.rs` `supervise`, `step` |
| LOW | `span.enter()` guard held across `.await` in `supervise` (the tracing anti-pattern). Fixed with `.instrument(span)` as part of the same change | `engine.rs` |
| LOW | Unused static-serving path (`FRONTEND_DIR`) still in Rust; the local `.env` still sets `FRONTEND_DIR=frontend/dist`, which makes `api` refuse to start if run directly (`dev.sh` unsets it) | `apps/api/src/lib.rs`, `.env` |
| LOW | Agents keep every task in `InMemoryTaskStore` forever (unbounded memory over a long uptime) | `crates/agent-runtime/src/server.rs:249` |
| LOW | Error envelope inconsistency: unknown `/api/*` path says "workflow not found"; a bad `before=` query returns plain text 400, not the JSON envelope | `routes.rs:216`, `Query` extractor |

No duplicated business logic, no blocking calls in async code, no `unwrap`/`expect` outside tests
(clippy `unwrap_used`/`expect_used` warn + `-D warnings`), `unsafe_code = "forbid"`. No extra layers or crates recommended.

## 3. Test strategy and traceability matrix

Environment: isolated local stack (scratch DB `a2a_e2e`, mock inference, ports 18080/19101/19102,
portals on 14321/14322), started by a copy of `scripts/e2e-stack.sh` that creates the database with
node-postgres (Docker CLI unusable) and runs the HEAD + fix binaries. `#[sqlx::test]` gives every Rust
DB test its own database. No shared or production database was touched; no paid LLM was called.

| TC | Requirement | Automated evidence | Live evidence (this session) | Result |
|---|---|---|---|---|
| 1 | Submit workflow | `full_workflow_completes_and_persists`, `dashboard.spec.ts` | `POST /api/workflows` → 202, `Location`, `status=queued`, UUIDv7 id; replay with same `Idempotency-Key` → 200 | PASS |
| 2 | Worker A2A | `worker-agent/tests/a2a.rs`, `direct_message_response_is_supported` | Card served (name, JSONRPC 1.0 interface, skill `draft-answer`); run `TASK_STATE_COMPLETED`, task id recorded; draft has `title, summary, content, assumptions` | PASS |
| 3 | Reviewer A2A | `reviewer-agent/tests/a2a.rs`, `reviewer_pass_verdict` | Review artifact `verdict=needs_changes`, 2 findings, persisted | PASS |
| 4 | Complete workflow | `full_workflow_completes_and_persists`, `detail.spec.ts` live updates | worker run finished before reviewer started; `completed` in 3 s; still readable after two API restarts | PASS |
| 5 | Failure handling | worker/reviewer unavailable, invalid/missing/untrusted card, malformed artifact, completed-without-artifact, input-required, auth-required, timeout + cancel, DB outage (2 tests incl. new), malformed/refused LLM output, provider outage | Worker killed → `failed / agent_unreachable / worker`, URL redacted (`<agent-url>`), capacity released (next submit 202) | PASS |
| 6 | Restart recovery | `workflows_survive_a_killed_process_and_interrupted_ones_fail`, `restart_recovery_fails_interrupted_workflows` | `kill -9` API right after submit → restart logs `recovered=1`; workflow `failed / interrupted / worker`; worker task count unchanged (no replay); reviewer never called; new submit 202. A second API on the same DB was refused by the instance lock | PASS |
| 7 | Concurrency | `concurrent_submissions_admit_exactly_one` | Second submit while active → 409 `workflow_active`; released after completion and after failure | PASS |
| 8 | Frontend UX | Playwright 31 tests (validation, loading, empty, history, detail, errors, navigation, responsive, a11y) | 31/31 on the live stack; screenshots at 375/768/1440 reviewed | PASS (Chromium) |
| Sec | Validation, limits, XSS, CSRF | `api_input_validation`, markdown unit tests, XSS browser test | §4 probes | PASS |

Acceptance criteria: every row above must pass on the isolated stack with mock inference;
live Supabase and live LLM are out of reach in this environment (no credentials) and stay UNVERIFIED.

## 4. Security assessment

Trust boundaries: (1) browser → portal (untrusted, unauthenticated); (2) portal → API (private
network); (3) API → agents (configured URLs only); (4) agents → LLM provider; (5) API → Postgres.
Untrusted data: the question (user) and all agent/LLM output.

Live probes (API on 18080, agent on 19101, portal on 14321/14322):

| Probe | Result |
|---|---|
| Unknown JSON field (`worker_url`) | 422 `validation_failed` (`deny_unknown_fields`); no user-controlled outbound URL exists |
| Question < 10 chars / control char / bad `Idempotency-Key` / malformed JSON | 422 each |
| 20 KB body; 1 MB chunked body | 413 `payload_too_large` (16 KiB cap holds for chunked) |
| `limit=1000`; `before=xyz`; id `1' OR 1=1--` | 422; 400; 404 (UUID parse, parameterized SQL only) |
| CORS preflight from `https://evil.example` | 405, no `Access-Control-*` headers (no CORS: the API is server-to-server) |
| API headers | CSP, `nosniff`, `DENY`, `no-store`, `x-request-id` |
| Agent: wrong method / non-JSON / 300 KB body / invalid payload / unknown task | JSON-RPC version error / -32700 / 413 / task `rejected` / -32001 |
| Portal cross-site POST | 403 (Astro `checkOrigin`) |
| Portal 4 MiB chunked POST (no `Content-Length`) | connection closed by the server after the 64 KiB limit (fix §6) |
| Portal headers | strict CSP without `unsafe-inline`, `DENY`, `nosniff`, `no-referrer`, `no-store` |

Findings:

| Sev | Finding | Evidence / impact | Mitigation | Status |
|---|---|---|---|---|
| HIGH | `markdown-it` 14.1.0: 6 advisories incl. GHSA-r7fv-28h4-cvq7, GHSA-gfr2-rv44-r23c (quadratic-time DoS). Agent output (untrusted, ≤ 20k chars) is rendered with it on every page view and every 2 s poll | `npm audit` | Upgrade to 14.3.3 (patch, non-breaking) | **Fixed**, `npm audit`: 0 |
| MODERATE | `@astrojs/node` 11.1.9: GHSA-r3h5-4585-7x3w (image endpoint file read) | `npm audit` | Upgrade to 11.1.13 | **Fixed** |
| MEDIUM | Portal accepted request bodies up to the adapter default of **1 GiB**; the page's 64 KiB guard reads `Content-Length` only, so chunked uploads bypassed it (memory exhaustion of the portal) | `node_modules/@astrojs/node/dist/index.js:62` | `bodySizeLimit: 64 * 1024` | **Fixed**, verified live |
| MEDIUM | No authentication/authorization anywhere (portal, API, agents). Any client that reaches a port can submit work and spend LLM budget | by design for P0 | Keep everything on loopback/private network; P1 must add access control at the proxy. **The portal is unsuitable for untrusted or public access** | Documented |
| MEDIUM | Agents accept any number of concurrent tasks (the orchestrator admits one workflow, but direct callers are not limited) | code review | Network isolation now; per-agent concurrency limit or auth before remote deployment | Open |
| LOW | Prompt injection: the question is wrapped in `<question>…</question>`, the review input in `<review_input>…</review_input>`; a literal closing tag in the question is not neutralized | `worker-agent/src/lib.rs:118`, `reviewer-agent/src/lib.rs:134` | Residual risk is bounded: output is schema-validated, no tools/exec exist, the verdict is stored as a label and authorizes nothing. Optionally strip/escape the delimiter tags | Open (residual) |
| LOW | Agent HTTP responses (card, JSON-RPC) have no explicit size cap on the client side; agents are trusted configuration | `a2a.rs:160` | Acceptable for P0 | Open |
| LOW | CI actions pinned by tag (`@v4`, `@stable`), not SHA; no Rust advisory scan | `.github/workflows/ci.yml` | Pin SHAs; add `cargo audit`/`cargo deny` | Open |
| INFO | `cargo audit` / `cargo deny` not installed here | | Rust dependency advisories **UNVERIFIED** | |

Verified safe: parameterized SQL only (one `format!` with a hex-only generated identifier in a test);
secrets server-side only and redacted in `Debug`/logs; remote DB refused without `sslmode=verify-full`;
`check-secrets.sh` OK; Markdown with `html:false`, link allowlist, images off; agent text rendered
inert (Playwright XSS test); no `eval`/`innerHTML` sinks; no process execution in non-test code.

## 5. Performance and reliability baseline

50 sequential requests per endpoint, loopback, debug build, saturated host:

| Endpoint | p50 | p95 | max | Errors | Size |
|---|---:|---:|---:|---:|---:|
| `GET /healthz` | 2.3 ms | 3.7 ms | 12 ms | 0 | 15 B |
| `GET /api/agents` (2 card fetches) | 6.7 ms | 13 ms | 232 ms | 0 | 432 B |
| `GET /api/workflows?limit=25` | 40 ms | 184 ms | 439 ms | 0 | 394 B |
| `GET /api/health` (DB ping) | 60 ms | 197 ms | 335 ms | 0 | 31 B |
| `GET /api/workflows/{id}` (3 queries) | 118 ms | 425 ms | 520 ms | 0 | 2.1 KB |

DB-backed endpoints are dominated by the overloaded Docker Postgres (`/healthz` without DB is 2 ms).
Workflow end-to-end (mock): submit → completed in ~3 s (worker run 0.75 s, reviewer 0.63 s, rest is
scheduling and DB). Resident memory: API 17 MB, each agent 10 MB. Portal JS 5 KB / CSS 34 KB (Phase 4).
Polling: one `/live` request per 2 s per open active detail page, none when terminal/hidden.

Timeouts are bounded everywhere: agent deadline `AGENT_TIMEOUT_SECS` (240 s) + 5 s CancelTask, card
fetch 10 s, connect 5 s, API request 30 s, DB acquire 10 s, portal → API 10 s, LLM `LLM_TIMEOUT_SECS`.
Acceptable for a single-user MVP; no latency targets are defined in the PRD, so none are claimed.

Residual reliability risks: an orchestrator crash cannot cancel the agent's in-flight task (the
agent finishes it and the result is discarded, i.e. possible duplicate LLM spend if the user resubmits);
if the database stays down for longer than the failure-recording retry budget (~1 min), the workflow
remains active until the next restart (recovery then fails it).

## 6. Corrective changes

| # | Root cause | Fix (smallest safe) | Regression test |
|---|---|---|---|
| 1 | `supervise`/`step` called `fail_workflow`/`fail_run` once; a transient DB error dropped the failure, leaving an active row that blocks admission | `record_failure()` in `backend/apps/api/src/engine.rs`: retries only `DbError::Sqlx` with capped exponential backoff (10 attempts, 250 ms → 10 s); `InvalidTransition` returns at once (terminal rows are never touched). Also replaced the `span.enter()` guard held across `.await` with `.instrument(span)` | `failure_is_recorded_after_a_transient_database_outage` (`apps/api/tests/phase3.rs`): holds the API's only DB connection across the agent deadline, then releases it. **Red without the fix** (panics: workflow never reaches `failed`), **green with it** (failed / `agent_timeout` / `worker`, next submission 202) |
| 2 | Vulnerable `markdown-it` / `@astrojs/node` | `frontend/package.json` + lock: 14.3.3 / 11.1.13 (exact pins, non-major) | `npm audit` 0; unit + build + E2E re-run |
| 3 | 1 GiB default request-body limit in the portal | `frontend/astro.config.mjs`: `bodySizeLimit: 64 * 1024` | Live chunked 4 MiB POST cut off; normal form posts work (E2E) |
| 4 | No dependency scan in CI | `.github/workflows/ci.yml`: `npm audit --omit=dev --audit-level=moderate` | Passes locally (0 vulnerabilities) |

No business logic, API contract, schema or UI design changed.

## 7. Verification report

| Command | Outcome |
|---|---|
| `cargo fmt --all -- --check` (HEAD + fix) | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` (HEAD + fix) | PASS |
| `cargo build --workspace --locked` (HEAD) | PASS |
| `cargo test --workspace --locked --no-fail-fast -- --test-threads=4` (HEAD + fix) | **86 passed, 0 failed** (333 s) |
| `cargo build` in the working tree, before / after removing the root package | FAIL / PASS; after removal the working tree also passes fmt, clippy and **86/86** tests |
| `npm ci`, `npm test`, `npm run build`, `check-secrets.sh` (Node 24.21.0, after fixes) | PASS: 28/28; 0 errors/warnings/hints; secret check OK |
| `npm audit` before / after | 1 high + 1 moderate package / 0 |
| `npx playwright test` (real stack, Chromium, after all fixes) | **31/31 passed** (2.1 min): dashboard, history, detail/live, API-down, XSS, axe at 375/768/1440 + dark, keyboard, reduced motion |
| Live probes §3–§5 | as recorded |

Unverified: live Supabase (TLS, pooler, roles), live hosted LLM, Firefox/Safari, manual screen-reader
pass, Lighthouse, Rust advisory scan, remote GitHub Actions run (none observed), the new CI step on GitHub.

## 8. Final production-readiness assessment

Quality gate:

| Criterion | Result |
|---|---|
| Dashboard connects to Rust | PASS |
| Worker and Reviewer communicate through A2A | PASS |
| Valid workflows complete | PASS |
| Artifacts persist in PostgreSQL | PASS |
| History survives API restart | PASS |
| Failures handled consistently | PASS (after fix 1) |
| Required E2E tests pass | PASS (31/31 browser + 86 Rust integration) |
| No unresolved critical security findings | PASS (none critical) |
| Server-side credentials protected | PASS |
| Database permissions appropriate | PASS locally (Supabase UNVERIFIED) |
| Agent endpoints restricted | PASS by configuration only (loopback, fixed URLs, card origin check); no auth |
| User and agent content validated | PASS |
| XSS / unsafe output tested | PASS |
| Authentication limitations documented | PASS (none implemented; private use only) |
| Timeouts bounded | PASS |
| Interrupted workflows reach accurate states | PASS |
| Concurrency limits behave correctly | PASS |
| DB failures do not silently corrupt state | PASS (bounded retry; longer outages resolved at restart) |
| Agent failures represented accurately | PASS |
| Core UI, responsive, a11y, loading/error states, visual consistency | PASS (Chromium; Firefox/Safari and screen reader UNVERIFIED) |
| Rust fmt / lint / tests | PASS (working tree, after removing the root package) |
| Astro build | PASS (Node 24); FAIL on Node 22.20 |
| CI configuration validated | UNVERIFIED (reviewed; no remote run observed) |

Scorecard (advisory):

| Category | Weight | Score | Basis |
|---|---:|---:|---|
| Functional correctness | 25% | 4.5 | All TCs pass live and in tests |
| Security | 20% | 3.5 | Strong validation/headers/least privilege; two vulnerable deps and 1 GiB body limit (fixed); no auth (by scope) |
| Reliability | 20% | 4.0 | Guarded transitions, recovery, lock; stuck-active gap fixed; orphaned agent work on crash |
| Architecture & maintainability | 15% | 4.0 | Clean boundaries; a broken manifest reached the working tree unnoticed (now removed) |
| Test coverage & CI | 10% | 3.5 | 86 Rust + 28 unit + 31 browser tests; E2E and dependency scans not in CI; Node-version trap |
| Frontend UX/accessibility | 10% | 4.5 | axe clean at 3 widths + dark; Chromium only |
| **Weighted** | | **4.03** | |

**Phase 5: CONDITIONAL PASS** for entering controlled deployment engineering, on the committed code
plus the Phase 5 changes. This is not a statement that the platform is safe for public traffic.

Phase 6 entry criteria (smallest tasks first):

1. ~~Resolve the uncommitted `backend/Cargo.toml` root package~~ Done: removed, working tree builds and passes 86/86.
2. Commit the Phase 5 changes and observe a green GitHub Actions run (including the new `npm audit` step).
3. Add `cargo audit` (or `cargo deny`) to CI and record its result.
4. Run the E2E suite in CI (Postgres service + Playwright), so browser regressions are gated.
5. Access control in front of the portal and network isolation for API/agents (P1 design), before any non-local exposure.
6. Verify against a real Supabase project (TLS `verify-full`, runtime role) and one bounded live-LLM run.
