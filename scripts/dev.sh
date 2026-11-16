#!/usr/bin/env bash
# Runs the worker agent, reviewer agent and API locally (Ctrl+C stops all).
# Requires: .env (copy from .env.example), a reachable Postgres
# (infrastructure/local/compose.yaml), and `npm run build` in frontend/.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f .env ] || { echo "Create .env from .env.example first" >&2; exit 1; }
set -a; . ./.env; set +a
cargo build --manifest-path backend/Cargo.toml --bins
bin=backend/target/debug
trap 'kill 0' EXIT INT TERM
"$bin/worker-agent" &
"$bin/reviewer-agent" &
sleep 1
"$bin/api" &
wait
