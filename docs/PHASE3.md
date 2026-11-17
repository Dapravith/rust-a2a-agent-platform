# Phase 3: Supabase persistence and LLM integration (verification record)

Date: 2026-11-17 · Branch: `phase3/llm-integration` · Rust 1.100.0 · SQLx 0.9.0 · Postgres 17 (local) ·
Provider: Anthropic Messages API (`anthropic-version: 2026-10-21`, default model `claude-opus-5-5`).

## Phase 2 gate (re-run before any change)

`fmt`, `clippy -D warnings`, `build`: clean. `cargo test --workspace`: **57 passed, 0 failed**.
25 of these are orchestrator end-to-end tests that run real SDK servers and clients over HTTP
against a real Postgres database. Agent tests (`apps/*/tests/a2a.rs`, `cancel.rs`) also run over
real HTTP. Only the `contracts`, `config` and `a2a::trusted_card` unit tests are in-process.
Verdict: **PASS**.

## Design

```mermaid
flowchart LR
  O[api serve<br/>role: a2a_app member] -->|SQLx, verify-full TLS| P[(Postgres / Supabase)]
  M[api migrate<br/>role: schema owner] -->|DDL| P
  O -->|A2A JSON-RPC| W[worker-agent]
  O -->|A2A JSON-RPC| R[reviewer-agent]
  W & R -->|reqwest, typed| L[(Anthropic API)]
  W & R -.->|INFERENCE_MODE=mock| K[deterministic mock]
```

| Layer | Module | Responsibility (Phase 3 changes) |
|---|---|---|
| HTTP | `apps/api/src/routes.rs` | Unchanged routes. Responses gain `error_code`, `failed_stage` (workflow) and `error_code` (run) |
| Application | `apps/api/src/engine.rs` | Each failure carries an `ErrorCode` and the stage it happened in. A refused terminal transition is logged, never overwritten |
| A2A transport | `apps/api/src/a2a.rs` | `AgentError::code()`. Reads the agent's failure report from the status message, accepting only codes from the closed set |
| Persistence | `apps/api/src/db.rs` | Guarded transitions, `verify_schema`, `InstanceLock`, `connect` |
| Vocabulary | `apps/api/src/error_code.rs`, `crates/contracts` (`AgentFailure`) | Stable codes, mirrored by DB CHECK constraints |
| Startup | `apps/api/src/main.rs`, `config.rs` | `api migrate` / `api serve`. Verified-TLS check on the DB URL |
| Inference | `crates/agent-runtime/src/llm.rs` | Typed Messages API contracts, bounded body, certain-only retries, codes, redaction |

Workflow and data flow (unchanged order): `POST` inserts `queued` (partial unique index = 1 active)
→ `running` → Worker run inserted → A2A call (no DB transaction open) → validated draft and run
completion committed in one transaction → Reviewer run → validated review → `completed` exactly
once. On any failure: run `failed` (+ code), workflow `failed` (+ code, stage); the Worker draft
stays stored.

## Implementation backlog (all done)

| ID | Task | Files | Acceptance evidence |
|---|---|---|---|
| P3-01 | Error-code and stage columns plus consistency CHECKs; `a2a_app` role, grants, RLS policies, Supabase `anon`/`authenticated` revoke; verified-TLS URL check; `api migrate` | `migrations/20261117000001_error_codes.sql`, `migrations/20261117000002_runtime_role.sql`, `config.rs`, `main.rs` | `migrations_apply_and_schema_is_verified`, `schema_constraints_reject_invalid_rows`, `runtime_role_runs_the_workflow_but_cannot_change_schema`, `database_url_requires_verified_tls_off_host` |
| P3-02 | Guarded transitions; failure codes and stage; A2A task id kept | `db.rs` | `transitions_happen_at_most_once`, `recovery_records_code_and_stage` |
| P3-03 | Orchestrator persists codes/stage; schema verification and single-instance lock at startup | `engine.rs`, `main.rs`, `db.rs` | `workflows_survive_a_killed_process_and_interrupted_ones_fail`, `second_api_process_refuses_to_start`, `serve_requires_migrated_schema` |
| P3-04 | Typed Anthropic adapter; failure codes via A2A | `llm.rs`, `server.rs`, `contracts`, both agents | `llm_stub.rs` (11 tests), `hosted_mode_end_to_end_is_persisted` |
| P3-05 | Mock kept; explicit mode; no fallback | `llm.rs` | `provider_outage_never_falls_back_to_mock`, existing mock E2E tests |
| P3-06 | Timeouts, rate limits, refusals, malformed/oversized output, DB outage, redaction | as above | `malformed_reviewer_json_fails_with_stable_code`, `reviewer_refusal_fails_with_stable_code`, `database_outage_is_reported_safely`, `provider_error_is_typed_bounded_and_scrubbed` |
| P3-07 | Integration tests | `apps/api/tests/phase3.rs`, `crates/agent-runtime/tests/llm_stub.rs` | See evidence below |

