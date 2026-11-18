# Phase 6B: Phase 6A audit, design and corrective backlog

Date: 2026-11-18 · Branch `phase6/native-rust-protocols` @ `49c8773` (clean tree) · Rust 1.100.0 · Postgres 17 (local docker, `127.0.0.1:54329`)

> **Superseded by §0 (re-audit at `c8bff57`).** Sections 1–9 below are the original audit at `49c8773` and are kept as the record.

## 0. Re-audit at `c8bff57` (2026-11-18 10:37–10:50)

Commit `c8bff57` ("add mock output indication draft in worker", 51 files, +4530) adds backlog items C1–C6: `apps/mcp-server` (`rmcp =3.11.0`), `apps/role-agents` (Code Reviewer over A2A), `crates/workspace`, `engineering::mcp`, `eng/a2a_role.rs`, migrations `20261118000001/2`, `tests/phase6b.rs`, ADR-004. Clean tree; every peer session idle when the audit started.

**Verdict: Phase 6A now PASSES checks 1–8 and 10. Check 9 passes for every pre-existing test; the full suite has 1 deterministic failure, a new workspace unit test whose fixture is defective (C8 below).** Phase 6B may start once C8 is fixed (**C8 fixed, see below**). No code was changed by this audit.

| # | Check | Result | Evidence |
|---|---|---|---|
| 1 | Workspace compiles | PASS | `cargo fmt --check` exit 0; `cargo clippy --workspace --all-targets --locked -D warnings` exit 0 |
| 2 | Worker and Reviewer over official A2A | PASS | `worker-agent/tests/a2a.rs` 5/5, `reviewer-agent/tests/a2a.rs` 5/5 |
| 3 | Agent Cards discoverable and valid | PASS | Card tests above; the Code Reviewer role agent publishes its own card (`role-agents`), resolved through the trusted-card `A2AGateway` in `phase6b` |
| 4 | MCP server starts independently | PASS | **Real binary** `target/debug/mcp-server` started on its own (loopback, token key, workspaces root, scratch DB); `initialize` answers `rmcp 3.11.0`. Probe: scratchpad `mcp6a_probe.py` (stdlib, raw Streamable HTTP) |
| 5 | MCP tool discovery and invocation | PASS | `tools/list` = `repository.diff`, `repository.read_file`, `testing.run`; `read_file` returns content bound to HEAD; `diff` = exact base..HEAD; `testing.run` ran in the **real Docker sandbox** (`runner=docker`, honest `exit_code=101` on a fixture without `Cargo.toml`); ignored live test `live_sandbox_runs_checks_without_network` PASS with `rust@sha256:2752b33…` (cargo test exit 0, HEAD = candidate); `mcp-server/tests/mcp.rs` 9/9 |
| 6 | A2A agent invokes an MCP tool | PASS | `phase6b` 6/6: orchestrator → Code Reviewer over A2A on real TCP → `repository.diff` over MCP on real TCP → audit in real Postgres |
| 7 | A2A artifacts carry validated tool results | PASS (gap noted) | `verify_tool_evidence`: every cited `tool_call_id` must exist in `eng_tool_calls`, match run and role, be `allowed`, and (reviewer/QA) match the candidate HEAD; a review must cite `repository.diff`, QA must cite `testing.run`. Fabricated / foreign / refused / stale evidence is rejected (`phase6b` c5 tests). **Gap:** the artifact's *content* is not compared with the audited result (e.g. QA exit codes vs `exit_code`, `patch_sha256`); needed before a real QA agent (6B M6) |
| 8 | Tool authorization server-side | PASS | Probe: 12 refusals before execution — not granted, traversal, absolute, `.git`, out of scope, unknown tool `shell.exec`, extra argument, arbitrary check id; 401 for no token, forged signature, over-granted token, TTL > 15 min |
| 9 | Existing tests pass | PASS (existing) / 1 new FAIL | `cargo test --workspace --locked --no-fail-fast -- --test-threads=2`: **175 passed, 1 failed, 1 ignored**. All 6 failures from the first audit now pass (phase3 17/17 in 100 s vs 717 s; phase6 21/21; host load ~115 vs ~157). Failing: `workspace::tests::diff_refuses_wrong_or_abbreviated_base`, 3/3 on re-run (see C8) |
| 10 | Git status and user changes preserved | PASS | No tracked file changed except this document; the scratch database `audit6a_scratch` was created and dropped |

