#!/usr/bin/env bash
# Runs the four pilot images together under production-like restrictions and
# checks: probes, Agent Card discovery, one mock Worker -> Reviewer workflow,
# portal rendering, security headers, and SIGTERM shutdown time.
#
#   scripts/container-smoke.sh [tag]      # default tag: dev (a2a-<name>:<tag>)
#
# Uses a throwaway Postgres; nothing is published beyond 127.0.0.1.
set -euo pipefail
tag="${1:-dev}"
net=a2a-smoke
pg=a2a-smoke-pg
api_port=18080 portal_port=14321
pw="smoke-$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"

harden=(--read-only --cap-drop ALL --security-opt no-new-privileges:true
        --user 65532:65532 --tmpfs /tmp:rw,noexec,nosuid,size=16m
        --memory 256m --pids-limit 128 -e LOG_FORMAT=json)

cleanup() { docker rm -f a2a-smoke-portal a2a-smoke-api a2a-smoke-worker a2a-smoke-reviewer "$pg" >/dev/null 2>&1 || true
            docker network rm "$net" >/dev/null 2>&1 || true; }
trap cleanup EXIT
cleanup
fail() { echo "FAIL: $*" >&2; docker logs a2a-smoke-api 2>&1 | tail -20 >&2 || true; exit 1; }
wait_for() { # url seconds
  for _ in $(seq 1 "$2"); do curl -fsS -o /dev/null "$1" 2>/dev/null && return 0; sleep 1; done; return 1; }

docker network create --internal=false "$net" >/dev/null
# The API runs inside this container's network namespace, so the database is
# on loopback (the API demands verify-full TLS for any remote host).
docker run -d --name "$pg" --network "$net" \
  -p "127.0.0.1:$api_port:8080" -e POSTGRES_PASSWORD="$pw" -e POSTGRES_DB=a2a \
  --tmpfs /var/lib/postgresql/data postgres:17-alpine >/dev/null
for _ in $(seq 1 120); do docker exec "$pg" pg_isready -U postgres -d a2a >/dev/null 2>&1 && break; sleep 1; done
db="postgres://postgres:$pw@127.0.0.1:5432/a2a"

for a in worker:9101 reviewer:9102; do
  n=${a%%:*} p=${a##*:}; up=$(echo "$n" | tr a-z A-Z)
  docker run -d --name "a2a-smoke-$n" --network "$net" "${harden[@]}" \
    -e INFERENCE_MODE=mock -e "${up}_PUBLIC_URL=http://a2a-smoke-$n:$p" "a2a-$n-agent:$tag" >/dev/null
done

docker run --rm --network "container:$pg" "${harden[@]}" -e DATABASE_URL="$db" "a2a-api:$tag" migrate \
  || fail "migrate"
docker run -d --name a2a-smoke-api --network "container:$pg" "${harden[@]}" \
  -e DATABASE_URL="$db" -e WORKER_URL=http://a2a-smoke-worker:9101 \
  -e REVIEWER_URL=http://a2a-smoke-reviewer:9102 "a2a-api:$tag" >/dev/null
docker run -d --name a2a-smoke-portal --network "$net" "${harden[@]}" \
  -p "127.0.0.1:$portal_port:4321" -e API_ORIGIN="http://$pg:8080" "a2a-portal:$tag" >/dev/null

api="http://127.0.0.1:$api_port" portal="http://127.0.0.1:$portal_port"
wait_for "$api/healthz" 90 || fail "api liveness"
curl -fsS "$api/api/health" | grep -q '"database":"ok"' || fail "api readiness"
echo "ok   api /healthz and /api/health"
agents=$(curl -fsS "$api/api/agents")
[ "$(echo "$agents" | grep -o '"reachable":true' | wc -l | tr -d ' ')" = 2 ] || fail "agent discovery: $agents"
echo "ok   both Agent Cards discovered"

wait_for "$portal/healthz" 60 || fail "portal liveness"
headers=$(curl -fsS -D - -o /dev/null "$portal/")
echo "$headers" | grep -qi '^content-security-policy:' || fail "portal CSP header"
echo "$headers" | grep -qi '^x-frame-options: DENY' || fail "portal X-Frame-Options"
echo "ok   portal /healthz, / renders with security headers"

loc=$(curl -fsS -D - -o /dev/null -H 'Content-Type: application/json' -H 'Idempotency-Key: smoke-1' \
  -d '{"question":"Propose validation rules for a Rust API that creates crowdfunding campaigns."}' \
  "$api/api/workflows" | tr -d '\r' | awk -F': ' 'tolower($1)=="location"{print $2}')
[ -n "$loc" ] || fail "workflow not accepted"
status=""
for _ in $(seq 1 120); do
  status=$(curl -fsS "$api$loc" | grep -o '"status":"[a-z_]*"' | head -1 | cut -d'"' -f4)
  case "$status" in completed|failed) break;; esac; sleep 1
done
[ "$status" = completed ] || fail "workflow ended as '$status'"
id=${loc##*/}
curl -fsS "$portal/workflows/$id" | grep -qi 'review' || fail "portal workflow detail page"
echo "ok   mock Worker -> Reviewer workflow completed and renders in the portal"

for c in a2a-smoke-portal a2a-smoke-api a2a-smoke-worker a2a-smoke-reviewer; do
  start=$(date +%s); docker stop -t 20 "$c" >/dev/null; secs=$(( $(date +%s) - start ))
  code=$(docker inspect -f '{{.State.ExitCode}}' "$c")
  [ "$secs" -lt 10 ] || fail "$c ignored SIGTERM (${secs}s)"
  echo "ok   $c stopped on SIGTERM in ${secs}s (exit $code)"
done
echo "container smoke: PASS"
