# Phase 2: A2A communication and orchestration (verification record)

Date: 2026-11-17 · Branch: `phase2/a2a-orchestration` · SDK: `a2a-lf 0.4.2`, `a2a-server-lf 0.5.3`,
`a2a-client-lf 0.2.9` (official [`a2aproject/a2a-rs`](https://github.com/a2aproject/a2a-rs)), A2A protocol 1.0, JSON-RPC binding.

## Starting point

Phase 1 already delivered the Worker and Reviewer as independent A2A servers, the SDK-based client
gateway, and the Postgres-backed orchestrator (see [`ARCHITECTURE.md`](ARCHITECTURE.md)). Phase 1
gates were re-run before any change: `fmt`, `clippy -D warnings`, `build` clean; 46 tests passed.

## Deliberate deviations from the Phase 2 brief

The Phase 2 brief conflicted with the PRD (source of truth) on three points. The PRD was kept
(decision confirmed by the project owner on 2026-11-17):

| Brief | Kept instead | Reason |
|---|---|---|
| In-memory workflow state, lost on restart | Postgres persistence + restart recovery | PRD §7: "results survive an API restart"; already implemented and tested |
| `findings: string[]` | `findings: [{severity, title, detail}]` | Richer superset; consumed by the dashboard. `verdict`/`summary` rules are identical |
| `/api/v1/workflows`, `/healthz` only | `/api/...` **and** `/api/v1/...` (same handlers), plus `/healthz` | Versioned path added without breaking the dashboard |

## Changes in Phase 2

| Change | Files |
|---|---|
| `GET /healthz` liveness (no DB); `/api/v1` prefix mounted on the existing handlers | `backend/apps/api/src/routes.rs` |
| Typed `InvalidCard` error: a reachable agent with a missing or malformed card is no longer reported as "unreachable" | `backend/apps/api/src/a2a.rs` |
| **Defect fix:** agents accepted 10 MiB bodies, not the intended 256 KiB (the SDK's inner `DefaultBodyLimit` overrode ours). Now enforced with `RequestBodyLimitLayer` | `backend/crates/agent-runtime/src/server.rs`, `Cargo.toml` |
| Tests: Reviewer unavailable / malformed review / `auth-required` / timeout (each asserts the Worker draft is preserved), malformed and missing Agent Card, `/api/v1` + `/healthz`, Reviewer oversized input and body, draft prompt-injection treated as data | `backend/apps/api/tests/workflow.rs`, `backend/apps/reviewer-agent/tests/a2a.rs` |

## Automated verification

```bash
cd backend
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:54329/a2a cargo test --workspace --locked -- --test-threads=4
```

Result: all clean; **55 passed, 0 failed** (25 orchestrator end-to-end tests over real HTTP with
real SDK servers/clients and a real Postgres database per test).

## Live verification (three separate processes)

Each binary was started on its own (`worker-agent`, `reviewer-agent`, `api`) with `INFERENCE_MODE=mock`.

| Check | Observed |
|---|---|
| Worker / Reviewer Agent Cards | `Draft Worker` / `Draft Reviewer`, one `JSONRPC` interface each, protocol `1.0` |
| Raw JSON-RPC `SendMessage` to Worker | `TASK_STATE_COMPLETED`, artifact `draft` |
| Raw JSON-RPC `SendMessage` to Reviewer | `TASK_STATE_COMPLETED`, artifact `review`, `verdict: needs_changes`, 2 findings |
| Text part instead of JSON contract | `TASK_STATE_REJECTED` ("no JSON data part") |
| 300 KB body to an agent | HTTP 413 |
| `POST /api/v1/workflows` | 202, then `completed`, `needs_changes`, both runs `TASK_STATE_COMPLETED` |
| Second submit while one is active | HTTP 409 |
| `question: "short"` | HTTP 422 `validation_failed` |
| Reviewer process killed, then submit | `failed`, error `reviewer: agent unreachable … (<agent-url>)`, Worker draft preserved, no review |

<!-- TCK section appended below -->
