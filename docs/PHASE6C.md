# Phase 6C: Phase 6B readiness re-audit (after corrective backlog C1–C7)

Date: 2026-11-18 · Branch `phase6/native-rust-protocols` @ `49c8773` + uncommitted Phase 6B work (nothing committed) · Rust 1.100.0 · Node 24.21.0 (per `.nvmrc`) · Postgres 17
Test database: a private throwaway container `a2a-6b-testdb` (`127.0.0.1:54339`, tmpfs). The shared database at `:54329` was in use by another session's test run, and concurrent sqlx runs on one server collide (PHASE6 §8a).

**Verdict: the Phase 6B corrective backlog C1–C7 is done and verified, but Phase 6B itself is still incomplete (its M2–M10). Following the 6C instruction ("if mandatory Phase 6B checks fail, stop and provide a corrective backlog"), no Phase 6C provider code was written.** Also, no provider credential exists in this environment, so the 6C live gate could not pass even with the foundation complete.

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
| B2 | Workspace creation from an approved base by the MCP side (replaces the `workspace_id = workflow_id` convention with a stored mapping) | Orchestrator never names a path |
| B3 | Role agents over A2A for Developer and QA (QA calls `testing.run`, and its exit codes come from audit rows), then PM/Coordinator/QL | All six stages A2A; `phase6b` E2E reaches the delivery gate with a real patch and sandbox QA |
| B4 | Candidate SHA and patch digest taken from MCP rows, never from the agent (orchestrator overwrite/cross-check for Developer) | Test: a lying developer is rejected |
| B5 | Remove the MCP token from the role agent's stored A2A history | Test: `GetTask` history has no token |
| B6 | Portal pages for `/api/eng` (read-only; no approval actions while `Disabled`) | Astro check, unit and Playwright |
| B7 | State-machine delta and contract v2 fields (PHASE6B §5–6) | Unit tests for every transition |

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

## 6. Phase 6C quality gate (current)

| Criterion | Status |
|---|---|
| Phase 6B remains functional | PASS for what exists (§1 row 11); 6B itself incomplete (§3) |
| Provider-neutral contracts | PARTIAL: `RoleAgent` + `RoleTask`/`RoleResult` + tool evidence; no registry |
| Provider capabilities validated | UNVERIFIED |
| ≥ 2 independent live providers | **BLOCKED** (D1) |
| Cross-provider handoffs over real A2A | PARTIAL (reviewer only, deterministic) |
| Authorized MCP tool calls work | PASS |
| Code changes in a restricted workspace | BLOCKED (B1, D3) |
| Reviewer inspects the exact candidate | PASS (MCP diff, verified against the audit) |
| QA genuine test evidence | PARTIAL: sandbox runs real tests (C4), but no QA agent uses it yet (B3) |
| Artifacts persisted and correlated | PASS |
| Provider/model identity recorded | PASS (per run and artifact) |
| Timeouts and errors handled | PASS (existing + MCP refusal path) |
| Cost/execution budgets | PARTIAL (run budgets; no money caps) |
| Secret and tool boundaries tested | PASS for tools; secrets: no provider secrets exist yet |
| Astro shows real provider execution | FAIL (B6) |
| No agent bypasses human delivery approval | PASS |
| Mandatory tests pass | PASS (Playwright not run) |

**Phase 6C: NOT STARTED, blocked on §3 (code) and §4 (owner decisions). Phase 6D readiness: FAIL.**
