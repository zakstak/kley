#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${KLEY_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd -P)}"
OUTPUT_PATH="${ROOT_DIR}/agent-vm/.generated/vault-environment.json"

usage() {
  cat <<'EOF'
Usage: agent-vm/scripts/write-generated-vault-env.sh

Reads VAULT_ADDR and VAULT_TOKEN from the current shell environment and writes
them to agent-vm/.generated/vault-environment.json for local-only agent VM
builds. The generated file is gitignored.
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

: "${VAULT_ADDR:?VAULT_ADDR must be set in the current shell}"
: "${VAULT_TOKEN:?VAULT_TOKEN must be set in the current shell}"

mkdir -p "$(dirname "${OUTPUT_PATH}")"

python3 - <<'PY' >"${OUTPUT_PATH}"
import json
import os

print(json.dumps({
    "VAULT_ADDR": os.environ["VAULT_ADDR"],
    "VAULT_TOKEN": os.environ["VAULT_TOKEN"],
}, indent=2, sort_keys=True))
PY

chmod 600 "${OUTPUT_PATH}"
printf 'Wrote %s\n' "${OUTPUT_PATH}"
