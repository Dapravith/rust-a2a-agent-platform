# Phase 6C: Phase 6B readiness audit and corrective backlog

Date: 2026-11-18 · Branch `phase6/native-rust-protocols` @ `49c8773` (working tree: `docs/PHASE6.md` modified, `docs/PHASE6B.md` untracked, both left untouched) · Rust 1.100.0 · Postgres 17 at `127.0.0.1:54329`

**Verdict: Phase 6B is NOT STARTED. Its mandatory checks fail. Following the Phase 6C instruction ("if mandatory Phase 6B checks fail, stop and provide a corrective backlog; do not begin provider integration over an unverified foundation"), I wrote no Phase 6C code.** This file holds the audit, the provider assessment from local evidence, the adapter design that the backlog leads to, and the ordered work that has to land first.

## 1. Phase 6B quality audit (Step 1)

| # | Check | Result | Evidence (this session) |
|---|---|---|---|
| 1 | Git status / structure | PASS | Only the two doc changes above; workspace = `api`, `worker-agent`, `reviewer-agent`, `contracts`, `agent-runtime`, `engineering` |
| 2 | PRD / ADRs read | PASS | `A2A_PRD.md` §9, `docs/ADR.md` ADR-003, `docs/PHASE6.md`, `docs/PHASE6B.md` |
| 3 | Phase 6B implementation and evidence | **FAIL** | `docs/PHASE6B.md` is an audit that stopped on backlog C1–C7. No 6B code exists. Its result placeholders (`__R1__`, `__R2__`, `__R3__`, `__R9__`, `__R9S__`) were never filled in |
| 4 | Workflow state machine | PASS (Phase 6 scope) | `crates/engineering::state`, 25 unit tests per PHASE6 §8. The 6B state delta (`coordinating`, `developing`, `quality_assessment`) is not implemented |
| 5 | Agent role contracts | PASS (domain) | `roles.rs`, `artifacts.rs`. The 6B v2 fields (`patch_digest`, `tool_call_id`, …) are not implemented |
| 6 | Real A2A communication for roles | **FAIL** | Only P0 Worker/Reviewer use A2A. The six engineering roles run in-process (`eng/sim.rs`, `eng/providers/anthropic.rs`). There is no `apps/role-agents` and no `eng/a2a_role.rs` |
| 7 | MCP tool permissions | **FAIL: no MCP** | `rmcp` is not in `Cargo.lock`. `grep -ri mcp backend/apps backend/crates frontend/src` returns no matches |
| 8 | Isolated code execution | **FAIL: none** | There is no `crates/workspace`, sandbox runner or ADR-004. `RoleInput.patch` and `base_commit_sha` are still placeholders |
| 9 | Artifact persistence and correlation | PASS (simulation) | PHASE6 E2E-1/2, N4, N11 |
| 10 | Human approval boundaries | PASS (logic) / BLOCKED (prod) | PHASE6 N1, N2, N10, N14. `api serve` wires `Disabled` → 503 `auth_unavailable` (correct behaviour) |
| 11 | Regression suite | **UNVERIFIED** | `cargo fmt --check` PASS. I did **not** run tests: another `cargo test --workspace` (PID 62834, not started here, running 39 min in this repo) was using the shared Postgres, and a concurrent run is what corrupted the last regression (PHASE6 §8a). `phase3.rs` still needs a solo re-run |

**Can the platform run the deterministic engineering workflow without violating authorization policy?** Yes, in-process with simulated agents (Phase 6 Mode A). It cannot run it over A2A, through MCP, or against a real workspace, and those are exactly the parts Phase 6C builds on.

## 2. Existing provider integration assessment

| Asset | State |
|---|---|
| `RoleAgent` trait (`eng/agents.rs`) + `AgentIdentity{provider, model}` | Good seam for a provider-neutral contract. Provider ≠ role already holds |
| `eng/providers/anthropic.rs` (Messages API, PM/Coordinator/Reviewer/QL only) | Mode B PASS (9 contract tests). Mode C never run (no API key). It refuses Developer/QA, which is correct |
| Simulation | Test-only, never wired by `serve` |
| Provider registry / allowlist | Missing |
| Usage / cost recording | Tokens recorded per run. No cost, no budget in money |
| Coding-agent runtime (Claude Agent SDK, Codex, Gemini CLI) | Missing. Each needs the workspace/sandbox from C4/C8 below |

