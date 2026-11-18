# Phase 6C: Phase 6B readiness re-audit (after corrective backlog C1–C7)

Date: 2026-11-18 · Branch `phase6/native-rust-protocols` @ `49c8773` + uncommitted Phase 6B work (nothing committed) · Rust 1.100.0 · Node 24.21.0 (per `.nvmrc`) · Postgres 17
Test database: a private throwaway container `a2a-6b-testdb` (`127.0.0.1:54339`, tmpfs). The shared database at `:54329` was in use by another session's test run, and concurrent sqlx runs on one server collide (PHASE6 §8a).

**Verdict (updated after B1–B7): Phase 6B PASS, with three stated exceptions (§6): no killed-process restart test, the Playwright e2e suite was not run, and production approvals are BLOCKED by design (no IdP). The Phase 6C foundation is ready. Phase 6C live work is blocked only by owner inputs: D1 (provider API keys) and D2 (spend cap).** The audit below was first written before B1–B7 and is kept as the record; §3 shows each item as done.

## 1. Step 1 audit

| # | Check | Result | Evidence (this session) |
|---|---|---|---|
| 1 | Git status / structure | PASS | New: `apps/mcp-server`, `apps/role-agents`, `crates/workspace`, `fixtures/sample-crate`, 2 migrations. Existing modules changed additively only |
| 2 | PRD / ADRs | PASS | `A2A_PRD.md` §9; ADR-003; **ADR-004 (new)**: MCP server and execution boundary |
| 3 | Phase 6B evidence | **PARTIAL** | Backlog C1–C7 PASS (§2). 6B M2–M10 not done (§3) |
| 4 | State machine | PASS (Phase 6 states) | `engineering` 30 unit tests. The 6B state delta (`coordinating`, `developing`, `quality_assessment`) is not implemented |
| 5 | Role contracts | PASS (v1) | Artifact contracts unchanged; the new A2A `RoleTask`/`RoleResult` envelope is in `contracts::role` |
| 6 | Real A2A for roles | **PARTIAL** | Code Reviewer runs as a real A2A agent (`phase6b.rs` C5: task id recorded, Agent Card trust check). PM, Coordinator, Developer, QA and QL are still in-process simulation |
| 7 | MCP tool permissions | PASS | `mcp.rs` 9 tests over real HTTP with the rmcp client; `engineering::mcp` 5 unit tests; mutation check red/green |
| 8 | Isolated code execution | **PARTIAL** | `testing.run` sandbox live test PASS (real `cargo test` in Docker, `--network none`, read-only, non-root, limits, 94 s). **No write path** (`workspace.apply_patch` not built), so no agent can produce a patch. Running LLM-written code stays BLOCKED by ADR-004 (a container is not an accepted boundary for it) |
| 9 | Persistence and correlation | PASS | `eng_tool_calls` row ↔ `eng_agent_runs.id` ↔ `artifact_recorded.detail.tool_calls` (`phase6b.rs` C5); append-only and least-privilege (C6 test) |
| 10 | Human approval boundaries | PASS (logic) / BLOCKED (prod) | Phase 6 N1/N2/N10/N14 still pass; `serve` keeps `Disabled` (no IdP) |
| 11 | Regression | PASS | Full workspace on the isolated DB: exit 0, incl. `phase3.rs` 17/17 (previously unverified), `phase6` 21, `phase6_provider` 9, `workflow` 25. `fmt` and `clippy -D warnings` clean. Portal `astro check` 0 errors, unit 31/31 (Playwright not run) |

**Can the platform run the deterministic workflow without violating policy?** Yes: end to end to the delivery gate, with the reviewer over A2A → MCP and its evidence verified against the audit. **Not yet** with a real patch produced in a workspace, or with QA running the sandboxed tests as a role agent.

## 2. Corrective backlog C1–C7 (result)

| ID | Done when | Result | Evidence |
|---|---|---|---|
| C0 | Clean solo regression | PASS | §1 row 11 |
| C1 | MCP server (official `rmcp =3.11.0`), Streamable HTTP, `tools/list` with schemas | PASS | `c1_tools_list_has_three_tools_with_closed_schemas` |
| C2 | `repository.read_file`, `repository.diff`, `testing.run` on a fixture | PASS | `c2_reviewer_reads_and_diffs_the_exact_candidate`; `workspace` 9 tests (traversal, symlinks, size, binary, base not an ancestor, repo-local git config cannot execute) |
| C3 | Capability tokens; refusal **before** execution; refusals audited | PASS | `c3_requests_without_a_valid_token_…` (missing / forged / expired / wrong key → 401, audited); `c3_role_and_token_limits_…` |
| C4 | ADR-004 + boundary test | PASS for fixtures | `live_sandbox_runs_checks_without_network` (live, Docker); `docker_args_enforce_the_boundary`; image must be digest-pinned. Network denial is asserted from the argv, not probed from inside the container |
| C5 | A2A agent → MCP → artifact, validated against the audit | PASS | `c5_reviewer_reviews_the_exact_candidate_over_a2a_and_mcp`; negatives: fabricated id, no diff, refused call, foreign run, other commit → `tool_evidence_invalid`; MCP refusal → honest `agent_failed`. Mutation check red/green |
| C6 | Append-only `eng_tool_calls` | PASS | `c6_tool_call_audit_is_append_only_and_least_privilege` (UPDATE/DELETE/TRUNCATE refused as owner; `a2a_mcp` INSERT only, `a2a_app` SELECT only) |
| C7 | Re-audit | Done | This file |

