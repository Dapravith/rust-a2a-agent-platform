# Phase 6F: codebase cleanup, refactoring and technical-debt audit

Date: 2026-11-18 · Branch `phase6-d-revised-structure` @ `3ec8e1a` (cleanup changes; parent `02a88bc`) · Rust 1.100.0 · Node 24.21.0 · Postgres 17 (private test container `127.0.0.1:54339`)

**Verdict: Phase 6F CONDITIONAL PASS.** All mandatory builds and tests pass, one security weakness was fixed with a regression test, and no contract, migration or persisted-state change was made. Remaining issues are documented, noncritical for a private-network deployment, and listed in §6. Phase 7 was not started.

---

## 1. Pre-refactoring assessment

The codebase was already disciplined: every `unwrap`/`expect` is inside `#[cfg(test)]` modules, there are no TODO/FIXME markers and no commented-out code, workspace lints forbid `unsafe` and warn on `unwrap`/`expect`, and the approval engine and MCP authorization are layered correctly (row lock → version → artifact hash + commit SHA → gates; tool grant → role policy → arguments → path scope). The cleanup was therefore deliberately small. Large structural rewrites (splitting `orchestrator.rs`, merging `db.rs` with `eng/store.rs`, renaming `eng`) were assessed and rejected: they touch security-critical paths for no measurable benefit (§6).

| Area | Finding | Action |
|---|---|---|
| Backend | `store::audit` took 9 positional args (adjacent `actor_kind`/`actor_id` strings, `from`/`to` options): silent swaps possible in the audit trail | `NewAuditEvent` struct (B2) |
| Backend | `Idempotency-Key` format check duplicated in `routes.rs` and `eng/routes.rs` | shared `is_valid_idempotency_key` (B3) |
| Backend | `approval.rs` re-spelled gate/decision DB values inline | `GateKind::as_str`, `Decision::as_str` + drift test (B4) |
| Backend | `eng::sim` (simulated agents) compiled into the production `api` library; only tests use it | `simulation` feature, dev-only (B5) |
| Frontend | `components/ui` and `lib/api-client.ts` imported from `features/workflows` (inverted dependency) | `Tone` → `components/ui/tone.ts`; error envelope → `lib/api-client.ts` (F1, F2) |
| Frontend | `DraftPanel`/`ReviewPanel` (workflow artifacts) lived in `features/agents` | moved to `features/workflows/components` (F3) |
| Security | `/api/eng/*` responses had no `Cache-Control` (S1) | fixed + test |
| CI | raw `npm audit` failed on an allowlisted advisory; `scripts/audit.mjs` not wired (S2) | `npm run audit` in CI |
| Docs | README layout listed 5 of 9 crates; `.env.example` lacked ~25 variables | updated (D1) |

## 2. Changes made

### Renamed / moved files

| From | To |
|---|---|
| `frontend/src/features/agents/components/DraftPanel.astro` | `frontend/src/features/workflows/components/DraftPanel.astro` (`git mv`) |
| `frontend/src/features/agents/components/ReviewPanel.astro` | `frontend/src/features/workflows/components/ReviewPanel.astro` (`git mv`) |
| `Tone` type in `features/workflows/present.ts` | new `frontend/src/components/ui/tone.ts` |
| `errorEnvelopeSchema` in `features/workflows/schemas.ts` | module-private const in `frontend/src/lib/api-client.ts` (only consumer) |

No Rust file was moved; no crate was added.

### Rust

