#!/usr/bin/env bash
# Sets the image digests of one overlay. Never builds: the same digests CI
# built, scanned and attested move between environments.
#
#   promote.sh --release release.json dev   # from release.yml's manifest
#   promote.sh dev staging                  # copy digests dev -> staging
#   promote.sh staging production           # copy digests staging -> production
#
# Order is enforced (release -> dev -> staging -> production). The script only
# edits files; commit it on a branch and open a PR (docs/deployment/gitops.md).
set -euo pipefail
[ "${1:-}" = --release ] && set -- "$1" "$(cd "$(dirname "$2")" && pwd)/$(basename "$2")" "${@:3}"
cd "$(dirname "$0")/../kubernetes/overlays"
images=(a2a-api a2a-worker-agent a2a-reviewer-agent a2a-portal)

digest_of() { # overlay image -> digest
  awk -v img="ghcr.io/dapravith/$2" '
    $0 ~ "- name: " img "$" { found = 1; next }
    found && /digest:/ { print $2; exit }' "$1/kustomization.yaml"
}

set_digest() { # overlay image digest
  local f="$1/kustomization.yaml" tmp; tmp="$(mktemp)"
  awk -v img="ghcr.io/dapravith/$2" -v d="$3" '
    $0 ~ "- name: " img "$" { print; found = 1; next }
    found && /digest:/ { sub(/sha256:[0-9a-f]+/, d); found = 0 }
    { print }' "$f" > "$tmp"
  mv "$tmp" "$f"
}

valid() { [[ "$1" =~ ^sha256:[0-9a-f]{64}$ ]] && [[ "$1" != sha256:0000000000000000000000000000000000000000000000000000000000000000 ]]; }

if [ "${1:-}" = --release ]; then
  manifest="$2" to="$3"
  [ "$to" = dev ] || { echo "a release goes to dev first" >&2; exit 2; }
  get() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]]["digest"])' "$manifest" "$1"; }
else
  from="${1:?from}" to="${2:?to}"
  case "$from>$to" in dev\>staging|staging\>production) ;; *)
    echo "allowed promotions: dev -> staging, staging -> production" >&2; exit 2 ;; esac
  get() { digest_of "$from" "$1"; }
fi

for img in "${images[@]}"; do
  d="$(get "$img")"
  valid "$d" || { echo "refusing: $img has no released digest ($d)" >&2; exit 1; }
  old="$(digest_of "$to" "$img")"
  set_digest "$to" "$img" "$d"
  [ "$old" = "$d" ] && echo "$to  $img  unchanged" || echo "$to  $img  ${old:7:12} -> ${d:7:12}"
done
