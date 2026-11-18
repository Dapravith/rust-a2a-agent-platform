#!/usr/bin/env bash
# Installs pinned, checksum-verified trivy and gitleaks (linux x86_64) into
# $1 (default ./.bin). Used by CI instead of third-party wrapper actions.
# To upgrade: change the version and the SHA-256 from the release's checksums file.
set -euo pipefail
dest="${1:-.bin}"
mkdir -p "$dest"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fetch() { # url sha256 binary
  curl -fsSL --retry 3 -o "$tmp/pkg.tar.gz" "$1"
  echo "$2  $tmp/pkg.tar.gz" | sha256sum -c -
  tar -xzf "$tmp/pkg.tar.gz" -C "$tmp" "$3"
  install -m 0755 "$tmp/$3" "$dest/$3"
  rm -f "$tmp/pkg.tar.gz" "$tmp/$3"
}

fetch https://github.com/aquasecurity/trivy/releases/download/v0.76.2/trivy_0.76.2_Linux-64bit.tar.gz \
  bc0fc726e017b8927a62b2d3531d7a84ee01769daf5c99ed866dced54089fdbf trivy
fetch https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_x64.tar.gz \
  551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb gitleaks

"$dest/trivy" --version | head -1
"$dest/gitleaks" version