- **B1 / S1** `apps/api/src/eng/routes.rs`: `Cache-Control: no-store` layer on the engineering router (the P0 router already had it; the engineering router is nested separately in `lib.rs` and was missed).
- **B2** `apps/api/src/eng/store.rs`: `audit(tx, NewAuditEvent { … })`; removes `#[allow(clippy::too_many_arguments)]`; 5 call sites (4 in `store.rs`, 1 in `orchestrator.rs`) now name every field. Named `NewAuditEvent` to match `NewArtifact` and to avoid the existing read-model `AuditEvent`.
- **B3** `apps/api/src/routes.rs`: `pub(crate) fn is_valid_idempotency_key` and `IDEMPOTENCY_HEADER`, reused by `eng/routes.rs`. Error messages are unchanged.
- **B4** `apps/api/src/eng/approval.rs`: `as_str()` on `GateKind`/`Decision`; values match the `eng_approvals` CHECK constraints (migration `20261117000003`); unit test `stored_spelling_matches_json` asserts they equal the serde spelling.
- **B5** `apps/api/src/eng/mod.rs` + `apps/api/Cargo.toml`: `#[cfg(any(test, feature = "simulation"))] pub mod sim;`; the feature is enabled only through a self dev-dependency, so `cargo test --workspace` still runs every simulation test (no `required-features` that could silently skip them). `Cargo.lock` gains one self-reference line.

### Astro

- Removed decorative `// ----` separators in `features/workflows/present.ts`.
- `components/` and `lib/` no longer import from `features/`, except `lib/config.ts`, the composition root that binds feature APIs to the client.

### Config / CI / docs

- `frontend/package.json`: `"audit": "node scripts/audit.mjs"`; `.github/workflows/ci.yml` runs `npm run audit` instead of raw `npm audit`.
- `.env.example`: LLM, engineering (`ENG_PROVIDER=a2a`, `ROLE_AGENTS_URL`, `MCP_CONTROL_URL`, `MCP_TOKEN_KEY`, …), MCP server, sandbox and role-agent variables. New entries are commented out with their defaults, because `mcp-server` and `role-agents` read raw `std::env::var` and an empty `KEY=` would override the default with `""`.
- `README.md`: repository layout (all 9 crates, fixtures), dependency direction, `npm run audit` in checks.

### Not changed (compatibility)

REST paths and JSON bodies, A2A Agent Cards and message types, MCP tool names and schemas, migrations, DB columns, persisted states, approval semantics, error codes.

## 3. Security

| ID | Severity | Location | Root cause | Status |
|---|---|---|---|---|
| S1 | Medium | `backend/apps/api/src/lib.rs:33`, `eng/routes.rs` | `no-store` layer applied to the P0 sub-router only; `/api/eng` responses (requirements, approvals, audit trail) were cacheable by proxies/browsers | **Fixed.** Test `eng_responses_are_not_cacheable` (200, 404, 503 on `/api/eng` and `/api/v1/eng`); verified it **fails** without the fix (`left: None`) |
| S2 | Medium (CI) | `.github/workflows/ci.yml` | allowlist script existed but CI ran raw `npm audit`, which fails on GHSA-fc5q-39xc-vjpf (picomatch ReDoS; no patched release, 4.0.7 is latest; patterns come only from our config) | **Fixed.** Allowlist entry expires 2026-12-31. `npm audit fix --force` was rejected: it would downgrade Astro 7 → 5.3 |
| S3 | High for public exposure / accepted for private network | `eng/routes.rs` module doc | `/api/eng` and `/api` reads are unauthenticated by design until Phase 6D OIDC; writes fail closed (`Disabled` authenticator → 503) | **Open.** Phase 7 must keep the API, agents and MCP server off public ingress (portal only) until 6D lands |
| S4 | Info | `backend/crates/agent-runtime/src/llm.rs:492` | gitleaks `generic-api-key` on history | **False positive**: `"test-key-0123456789"` in a `#[cfg(test)]` config |

Preserved and re-verified by tests: human-only approval (`ai_agent_cannot_approve_delivery`, `unauthorized_user_cannot_approve`), stale/forged/replayed decisions, candidate-SHA binding, MCP tool authorization and path scope, protected-branch refusal, append-only audit tables.

## 4. Verification (actual results)

Baseline ran on `02a88bc` before backend edits; post-change on the tree committed as `3ec8e1a`.