## 3. Remaining Phase 6B work (corrective backlog for 6C)

| ID | Item | Done when |
|---|---|---|
| B1 | **DONE 2026-11-18.** `workspace.apply_patch` (MCP, Developer only): pure pre-check `engineering::mcp::patch_paths` (git-format only; no binary, symlink/submodule/exec modes, renames, quoted paths; header consistency; ≤ 512 KiB; every path through `roles::authorize` + commit capability). Then `git apply --check`, `git apply --index`, a **staged-state re-check** (mode 100644 only, in scope), and a commit on `agent/<wf>/candidate` with a token-bound base. Any failure resets to HEAD. A per-workspace lock covers every tool (closes the read time-of-check race) | `engineering` 2 new unit tests; `workspace` 3 (incl. a symlink and an exec patch that bypass the parser, `build.rs` out of scope, wrong branch/base); `mcp.rs` 3 over real HTTP (commit + audit row, reviewer/QA refused, `.git/hooks` refused, nothing written); concurrency test fails 3/3 without the lock (mutation) |
| B2 | **DONE.** `POST /control/workspaces` on the MCP server, behind an orchestrator-only control token (its own HMAC domain; agent tokens get 401). It clones the configured `MCP_SOURCE_REPO` at the approved assignment's base into a **server-chosen** id, with no remote left. The orchestrator records the id in append-only `eng_workspaces` (migration `…000003`); `RoleInput.workspace_id` replaces the `workspace_id = workflow_id` convention | `workspace` 2 tests (create, unknown/short base leaves nothing); `phase6b` `b2_control_endpoint_…` (no/garbage/agent token → 401 and audited; unknown base refused); `mcp_backed_stage_without_mcp_control_fails_before_running` |
| B3 | **DONE.** `apps/role-agents` hosts all six role agents over A2A (one process, one Agent Card each at `/agents/<role>`), all **deterministic (rule-based, no model)**. Developer: `workspace.apply_patch` with a configured patch. QA: the assignment's checks via `testing.run`. PM/Coordinator/QL: artifacts only, no MCP token. `api serve` wires them with `ENG_PROVIDER=a2a` | `phase6b` `six_role_agents_over_a2a_and_mcp_reach_the_delivery_gate` (6 A2A task ids, MCP calls per role, delivery gate); **live**: `live_sandbox_qa_runs_real_tests_on_the_candidate` (real `cargo test` in Docker on the developer's MCP commit, exit 0, ~20 s); `developer_without_a_patch_fails_honestly` |
| B4 | **DONE.** The orchestrator requires the latest audited `apply_patch` of the run to match the candidate's commit, patch digest and file set. A review is accepted only if its audited diff has the candidate's digest. `eng_tool_calls.patch_sha256` is computed by the MCP server | `b4_developer_cannot_report_facts_mcp_did_not_produce` (wrong commit, wrong digest, hidden file, no apply); `b4_review_of_a_different_patch_is_rejected`; mutation red/green |
| B5 | **DONE.** `agent_runtime::RedactingTaskStore` strips `mcp_token` from stored task history; the executor still receives the original request (SDK passes `req.message`) | `b5_mcp_tokens_are_not_kept_in_agent_task_history` (real `GetTask` with full history on 3 agents); mutation (plain store) → "token leaked" |
| B6 | **DONE.** `/engineering` (providers + workflows) and `/engineering/{id}` (status, stages with provider/model/A2A task, MCP tool calls per run, artifacts, gates, human decisions, audit). Read-only: no approval or create action; a callout says approvals are disabled without an IdP. Rule-based/simulated runs are never labelled as models. The API detail gained `tool_calls` (digests only, no arguments) and `workspace_id` | `astro check` 0 errors; unit 35/35 (6 new); E2E asserts the detail API; real detail JSON rendered: no horizontal overflow at 320/390/1280 px (nav wrap fix); Playwright e2e suite not run |
| B7 | **DONE (additive scope).** New state `quality_assessment` + agent event `qa_recorded` (QA in `testing`, QL in `quality_assessment`; migration `…000004`). Contract v2: `RequirementsPlan.non_goals` (default empty). **Rejected on purpose:** an agent-written `workspace_id` in the assignment (it would let an agent pick its workspace; the orchestrator owns it); per-command `tool_call_id` in artifacts (superseded by B4 audit cross-checks and `artifact_recorded.tool_calls`); renaming verdict/recommendation values (breaking, no safety gain) | `engineering` 34 (new QA→QL ordering/actor test); `b7_every_workflow_status_is_accepted_by_the_schema`; `phase6` 21 unchanged |

## 4. Phase 6C blockers outside the code