## Technical decisions

| Decision | Reason |
|---|---|
| Keep the existing schema; add migrations instead of rewriting | Phase 1/2 already met the table, UUID, UTC, FK, uniqueness and index requirements. Applied migrations are immutable |
| `sslmode=verify-full` mandatory off-host | `require` encrypts but accepts any certificate, so an impostor server would receive the password |
| Separate `migrate` and `serve` roles | A runtime role without DDL/DELETE limits damage from a compromised or buggy API. Migrations are explicit, not a startup side effect |
| RLS policies for `a2a_app` only | Found by test: with RLS on and no policies, a non-owner role cannot insert (503). Policies are scoped to `a2a_app`; Data API roles still see nothing |
| Session advisory lock per database | Restart recovery fails *every* active workflow. That is only accurate when no second orchestrator is running |
| Retry only certain-unprocessed provider failures | The brief forbids retrying uncertain non-idempotent submissions. A read timeout or a 500 may already have been billed |
| Codes from a closed set, both directions | Agent-supplied data never chooses behaviour. An unknown code is ignored |
| Stored `error` text unchanged | Preserves existing REST and dashboard behaviour; codes are additive fields |

## Testing evidence

Commands (from `backend/`, local Postgres on `127.0.0.1:54329`):

```bash
cargo fmt --all -- --check                                  # clean
cargo clippy --workspace --all-targets --locked -- -D warnings  # clean
cargo build --workspace --locked                            # clean
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:54329/a2a \
  cargo test --workspace --locked -- --test-threads=4       # 85 passed, 0 failed
../scripts/check-secrets.sh                                 # secret check: OK
```

| Suite | Tests | Kind |
|---|---|---|
| `contracts` unit | 8 | in-process |
| `agent-runtime` unit + `llm_stub.rs` | 4 + 11 | HTTP test double of the provider |
| `cancel.rs`, agents `a2a.rs` | 1 + 10 | real A2A over HTTP |
| `api` unit | 6 | in-process |
| `api/tests/workflow.rs` (Phase 2, unchanged) | 25 | real A2A + Postgres |
| `api/tests/phase3.rs` (new) | 16 | real A2A + Postgres + provider double + real `api` process |

Process-restart, single-instance and instance-lock tests were run three times: all passed.

| # | Required test | Test | Result |
|---|---|---|---|
| 1 | Supabase connectivity | none (no Supabase credentials available) | **BLOCKED** |
| 1b | Postgres connectivity (local, same driver/TLS code path minus TLS) | all DB tests | PASS |
| 2 | Migration correctness | `migrations_apply_and_schema_is_verified`, `schema_constraints_reject_invalid_rows`, `serve_requires_migrated_schema` | PASS |
| 3 | Workflow insert/update | `transitions_happen_at_most_once` | PASS |
| 4–5 | Worker / Reviewer artifact persistence | `hosted_mode_end_to_end_is_persisted`, `full_workflow_completes_and_persists` | PASS |
| 6–7 | Completed / failed retrieval | `hosted_mode_end_to_end_is_persisted`, `malformed_reviewer_json_fails_with_stable_code`, `stage_and_code_recorded_for_a2a_failures` | PASS |
| 8 | Process restart recovery | `workflows_survive_a_killed_process_and_interrupted_ones_fail` (SIGKILL of the real binary, mid-call) | PASS |
| 9 | Mock inference | Phase 2 E2E suite, `mock_is_deterministic_and_valid` | PASS |
| 10 | Hosted LLM, live | none (no `ANTHROPIC_API_KEY`) | **BLOCKED** |
| 10b | Hosted path vs. HTTP test double | `hosted_mode_end_to_end_is_persisted` | PASS |
| 11 | Provider timeout / invalid output | `read_timeout_is_typed_and_not_retried`, `retries_stop_at_overall_deadline`, `malformed_and_contract_violating_output_is_rejected`, `oversized_response_is_rejected` | PASS |
| 12 | E2E Worker → Reviewer → DB | `hosted_mode_end_to_end_is_persisted`, live run below | PASS |

