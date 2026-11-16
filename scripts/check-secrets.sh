#!/usr/bin/env bash
# Credential exposure check (P0). Fails if likely secrets are committed or if
# server-side configuration leaks into the browser bundle.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0

# 1. Secret-looking strings in tracked/source files (excluding lockfiles, build output).
patterns='sk-ant-[A-Za-z0-9_-]{10,}|-----BEGIN [A-Z ]*PRIVATE KEY-----|postgres(ql)?://[^:@/[:space:]]+:[^@<>[:space:]]+@[^[:space:]]*supabase|eyJhbGciOi[A-Za-z0-9_-]{20,}'
if grep -rEn --exclude-dir={target,node_modules,dist,.git,.astro} --exclude='*.lock' --exclude='package-lock.json' \
     --exclude='check-secrets.sh' "$patterns" . ; then
  echo "FAIL: secret-like value found in source" >&2; fail=1
fi

# 2. .env files must never be committed.
if git rev-parse --git-dir >/dev/null 2>&1 && git ls-files | grep -E '(^|/)\.env($|\.)' | grep -v '\.env\.example$'; then
  echo "FAIL: .env file is tracked by git" >&2; fail=1
fi

# 3. The static dashboard must not contain server-side config names or values.
if [ -d frontend/dist ]; then
  if grep -rEl 'ANTHROPIC_API_KEY|DATABASE_URL|x-api-key|sk-ant-|postgres://' frontend/dist; then
    echo "FAIL: server-side configuration referenced in frontend bundle" >&2; fail=1
  fi
else
  echo "note: frontend/dist not built; skipping bundle check"
fi

[ "$fail" -eq 0 ] && echo "secret check: OK"
exit "$fail"
