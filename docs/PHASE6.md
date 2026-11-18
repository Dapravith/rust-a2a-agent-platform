# Phase 6: multi-agent engineering collaboration and human-in-the-loop approval (verification record)

Date: 2026-11-17 · Branch: `phase5/agent-e2e-workflow` (HEAD `3e94306` + uncommitted Phase 5 changes + Phase 6 increment, nothing committed) ·
Rust 1.100.0 · Postgres 17 (local) · host load average 130–150 on 8 cores for the whole session.

**Bottom line.** Delivered: milestones **M1–M3**, the logic of **M7**, and **M4 partially**. M4 is the first
real provider adapter (Anthropic Messages API), for the four non-executing roles only. M1–M3 and M7 are verified
with deterministic simulation (Mode A); the adapter is verified against an HTTP test double of the provider
(Mode B). **Mode C (real provider calls) is BLOCKED: no API key in this environment. M5 (isolated code
execution), M8 (portal) and M9 are not started.** Human approvals are implemented but
**BLOCKED in production wiring**: no identity provider is configured, so `api serve` answers 503
`auth_unavailable`. Nothing below claims real multi-provider interoperability.

## 1. Starting point (Phase 5 inspection)

- Phase 5 verdict: CONDITIONAL PASS; its changes (engine failure-recording retry, frontend dependency and
  body-limit fixes, CI `npm audit`) are still **uncommitted** in this working tree. Phase 6 builds on top of them
  and does not modify them.
- P0 Worker → Reviewer workflow: verified (86 Rust + 31 browser tests in Phase 5). Preserved untouched:
  Phase 6 adds new tables (`eng_*`), a new crate and a new module; the P0 tables, engine and routes are unchanged.
- Smallest viable increment chosen: the parts every later milestone depends on and that can be verified
  without credentials: state machine, role/tool policy, artifact contracts, persistence + audit, approval engine,
  deterministic orchestration, and the mandatory negative tests.

## 2. Architecture review and ADR / PRD updates (M1)

- `docs/ADR.md` **ADR-003** (new): additive subsystem in the modular monolith, no new infrastructure,
  roles as policy, A2A as transport only, `RoleAgent` adapter trait, no simulation fallback, server-side identity.
- `A2A_PRD.md` **§9** (new): the PRD changes Phase 6 needs (it moves "extra agents", "tool execution" and
  "automated repository edits" from out-of-scope into a gated extension; adds human authority, identity,
  delivery boundary, execution isolation, evidence and budget requirements).

## 3. Backend module design

| Unit | Owns | I/O |
|---|---|---|
| `crates/engineering` (new) | `state` (12 states, events, actor rules, budgets, recovery rule), `roles` (6 roles, closed tool set, check allowlist by id, path scope, branch namespace, separation of duties), `artifacts` (8 typed contracts, canonical-JSON SHA-256), `gates` (mandatory delivery gates) | none |
| `apps/api/src/eng/store.rs` | Locked, version-guarded transitions + audit in one transaction; runs with duplicate-execution guard; append-only artifacts; restart recovery | Postgres |
| `eng/orchestrator.rs` | Runs stages in role order until a human gate; deadline per stage; bounded retry (only unavailable/timeout); validates every output before handoff; builds the delivery manifest | adapters, Postgres |
| `eng/approval.rs` | Human decisions: authorization, version, artifact-hash + commit binding, gates, idempotency | Postgres |
| `eng/auth.rs` | `Authenticator` trait; production impl `Disabled` | — |
| `eng/agents.rs` / `eng/sim.rs` | Provider adapter trait; deterministic simulated agents (provider `simulation`, test-only wiring) | — |
| `eng/routes.rs` | `/api/eng/{status,workflows,workflows/{id},workflows/{id}/decisions}` (also under `/api/v1/eng`) | — |
| `migrations/20261117000003_engineering_workflows.sql` | 5 tables, CHECKs, append-only triggers, least-privilege grants, RLS, Supabase role revokes | — |

## 4. Workflow state machine