Live run (three separate processes, `INFERENCE_MODE=mock`, scratch database `a2a_p3live`, API
connected as a LOGIN member of `a2a_app`; database and role dropped afterwards):

| Check | Observed |
|---|---|
| `api migrate` as owner | 3 migrations applied, "database schema is up to date" |
| Agent Cards | `Draft Worker` / `Draft Reviewer`, `JSONRPC` |
| Second `api` on same DB | exit 1, "another orchestrator instance holds the lock on this database" |
| `POST /api/v1/workflows` + concurrent submit | `completed`, `needs_changes`, 2 findings, both runs `TASK_STATE_COMPLETED` with task ids; concurrent submit `409` |
| `kill -9` API, restart, `GET` | Same workflow, draft and review intact |
| Reviewer process killed, submit | `failed`, `error_code: agent_unreachable`, `failed_stage: reviewer`, draft kept, no review |
| `DELETE FROM workflows` / `DROP TABLE artifacts` as runtime role | `permission denied` / `must be owner` |
| DB password in any process log | 0 occurrences |

## Security review

| Area | Finding / control |
|---|---|
| Credentials | `.env` git-ignored; `check-secrets.sh` OK; `Config` and `LlmConfig` `Debug` redact the DB URL and API key (unit-tested); the `x-api-key` header is marked sensitive; the key is scrubbed from provider error text (tested); the URL check never echoes the URL (tested, including via the real binary) |
| Database access | Only the API connects. Runtime role: SELECT/INSERT/UPDATE on 3 tables, SELECT on migration history; DELETE/TRUNCATE/DDL/RLS changes denied (7 statements tested). Verified TLS required off-host. Supabase `anon`/`authenticated` privileges revoked; RLS on |
| SQL injection | All queries parameterized, `&'static str` only. The one formatted statement is test-only (generated hex role name) |
| Input validation | Unchanged API limits; LLM output decoded with unknown fields rejected, then contract-validated; responses ≤ 1 MiB; provider message ≤ 300 chars; agent failure reason ≤ 500 chars |
| Failure handling | Stable codes for 20 failure classes; no duplicate completion; interrupted workflows failed, not replayed; no silent mock fallback (tested) |
| Untrusted content | Agent-reported codes are labels from a closed enum; unknown codes ignored; prompts mark user/draft text as data (unchanged) |

Residual risks:

1. **No authentication between orchestrator and agents.** They rely on loopback binding (unchanged from Phase 2; planned for P1).
2. **Lost instance-lock session.** If the database connection holding the lock drops (for example a DB restart), the lock is released while the API keeps running. A second instance could then start. Operate one instance per database.
3. **List pagination cursor is `created_at` only.** Two rows with an identical microsecond could straddle a page boundary. Pre-existing; practically unreachable with one active workflow.

## Supabase setup

1. In the Supabase dashboard, open **Database → SSL** and download the CA certificate.
2. Migrate with the owner. Use the session pooler (port 5432) or the direct connection. This
   also creates the NOLOGIN group role `a2a_app`:

   ```bash
   DATABASE_MIGRATION_URL='postgresql://postgres.<ref>:<pw>@aws-0-<region>.pooler.supabase.com:5432/postgres?sslmode=verify-full&sslrootcert=/path/supabase-ca.crt' \
     backend/target/debug/api migrate
   ```

3. As `postgres` (SQL editor), create the login role the API uses:

   ```sql
   CREATE ROLE a2a_api LOGIN PASSWORD '<generate a strong password>' IN ROLE a2a_app;
   ```

4. Run the API with the runtime role:
   `DATABASE_URL=postgresql://a2a_api.<ref>:<pw>@…:5432/postgres?sslmode=verify-full&sslrootcert=…`.
5. Do not use the transaction pooler (6543). It breaks prepared statements and the session lock.

## Remaining blockers

| Item | Status | What unblocks it |
|---|---|---|
| Live Supabase connection (TLS verify-full against Supabase CA, pooler, role setup) | BLOCKED | A Supabase project URL plus CA certificate in a git-ignored `.env`, then run the setup above and the live script |
| Live hosted LLM (Worker draft + Reviewer assessment from `claude-opus-5-5`) | BLOCKED | `INFERENCE_MODE=anthropic` + `ANTHROPIC_API_KEY`, then run one workflow |

Both paths are implemented and exercised against local Postgres and an HTTP test double of the
Messages API. Neither is claimed as live-verified.
