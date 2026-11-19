#!/usr/bin/env bash
# K3s etcd snapshot + restore test in a disposable K3s container (embedded
# etcd, secrets encryption on, same as the Ansible role). Proves that:
#   - `k3s etcd-snapshot save` produces a snapshot
#   - objects deleted after the snapshot come back after a cluster-reset restore
#   - a restored Secret is still readable (the encryption config survives)
#   - the Secret is not stored in plaintext in etcd
#
#   infrastructure/scripts/etcd-restore-test.sh
set -euo pipefail
name=a2a-etcd-test
image="rancher/k3s:v1.37.2-k3s1@sha256:21ea6e0fac4e3930c2ce8aa40ddd41a8b18cb3e9a0222c030dc005e7e88c4332"
marker="restore-marker-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
cfg="$(mktemp)"
cleanup() { docker rm -f "$name" >/dev/null 2>&1 || true; docker volume rm "$name-data" >/dev/null 2>&1 || true; rm -f "$cfg"; }
trap cleanup EXIT
cleanup
fail() { echo "FAIL: $*" >&2; docker logs --tail 20 "$name" 2>&1 | tail -5 >&2; exit 1; }
k() { docker exec "$name" kubectl --request-timeout=60s "$@"; }
ready() { for _ in $(seq 1 180); do k get --raw /readyz >/dev/null 2>&1 && return 0; sleep 2; done; return 1; }

cat > "$cfg" <<'EOF'
write-kubeconfig-mode: "0600"
secrets-encryption: true
cluster-init: true
etcd-snapshot-dir: /var/lib/rancher/k3s/snapshots
etcd-arg: [heartbeat-interval=500, election-timeout=5000]
kube-controller-manager-arg: [leader-elect=false]
kube-scheduler-arg: [leader-elect=false]
kube-cloud-controller-manager-arg: [leader-elect=false]
disable: [traefik, servicelb, metrics-server]
EOF
docker run -d --name "$name" --hostname "$name" --privileged --tmpfs /run --tmpfs /var/run \
  -v "$name-data:/var/lib/rancher/k3s" -v "$cfg:/etc/rancher/k3s/config.yaml:ro" \
  "$image" server >/dev/null
ready || fail "K3s did not become ready"
docker exec "$name" mkdir -p /var/lib/rancher/k3s/snapshots
echo "ok   K3s (embedded etcd, secrets encryption) ready"

k create configmap "$marker" --from-literal=state=before-snapshot >/dev/null
k create secret generic "$marker" --from-literal=token=s3cr3t-value >/dev/null
docker exec "$name" k3s etcd-snapshot save --name pre-change 2>&1 | grep -qi 'saved\|snapshot' || fail "snapshot save"
snap=$(docker exec "$name" sh -c 'ls -1 /var/lib/rancher/k3s/snapshots | grep pre-change | head -1')
[ -n "$snap" ] || fail "no snapshot file"
echo "ok   snapshot saved: $snap ($(docker exec "$name" stat -c %s "/var/lib/rancher/k3s/snapshots/$snap") bytes)"
if docker exec "$name" sh -c "grep -c 's3cr3t-value' /var/lib/rancher/k3s/snapshots/$snap" >/dev/null 2>&1; then
  fail "secret value found in plaintext in the snapshot"
fi
echo "ok   secret value not present in plaintext in the snapshot"

k delete configmap "$marker" >/dev/null
k delete secret "$marker" >/dev/null
k get configmap "$marker" >/dev/null 2>&1 && fail "marker not deleted"
echo "ok   markers deleted after the snapshot (simulated loss)"

# Restore: stop the server, reset the cluster from the snapshot, start again.
docker stop -t 30 "$name" >/dev/null
docker run --rm --hostname "$name" --privileged --tmpfs /run --tmpfs /var/run \
  -v "$name-data:/var/lib/rancher/k3s" -v "$cfg:/etc/rancher/k3s/config.yaml:ro" \
  "$image" server --cluster-reset --cluster-reset-restore-path="/var/lib/rancher/k3s/snapshots/$snap" \
  > /dev/null 2>&1 || true
docker start "$name" >/dev/null
ready || fail "K3s did not come back after restore"
[ "$(k get configmap "$marker" -o jsonpath='{.data.state}' 2>/dev/null)" = before-snapshot ] || fail "configmap not restored"
[ "$(k get secret "$marker" -o jsonpath='{.data.token}' | base64 -d)" = s3cr3t-value ] || fail "secret not restored/readable"
echo "ok   restore: ConfigMap and Secret are back and the Secret decrypts"
docker exec "$name" k3s secrets-encrypt status | grep -q 'Encryption Status: Enabled' || fail "encryption disabled after restore"
echo "ok   secrets encryption still enabled after restore"
echo "etcd restore test: PASS"
