#!/usr/bin/env bash
# Restore test for the application database, in disposable containers:
#   1. source DB: migrate, run one real (mock) Worker -> Reviewer workflow
#   2. logical backup: pg_dump -Fc (what docs/deployment/backup-restore.md prescribes)
#   3. restore into a brand-new Postgres
#   4. the unmodified API image verifies the schema, starts, and serves the same
#      workflow with identical draft, review and audit rows
#
#   infrastructure/scripts/db-restore-test.sh [image-tag]    # default: dev
set -euo pipefail
tag="${1:-dev}"
net=a2a-restore
pw="restore-$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
out="$(mktemp -d)"
harden=(--read-only --cap-drop ALL --security-opt no-new-privileges:true --user 65532:65532
        --tmpfs /tmp:rw,noexec,nosuid,size=16m -e LOG_FORMAT=json)
names=(a2a-restore-src a2a-restore-dst a2a-restore-api a2a-restore-api2 a2a-restore-worker a2a-restore-reviewer)
cleanup() { docker rm -f "${names[@]}" >/dev/null 2>&1 || true; docker network rm "$net" >/dev/null 2>&1 || true; rm -rf "$out"; }
trap cleanup EXIT
cleanup; out="$(mktemp -d)"
fail() { echo "FAIL: $*" >&2; exit 1; }
pg() { # name port
  docker run -d --name "$1" --network "$net" -p "127.0.0.1:$2:8080" -e POSTGRES_PASSWORD="$pw" \
    -e POSTGRES_DB=a2a --tmpfs /var/lib/postgresql/data postgres:17-alpine >/dev/null
  for _ in $(seq 1 120); do docker exec "$1" pg_isready -h 127.0.0.1 -U postgres -d a2a >/dev/null 2>&1 && return; sleep 1; done
  fail "$1 did not start"
}
api() { # container db-container
  docker run -d --name "$1" --network "container:$2" "${harden[@]}" -e DATABASE_URL="$db" \
    -e WORKER_URL=http://a2a-restore-worker:9101 -e REVIEWER_URL=http://a2a-restore-reviewer:9102 \
    "a2a-api:$tag" >/dev/null
}
wait_api() { for _ in $(seq 1 90); do curl -fsS -o /dev/null "$1/api/health" 2>/dev/null && return; sleep 1; done; fail "api at $1"; }
db="postgres://postgres:$pw@127.0.0.1:5432/a2a"
counts="SELECT (SELECT count(*) FROM workflows)||'/'||(SELECT count(*) FROM agent_runs)||'/'||(SELECT count(*) FROM _sqlx_migrations)"