## 3. Provider compatibility and capability matrix

Docs-based columns come from PHASE6.md §7 (first-party docs checked 2026-11-17, not re-checked today). "Local" is what I observed on this machine.

| Provider | Integration method | Local CLI | Credentials found (no secrets shown) | Coding / tools | Structured output | Cancel / timeout | Usage | Test status |
|---|---|---|---|---|---|---|---|---|
| Claude Code | Claude Agent SDK (TS/Python) sidecar; `canUseTool` + hooks | `claude` 2.1.326 | **claude.ai Pro login. No `ANTHROPIC_API_KEY`** | Built-in Read/Edit/Bash, `allowedTools` | final message / schema | interrupt, sessions | per-result | Not started; blocked by sandbox (C4) and credential decision (D1) |
| Anthropic Messages API | Existing Rust adapter | n/a | **None** | none (non-executing roles) | JSON schema | deadline drop | tokens | Mode B PASS, Mode C BLOCKED |
| OpenAI Codex | Codex SDK / `codex exec --json` sidecar | `codex` 0.167.0-alpha.3.2 | **ChatGPT login. No `OPENAI_API_KEY`/`CODEX_API_KEY`** | `--sandbox workspace-write` | `--json` events | process kill / resume | in events (unverified) | Not started; same blockers |
| Gemini | Gemini CLI headless `-p --output-format json` | `gemini` 0.65.0 | OAuth token file only; no `GEMINI_API_KEY`; no `settings.json` auth type | built-in tools, `-s` sandbox | json / stream-json | process kill | `stats.models.*.tokens` | Not started |
| Grok | xAI API (Rust `reqwest`), client-side function calling | n/a | **None** (`XAI_API_KEY` unset) | via our MCP tools only | `json_schema` strict | deadline drop | in response | Not started; BLOCKED by credentials |

Not verified (still open from Phase 6): whether a consumer subscription login (claude.ai Pro, ChatGPT) may drive an application's automated workflow under each vendor's terms. **Phase 6C must not assume it may** (decision D1).

## 4. HLD and LLD (target, after the backlog)

```
Human ─► Astro ─► API (Axum): orchestrator, state machine, approvals, provider registry, audit (Postgres)
                    │ A2A JSON-RPC (official SDK)                │ per-run HMAC capability token
                    ▼                                            ▼
     role-agents process: six A2A agents          MCP server (rmcp, pinned): the only process that
     /agents/{role}; each binds a provider ──MCP──► touches workspaces; re-applies roles::authorize
     adapter from the registry                      │
        │ supervised child (stdio, bounded)         ▼
        ▼                                         sandbox: disposable workspace, no network,
     provider sidecar (Agent SDK / codex /          non-root, CPU/mem/time limits
     gemini), inside the sandbox
```