| ID | Blocker | Needed from the owner |
|---|---|---|
| D1 | **No provider credentials.** `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`/`CODEX_API_KEY`, `GEMINI_API_KEY`, `XAI_API_KEY` are unset (env and `.env`). Only consumer logins exist (Claude Code: claude.ai Pro; Codex: ChatGPT), and those are not assumed to authorize automated SDK use | Dedicated API keys (recommended, with spend limits) for at least two vendors, or written confirmation that the subscription logins may be used |
| D2 | Spend cap for live tests (Layers C/D) | A number |
| D3 | Isolation for LLM-written code (ADR-004): Docker on macOS is accepted only for fixtures | Accept Docker for **dev-only** live tests on fixture repos, or provide a Linux host with gVisor/Firecracker |

**Owner decisions (2026-11-18):** D1: the owner will add dedicated API keys with spend limits to the git-ignored `.env`; subscription logins are not used. D2: open (needed before the first live run). D3: Docker accepted for dev-only fixture runs (ADR-004 amendment). Next: finish Phase 6B B1–B7 before any 6C code.

## 5. Provider matrix (unchanged since PHASE6 §7; docs not re-checked in this pass)

| Provider | Method | Local CLI | Credentials | Status |
|---|---|---|---|---|
| Claude Code | Agent SDK sidecar (TS/Python), `canUseTool` hooks, MCP client to our server | `claude` 2.1.326 | Pro login only | Not started (D1, D3, B1) |
| Anthropic Messages API | Existing Rust adapter (non-executing roles) | n/a | none | Mode B PASS, live BLOCKED (D1) |
| OpenAI Codex | `codex exec --json` / SDK sidecar, MCP client | `codex` 0.167.0-alpha.3.2 | ChatGPT login only | Not started (D1, D3, B1) |
| Gemini | CLI headless `--output-format json` | `gemini` 0.65.0 | OAuth token file only | Not started (D1) |
| Grok | xAI API via Rust `reqwest`; our MCP tools via function calling | n/a | none | Not started (D1) |

## 6. Phase 6B gate (final, 2026-11-18) and Phase 6C gate

Full regression on the isolated DB: `fmt` PASS, `clippy -D warnings` PASS, `cargo test --workspace` **195 passed / 0 failed** (18 suites), live sandbox tests 2/2 PASS (workspace boundary; six-agent end to end with real `cargo test` QA in Docker, 28.7 s). Portal: `astro check` 0 errors, unit 35/35.

| Phase 6B criterion | Status | Evidence |
|---|---|---|
| Phase 6A (P0 + Phase 6) remains functional | PASS | `workflow` 25, `phase3` 17, `phase6` 21, `phase6_provider` 9 |
| Agent roles defined; provider ≠ role | PASS | `RoleAgent` + `AgentIdentity`; role policy in `engineering::roles` |
| A2A handoffs across role boundaries | PASS | six agents, six A2A task ids per workflow (`phase6b` E2E) |
| Each stage produces validated artifacts | PASS | contracts + orchestrator validation; liars rejected |
| Orchestrator enforces ordering | PASS | state machine incl. B7 `qa_recorded`; `engineering` 34 |
| MCP permissions per role, server-side | PASS | `mcp.rs` 12; tokens narrow and never widen; reviewer/QA cannot write |
| Developer patch in an isolated workspace | PASS (deterministic patch) | `apply_patch` via MCP into an MCP-created workspace; B4 cross-checks |
| Reviewer examines the exact patch | PASS | audited diff digest = candidate digest (B4) |
| QA tests the exact candidate | PASS (live) | Docker sandbox, exit code from the audit row |
| Quality Lead evidence-based (advice only) | PASS | rule-based from review + QA; workflow waits for a human |
| Pauses at approval boundaries; agents cannot approve | PASS | Phase 6 N1/N2; delivery gate reached, `awaiting_decision = delivery` |
| Restart recovery | PASS in-process / **OPEN**: killed-process test | Phase 6 N9; B7 state covered by `recovery_event` test |
| Astro valid | PASS (check, unit, visual at 320/390/1280) / **UNVERIFIED**: Playwright e2e not run | |
| Production human approval | **BLOCKED (by design)** | no IdP → `Disabled` (Phase 6D) |

Security notes still open: refused-request auditing has no rate limit (loopback-only today); the Docker boundary is accepted for dev fixtures only (ADR-004 amendment); `api serve` with `ENG_PROVIDER=a2a` needs `MCP_TOKEN_KEY` shared with the MCP server, so a secret store is needed outside dev.

### Phase 6C gate (current)

| Criterion | Status |
|---|---|
| Phase 6B remains functional | PASS |
| Provider-neutral contracts | PASS (`RoleAgent`, `RoleTask`/`RoleResult`, tool evidence); provider registry not yet built (6C P6C-01) |
| ≥ 2 independent live providers | **BLOCKED** (D1 keys, D2 cap) |
| Everything else | Foundation PASS (as above); provider-specific items not started |

**Phase 6C: ready to start (P6C-01 registry first) as soon as D1 and D2 are provided. Phase 6D readiness: FAIL (no IdP).**
