# Local containers

Images for the four pilot workloads. `role-agents` and `mcp-server` are not containerized for the pilot (see `docs/PHASE7.md` §2).

| Image | Dockerfile target | Port | Probes | Runtime base |
|---|---|---|---|---|
| `a2a-api` | `backend/Dockerfile` `api` | 8080 | `/healthz` (liveness), `/api/health` (readiness, DB) | `gcr.io/distroless/cc-debian13:nonroot` |
| `a2a-worker-agent` | `backend/Dockerfile` `worker-agent` | 9101 | `/healthz` | same |
| `a2a-reviewer-agent` | `backend/Dockerfile` `reviewer-agent` | 9102 | `/healthz` | same |
| `a2a-portal` | `frontend/Dockerfile` `portal` | 4321 | `/healthz` | `gcr.io/distroless/nodejs24-debian13:nonroot` |

All bases are pinned by digest. Images run as uid/gid 65532, contain no shell or package manager, and need no writable path except `/tmp`. Logs are JSON (`LOG_FORMAT=json`). Every process drains on SIGTERM (Rust: `agent_runtime::shutdown_signal`; portal: `frontend/server.mjs`).

## Build

```sh
# native arch (fast, local use)
docker buildx build --load --target api            -t a2a-api:dev            backend/
docker buildx build --load --target worker-agent   -t a2a-worker-agent:dev   backend/
docker buildx build --load --target reviewer-agent -t a2a-reviewer-agent:dev backend/
docker buildx build --load --target portal         -t a2a-portal:dev         frontend/

# deployment arch: add --platform linux/amd64 (on Apple Silicon this runs
# under emulation and is slow for the Rust images; CI builds amd64 natively)
```

`PORTAL_HOSTS` is a **build-time** argument (Astro's CSRF origin check). Pass every environment's hostname, comma-separated, so one image digest can be promoted unchanged: `--build-arg PORTAL_HOSTS=portal.dev.example,portal.example`.

## Configuration

| Variable | Image | Notes |
|---|---|---|
| `DATABASE_URL` | api | Remote hosts require `sslmode=verify-full` and `sslrootcert=`. Supabase: direct connection or session pooler only (the API holds a session advisory lock) |
| `DATABASE_MIGRATION_URL` | api `migrate` | Schema-owner role; `serve` only verifies the schema |
| `WORKER_URL`, `REVIEWER_URL` | api | Agent base URLs |
| `WORKER_PUBLIC_URL`, `REVIEWER_PUBLIC_URL` | agents | URL advertised in the Agent Card; must be what the API uses |
| `INFERENCE_MODE`, `ANTHROPIC_*` | agents | `mock` needs no key |
| `API_ORIGIN`, `API_TIMEOUT_MS` | portal | Server-side only |

Run exactly one API container per database: a second one exits (instance lock).

## Smoke test

```sh
scripts/container-smoke.sh dev
```

Starts a throwaway Postgres and the four images with `--read-only --cap-drop ALL --security-opt no-new-privileges --user 65532:65532`, runs migrations, then checks probes, Agent Card discovery, one mock Worker → Reviewer workflow rendered by the portal, security headers, and that each container exits on SIGTERM within 10 s. Ports bind to `127.0.0.1` only (28080, 24321).
