#!/usr/bin/env bash
# Installs pinned, checksum-verified trivy, gitleaks, kubeconform and promtool
# (linux x86_64) into $1 (default ./.bin). Used by CI instead of third-party
# wrapper actions.
# To upgrade: change the version and the SHA-256 from the release's checksums file.
set -euo pipefail
dest="${1:-.bin}"
mkdir -p "$dest"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fetch() { # url sha256 path-in-archive [installed-name]
  curl -fsSL --retry 3 -o "$tmp/pkg.tar.gz" "$1"
  echo "$2  $tmp/pkg.tar.gz" | sha256sum -c -
  tar -xzf "$tmp/pkg.tar.gz" -C "$tmp" "$3"
  install -m 0755 "$tmp/$3" "$dest/${4:-$(basename "$3")}"
  rm -rf "$tmp/pkg.tar.gz" "${tmp:?}/${3%%/*}"
}

fetch https://github.com/aquasecurity/trivy/releases/download/v0.76.2/trivy_0.76.2_Linux-64bit.tar.gz \
  bc0fc726e017b8927a62b2d3531d7a84ee01769daf5c99ed866dced54089fdbf trivy
fetch https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_x64.tar.gz \
  551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb gitleaks

fetch https://github.com/yannh/kubeconform/releases/download/v0.8.0/kubeconform-linux-amd64.tar.gz \
  9bc2bffbf71f261128533edaf912153948b7ff238f9a531ae6d34466ec287883 kubeconform

fetch https://github.com/prometheus/prometheus/releases/download/v3.16.0/prometheus-3.16.0.linux-amd64.tar.gz \
  68727009472a2c02e1ff7f062fd5b3e637285bffc8fc2d340c76fb449d6e0f34 prometheus-3.16.0.linux-amd64/promtool

"$dest/trivy" --version | head -1
"$dest/gitleaks" version
"$dest/kubeconform" -v
"$dest/promtool" --version | head -1