Additional evidence:

- **Audit completeness.** The probe made 15 tool calls; `eng_tool_calls` holds exactly 15 rows (3 allowed, 12 refused, including 4 transport-level refusals with no verified token), and `UPDATE` as superuser is refused (`eng_tool_calls is append-only`). The probe's own row-count assertion was wrong (it expected 18) and reported a FAIL; the data are correct.
- **Sandbox boundary, probed with the runner's exact flags.** uid 65534; read-only root; `CapEff 0`; only `lo` plus down tunnel devices; outbound TCP fails with `Network is unreachable`; DNS fails. ADR-004 scope stands: accepted for deterministic fixtures only, not for LLM-generated code.

### C8 (RESOLVED 2026-11-18 11:20, uncommitted): fixture yields identical commits

**Fix** (`crates/workspace/src/tests.rs` only; `fixture.rs` untouched): the non-ancestor case no longer uses a second fixture. It creates an unrelated root commit *inside the same repository* with `git commit-tree` (HEAD unchanged, asserted), checks it differs from base and candidate, and asserts the specific `Err(WorkspaceError::BaseNotAncestor)` rather than any error. **Verification:** on a clean export of `c8bff57`, 9/9 pass, 5/5 repeats, `rustfmt --check` and `clippy -D warnings` clean; **mutation** (ancestor check removed from `Workspace::diff`) turns the test red at the new assertion. In the live checkout (with another session's uncommitted 6B changes), `workspace` 9/9 and `engineering` 32/32 after a clean rebuild.

*Verification incident:* the export was first built with the live `target/` dir; cargo reused the export's (mutated) artifacts for the live tree, which produced 4 false failures. Resolved with `cargo clean -p workspace -p engineering` and a rebuild from live sources. Lesson: never share `CARGO_TARGET_DIR` between two source trees.

Original diagnosis:

`fixture::create` makes the same tree, author, message and (1-second) timestamp, so two fixtures created within one second have **identical SHAs**. `diff_refuses_wrong_or_abbreviated_base` builds a second fixture and expects its candidate SHA to be a non-ancestor; it is in fact the same commit, so `diff` correctly succeeds and the assertion fails. Proof by elimination: a SHA absent from the repo makes `git merge-base --is-ancestor` exit 128 → `is_ancestor` returns `Err` → `diff` returns `Err`; `Ok` is only possible if the commit exists in this repo. Product behaviour is correct; the test does not exercise the non-ancestor path. Fix: make the second repo differ (e.g. a distinct commit message or file), then re-run.

### Remaining risks found in review (not blocking 6A)

| Severity | Item | Where |
|---|---|---|
| Medium | Tool evidence checks existence/run/role/commit, not that artifact content matches the audited result | `eng/orchestrator.rs` `verify_tool_evidence` |
| Low | Symlink check and read are not atomic (TOCTOU); only the MCP server touches workspaces today | `workspace::Workspace::resolve` / `read_file` |
| Low | Capability token travels in the A2A message body over plain HTTP; acceptable only on loopback/private network | `eng/a2a_role.rs`, `RoleTask.mcp_token` |
| Info | Only the Code Reviewer is an A2A role agent; the other five roles are still in-process simulation. `workspace.apply_patch` does not exist yet (6B M5) | `apps/role-agents` |
| Info | `MCP_DATABASE_URL` should use a login role in `a2a_mcp`; the probe used the local superuser on a scratch DB | ops |
| Info | Commit message of `c8bff57` does not describe its content | history |

---

**Original verdict (at `49c8773`): Phase 6A FAILS its mandatory MCP checks (4–8). Following the Phase 6B instruction ("if mandatory Phase 6A checks fail, stop implementation and report a corrective backlog"), no Phase 6B code was written.** This document is the audit, the compatibility assessment, the design for 6B, and the backlog that has to land first.

## 1. Phase 6A technical audit

| # | Check | Result | Evidence |
|---|---|---|---|
| 1 | Rust workspace compiles | PASS | `cargo clippy --workspace --all-targets --locked -D warnings` |
| 2 | Worker and Reviewer callable through official A2A | PASS (5/5 each) | `worker-agent/tests/a2a.rs`, `reviewer-agent/tests/a2a.rs` (real HTTP, JSON-RPC, SDK `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9`) |
| 3 | Agent Cards discoverable and valid | PASS | `card_is_discoverable_and_advertises_jsonrpc_only` (both agents); orchestrator `A2AGateway::discover` + `trusted_card` (origin and JSON-RPC interface check) |
| 4 | MCP server starts independently | **FAIL: does not exist** | No MCP crate or binary in the workspace; `rmcp` is absent from `Cargo.lock`; no `mcp` reference in `backend/`, `frontend/src`, `docs/`, `.env.example` |
| 5 | MCP tool discovery and invocation | **FAIL: does not exist** | `repository.read_file`, `repository.diff` and `testing.run` are not defined anywhere |
| 6 | A2A agent invokes an MCP tool | **FAIL: does not exist** | Worker/Reviewer only call the LLM client (`agent_runtime::LlmClient`) |
| 7 | A2A artifacts contain validated tool results | **FAIL: does not exist** | Artifacts are LLM drafts and reviews; no tool evidence |
| 8 | Tool authorization enforced server-side | **FAIL as specified** (partial precursor) | `engineering::roles::authorize` checks tool requests *reported* by an adapter, **after** the fact; nothing executes, so there is no server-side enforcement point (PHASE6.md §6 says the same) |
| 9 | Existing tests pass | **FAIL (1 persistent, environment suspected)**: full run 137 passed / 6 failed; single-threaded re-run of the 6: 5 pass, `phase3::workflows_survive_a_killed_process_and_interrupted_ones_fail` fails again (see §1a) | `cargo test --workspace --locked --no-fail-fast -- --test-threads=4` |
| 10 | Git status and user changes preserved | PASS | Tree clean before the audit; this file is the only change |

### 1a. Regression evidence (built from `HEAD 49c8773`, before the concurrent edits in §1b)

| Command | Outcome |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS (finished, no warnings) |
| `cargo test --workspace --locked --no-fail-fast -- --test-threads=4` | **137 passed, 6 failed**. phase3 13/17 (717 s), phase6 19/21 (2747 s), phase6_provider 9/9, workflow 25/25, engineering 25/25, contracts 8/8, agent-runtime 16/16, api unit 8/8, worker/reviewer unit 2+2, A2A 5+5 |
| The 6 failures | `PoolTimedOut` (3), sqlx test-DB create collisions `42P04`/`23505` (2), `api process did not become ready` (1). **None is an application assertion** |
| Re-run of the 6, `--test-threads=1` | 5 PASS. `workflows_survive_a_killed_process_and_interrupted_ones_fail` FAILS again at `phase3.rs:707`: the spawned `api` did not answer `/api/health` within ~10 s (200 × 50 ms). Host load average was ~157 on 8 cores, and one migration query took 1.03 s. That suggests slow startup, **not proven**. Note: this re-run compiled the working tree with the concurrent changes from §1b |

Corrective item **C0**: re-run phase3 on an unloaded host. If it still fails, investigate API startup; otherwise make the readiness wait configurable (e.g. 30 s). There are also 13 leftover `_sqlx_test_*` databases in the local Postgres; clean them up before re-running.

### 1b. Concurrent work observed during this audit (not mine, not reviewed)

While this audit ran (from ~09:17), uncommitted changes appeared in the same checkout from other sessions: `apps/mcp-server` (`rmcp =3.11.0`, streamable HTTP), `apps/role-agents`, `crates/workspace`, `crates/engineering/src/mcp.rs`, `contracts/src/role.rs`, `eng/a2a_role.rs`, `tests/phase6b.rs`, `fixtures/`, migrations `20261118000001/2`, edits to `orchestrator.rs`, `store.rs`, `ADR.md`, `PHASE6.md`, plus a new `docs/PHASE6C.md`. They look like work on backlog items C1–C6 below. **This audit does not cover them.** Checks 4–8 must be re-run against them once they are finished and committed.

### What is verified, mocked, or missing

- **Verified protocol communication:** orchestrator → Worker → Reviewer over real HTTP JSON-RPC A2A with the pinned official SDK (P0); Agent Card resolution and trust checks; `CancelTask` on deadline.
- **Mocked / simulated:** all six engineering roles run through `eng::sim` (provider `simulation`, in-process, **not A2A**); the Anthropic adapter is tested only against an HTTP test double (Mode B); the approval authenticator in tests is a test double, while `api serve` wires `Disabled` (503 `auth_unavailable`).
- **Incomplete or unsupported:** MCP (entirely); role agents over A2A; any real workspace, patch, or check execution (PHASE6 M5 not started); IdP; the Phase 6 portal (M8).

"Phase 6A" as the 6B prompt describes it (A2A + MCP integration with three repository/testing tools) **was never implemented in this repository.** The repo's Phase 6 record (`docs/PHASE6.md`) covers the engineering domain, persistence and approvals, and states that no execution exists yet.

## 2. Architecture compatibility assessment

The existing design is a good base for 6B; nothing has to be rewritten.

| 6B need | Existing asset | Fit |
|---|---|---|
| Central state machine | `crates/engineering::state::next` (actor rules, budgets, version) | Good. The state names differ from the 6B list (see §6); additive change |
| Role policy enforced in code | `roles::{Role, Tool, authorize, ALLOWED_CHECKS}` (closed enum, checks by id → fixed argv, path scope, branch namespace) | Good. Reuse as the MCP server's policy source |
| Typed, versioned, hashed artifacts | `artifacts.rs` (8 kinds, `deny_unknown_fields`, canonical-JSON SHA-256) | Good. The 6B names map 1:1 (§5) |
| Persistence and audit | `eng/store.rs`, migration `…000003` (append-only triggers, runtime role without UPDATE/DELETE) | Good. Add tables for MCP audit and workspaces |
| Human gates | `eng/approval.rs` (version, hash and commit binding, idempotency); `Authenticator` trait | Logic good; production BLOCKED (no IdP), which is correct behaviour |
| A2A transport | `api::a2a::A2AGateway`, `agent_runtime::server` | Good. Role agents can reuse it unchanged |
| Provider independence | `RoleAgent` adapter trait, `AgentIdentity{provider, model}` | Good |
| MCP | — | **Missing** |
| Isolated execution | — | **Missing**; `RoleInput.patch` and `base_commit_sha` are placeholders waiting for it |

Constraint from the ADRs: modular monolith, no broker, no microservice per role. 6B fits this: **one** new MCP server process and **one** shared role-agent A2A runtime.

## 3. HLD and LLD

### HLD

```
Human ─► Astro portal ─► API (Axum): orchestrator, state machine, approvals, Postgres
                          │  A2A JSON-RPC (official SDK)          │ issues per-run capability token
                          ▼                                      ▼
            role-agents process (one runtime, six A2A agents     MCP server (rmcp), the only
            at /agents/{role}, one Agent Card each)  ──MCP──►    process that touches workspaces
                                                                 │
                                                                 ▼
                                                    sandbox runner: disposable workspace,
                                                    no network, non-root, CPU/mem/time limits
```

- The orchestrator owns order and state. Agents never call each other and never change workflow state.
- Each agent run gets a **capability token** from the orchestrator: HMAC-signed, about 5-minute expiry, bound to `{workflow_id, run_id, role, workspace_id, allowed_tools, base_sha}`. The MCP server verifies it on **every call** and re-applies `roles::authorize`. A prompt cannot widen it.
- Facts come from tools, not models: the candidate SHA, patch digest, changed files, exit codes and logs are produced by the MCP server and returned to the orchestrator through the MCP audit record. The orchestrator **overwrites or cross-checks** any matching field in an agent's artifact, which is how a false "tests passed" claim is caught.

### LLD (new units)

| Unit | Responsibility |
|---|---|
| `apps/mcp-server` (new bin, `rmcp`, pinned exact) | Tools: `repository.read_file`, `repository.diff`, `testing.run`, `workspace.apply_patch`. Token verification, policy, path canonicalisation, size limits, audit insert |
| `crates/engineering::mcp` (pure) | Tool name ↔ `Tool` mapping, input/output contracts, `CapabilityClaims` |
| `crates/workspace` (new, pure + git) | Create workspace from a fixture/base SHA, apply a unified diff with `git apply --check` then `git apply` (argv, no shell), commit on `agent/<wf>/candidate`, compute digests. Rejects absolute paths, `..`, `.git/`, symlinks (`lstat` along the path), files over the limit, and a patch whose base ≠ HEAD |
| `apps/role-agents` (new bin, `agent_runtime::server`) | Six A2A agents with deterministic role adapters (Mode A) behind the existing `RoleAgent` trait; Developer/QA/Reviewer call MCP with the token received in message metadata |
| `eng/a2a_role.rs` | `RoleAgent` implementation that calls a remote role agent through `A2AGateway` (correlation id = run id; `CancelTask` on deadline) |
| `eng/store.rs` additions | `eng_workspaces`, `eng_tool_calls` (append-only), new statuses |
| `apps/api/tests/phase6b_*.rs` | Layers B–D and the negative tests |

## 4. Role and permission matrix (enforced at the MCP server and in the orchestrator)

| Role | A2A agent | `repository.read_file` | `repository.diff` | `workspace.apply_patch` | `testing.run` | Produces |
|---|---|---|---|---|---|---|
| Project Manager | `/agents/project-manager` | scoped (plan scope only) | no | no | no | `RequirementsPlan` |
| Coordinator | `/agents/coordinator` | no (metadata via artifacts) | no | no | no | `WorkAssignment` |
| Developer | `/agents/developer` | yes (assignment scope) | yes | yes, own workspace, token-bound base SHA | allowlisted ids | `CodeChangeArtifact` |
| Code Reviewer | `/agents/code-reviewer` | yes | yes (the exact candidate) | **no** | no | `CodeReviewReport` |
| QA Tester | `/agents/qa-tester` | yes | yes | **no** | yes (allowlist) | `QATestReport` |
| Quality Lead | `/agents/quality-lead` | no (artifacts only) | no | no | no | `DeliveryReadinessReport` |

No role has push, merge, deploy, approve, secret or shell tools; the `Tool` enum cannot express them. Separation of duties (reviewer ≠ developer, QA ≠ developer) is already enforced on agent id.

## 5. Agent communication contracts

| 6B name | Existing kind | Change needed |
|---|---|---|
| `RequirementsPlan` | `requirements_plan` | add `non_goals`, `dependencies`, `risks` (bump `schema_version` to 2) |
| `WorkAssignment` | `work_assignment` | add `workspace_id`, `depends_on` |
| `CodeChangeArtifact` | `code_candidate` | add `patch_digest`, `tool_call_id` (must match the MCP audit row) |
| `CodeReviewReport` | `review_report` | verdict `pass / needs_changes / blocked`; findings need `file` and `line`, checked against the diff |
| `QATestReport` | `qa_report` | each command references a `tool_call_id`; exit code and log digest are taken from the audit row, not the agent |
| `DeliveryReadinessReport` | `quality_assessment` | rename the values to `ready_for_human_review / changes_required / blocked` |

The envelope (already present except where noted) has: artifact id, workflow id, run id, producer agent id, role, schema version, created_at, content (stored inline; **content reference: add** for large logs), content digest, candidate SHA, provider and model.

A2A message: one `DataPart` (`application/json`) carrying `RoleInput`; metadata has `correlation_id` and the MCP capability token. Response: a completed `Task` with one artifact named after the kind. Typed errors reuse `AgentErrorKind` and the existing failure codes.

## 6. Workflow state machine (proposed delta)

```
draft → planning → awaiting_plan_approval ─(human)→ approved_for_execution → coordinating → developing
  → reviewing → testing → quality_assessment → awaiting_delivery_approval ─(human)→ approved_for_delivery*
side states: changes_requested (→ coordinating, bounded) · rejected · failed · cancelled
```

Mapping from today's states: `Draft` splits into `draft` (submitted) and `planning` (PM running); `ApprovedForExecution` stays a resting state and `coordinating` is new; `Executing` becomes `developing`; `Testing` splits into `testing` and `quality_assessment`. Implementation: extend `Status` and `next`, add a DB CHECK migration, and map old rows (no live production data). `approved_for_delivery`* stays reachable only through `approval.rs` with an authenticated approver, which is BLOCKED in `serve` until an IdP exists. New rule: a candidate change after review (new `tool_call_id` or SHA) invalidates the review and QA (already covered by manifest binding; add an explicit test).

## 7. Corrective backlog (must pass before 6B starts)

| ID | Item | Done when |
|---|---|---|
| C1 | Add `apps/mcp-server` using the official Rust MCP SDK `rmcp`, pinned to an exact version (check crates.io and the SDK examples at implementation time; the repo has no pin today). Streamable-HTTP transport on a private bind | The server starts on its own; `tools/list` returns the three tools with JSON schemas |
| C2 | Implement `repository.read_file`, `repository.diff`, `testing.run` (check id → `ALLOWED_CHECKS` argv; never a shell string) on a **fixture repository** | Invocation tests pass against the fixture |
| C3 | Capability tokens and server-side authorization in the MCP server, reusing `roles::authorize` | Unauthorized role/tool/path/workspace is refused **before** execution; refusals are audited |
| C4 | Execution boundary for `testing.run`. Decide and document: on this macOS host the candidates are a Docker container (`--network none`, read-only root, non-root, cgroup limits) for **test fixtures with deterministic adapters only**; LLM-generated code needs gVisor/Firecracker or a disposable VM (Linux), so it stays **BLOCKED** until one is available | ADR-004 recorded; boundary tests (no network, no host paths, limits enforced) pass |
| C5 | One A2A agent (Reviewer) calls `repository.diff` over MCP and embeds the result; the orchestrator validates it against the MCP audit row | A2A → MCP → artifact test passes |
| C6 | `eng_tool_calls` append-only audit table and migration | Every call, allowed or refused, is recorded with the token's run id |
| C7 | Re-run this audit | Checks 1–10 PASS |

## 8. Phase 6B backlog and test plan (after C1–C7)

1. **M2** Contract v2 fields (§5), the MCP tool contracts, and the permission matrix as a unit-tested table.
2. **M3** State machine delta (§6) and migration; unit tests for every transition, actor, budget and terminal state.
3. **M4** `apps/role-agents` (six cards) and `eng/a2a_role.rs`; Layer B tests: per-agent card capabilities, independent invocation, malformed input, timeout plus `CancelTask`, unavailable server.
4. **M5** `workspace.apply_patch` in `crates/workspace` and the MCP server; Layer C tests: traversal, symlink escape, wrong base SHA, protected branch, size limits, reviewer write refused, QA arbitrary command refused, audit rows present.
5. **M6–M7** Wire Developer → Reviewer (exact diff) → QA (real `cargo test` in the sandbox on the candidate SHA) → QL; facts cross-checked against audit rows.
6. **M8** Astro pages (overview, detail, artifacts, decision status) reading `/api/eng`; no approval buttons while the authenticator is `Disabled`.
7. **M9** Layer D end-to-end test on a fixture Axum crate ("add request validation…") with the test-only approval authenticator (already `#[cfg(test)]`-wired, never in `serve`); asserts that a real file changed, the candidate SHA came from git, and the QA exit code came from the sandbox.
8. **M10** The 16 negative tests from the 6B prompt. Most map to existing tests (N1–N14 in PHASE6.md) that must be re-run over A2A + MCP; new ones: reviewer write, QA shell command, path traversal at MCP, wrong base SHA, false test-success claim, candidate change after review, secrets request, and a killed-process restart.

## 9. Phase 6B quality gate (current status)

| Criterion | Status |
|---|---|
| Phase 6A remains functional | **FAIL** (6A MCP never built; the A2A/P0 part passes except the killed-process restart test, §1a) |
| Agent roles correctly defined | PASS (domain, Phase 6) |
| A2A handoffs across role boundaries | UNVERIFIED (roles are in-process simulation) |
| Each stage produces validated artifacts | PASS (simulation) |
| Orchestrator enforces task ordering | PASS (simulation) |
| MCP permissions enforced per role | BLOCKED (no MCP) |
| Developer patch generation in isolation | BLOCKED (no workspace or sandbox) |
| Reviewer examines the exact patch | BLOCKED |
| QA tests the exact candidate | BLOCKED |
| Quality Lead evidence-based | UNVERIFIED (evidence is simulated) |
| Workflow pauses at approval boundaries | PASS (Phase 6 tests) |
| Agents cannot bypass human authorization | PASS (Phase 6 N1/N2) |
| Restart recovery and concurrency | PASS in-process; killed-process test missing |
| Existing and new Rust tests pass | FAIL: 1 existing test still fails (§1a); no new tests |
| Astro functionality valid | UNVERIFIED in this audit (not re-run) |
| No critical unresolved security issues | No new code; the open item is the missing isolation boundary (C4) |

**Phase 6B: NOT STARTED (blocked on C1–C7). Phase 6C readiness: FAIL.**
