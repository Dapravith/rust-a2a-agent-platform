#!/usr/bin/env bash
# GitOps test on the local K3s test bed (local-cluster.sh up first):
#   - installs Argo CD + Sealed Secrets exactly as infrastructure/argocd/install
#   - serves a snapshot of this working tree from a throwaway Gitea (nothing is
#     pushed anywhere else; the repo itself is not modified)
#   - seals the test credentials with the in-cluster controller (the AppProject
#     allows SealedSecret, not Secret) and syncs through the real project policy
# Checks: sync + health, workflow through the GitOps-deployed stack, drift
# self-heal, Git change applied, Git revert (rollback) applied, a disallowed
# kind is refused, and the production sync window blocks automated sync.
set -uo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
gen="$root/infrastructure/kubernetes/overlays/local/.generated"
export KUBECONFIG="${KUBECONFIG:-$gen/kubeconfig}"
gitea_image="gitea/gitea:28.4.0-rootless@sha256:15ab7fadb44981a2b781f40c712cad07066197f395100ea1c6326e14e5bb073a"
ns=a2a-gitops
work="$(mktemp -d)"
pass=0 fail=0
ok()  { echo "PASS  $*"; pass=$((pass+1)); }
bad() { echo "FAIL  $*"; fail=$((fail+1)); }
die() { echo "ABORT $*" >&2; exit 1; }
until_true() { # seconds command...
  local n=$1; shift
  for _ in $(seq 1 "$n"); do "$@" >/dev/null 2>&1 && return 0; sleep 1; done; return 1
}
app_field() { kubectl -n argocd get application "$1" -o jsonpath="$2" 2>/dev/null; }
synced_healthy() { [ "$(app_field "$1" '{.status.sync.status}/{.status.health.status}')" = Synced/Healthy ]; }

echo "== install Argo CD + Sealed Secrets (infrastructure/argocd/install)"
kubectl apply --server-side --force-conflicts -k "$root/infrastructure/argocd/install" >/dev/null || die "install"
for d in argocd-server argocd-repo-server argocd-redis; do
  kubectl -n argocd rollout status "deploy/$d" --timeout=900s >/dev/null || die "$d not ready"
done
kubectl -n argocd rollout status statefulset/argocd-application-controller --timeout=900s >/dev/null || die "controller"
kubectl -n kube-system rollout status deploy/sealed-secrets-controller --timeout=600s >/dev/null || die "sealed-secrets"
ok "Argo CD and Sealed Secrets running (pinned digests)"
for d in argocd-dex-server argocd-applicationset-controller; do
  [ "$(kubectl -n argocd get deploy "$d" -o jsonpath='{.spec.replicas}')" = 0 ] && ok "$d scaled to 0" || bad "$d running"
done

echo "== Git server (throwaway Gitea) with a snapshot of the working tree"
docker rm -f a2a-gitea >/dev/null 2>&1
docker run -d --name a2a-gitea --network a2a-k3s -p 127.0.0.1:13000:3000 \
  -e GITEA__security__INSTALL_LOCK=true -e GITEA__database__DB_TYPE=sqlite3 \
  -e GITEA__server__ROOT_URL=http://127.0.0.1:13000/ "$gitea_image" >/dev/null
until_true 120 curl -fsS http://127.0.0.1:13000/api/healthz || die "gitea"
gpw="$(openssl rand -hex 16)"
docker exec a2a-gitea gitea admin user create --username a2a --password "$gpw" \
  --email a2a@example.invalid --admin --must-change-password=false >/dev/null || die "gitea user"
curl -fsS -u "a2a:$gpw" -H 'Content-Type: application/json' -d '{"name":"platform","private":false}' \
  http://127.0.0.1:13000/api/v1/user/repos >/dev/null || die "gitea repo"
gitea_ip="$(docker inspect -f '{{(index .NetworkSettings.Networks "a2a-k3s").IPAddress}}' a2a-gitea)"
repo_url="http://$gitea_ip:3000/a2a/platform.git"

snap="$work/repo"; mkdir -p "$snap"
(cd "$root" && git ls-files -co --exclude-standard -z | xargs -0 -I{} cp -p --parents {} "$snap" 2>/dev/null \
  || git ls-files -co --exclude-standard | while IFS= read -r f; do mkdir -p "$snap/$(dirname "$f")"; cp -p "$f" "$snap/$f"; done)

echo "== seal test credentials with the cluster's public key"
kubeseal --controller-namespace kube-system --fetch-cert > "$work/pub.pem" 2>/dev/null \
  || { sleep 20; kubeseal --controller-namespace kube-system --fetch-cert > "$work/pub.pem"; } || die "fetch cert"
