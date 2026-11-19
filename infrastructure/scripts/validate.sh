#!/usr/bin/env bash
# Offline validation of every Kubernetes overlay and the Argo CD manifests:
# render with kustomize, schema-check (kubeconform, strict), misconfiguration
# scan (trivy), and policy checks this repo relies on:
#   - dev/staging/production reference images by digest only
#   - every app namespace enforces Pod Security "restricted"
# Needs kubectl (kustomize built in), kubeconform, trivy on PATH.
# Exit 0 = all pass; unreleased (all-zero) digests are reported, not failed.
set -euo pipefail
cd "$(dirname "$0")/../kubernetes"
K8S_VERSION="${K8S_VERSION:-1.37.0}"
out="$(mktemp -d)"; trap 'rm -rf "$out"' EXIT
fail=0
crds='https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json'

for dir in overlays/*/; do
  env="$(basename "$dir")"
  [ "$env" = local ] && [ ! -d overlays/local/.generated ] && { echo "skip  local (run local-cluster.sh up first)"; continue; }
  kubectl kustomize "$dir" > "$out/$env.yaml"
  kubeconform -strict -summary -kubernetes-version "$K8S_VERSION" \
    -schema-location default -schema-location "$crds" "$out/$env.yaml" | sed "s/^/$env: /" || fail=1

  if [ "$env" != local ]; then
    if grep -E '^\s+image:' "$out/$env.yaml" | grep -vq '@sha256:[0-9a-f]\{64\}$'; then
      echo "$env: FAIL image without digest:"; grep -E '^\s+image:' "$out/$env.yaml" | grep -v '@sha256:' ; fail=1
    fi
    if grep -q '@sha256:0\{64\}' "$out/$env.yaml"; then echo "$env: note  not released yet (all-zero digests)"; fi
  fi
  grep -q 'pod-security.kubernetes.io/enforce: restricted' "$out/$env.yaml" \
    || { echo "$env: FAIL namespace does not enforce restricted"; fail=1; }
done

# Argo CD project + applications (ours: strict). The bootstrap install is
# upstream Argo CD + Sealed Secrets: schema-checked, misconfigurations reported
# but not failed (cluster-wide controller RBAC is inherent; see security.md).
mkdir -p "$out/upstream"
kubectl kustomize ../argocd > "$out/argocd.yaml"
kubectl kustomize ../argocd/install > "$out/upstream/argocd-install.yaml"
kubeconform -strict -summary -kubernetes-version "$K8S_VERSION" \
  -schema-location default -schema-location "$crds" "$out/argocd.yaml" | sed 's/^/argocd: /' || fail=1
kubeconform -strict -summary -kubernetes-version "$K8S_VERSION" -skip CustomResourceDefinition \
  -schema-location default -schema-location "$crds" "$out/upstream/argocd-install.yaml" \
  | sed 's/^/argocd-install: /' || fail=1
# Upstream install.yaml has no namespaces; workloads must not land in `default`.
misplaced=$(python3 -I -c '
import re, sys
for d in open(sys.argv[1]).read().split("\n---\n"):
    if re.search(r"^kind: (Deployment|StatefulSet)$", d, re.M):
        ns = re.search(r"^  namespace: (\S+)", d, re.M)
        if not ns or ns.group(1) not in ("argocd", "kube-system"):
            print(re.search(r"^  name: (\S+)", d, re.M).group(1))' "$out/upstream/argocd-install.yaml")
[ -z "$misplaced" ] || { echo "argocd-install: FAIL workloads outside argocd/kube-system: $misplaced"; fail=1; }
if sed -n '/name: a2a-production/,/^---/p' "$out/argocd.yaml" | grep -q 'automated'; then
  echo "argocd: FAIL a2a-production must not auto-sync"; fail=1
fi

trivy config --quiet --disable-telemetry --severity HIGH,CRITICAL --exit-code 1 --skip-dirs "$out/upstream" "$out" || fail=1
echo "upstream install (report only):"
trivy config --quiet --disable-telemetry --severity HIGH,CRITICAL --format table "$out/upstream" \
  | grep -E '^(CRITICAL|HIGH|[A-Z]+: )|Failures:' | sed 's/^/  /' || true
[ "$fail" -eq 0 ] && echo "validate: PASS" || echo "validate: FAIL"
exit "$fail"