```mermaid
stateDiagram-v2
  [*] --> draft: human submits (authenticated requester)
  draft --> awaiting_plan_approval: PM plan (agent: project_manager)
  awaiting_plan_approval --> approved_for_execution: HUMAN approver approves plan hash
  awaiting_plan_approval --> draft: HUMAN requests changes (bounded)
  approved_for_execution --> executing: work assignment (agent: coordinator)
  executing --> reviewing: candidate commit (agent: developer)
  reviewing --> testing: review of that commit (agent: code_reviewer ≠ developer)
  testing --> awaiting_delivery_approval: QA report then assessment (agents: qa_tester, quality_lead) + manifest
  awaiting_delivery_approval --> approved_for_delivery: HUMAN approves manifest hash + commit, all gates pass
  awaiting_delivery_approval --> changes_requested: HUMAN requests changes (bounded)
  awaiting_delivery_approval --> rejected: HUMAN rejects
  changes_requested --> executing: new assignment (agent: coordinator)
  draft --> failed
  executing --> failed: system only, from agent-active states
  awaiting_plan_approval --> cancelled: any human (requester: own)
```

Guards (`engineering::state::next`): one actor kind per event; human gates need `HumanRole::Approver`
(no agent role, not the system); terminal states are final; `fail` is impossible at a human gate (nothing runs);
each accepted transition increments `version`. Agent-run status is stored separately (`eng_agent_runs`).
**Recovery:** agent-active states → `failed/interrupted`, running runs → `interrupted` (no replay, as P0);
`awaiting_*` preserved exactly (version, hashes). Budgets: plan revisions, delivery revisions, agent runs
(defaults 2/2/24, snapshotted per workflow, CHECK-enforced in the DB too).

## 5. Agent and artifact contracts

All contracts: `deny_unknown_fields` (no place for hidden reasoning), bounded text, schema version 1,
hash = `sha256(kind:version:canonical_json)`. Agent output is validated before the next agent sees it.

| Kind | Producer | Key rules |
|---|---|---|
| `requirements_plan` | PM | ≥1 deliverable, unique `AC-n` criteria, steps, repo-relative `scope` |
| `work_assignment` | Coordinator | scope ⊆ approved plan scope; criteria ⊆ plan; checks from allowlist; full base SHA |
| `code_candidate` | Developer | full SHAs, commit ≠ base, base = assigned base, branch = `agent/<workflow>/candidate`, every changed file in scope, patch SHA-256 |
| `review_report` | Reviewer | commit = candidate; conclusion consistent with findings (high/critical ⇔ `blocking_findings`); reviewer ≠ developer |
| `qa_report` | QA | commit = candidate; commands by check id with exit codes; per-criterion status + evidence; QA ≠ developer |
| `quality_assessment` | Quality Lead | commit = candidate; `ready / changes_required / blocked` (advice only) |
| `delivery_manifest` | orchestrator | ids + hashes of plan, assignment, candidate, review, QA, assessment for one commit |
| `approval_decision` | human only | written by the approval engine; agents cannot submit it (contract refuses) |

Every stored artifact has id, workflow id, producer kind/id, provider/model (agents), created_at, schema
version, content, content hash, commit SHA where applicable. Each run records role, agent id, provider, model,
attempt, correlation id, A2A task id (when A2A), token usage (when reported).

## 6. Human approval data model and security/permission model

`eng_approvals`: id, workflow id, gate, decision, authenticated approver id, approver role, identity issuer,
comment, reviewed commit SHA, artifact hash (plan hash or manifest hash), workflow version, idempotency key,
request hash, timestamp. `UNIQUE(workflow_id, workflow_version)` (one decision per version) and
`UNIQUE(workflow_id, idempotency_key)`; CHECKs tie gate ↔ decision ↔ required bindings ↔ approver role.