docker network create "$net" >/dev/null
pg a2a-restore-src 28181
for a in worker:9101 reviewer:9102; do
  n=${a%%:*} p=${a##*:}; up=$(echo "$n" | tr a-z A-Z)
  docker run -d --name "a2a-restore-$n" --network "$net" "${harden[@]}" -e INFERENCE_MODE=mock \
    -e "${up}_PUBLIC_URL=http://a2a-restore-$n:$p" "a2a-$n-agent:$tag" >/dev/null
done
docker run --rm --network container:a2a-restore-src "${harden[@]}" -e DATABASE_URL="$db" "a2a-api:$tag" migrate >/dev/null
api a2a-restore-api a2a-restore-src
src="http://127.0.0.1:28181"; wait_api "$src"
loc=$(curl -fsS -D - -o /dev/null -H 'Content-Type: application/json' \
  -d '{"question":"Propose validation rules for a Rust API that creates crowdfunding campaigns."}' \
  "$src/api/workflows" | tr -d '\r' | awk -F': ' 'tolower($1)=="location"{print $2}')
# Top-level status only: nested agent runs complete before the workflow does.
top_status() { python3 -I -c 'import json,sys; print(json.load(sys.stdin)["status"])'; }
for _ in $(seq 1 60); do
  [ "$(curl -fsS "$src$loc" | top_status)" = completed ] && break; sleep 1
done
curl -fsS "$src$loc" > "$out/before.json"
[ "$(top_status < "$out/before.json")" = completed ] || fail "source workflow did not complete"
before=$(docker exec a2a-restore-src psql -qAt -U postgres -d a2a -c "$counts")
echo "ok   source: workflow completed; workflows/agent_runs/migrations = $before"

# Backup: custom format, ownership dropped (the restoring role owns objects),
# ACLs kept: the runtime role's GRANTs and row-level-security policies are
# part of the security model and must survive a restore.
docker exec a2a-restore-src pg_dump -U postgres -d a2a -Fc --no-owner -f /tmp/a2a.dump
docker cp a2a-restore-src:/tmp/a2a.dump "$out/a2a.dump"
echo "ok   backup: $(wc -c < "$out/a2a.dump" | tr -d ' ') bytes (pg_dump -Fc)"
docker rm -f a2a-restore-api a2a-restore-src >/dev/null   # the original is gone

pg a2a-restore-dst 28182
docker cp "$out/a2a.dump" a2a-restore-dst:/tmp/a2a.dump
# Roles are cluster-wide and not in pg_dump: re-create the group roles the
# migrations define first (login roles get new passwords: a restore rotates them).
docker exec a2a-restore-dst psql -q -v ON_ERROR_STOP=1 -U postgres -d a2a \
  -c "CREATE ROLE a2a_app NOLOGIN" -c "CREATE ROLE a2a_mcp NOLOGIN"
docker exec a2a-restore-dst pg_restore -U postgres -d a2a --no-owner --exit-on-error /tmp/a2a.dump
after=$(docker exec a2a-restore-dst psql -qAt -U postgres -d a2a -c "$counts")
[ "$before" = "$after" ] || fail "row counts differ: $before vs $after"
echo "ok   restore: row counts identical ($after)"
grants=$(docker exec a2a-restore-dst psql -qAt -U postgres -d a2a -c \
  "SELECT has_table_privilege('a2a_app','workflows','SELECT') AND has_table_privilege('a2a_app','agent_runs','INSERT')
          AND NOT has_table_privilege('a2a_app','_sqlx_migrations','INSERT')")
[ "$grants" = t ] || fail "runtime role grants were not restored"
policies=$(docker exec a2a-restore-dst psql -qAt -U postgres -d a2a -c "SELECT count(*) FROM pg_policies")
[ "$policies" -gt 0 ] || fail "row-level-security policies missing"
echo "ok   restore: runtime-role grants and $policies RLS policies present"

# `migrate` must find nothing to do, and `serve` must accept the schema as-is.
docker run --rm --network container:a2a-restore-dst "${harden[@]}" -e DATABASE_URL="$db" "a2a-api:$tag" migrate 2>&1 \
  | grep -q 'database schema is up to date' || fail "migrate on restored DB"
api a2a-restore-api2 a2a-restore-dst
dst="http://127.0.0.1:28182"; wait_api "$dst"
curl -fsS "$dst$loc" > "$out/after.json"
python3 -I - "$out/before.json" "$out/after.json" <<'EOF' || fail "restored workflow differs"
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3])
for k in ("id", "status", "verdict", "draft", "review", "question"):
    assert a.get(k) == b.get(k), f"{k}: before={a.get(k)!r:.120} after={b.get(k)!r:.120}"
assert [r["id"] for r in a["runs"]] == [r["id"] for r in b["runs"]], "runs"
EOF
echo "ok   restored API serves the same workflow (draft, review, runs identical)"

# Rollback guard: an image older than the schema must refuse to start (it does
# not know the newer migrations), so a container rollback across a migration
# fails closed instead of running against a schema it does not understand.
docker rm -f a2a-restore-api2 >/dev/null
docker exec a2a-restore-dst psql -q -U postgres -d a2a -c \
  "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
   VALUES (29991231000000, 'future migration', true, '\\x00', 0)"
if docker run --rm --network container:a2a-restore-dst "${harden[@]}" -e DATABASE_URL="$db" \
     -e WORKER_URL=http://a2a-restore-worker:9101 -e REVIEWER_URL=http://a2a-restore-reviewer:9102 \
     "a2a-api:$tag" > "$out/old-binary.log" 2>&1; then
  fail "API started against a newer schema"
fi
grep -q 'unknown to this binary' "$out/old-binary.log" || fail "unexpected refusal reason: $(tail -1 "$out/old-binary.log")"
echo "ok   rollback guard: API refuses a schema newer than itself (fail closed)"
echo "db restore test: PASS"
