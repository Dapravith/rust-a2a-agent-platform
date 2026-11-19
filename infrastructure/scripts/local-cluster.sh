#!/usr/bin/env bash
# Local K3s test bed (Docker). Runs the pinned K3s release with the same Pod
# Security admission and audit policy files the Ansible role installs,
# secrets encryption and embedded etcd, then deploys overlays/local.
#
#   infrastructure/scripts/local-cluster.sh up      # create + deploy
#   infrastructure/scripts/local-cluster.sh deploy  # re-apply overlays/local
#   infrastructure/scripts/local-cluster.sh down    # delete everything
#   export KUBECONFIG=infrastructure/kubernetes/overlays/local/.generated/kubeconfig
#
# Needs: docker, kubectl, openssl, and images a2a-{api,worker-agent,reviewer-agent,portal}:dev.
# The K3s container is privileged (it is a Kubernetes node); local testing only.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
gen="$root/kubernetes/overlays/local/.generated"
role="$root/ansible/roles/k3s/files"
name=a2a-k3s
image="rancher/k3s:v1.37.2-k3s1@sha256:21ea6e0fac4e3930c2ce8aa40ddd41a8b18cb3e9a0222c030dc005e7e88c4332"
export KUBECONFIG="$gen/kubeconfig"

generate() {
  mkdir -p "$gen" && chmod 700 "$gen"
  [ -f "$gen/ca.crt" ] && return 0
  local db_host=testdb.a2a-local.svc.cluster.local
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 \
    -subj "/CN=a2a local test CA" -keyout "$gen/ca.key" -out "$gen/ca.crt" 2>/dev/null
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=$db_host" \
    -keyout "$gen/db.key" -out "$gen/db.csr" 2>/dev/null
  printf 'subjectAltName=DNS:%s,DNS:testdb\nextendedKeyUsage=serverAuth\n' "$db_host" > "$gen/db.ext"
  openssl x509 -req -in "$gen/db.csr" -CA "$gen/ca.crt" -CAkey "$gen/ca.key" -CAcreateserial \
    -days 30 -extfile "$gen/db.ext" -out "$gen/db.crt" 2>/dev/null
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 \
    -subj "/CN=portal.local.test" -addext "subjectAltName=DNS:portal.local.test" \
    -keyout "$gen/portal.key" -out "$gen/portal.crt" 2>/dev/null
  openssl rand -hex 24 | tr -d '\n' > "$gen/db-password"
  printf 'postgres://a2a:%s@%s:5432/a2a?sslmode=verify-full&sslrootcert=/etc/a2a/db-ca/ca.crt' \
    "$(cat "$gen/db-password")" "$db_host" > "$gen/database-url"
  rm -f "$gen/db.csr" "$gen/db.ext" "$gen/ca.srl"
  chmod 600 "$gen"/*
}

# LOCAL_DATASTORE=sqlite: K3s's lighter SQLite datastore instead of embedded
# etcd, for hosts too loaded for etcd. Everything except etcd snapshots works.
datastore="${LOCAL_DATASTORE:-etcd}"

k3s_config() {
  cat > "$gen/k3s-config.yaml" <<'EOF'
# Same security settings as roles/k3s/templates/config.yaml.j2, minus
# host-specific ones (node-ip, protect-kernel-defaults) a container cannot use.
write-kubeconfig-mode: "0600"
tls-san: [127.0.0.1]
secrets-encryption: true
# Test-bed only: a shared, overloaded laptop starves the datastore, and the
# controllers exit when their leader lease lapses; single-node leader election is off.
kube-controller-manager-arg: [leader-elect=false]
kube-scheduler-arg: [leader-elect=false]
kube-cloud-controller-manager-arg: [leader-elect=false]
kube-apiserver-arg:
  - admission-control-config-file=/etc/rancher/k3s/psa.yaml
  - audit-policy-file=/etc/rancher/k3s/audit-policy.yaml
  - audit-log-path=/var/lib/rancher/k3s/server/logs/audit.log
EOF
  if [ "$datastore" = etcd ]; then
    cat >> "$gen/k3s-config.yaml" <<'EOF'
cluster-init: true
# etcd data on tmpfs (Docker Desktop disks are too slow), relaxed timeouts;
# snapshots stay on the persistent volume.
etcd-snapshot-dir: /var/lib/rancher/k3s/snapshots
etcd-arg: [heartbeat-interval=500, election-timeout=5000]
EOF
  fi
}

up() {
  generate; k3s_config
  docker network inspect "$name" >/dev/null 2>&1 || docker network create "$name" >/dev/null
  if ! docker container inspect "$name" >/dev/null 2>&1; then
    local dbfs=(); [ "$datastore" = etcd ] && dbfs=(--tmpfs /var/lib/rancher/k3s/server/db:rw,size=1g)
    docker run -d --name "$name" --hostname "$name" --privileged --network "$name" \
      --tmpfs /run --tmpfs /var/run "${dbfs[@]}" \
      -p 127.0.0.1:16443:6443 -p 127.0.0.1:18443:443 \
      -v "$name-data:/var/lib/rancher/k3s" \
      -v "$gen/k3s-config.yaml:/etc/rancher/k3s/config.yaml:ro" \
      -v "$role/psa.yaml:/etc/rancher/k3s/psa.yaml:ro" \
      -v "$role/audit-policy.yaml:/etc/rancher/k3s/audit-policy.yaml:ro" \
      "$image" server >/dev/null
  fi
  for _ in $(seq 1 120); do
    docker exec "$name" test -s /etc/rancher/k3s/k3s.yaml 2>/dev/null && break; sleep 2
  done
  docker exec "$name" cat /etc/rancher/k3s/k3s.yaml | sed 's#https://127.0.0.1:6443#https://127.0.0.1:16443#' > "$KUBECONFIG"
  chmod 600 "$KUBECONFIG"
  # The kubeconfig is written before the API server serves requests.
  for _ in $(seq 1 150); do kubectl get --raw /readyz >/dev/null 2>&1 && break; sleep 2; done
  for _ in $(seq 1 60); do kubectl get node -o name 2>/dev/null | grep -q . && break; sleep 2; done
  kubectl wait --for=condition=Ready node --all --timeout=300s
  [ "$datastore" = etcd ] && docker exec "$name" mkdir -p /var/lib/rancher/k3s/snapshots
  for i in api worker-agent reviewer-agent portal; do
    # containerd can restart briefly while the node settles; retry.
    for attempt in 1 2 3 4 5; do
      docker save "a2a-$i:dev" | docker exec -i "$name" ctr -n k8s.io images import - >/dev/null 2>&1 && break
      [ "$attempt" = 5 ] && { echo "image import failed: a2a-$i" >&2; exit 1; }
      sleep 10
    done
  done
  echo "images imported"
  deploy
}

deploy() {
  # A loaded laptop makes the API server slow; retry rather than fail.
  for attempt in 1 2 3 4 5 6; do
    kubectl apply --request-timeout=120s -k "$root/kubernetes/overlays/local" && break
    [ "$attempt" = 6 ] && return 1
    sleep 20
  done
  kubectl -n a2a-local wait --for=condition=complete job/api-migrate --timeout=300s
  for d in testdb worker-agent reviewer-agent api portal; do
    kubectl -n a2a-local rollout status "deploy/$d" --timeout=300s
  done
}

down() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm "$name-data" >/dev/null 2>&1 || true
  docker network rm "$name" >/dev/null 2>&1 || true
  rm -rf "$gen"
  echo "local cluster removed"
}

case "${1:-}" in
  up) up ;; deploy) deploy ;; down) down ;;
  *) echo "usage: $0 up|deploy|down" >&2; exit 2 ;;
esac
