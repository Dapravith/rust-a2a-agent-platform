#!/usr/bin/env bash
# Starts an isolated backend for the portal's browser tests (frontend/tests/e2e):
# a fresh scratch database, both A2A agents in mock inference mode and the API,
# on ports that do not clash with ./scripts/dev.sh. Ctrl+C / SIGTERM stops all.
#
# Needs the local Postgres (infrastructure/local/compose.yaml) running in Docker.
# Override with E2E_PG_CONTAINER, E2E_DB, E2E_API_PORT if required.
set -euo pipefail
cd "$(dirname "$0")/.."

container="${E2E_PG_CONTAINER:-a2a-local-postgres-1}"
db="${E2E_DB:-a2a_e2e}"
api_port="${E2E_API_PORT:-18080}"
worker_port=$((api_port + 1021))   # 19101
reviewer_port=$((api_port + 1022)) # 19102

psql() { docker exec -i "$container" psql -v ON_ERROR_STOP=1 -qAt -U postgres "$@"; }
psql -d postgres -c "DROP DATABASE IF EXISTS $db WITH (FORCE)" -c "CREATE DATABASE $db"

cargo build --quiet --manifest-path backend/Cargo.toml --bins
bin=backend/target/debug

export DATABASE_URL="postgres://postgres:postgres@127.0.0.1:54329/$db"
export INFERENCE_MODE=mock
export RUST_LOG="${RUST_LOG:-warn}"
unset FRONTEND_DIR DATABASE_MIGRATION_URL ANTHROPIC_API_KEY
"$bin/api" migrate

trap 'kill 0' EXIT INT TERM
WORKER_BIND="127.0.0.1:$worker_port" WORKER_PUBLIC_URL="http://127.0.0.1:$worker_port" "$bin/worker-agent" &
REVIEWER_BIND="127.0.0.1:$reviewer_port" REVIEWER_PUBLIC_URL="http://127.0.0.1:$reviewer_port" "$bin/reviewer-agent" &
sleep 1
ORCHESTRATOR_BIND="127.0.0.1:$api_port" \
  WORKER_URL="http://127.0.0.1:$worker_port" REVIEWER_URL="http://127.0.0.1:$reviewer_port" \
  "$bin/api" &
wait
