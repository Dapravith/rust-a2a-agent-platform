#!/usr/bin/env bash
# Behavioural tests against the local K3s test bed (local-cluster.sh up).
# Network checks run from busybox ephemeral containers attached to the real
# pods, so they share each pod's network namespace and NetworkPolicies.
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
export KUBECONFIG="${KUBECONFIG:-$root/kubernetes/overlays/local/.generated/kubeconfig}"
ns=a2a-local
probe="busybox:1.37.0@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec"
pass=0 fail=0
ok()  { echo "PASS  $*"; pass=$((pass+1)); }
bad() { echo "FAIL  $*"; fail=$((fail+1)); }
pod() { kubectl -n "$ns" get pod -l "app.kubernetes.io/name=$1" -o jsonpath='{.items[0].metadata.name}'; }

# Runs a shell command in an ephemeral container in the given app's pod.
from() { # app command...
  local app=$1; shift
  local p; p=$(pod "$app")
  kubectl -n "$ns" debug "$p" -q --profile=restricted --image="$probe" --target="$app" \
    --attach --rm=false -c "probe-$RANDOM" -- sh -c "$*" 2>/dev/null
}
reach()   { from "$1" "nc -z -w 3 $2 $3 && echo OPEN || echo CLOSED"; }
expect()  { # app host port OPEN|CLOSED description
  local got; got=$(reach "$1" "$2" "$3" | tail -1)
  [ "$got" = "$4" ] && ok "$5 ($1 -> $2:$3 $got)" || bad "$5 ($1 -> $2:$3 expected $4, got ${got:-no output})"
}

echo "== workloads"
for d in api worker-agent reviewer-agent portal testdb; do
  r=$(kubectl -n "$ns" get deploy "$d" -o jsonpath='{.status.readyReplicas}')
  [ "${r:-0}" -ge 1 ] && ok "$d ready" || bad "$d not ready"
done
kubectl -n "$ns" get job api-migrate -o jsonpath='{.status.succeeded}' | grep -q 1 && ok "migration job succeeded" || bad "migration job"

echo "== pod security admission (restricted)"
if kubectl -n "$ns" run psa-probe --image="$probe" --restart=Never --dry-run=server \
     --overrides='{"spec":{"containers":[{"name":"p","image":"'"$probe"'","securityContext":{"privileged":true}}]}}' >/dev/null 2>&1; then
  bad "privileged pod admitted"
else ok "privileged pod rejected"; fi
if kubectl -n "$ns" run psa-probe2 --image="$probe" --restart=Never --dry-run=server >/dev/null 2>&1; then
  bad "pod without restricted securityContext admitted"
else ok "pod without restricted securityContext rejected"; fi

echo "== secrets encryption"
docker exec a2a-k3s k3s secrets-encrypt status 2>/dev/null | grep -q 'Encryption Status: Enabled' \
  && ok "secrets encryption enabled" || bad "secrets encryption not enabled"

echo "== network policy: allowed paths"
expect portal api 8080 OPEN "portal reaches api"
expect api worker-agent 9101 OPEN "api reaches worker"
expect api reviewer-agent 9102 OPEN "api reaches reviewer"
expect api testdb 5432 OPEN "api reaches database"

echo "== network policy: denied paths"
expect portal worker-agent 9101 CLOSED "portal cannot reach agents"
expect portal testdb 5432 CLOSED "portal cannot reach database"
expect worker-agent api 8080 CLOSED "agent cannot call api"
expect worker-agent reviewer-agent 9102 CLOSED "agents cannot call each other"
expect worker-agent testdb 5432 CLOSED "agent cannot reach database"
expect api portal 4321 CLOSED "api cannot reach portal"
expect portal 1.1.1.1 443 CLOSED "portal has no internet egress"
expect api 1.1.1.1 443 CLOSED "api has no HTTPS egress"
expect worker-agent kubernetes.default.svc 443 CLOSED "agent cannot reach the Kubernetes API"
if kubectl -n "$ns" run np-outsider --image="$probe" --restart=Never --rm -i -q \
     --overrides='{"spec":{"securityContext":{"runAsNonRoot":true,"runAsUser":65532,"seccompProfile":{"type":"RuntimeDefault"}},"containers":[{"name":"np-outsider","image":"'"$probe"'","command":["sh","-c","nc -z -w 3 api 8080 && echo OPEN || echo CLOSED"],"securityContext":{"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]}}}]}}' \
     2>/dev/null | grep -q CLOSED; then ok "unlabelled pod cannot reach api"; else bad "unlabelled pod reached api"; fi

echo "== ingress"
code=$(curl -sk -o /dev/null -w '%{http_code}' --resolve portal.local.test:18443:127.0.0.1 https://portal.local.test:18443/healthz)
[ "$code" = 200 ] && ok "portal served through Traefik over TLS" || bad "portal via ingress: HTTP $code"
code=$(curl -sk -o /dev/null -w '%{http_code}' -H 'Host: api.local.test' https://127.0.0.1:18443/api/health)
[ "$code" = 404 ] && ok "no ingress route to the api (HTTP 404)" || bad "unexpected route to api: HTTP $code"

echo "== workflow through the cluster (portal pod -> api -> agents -> db)"
loc=$(from portal "wget -q -S -O /dev/null --header 'Content-Type: application/json' --header 'Idempotency-Key: k3s-$RANDOM' \
  --post-data '{\"question\":\"Propose validation rules for a Rust API that creates crowdfunding campaigns.\"}' http://api:8080/api/workflows 2>&1 | sed -n 's/.*[Ll]ocation: //p'" | tr -d '\r' | tail -1)
if [ -n "$loc" ]; then
  st=""
  for _ in $(seq 1 30); do
    st=$(from portal "wget -q -O - http://api:8080$loc" | grep -o '"status":"[a-z_]*"' | head -1 | cut -d'"' -f4)
    case "$st" in completed|failed) break;; esac; sleep 2
  done
  [ "$st" = completed ] && ok "mock Worker -> Reviewer workflow completed" || bad "workflow ended as '${st:-unknown}'"
else bad "workflow not accepted"; fi

echo "== resources"
kubectl -n "$ns" top pods 2>/dev/null || echo "(metrics not ready yet)"

echo "cluster tests: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