| Control | Enforcement |
|---|---|
| Identity from server only | `Authenticator` trait; body is `deny_unknown_fields` with no identity field (spoof → 422) |
| No IdP → no approvals | `Disabled` authenticator in `api serve` → 503 `auth_unavailable`; `/api/eng/status` reports `approvals_enabled:false` |
| AI ≠ approval | State machine actor rules; reviewer/QL produce "evidence"/"recommendation" types, never "approved" |
| Stale / conflicting / forged | Row lock + `expected_version`; per-version unique; hash + commit binding; idempotency with request hash |
| Reapproval after change | New candidate ⇒ new manifest hash and version ⇒ old decision refused |
| Failed stages never approved | Mandatory gates (candidate, review for commit, no high/critical, QA for commit, all checks exit 0, all criteria verified, assessment for commit, not `blocked`) |
| Tool requests untrusted | Closed `Tool` enum (no push/merge/approve/deploy/secret/shell); checks by id → fixed argv; path scope with traversal / `.git` refusal; branch namespace |
| Audit integrity | Append-only trigger on artifacts, approvals, audit (binds the table owner, blocks TRUNCATE); runtime role has SELECT/INSERT only |
| Duplicate execution | Partial unique index: one live/completed run per `(workflow, role:version)` |

**Not yet enforced (needs M5):** tool requests are *reported* by the adapter and checked after the fact; with
simulation nothing executes. A real Developer/QA adapter must run inside a sandbox that enforces the same
policy *before* execution (filesystem scope, no network except an allowlisted registry mirror, CPU/memory/time
limits, output caps, secret redaction, no credentials). Isolation model to assess in M5: containers alone are
not adequate for untrusted code from an LLM; candidates are a microVM (Firecracker/gVisor) or a disposable VM per run.

## 7. Provider integration matrix

Docs checked on 2026-11-17 (first-party pages unless noted). "Verified" means verified **from the docs only**;
the "Tested here" column is the only claim of execution.

| Provider | What it is | Auth | Tools / execution | Structured output | Approval hooks | Sessions / cancel | Usage | Tested here |
|---|---|---|---|---|---|---|---|---|
| **Anthropic Messages API** (`eng::providers::anthropic`) | LLM API, **not** a coding runtime | `x-api-key` header (also OAuth / WIF in SDKs) | none offered here (no tools, nothing executes) | `output_config.format` JSON schema; no `min*/max*`/`pattern`, `additionalProperties:false` required | n/a (no tools) | stateless; cancel = drop request (deadline) | `usage.input_tokens/output_tokens` | **Mode B** (HTTP test double); Mode C BLOCKED (no key) |
| Claude Agent SDK (`claude-agent-sdk`, Python/TS) | coding-agent harness (Claude Code as a library); built-in Read/Write/Edit/Bash/... | Anthropic credentials | built-in tools on the host it runs on, `allowedTools` | via prompt / final message | `canUseTool` callback, `PreToolUse`/`PostToolUse` hooks (code.claude.com/docs/en/hooks) | sessions, interrupt | per-result usage | NOT STARTED: no Rust SDK; needs a Node/Python sidecar inside the M5 sandbox |
| OpenAI Codex (`codex exec`) | coding-agent CLI | `CODEX_API_KEY` env (learn.chatgpt.com/docs/non-interactive-mode) | read-only sandbox by default; `--sandbox workspace-write` | `--json` event stream | approval policy flags (`-a never` for headless) | session resume | in JSON events (unverified) | NOT STARTED |
| Gemini CLI (`gemini -p`) | coding-agent CLI | `GEMINI_API_KEY` | built-in tools; `-s` sandbox; `--approval-mode` | `--output-format json` / `stream-json` | `--approval-mode` (e.g. `auto_edit`; `--yolo` = auto-approve all, unacceptable here) | `-r` resume | `stats.models.*.tokens` in JSON (google-gemini.github.io/gemini-cli/docs/cli/headless.html) | NOT STARTED |
| Grok (xAI API) | LLM API (OpenAI-compatible Responses/Chat) with server-side tools incl. code execution; **not** a local coding runtime | `Authorization: Bearer $XAI_API_KEY` | function calling (client-side), server web/X search, server code execution (docs.x.ai) | `json_schema` strict response format | n/a for client tools (we decide) | `previous_response_id` | usage in response | NOT STARTED; would fit the same non-executing roles as the Anthropic adapter |
| Simulation (`eng::sim`) | deterministic test double | none | none | n/a | n/a | n/a | none | Mode A only, never wired by `serve` |