ov="$snap/infrastructure/kubernetes/overlays/gitops-test"; mkdir -p "$ov"
pw="$(cat "$gen/db-password")"
url="postgres://a2a:$pw@testdb:5432/a2a?sslmode=verify-full&sslrootcert=/etc/a2a/db-ca/ca.crt"
seal() { kubeseal --cert "$work/pub.pem" --format yaml --namespace "$ns" > "$ov/sealed-$1.yaml"; }
kubectl create secret generic api-database -n "$ns" --from-literal=url="$url" --dry-run=client -o yaml | seal api-database
kubectl create secret generic api-database-migration -n "$ns" --from-literal=url="$url" --dry-run=client -o yaml | seal api-database-migration
kubectl create secret generic testdb-auth -n "$ns" --from-literal=password="$pw" --dry-run=client -o yaml | seal testdb-auth
kubectl create secret tls testdb-tls -n "$ns" --cert="$gen/db.crt" --key="$gen/db.key" --dry-run=client -o yaml | seal testdb-tls
kubectl create secret tls portal-tls -n "$ns" --cert="$gen/portal.crt" --key="$gen/portal.key" --dry-run=client -o yaml | seal portal-tls
grep -rq "$pw" "$ov" && bad "plaintext password in sealed files" || ok "sealed files contain no plaintext credential"
cp "$gen/ca.crt" "$ov/ca.crt"
sed "s/a2a-local/$ns/" "$root/infrastructure/kubernetes/overlays/local/namespace.yaml" > "$ov/namespace.yaml"
cat > "$ov/kustomization.yaml" <<EOF
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
namespace: $ns
resources:
  - namespace.yaml
  - ../../base
  - ../local/testdb.yaml
  - sealed-api-database.yaml
  - sealed-api-database-migration.yaml
  - sealed-testdb-auth.yaml
  - sealed-testdb-tls.yaml
  - sealed-portal-tls.yaml
images:
  - { name: ghcr.io/dapravith/a2a-api, newName: docker.io/library/a2a-api, newTag: dev }
  - { name: ghcr.io/dapravith/a2a-worker-agent, newName: docker.io/library/a2a-worker-agent, newTag: dev }
  - { name: ghcr.io/dapravith/a2a-reviewer-agent, newName: docker.io/library/a2a-reviewer-agent, newTag: dev }
  - { name: ghcr.io/dapravith/a2a-portal, newName: docker.io/library/a2a-portal, newTag: dev }
configMapGenerator:
  - name: db-ca
    files: [ca.crt]
patches:
  - target: { kind: Ingress, name: portal }
    patch: |-
      - { op: replace, path: /spec/rules/0/host, value: portal.gitops.test }
      - { op: replace, path: /spec/tls/0/hosts/0, value: portal.gitops.test }
EOF
mkdir -p "$snap/test/prod-window"
cat > "$snap/test/prod-window/configmap.yaml" <<EOF
apiVersion: v1
kind: ConfigMap
metadata: { name: window-probe }
data: { synced: "yes" }
EOF

git -C "$snap" init -q -b main && git -C "$snap" add -A \
  && git -C "$snap" -c user.name=t -c user.email=t@example.invalid commit -qm snapshot \
  && git -C "$snap" push -q "http://a2a:$gpw@127.0.0.1:13000/a2a/platform.git" main || die "push"
push() { git -C "$snap" -c user.name=t -c user.email=t@example.invalid commit -qam "$1" \
  && git -C "$snap" push -q "http://a2a:$gpw@127.0.0.1:13000/a2a/platform.git" main; }
ok "snapshot pushed to $repo_url"

echo "== project + applications (real AppProject policy, test repo and namespaces)"
sed -e "s#https://github.com/Dapravith/rust-a2a-agent-platform.git#$repo_url#" \
    -e 's/namespace: a2a-dev }/namespace: a2a-gitops }/' \
    -e 's/namespace: a2a-production }/namespace: a2a-window }/' \
  "$root/infrastructure/argocd/projects/a2a.yaml" | kubectl apply -f - >/dev/null || die "project"
