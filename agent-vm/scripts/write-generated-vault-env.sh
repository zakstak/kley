#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${KLEY_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd -P)}"
OUTPUT_PATH="${ROOT_DIR}/agent-vm/.generated/vault-environment.json"

usage() {
  cat <<'EOF'
Usage: agent-vm/scripts/write-generated-vault-env.sh

Reads VAULT_ADDR from the current shell environment and writes it to
agent-vm/.generated/vault-environment.json for local-only agent VM builds. The
generated file is gitignored. VAULT_TOKEN is intentionally not propagated to
deployed hosts.
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

: "${VAULT_ADDR:?VAULT_ADDR must be set in the current shell}"

mkdir -p "$(dirname "${OUTPUT_PATH}")"

python3 - <<'PY' >"${OUTPUT_PATH}"
import json
import os

print(json.dumps({
    "VAULT_ADDR": os.environ["VAULT_ADDR"],
}, indent=2, sort_keys=True))
PY

chmod 600 "${OUTPUT_PATH}"
printf 'Wrote %s\n' "${OUTPUT_PATH}"