Licensing/operational restrictions for each provider were **not** reviewed (terms of service, data retention,
rate limits) and must be before any is enabled.

**M4 decision.** First real provider = Anthropic Messages API, through the existing tested `reqwest` client
(`agent_runtime::LlmClient`, now also returning raw JSON + token usage via `generate_json`). It serves only
PM, Coordinator, Code Reviewer and Quality Lead; `AnthropicRoleAgent::new(Developer|QaTester)` is refused.
Facts are never taken from the model: the Coordinator's `base_commit_sha` comes from `ENG_BASE_COMMIT_SHA`
(until the M5 workspace supplies it) and the Reviewer/QL `commit_sha` from the orchestrator. The Reviewer refuses
without the real candidate diff (needs M5), so it cannot "review" metadata. Configuration: `ENG_PROVIDER=anthropic`
(+ `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL` default `claude-opus-5-5`, `LLM_*`); default `none`.
With it set, a workflow runs PM → (human) → Coordinator and then fails honestly at the Developer
(`provider_unconfigured`); and in `api serve` no one can submit yet, because the authenticator is `Disabled`.

## 8. Test results (Mode A, real Postgres, real HTTP for the API)

Commands: `cargo test -p engineering` (25 unit tests) and `cargo test -p api --test phase6` (21 integration tests).

| # | Test (§12 / §13) | Evidence | Result |
|---|---|---|---|
| E2E-1 | Approved flow over HTTP | `approved_flow_over_http`: runs in order PM → Coordinator → Developer → Reviewer → QA → QL, all `provider=simulation`; pause at plan and delivery; all gates pass; approval row has `alice` / `approver` / `test-idp`, commit SHA, manifest hash; audit has human actor | PASS |
| E2E-2 | Changes-requested flow | `changes_requested_flow_requires_reapproval`: new candidate SHA + new manifest; old version → `Stale`; old SHA or old manifest at new version → `Mismatch`; re-approval succeeds; 2 candidates, 2 manifests | PASS |
| E2E-3 | Rejection | `delivery_can_be_rejected` | PASS |
| N1 | Unauthorized user approves | no token 401; forged token 401; requester 403; `approver_id` in body 422; no IdP 503 `auth_unavailable`; no approval row written | PASS |
| N2 | AI agent approves delivery | every agent role and the system → `Unauthorized`; QL requesting `approve_delivery` tool → workflow `failed/tool_not_permitted` | PASS |
| N3 | Reviewer reviews own patch | same agent id as developer → `failed/policy_violation`, no review artifact | PASS |
| N4 | Candidate changes after approval | artifact insert into approved workflow refused (`Terminal`); UPDATE/DELETE/TRUNCATE of artifacts/approvals/audit refused **as table owner**; `a2a_app` lacks UPDATE/DELETE | PASS |
| N5 | Provider unavailable | 2 failed attempts `agent_unavailable` → workflow failed, no candidate artifact (no mock success); 1 transient failure → recovered by retry | PASS |
| N6 | Malformed output | PM output with unknown fields (`reasoning`) → `artifact_invalid`, no artifact stored | PASS |
| N7 | Unauthorized tool | developer `git_push` → `tool_not_permitted` | PASS |
| N8 | Execution budget | `max_agent_runs=3` → `budget_exceeded` at the 4th stage, `agent_runs=3` | PASS |
| N9 | Restart during human approval | `recover_interrupted` → 0 changes, version and manifest hash identical, decision accepted afterwards; crash mid-developer → `failed/interrupted`, run `interrupted` | PASS (simulated crash: task abort + recovery call, not a killed process) |
| N10 | Conflicting approvers | concurrent approve (alice) / reject (carol) → exactly one recorded, the other `Stale`/`Transition` | PASS |
| N11 | QA for another commit | `candidate_mismatch`, no QA artifact | PASS |
| N12 | Protected branch | developer reports `main` → `policy_violation` | PASS |
| N13 | Retry duplicates execution | 2–3 concurrent `advance` loops → each role's agent called exactly once, 6 runs | PASS |
| N14 | Forged / stale request | wrong hash → `Mismatch`; old version → `Stale`; wrong gate → `Transition`; same key+body → replay (same id); same key, other body → `IdempotencyConflict` | PASS |
| + | Stage timeout | hanging reviewer, 300 ms deadline → 2 attempts → `agent_timeout` | PASS |
| + | Failed evidence never approvable | failing QA and high finding → approve `GatesFailed`, reject works | PASS |
| + | Plan-changes loop, cancel by requester | | PASS |
| + | Every failure code accepted by the DB CHECK | | PASS |

