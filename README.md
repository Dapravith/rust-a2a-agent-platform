# Rust + Astro A2A Agent Platform (P0)

Two independent AI agents collaborate over the **Agent2Agent (A2A) protocol**: a
**Worker** drafts an answer to a software-engineering question and a **Reviewer**
independently evaluates it. A Rust/Axum **API** owns the deterministic
Worker → Reviewer sequence and persists everything to Postgres (Supabase-compatible).
An Astro dashboard submits questions and shows history and results.

Source of truth: [`A2A_PRD.md`](A2A_PRD.md) · Decision record: [`docs/ADR.md`](docs/ADR.md) ·
Design: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) · P1 plan (not implemented): [`docs/P1_PLAN.md`](docs/P1_PLAN.md) ·
Phase 2 verification: [`docs/PHASE2.md`](docs/PHASE2.md) · Phase 3 (persistence, LLM): [`docs/PHASE3.md`](docs/PHASE3.md)

```mermaid
flowchart LR
  B[Browser · Astro static UI] -->|same-origin /api| O[api :8080]
  O -->|Agent Card + JSON-RPC| W[worker-agent :9101]
  O -->|Agent Card + JSON-RPC| R[reviewer-agent :9102]
  W & R -->|reqwest| L[(Anthropic API or deterministic mock)]
  O -->|SQLx| P[(Postgres / Supabase)]
```

## Repository layout

```text
.
├── frontend/                  Astro 7 + TypeScript dashboard (static build)
├── backend/                   Rust Cargo workspace (Cargo.toml, Cargo.lock)
│   ├── apps/
│   │   ├── api/               Axum REST API, A2A client gateway, deterministic orchestration, SQLx
│   │   ├── worker-agent/      Worker A2A server (skill `draft-answer`)
│   │   └── reviewer-agent/    Reviewer A2A server (skill `review-draft`)
│   ├── crates/
│   │   ├── contracts/         Shared A2A payload DTOs + validation
│   │   └── agent-runtime/     Shared agent plumbing: SDK server wiring + the one LLM client
│   └── migrations/            SQLx migrations (embedded into `api`, applied by `api migrate`)
├── infrastructure/local/      compose.yaml for the local Postgres only (no deployment manifests in P0)
├── docs/                      ADR, architecture, P1 plan
├── scripts/                   dev.sh (run all services), check-secrets.sh (credential exposure check)
└── .github/workflows/ci.yml   fmt, clippy, tests with Postgres, Astro build, secret check
```

Dependency direction: `api → contracts`; each agent → `agent-runtime → contracts`.
The API never depends on agent code at runtime; agents never call each other.

## Local setup

**Prerequisites:** Rust ≥ 1.94 (tested on 1.100.0), Node ≥ 22.23.4 with npm (pinned to
Node 24.21.0 LTS in `.nvmrc`; run `nvm use` in the repo root), and Docker (for the local Postgres;
or point `DATABASE_URL` at any Postgres 15+ / Supabase).

1. Start the local database:

```bash
docker compose -f infrastructure/local/compose.yaml up -d
```

2. Create your local environment file (git-ignored; holds every secret):

```bash
cp .env.example .env
```

3. Build the dashboard:

```bash
(cd frontend && npm ci && npm run build)
```

4. Build and run the two agents and the API (Ctrl+C stops all three):

```bash
./scripts/dev.sh
```

5. Open <http://127.0.0.1:8080>. `dev.sh` runs `api migrate` first; `api` itself only verifies
   the schema and refuses to start if it is out of date.

For frontend live-reload, keep `./scripts/dev.sh` running, then `cd frontend && npm run dev`
(proxies `/api` to `:8080`) and open <http://localhost:4321>.

**Real LLM:** set `INFERENCE_MODE=anthropic` and `ANTHROPIC_API_KEY` in `.env` (read by the agents only).
Mock mode needs no key; a provider failure is reported, never replaced by mock output.
**Supabase:** use the direct or *session-pooler* (port 5432) URL with `sslmode=verify-full` and
`sslrootcert=<project CA>` (`require` is refused: it does not verify the server). Run migrations
with the owner (`DATABASE_MIGRATION_URL`) and the API with a least-privilege role; see
[`docs/PHASE3.md`](docs/PHASE3.md#supabase-setup). The transaction pooler (6543) is incompatible.

## Checks

Run Rust checks from `backend/` (tests need the database from step 1):

```bash
cd backend && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cd backend && DATABASE_URL=postgres://postgres:postgres@127.0.0.1:54329/a2a cargo test --workspace
```

```bash
(cd frontend && npm run build) && ./scripts/check-secrets.sh
```

Database tests use `#[sqlx::test]`, which creates and drops an isolated database per test.
If Docker is busy with other workloads and tests fail with `failed to connect test pool: PoolTimedOut`,
reduce concurrent test databases with `cargo test --workspace -- --test-threads=4`.

## REST API (`backend/apps/api`)

Every `/api/...` route is also served under the versioned prefix `/api/v1/...` (same handlers).

| Method | Path | Result |
|---|---|---|
| `GET` | `/healthz` | `200 {status}`: process liveness only (no database check) |
| `GET` | `/api/health` | Readiness: `200 {status, database}` or `503` |
| `GET` | `/api/agents` | Discovery status of both agents (from their Agent Cards) |
| `POST` | `/api/workflows` `{question}` (+ optional `Idempotency-Key`) | `202` new · `200` same key replayed · `409` another workflow active · `413` · `422` |
| `GET` | `/api/workflows?limit=&before=` | Newest first, cursor pagination |
| `GET` | `/api/workflows/{id}` | Workflow + agent runs + draft + review |

Errors: `{"error": {"code", "message"}}`. A failed workflow carries a stable `error_code`
(e.g. `agent_unreachable`, `llm_rate_limited`, `interrupted`) and `failed_stage`
(`worker` | `reviewer`); runs carry `error_code` too. Key on codes, not on `error` text.

## Calling an agent directly (any A2A client)

```bash
curl -s localhost:9101/.well-known/agent-card.json
```

```bash
curl -s localhost:9101/jsonrpc -H 'content-type: application/json' -H 'A2A-Version: 1.0' -d '{"jsonrpc":"2.0","id":"1","method":"SendMessage","params":{"message":{"messageId":"m-1","role":"ROLE_USER","parts":[{"data":{"question":"How should a Rust service validate pagination parameters?"},"mediaType":"application/json"}]}}}'
```

## Secrets

All secrets (`DATABASE_URL`, `ANTHROPIC_API_KEY`) live in the git-ignored `.env` and are read
server-side only. `.env.example` contains placeholders and local throwaway values.
`scripts/check-secrets.sh` (also run in CI) fails on committed secret patterns, tracked `.env`
files, or server configuration appearing in the frontend bundle.