| Check | Baseline | After |
|---|---|---|
| `cargo fmt --all -- --check` | PASS | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS | PASS |
| `cargo build --workspace --locked` | PASS | PASS |
| `cargo test --workspace --locked -- --test-threads=4` | 195 passed, 0 failed, 2 ignored ¹ | **197 passed, 0 failed, 2 ignored** (+2 new tests) |
| `npm ci` | PASS | (unchanged lockfile) |
| `npm test` (unit) | 35/35 | **35/35** |
| `npm run build` (`astro check` + build) | PASS, 0 errors/warnings/hints | **PASS**, 62 files, 0 errors/warnings/hints |
| `npm audit --omit=dev --audit-level=moderate` (old CI step) | **FAIL** (1 high) | replaced |
| `npm run audit` (allowlisted) | — | **PASS** (1 finding, allowlisted) |
| `scripts/check-secrets.sh` | OK | OK |
| `gitleaks git` (history) | — | 1 finding, false positive (S4) |
| Playwright e2e / axe | not run | **UNVERIFIED** (not run: host load 140–160; frontend changes are import moves, type-checked by `astro check`) |
| `cargo audit` / `cargo deny` | not installed | **UNVERIFIED** |

¹ The baseline's `api` doc-test step compiled after a mid-run edit and failed on a name clash introduced by that edit (`AuditEvent` defined twice), so it is not counted as a baseline result. Fixed by naming the struct `NewAuditEvent`; the post-change run compiles all doc-tests. Ignored tests (both runs): `live_sandbox_runs_checks_without_network`, `live_sandbox_qa_runs_real_tests_on_the_candidate` (need Docker + `A2A_SANDBOX_IMAGE`).

Suites covered: A2A interop (`worker-agent`, `reviewer-agent`, `phase6b` over real A2A → MCP), MCP (`mcp-server/tests/mcp.rs`), DB (`phase3`, `workflow`, `phase6`, real Postgres via `sqlx::test`), approval/RBAC negatives (`phase6`), provider adapter (`phase6_provider`, HTTP test double). No regressions.

## 5. Quality gate

| Item | Result |
|---|---|
| Rust modules have clear responsibilities | PASS |
| Idiomatic Rust file naming | PASS |
| Descriptive functions and variables | PASS |
| No unnecessary public interfaces | PASS (`sim` now test-only; `pub(crate)` for the shared validator) |
| Duplicated logic reduced | PASS |
| Error handling reliable | PASS |
| A2A/MCP boundaries preserved | PASS |
| Approval and delivery policies enforced | PASS |
| Astro structure consistent | PASS |
| Feature modules cohesive | PASS |
| TypeScript naming clear | PASS |
| Components maintainable | PASS |
| Duplicate UI logic reduced | PASS |
| Design tokens consistent | PASS (no hard-coded colours outside `tokens.css`, except the logo SVG) |
| Unnecessary hydration avoided | PASS (no framework islands; three small vanilla scripts) |
| No accessibility/security regressions | PASS (security) / UNVERIFIED (axe e2e not run) |
| Redundant comments removed | PASS |
| Security-critical comments preserved | PASS |
| Dead code removed safely | PASS (none found besides test-only `sim` in prod builds) |
| Dependencies reviewed | PASS (npm) / UNVERIFIED (Rust advisories: no `cargo audit`) |
| No significant new technical debt | PASS |
| Critical paths have regression coverage | PASS |
| API contracts compatible | PASS |
| Database state and migrations valid | PASS (no migration change) |
| A2A interoperability | PASS |
| MCP integration | PASS |
| Provider integrations functional | PASS (test double only; no live provider key exists, see `PHASE6C.md` D1) |
| Human approvals cannot be bypassed | PASS |
| GitHub integration correct | **BLOCKED**: not implemented yet (Phase 6D backlog, `PHASE6DE.md`) |
| Required tests pass | PASS |

## 6. Remaining technical debt