Mutation check: disabling the gate check in `approval.rs` turns `failed_evidence_cannot_be_approved` red; restored.

Regression (full workspace, `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace`): see §8a.

### 8a. Full regression

| Command | Outcome |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS (after one `type_complexity` fix in `tests/phase6.rs`; it was a lint-only change, so the tests were not re-run) |
| `cargo test --workspace --locked --no-fail-fast -- --test-threads=4` | **132 passed, 0 failed**: the 86 existing tests are unchanged and all pass (P0 `workflow.rs` 25, `phase3.rs` 17, agents, runtime, contracts), plus 25 `engineering` unit tests and 21 `phase6` integration tests |

The P0 Worker → Reviewer workflow still passes its whole suite, so it is preserved.

### 8b. Mode B: provider contract tests (M4)

`cargo test -p api --test phase6_provider`: **9 passed**, plus 2 unit tests in `eng::providers::anthropic`.
The test double is a local axum server implementing `POST /v1/messages`. It asserts the key and version headers
and records every request body. The orchestrator and Postgres are real.

| Requirement | Test | Evidence | Result |
|---|---|---|---|
| Real adapter produces a validated artifact | `real_provider_plans_and_records_model_and_usage` | plan stored; run and artifact record provider `anthropic-messages`, model `claude-opus-5-5`, 1234/567 tokens; request has closed JSON schema, role system prompt, delimited untrusted data, no tools, no key in body | PASS |
| Authentication failure | `authentication_failure_is_not_retried_or_faked` | 401: one request, one failed run `agent_failed`, key redacted, no artifact, provider still `anthropic-messages` | PASS |
| Provider outage, bounded retry | `provider_outage_is_retried_within_bounds_then_fails` | 529: exactly 6 requests (3 client attempts × 2 stage attempts), then `agent_unavailable` | PASS |
| Provider timeout | `provider_timeout_fails_the_stage` | 1.5 s response vs 0.5 s timeout: 2 runs, `llm_timeout`, workflow failed | PASS |
| Invalid responses | `invalid_responses_are_rejected` | non-JSON / `refusal` / `max_tokens` → `agent_failed`; extra `approved` field or `/etc` scope → `artifact_invalid`; no artifact | PASS |
| Unsupported capability | `execution_roles_are_unsupported` | adapter construction for Developer and QA is refused | PASS |
| Permission enforcement on real output | `coordinator_cannot_invent_base_or_widen_scope` | model's base SHA overwritten by configured base; scope beyond plan → `policy_violation`; no base configured → refused before any call | PASS |
| No metadata-only review | `reviewer_without_a_real_diff_refuses` | no diff → `agent_failed`, provider never called | PASS |
| Commit binding / usage | `quality_lead_assessment_is_bound_to_the_candidate` | model's commit SHA overwritten by the candidate's; QL received QA and review reports | PASS |
| Schema features + prompt injection (unit) | `schemas_use_only_supported_features`, `delimiter_cannot_be_closed_by_content` | no unsupported constraints; `</workflow_data>` in user content is escaped | PASS |
| No silent mock fallback | all above | every failure ends `failed` with the real provider recorded; `serve` never wires simulation | PASS |