kubectl apply -f - >/dev/null <<EOF || die "applications"
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata: { name: a2a-gitops-test, namespace: argocd }
spec:
  project: a2a
  source: { repoURL: "$repo_url", targetRevision: main, path: infrastructure/kubernetes/overlays/gitops-test }
  destination: { server: https://kubernetes.default.svc, namespace: $ns }
  syncPolicy:
    automated: { prune: true, selfHeal: true }
    syncOptions: [PruneLast=true, ApplyOutOfSyncOnly=true]
---
# Named like production on purpose: the AppProject deny window must stop it
# even though this copy (wrongly) asks for automated sync.
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata: { name: a2a-production, namespace: argocd }
spec:
  project: a2a
  source: { repoURL: "$repo_url", targetRevision: main, path: test/prod-window }
  destination: { server: https://kubernetes.default.svc, namespace: a2a-window }
  syncPolicy:
    automated: { prune: true, selfHeal: true }
    syncOptions: [CreateNamespace=true]
EOF

echo "== initial sync"
if until_true 900 synced_healthy a2a-gitops-test; then ok "a2a-gitops-test Synced/Healthy"
else bad "not Synced/Healthy: $(app_field a2a-gitops-test '{.status.sync.status}/{.status.health.status} {.status.conditions[*].message}')"; fi
kubectl -n "$ns" get secret api-database >/dev/null 2>&1 && ok "SealedSecret decrypted into Secret api-database" || bad "secret not unsealed"
kubectl -n "$ns" get job api-migrate -o jsonpath='{.status.succeeded}' 2>/dev/null | grep -q 1 \
  && ok "PreSync migration job succeeded" || bad "migration hook"

echo "== workflow through the GitOps-deployed stack"
p=$(kubectl -n "$ns" get pod -l app.kubernetes.io/name=api -o jsonpath='{.items[0].metadata.name}')
kubectl -n "$ns" port-forward "pod/$p" 28480:8080 >/dev/null 2>&1 & pf=$!
until_true 30 curl -fsS http://127.0.0.1:28480/api/health
loc=$(curl -fsS -D - -o /dev/null -H 'Content-Type: application/json' \
  -d '{"question":"Propose validation rules for a Rust API that creates crowdfunding campaigns."}' \
  http://127.0.0.1:28480/api/workflows | tr -d '\r' | awk -F': ' 'tolower($1)=="location"{print $2}')
st=""; for _ in $(seq 1 60); do
  st=$(curl -fsS "http://127.0.0.1:28480$loc" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["status"])' 2>/dev/null)
  case "$st" in completed|failed) break;; esac; sleep 2; done
kill "$pf" 2>/dev/null
[ "$st" = completed ] && ok "workflow completed (api -> agents over A2A, TLS database)" || bad "workflow: ${st:-none}"

echo "== drift is reverted (selfHeal)"
kubectl -n "$ns" scale deploy/portal --replicas=0 >/dev/null
sleep 5
if until_true 300 test "$(kubectl -n "$ns" get deploy portal -o jsonpath='{.spec.replicas}')" = 1; then
  ok "manual scale to 0 reverted to Git's 1 replica"; else bad "drift not reverted"; fi

echo "== Git change is applied, Git revert rolls it back"
sed -i.bak 's/RUST_LOG: info/RUST_LOG: debug/' "$snap/infrastructure/kubernetes/base/agents/agents.yaml" && rm -f "$snap/infrastructure/kubernetes/base/agents/agents.yaml.bak"
push "agents: debug logging" || bad "push change"
kubectl -n argocd annotate application a2a-gitops-test argocd.argoproj.io/refresh=hard --overwrite >/dev/null
cm_level() { test "$(kubectl -n "$ns" get cm agent-config -o jsonpath='{.data.RUST_LOG}')" = "$1"; }
until_true 300 cm_level debug && ok "commit applied (agent-config RUST_LOG=debug)" || bad "commit not applied"
git -C "$snap" -c user.name=t -c user.email=t@example.invalid revert --no-edit HEAD -q \
  && git -C "$snap" push -q "http://a2a:$gpw@127.0.0.1:13000/a2a/platform.git" main
kubectl -n argocd annotate application a2a-gitops-test argocd.argoproj.io/refresh=hard --overwrite >/dev/null
until_true 300 cm_level info && ok "git revert rolled back (RUST_LOG=info)" || bad "revert not applied"

echo "== AppProject refuses a disallowed kind"
cat > "$ov/role.yaml" <<EOF
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata: { name: escalate }
rules: [{ apiGroups: ["*"], resources: ["*"], verbs: ["*"] }]
EOF
sed -i.bak 's/  - sealed-portal-tls.yaml/  - sealed-portal-tls.yaml\n  - role.yaml/' "$ov/kustomization.yaml" && rm -f "$ov/kustomization.yaml.bak"
git -C "$snap" add -A && push "try to add a Role" || bad "push role"
kubectl -n argocd annotate application a2a-gitops-test argocd.argoproj.io/refresh=hard --overwrite >/dev/null
sleep 45
if kubectl -n "$ns" get role escalate >/dev/null 2>&1; then bad "Role was created"
else ok "Role not created ($(app_field a2a-gitops-test '{.status.operationState.message}' | cut -c1-110))"; fi

echo "== production sync window blocks automated sync"
sleep 60
if kubectl get ns a2a-window >/dev/null 2>&1 && kubectl -n a2a-window get cm window-probe >/dev/null 2>&1; then
  bad "a2a-production synced automatically"
else ok "a2a-production not synced automatically (state: $(app_field a2a-production '{.status.sync.status}'))"; fi

kubectl -n argocd delete application a2a-production --wait=false >/dev/null 2>&1
docker rm -f a2a-gitea >/dev/null 2>&1; rm -rf "$work"
echo "argocd tests: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