LLD additions for 6C: `crates/engineering::providers` (pure: `ProviderId` allowlist, `Capabilities`, role compatibility check, budget arithmetic, error classes), `eng/providers/registry.rs` (config → validated catalog; no user-supplied endpoints), `eng/providers/process.rs` (supervised sidecar: env scrubbed to one provider's secret, stdin/stdout NDJSON with size caps, deadline → SIGTERM → SIGKILL on the process group), one adapter per provider, `eng_provider_executions` audit table.

## 5. Provider adapter contract (proposed; extends `RoleAgent`, does not replace it)

```rust
trait ProviderAdapter: Send + Sync {
    fn capabilities(&self) -> &Capabilities;                 // static, validated at registry load
    async fn execute(&self, req: ExecutionRequest) -> Result<ExecutionResult, ProviderError>;
    async fn cancel(&self, exec: &ProviderExecutionId) -> Result<(), ProviderError>; // Unsupported allowed
}
// ExecutionRequest: workflow_id, run_id, role, provider, model, instructions, input artifact refs,
//   workspace_id (opaque; resolved only by MCP), capability token, deadline, budget, correlation_id
// ExecutionResult: status, provider+model identity, output artifact (untrusted until validated),
//   tool_call_ids (cross-checked against eng_tool_calls), duration, usage{tokens, cost, cost_kind: reported|estimated},
//   provider_execution_id
// ProviderError: Auth | RateLimited{retry_after} | Timeout | Malformed | Unsupported | Unavailable | Budget
```

Provider SDK types stay inside each adapter. Code-changing executions are never retried automatically.

## 6. Authentication and sandbox design

- Secrets: one env var per provider, loaded only by the API. Each sidecar receives **only its own** secret via a scrubbed environment and runs in the sandbox. Secrets never reach the browser or the logs (redaction tests extend the existing ones).
- Sidecars get no repository write access of their own. Writes happen only through MCP `workspace.apply_patch` with a token bound to the run's role, workspace and base SHA. The reviewer token has no write tool.
- Sandbox: ADR-004 (C4) decides the boundary. Containers alone are not enough for LLM-generated code (PRD §9.5), so live Developer runs stay BLOCKED until that boundary exists.

## 7. Corrective backlog (ordered; must pass before any 6C code)

| ID | Item | Done when |
|---|---|---|
| C0 | Re-run the full regression **alone** on the database (no other `cargo test` process) | `cargo test --workspace` green, incl. `phase3.rs` 17/17 |
| C1–C7 | Phase 6B backlog exactly as `docs/PHASE6B.md` §7 (MCP server with `rmcp`, three tools on a fixture repo, capability tokens, ADR-004 execution boundary, Reviewer A2A→MCP test, `eng_tool_calls`, re-audit) | PHASE6B §1 checks 1–10 PASS with filled-in evidence |
| C8 | Phase 6B M2–M10 (role agents over A2A, `workspace.apply_patch`, Developer → Reviewer → QA → QL on a real candidate, negative tests) | PHASE6B §9 gate PASS |
| D1 | **Owner decision:** which credentials 6C may use (dedicated API keys with spend limits are recommended; confirm whether the subscription logins are allowed) | Written decision in ADR / this file |
| D2 | **Owner decision:** spend budget for live tests (Layer C, D) | Cap recorded and enforced in config |

## 8. Phase 6C implementation backlog and test plan (after section 7)

1. **P6C-01** Registry + capability catalog + role compatibility (Layer A unit tests: mapping, validation, budget, error classes).
2. **P6C-02** First provider: a coding runtime with scoped editing (Claude Agent SDK or Codex, chosen by D1). Layer B against a fake sidecar (auth fail, timeout, malformed, unsupported tool, cancel, rate limit, usage parsing). Then Layer C live with a small task.
3. **P6C-03** Second vendor in the Reviewer role (read-only token). Layers B + C.
4. **P6C-04** Layer D: "add validation to an existing Axum payload" on a fixture crate. Provider A develops, Provider B reviews the exact SHA, the QA runner runs real tests, QL summarizes, and the run stops at the delivery gate.
5. **P6C-05** Gemini, Grok where D1 allows.
6. Audit table, Astro provider/execution views (read-only; no approval action while `Disabled`), Layer E security tests (credential isolation, unauthorized MCP tool, fabricated success, reviewer write, self-approval, provider swap, candidate change invalidates review, SSRF via endpoint, orphan child processes after cancel/timeout).

## 9. Phase 6C quality gate (current)

| Criterion | Status |
|---|---|
| Phase 6B remains functional | **FAIL** (never built) |
| Provider-neutral contracts implemented | FAIL (designed only, §5) |
| Provider capabilities validated | UNVERIFIED (docs + local CLI presence only) |
| ≥2 independent providers pass live tests | **BLOCKED** (no API credentials; D1; foundation) |
| Cross-provider handoffs over real A2A | BLOCKED (roles not on A2A) |
| Authorized MCP tool calls work | BLOCKED (no MCP) |
| Code changes in a restricted workspace | BLOCKED (no workspace / sandbox) |
| Reviewer inspects the exact candidate | BLOCKED |
| QA genuine test evidence | BLOCKED |
| Artifacts persisted and correlated | PASS (simulation, Phase 6) |
| Provider/model identity recorded | PASS (per run, Phase 6) |
| Provider timeouts/errors handled | PASS (Anthropic adapter, Mode B) |
| Cost/execution budgets enforced | PARTIAL (run budgets yes; money no) |
| Secret and tool boundaries tested | PARTIAL (post-hoc tool check only) |
| Astro shows real provider execution | FAIL (not built) |
| No agent can bypass human delivery approval | PASS (Phase 6 N1/N2) |
| Mandatory tests pass | UNVERIFIED (C0) |

**Phase 6C: NOT STARTED, blocked on section 7. Phase 6D readiness: FAIL.** No provider was invoked, nothing was spent, nothing was committed.