**Mode C (real provider calls): BLOCKED.** No `ANTHROPIC_API_KEY` and no `ant` profile exist here. A Mode C run
also spends money, so it needs the owner's explicit go-ahead. Even then it covers only PM → plan gate →
Coordinator, because the Developer has no provider until M5.

## 9. Quality gate (§15)

| Criterion | Result | Basis |
|---|---|---|
| Multiple specialized agents in one workflow | PASS (simulation only) | E2E-1; six roles, distinct identities |
| Tasks and artifacts correctly correlated | PASS (simulation) | run ↔ artifact ↔ commit ↔ manifest; correlation id per run |
| A2A communication over real transport | **UNVERIFIED** | Role agents not yet A2A; P0 A2A unchanged and verified in Phase 5 |
| Handoffs use validated contracts | PASS | N6, N11, contract unit tests |
| Coding execution is isolated | **BLOCKED / NOT STARTED** | M5; no execution happens yet |
| Code review and QA evidence reproducible | **UNVERIFIED** | Evidence is simulated, not produced by real checks |
| Human approvals authenticated and authorized | **BLOCKED** | Logic PASS (N1, N10, N14) with a test authenticator; no IdP configured |
| Agents cannot approve their own delivery | PASS | N2, N3 |
| Humans can inspect the workflow | PARTIAL | `GET /api/eng/workflows/{id}` returns runs, artifacts, gates, approvals, audit; portal (M8) not built |
| Restart recovery preserves workflow + approval state | PASS (in-process) | N9; a killed-process test like Phase 3's is still to add |
| Failed stages cannot become approved deliveries | PASS | gates + N4, failed-evidence test |
| Security and integration regression tests pass | PASS (Modes A + B and the existing suites) | §8a, §8b; Mode C BLOCKED |

**Overall Phase 6: NOT COMPLETE.** M1–M3 PASS. M4 PARTIAL: Mode B PASS, Mode C BLOCKED (no credentials), and no
coding-agent runtime yet. M7 logic PASS, production BLOCKED (no IdP). M5, M6 (real evidence), M8, M9 and M10
NOT STARTED.

## 10. Remaining gaps, cost considerations, production limitations

1. **M4 – remaining.** (a) Mode C: with an owner-approved key and budget, run PM → plan approval → Coordinator
   against the real API, recording the request ids, token usage and cost. (b) A coding-agent runtime for
   Developer/QA (Claude Agent SDK, Codex or Gemini CLI). It needs a sidecar process inside the M5 sandbox; never
   on the host.
2. **M5 – isolation.** Real Git worktree per workflow on a disposable sandbox; run allowlisted checks there;
   compute candidate SHA/patch hash from the actual repo, not from the agent's claim; enforce tool policy before
   execution; limits and secret redaction. Assess microVM vs container.
3. **M7 – IdP.** OIDC JWT verification (issuer, audience, expiry, JWKS rotation) as an `Authenticator`; map
   groups → `requester`/`approver`. Until then approvals stay BLOCKED.
4. **M8 – portal.** Dashboard, detail timeline, review/QA evidence, approval review with confirmation, audit
   history, using `/api/eng` data only. The portal must forward the user's session credential; it must never
   mint identity.
5. **A2A for roles.** Expose remote role agents through the pinned SDK with Agent Card validation (reuse
   `api::a2a::Gateway`), cancellation on deadline, artifact extraction into these contracts.
6. **Cost.** Budgets bound runs, not money: add per-workflow token/cost caps once a provider reports usage;
   a full pass is ≥ 6 model runs (more with retries/revisions; defaults allow up to 24).
7. Known limits: stage timeout cancels by dropping the future (A2A adapters must also send `CancelTask`);
   a crash mid-stage still discards the in-flight agent's work (as in P0); one orchestrator per database (the
   existing instance lock covers both subsystems).

Nothing was committed, pushed, or deployed. GitOps/production deployment was not started.
