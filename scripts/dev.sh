#!/usr/bin/env bash
# Runs the worker agent, reviewer agent, API and the Astro portal locally
# (Ctrl+C stops all). Requires: .env (copy from .env.example), a reachable
# Postgres (infrastructure/local/compose.yaml), and `npm ci && npm run build`
# in frontend/. Open http://127.0.0.1:4321.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f .env ] || { echo "Create .env from .env.example first" >&2; exit 1; }
set -a; . ./.env; set +a
[ -f frontend/dist/server/entry.mjs ] || { echo "Build the portal first: (cd frontend && npm ci && npm run build)" >&2; exit 1; }
# Phase 4: the Astro server renders the portal; the API no longer serves a
# static build (an old FRONTEND_DIR would make it refuse to start).
unset FRONTEND_DIR
cargo build --manifest-path backend/Cargo.toml --bins
bin=backend/target/debug
# Schema first, as the owner (DATABASE_MIGRATION_URL, else DATABASE_URL).
# `api` itself only verifies the schema; it never runs DDL.
"$bin/api" migrate
trap 'kill 0' EXIT INT TERM
"$bin/worker-agent" &
"$bin/reviewer-agent" &
sleep 1
"$bin/api" &
# Only the portal needs PORTAL_* / API_ORIGIN; nothing else is passed to it.
env -i PATH="$PATH" HOME="$HOME" HOST="${PORTAL_HOST:-127.0.0.1}" PORT="${PORTAL_PORT:-4321}" \
  API_ORIGIN="${API_ORIGIN:-http://${ORCHESTRATOR_BIND:-127.0.0.1:8080}}" \
  node frontend/dist/server/entry.mjs &
wait
