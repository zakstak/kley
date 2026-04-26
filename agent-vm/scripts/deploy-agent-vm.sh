#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="${KLEY_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd -P)}"
TARGET_HOST="${1:-${TARGET_HOST:-saga-runtime}}"
AGENT_USER="${AGENT_USER:-agent}"
SSH_TARGET="${SSH_TARGET:-${AGENT_USER}@${TARGET_HOST}}"
KLEY_WEB_BIND_OPENCODE="${KLEY_WEB_BIND_OPENCODE:-127.0.0.1:3210}"
KLEY_WEB_BIND_HERMES="${KLEY_WEB_BIND_HERMES:-127.0.0.1:3211}"

usage() {
  cat <<EOF
Usage: agent-vm/scripts/deploy-agent-vm.sh [saga-runtime|saga-dev]

Build, apply, and validate a shared agent VM host.

Environment overrides:
  KLEY_REPO_ROOT         Local checkout root (default: repo root)
  TARGET_HOST            Explicit deploy target (default: saga-runtime)
  AGENT_USER             Agent SSH user (default: agent)
  SSH_TARGET             SSH target used for apply and verification (default: agent@<target-host>)
  KLEY_WEB_BIND_OPENCODE Opencode bind address for smoke checks (default: 127.0.0.1:3210)
  KLEY_WEB_BIND_HERMES   Hermes bind address for smoke checks (default: 127.0.0.1:3211)
EOF
}

if [[ "${TARGET_HOST}" == "--help" || "${TARGET_HOST}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ "${TARGET_HOST}" != "saga-dev" && "${TARGET_HOST}" != "saga-runtime" ]]; then
  printf 'Unsupported target: %s\n' "${TARGET_HOST}" >&2
  usage >&2
  exit 1
fi

log() {
  printf '[deploy-agent-vm] %s\n' "$*"
}

require_command() {
  local command_name="$1"
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    printf 'Required command not found: %s\n' "${command_name}" >&2
    exit 1
  fi
}

require_command nix
require_command nix-store
require_command ssh

log "Applying local checkout to ${TARGET_HOST}"
TARGET_HOST="${TARGET_HOST}" AGENT_USER="${AGENT_USER}" REMOTE_TARGET="${SSH_TARGET}" \
  "${ROOT_DIR}/agent-vm/scripts/apply-local-checkout.sh"

log "Running post-apply validation on ${TARGET_HOST}"
TARGET_HOST="${TARGET_HOST}" AGENT_USER="${AGENT_USER}" REMOTE_TARGET="${SSH_TARGET}" \
  KLEY_WEB_BIND_OPENCODE="${KLEY_WEB_BIND_OPENCODE}" KLEY_WEB_BIND_HERMES="${KLEY_WEB_BIND_HERMES}" \
  "${ROOT_DIR}/agent-vm/scripts/validate-kley.sh"

log "Deployment complete for ${TARGET_HOST}"