| ID | Class | Severity | Location | Note / proposed correction | Regression risk |
|---|---|---|---|---|---|
| TD1 | Security | High (if exposed) | `eng/routes.rs`, `routes.rs` | Unauthenticated reads (S3). Land 6D OIDC + RBAC before any public ingress | — |
| TD2 | Code quality | Low | `eng/orchestrator.rs` (`stage` 97, `validate` 117, `verify_tool_evidence` 112, `commit` 103 lines), `eng/approval.rs::decide` 180 lines | Split only alongside the 6D changes that touch them, so each split is reviewed with its test coverage; not worth a separate pass on security-critical code | Medium |
| TD3 | Code quality | Low | `orchestrator.rs` (`Orchestrator::commit`), `engine.rs` (`Engine::run_step`) | Two remaining `too_many_arguments` allows (private fns, distinct types) | Low |
| TD4 | Dependency | Medium | Rust workspace, CI | No `cargo audit`/`cargo deny` in CI; add with a reviewed `deny.toml` | Low |
| TD5 | Dependency | Low | `frontend/scripts/audit-allowlist.json` | **Closed 2026-11-19:** npm audit no longer reports GHSA-fc5q-39xc-vjpf; the entry was removed (the script fails on stale entries) and the allowlist is empty | Low |
| TD6 | Testing | Medium | `frontend/tests/e2e` | Playwright/axe not in CI and not run this phase | — |
| TD7 | Architecture | Low | `apps/api/src/eng` | Module name `eng` is an abbreviation; renaming to `engineering` collides with the crate name. Leave until a natural boundary (e.g. if `eng` becomes its own crate) | Medium |
| TD8 | Scope | — | `PHASE6C.md`, `PHASE6DE.md` | 6C (≥ 2 live providers) and 6D (OIDC, GitHub App, merge) remain not started; 5 of 6 roles still simulated in tests | — |

## 7. Engineering review

| Category | Weight | Score | Evidence |
|---|---:|---:|---|
| Backend architecture | 20% | 4 | Pure policy crate, thin adapters, fail-closed auth; long orchestrator functions (TD2) |
| Frontend architecture | 15% | 4 | SSR-only, typed client with schema validation, single sanitized HTML sink; dependency direction now correct |
| Maintainability and naming | 15% | 4 | Consistent naming, no dead code; `eng` abbreviation (TD7) |
| Security | 20% | 3 | Strong controls in place and tested; unauthenticated reads (TD1) and no Rust advisory scan (TD4) |
| Testability and regression coverage | 20% | 4 | 197 Rust + 35 unit tests on real Postgres and real A2A/MCP; e2e not run (TD6) |
| Technical debt | 10% | 4 | Small, documented backlog |

Weighted score: **3.8 / 5**. The score does not override §6: TD1 constrains Phase 7.

## 8. Phase 7 entry criteria

| # | Criterion | Status |
|---|---|---|
| 1 | Existing behaviour correct | PASS (all suites green) |
| 2 | Rust workspace builds cleanly | PASS |
| 3 | Astro production build | PASS |
| 4 | Critical regression tests pass | PASS |
| 5 | A2A and MCP integrations functional | PASS |
| 6 | Approval security enforced | PASS (fails closed: no IdP → writes 503) |
| 7 | GitHub delivery controls functional | BLOCKED (not built; 6D) |
| 8 | No unresolved critical vulnerability | PASS (none critical; S3 conditional on network placement) |
| 9 | Repository structure understandable | PASS |
| 10 | Configuration and startup instructions accurate | PASS (after D1) |
| 11 | Remaining debt documented | PASS (§6) |
| 12 | No architecture blocker for deployment | CONDITIONAL: deploy only with the API, agents and MCP server on a private network |

**Phase 7 readiness: CONDITIONAL PASS** for deploying the current platform privately (portal as the only ingress, approvals disabled). A public deployment, or one that enables approvals or GitHub delivery, requires Phase 6D first. Under the owner's phase ordering (`PHASE6C.md`), 6C/6D precede Phase 7; this report does not change that decision.
