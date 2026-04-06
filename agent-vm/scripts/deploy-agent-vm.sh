#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="${KLEY_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd -P)}"
TARGET_HOST="${1:-${TARGET_HOST:-}}"
CANARY_HOST="${CANARY_HOST:-saga-dev2}"
BASELINE_HOST="${BASELINE_HOST:-saga-dev}"
AGENT_USER="${AGENT_USER:-agent}"
VAULT_ENV_FILE="${ROOT_DIR}/agent-vm/.generated/vault-environment.json"
KLEY_WEB_BIND="${KLEY_WEB_BIND:-127.0.0.1:3210}"
KLEY_WEB_LOG_PATH="${KLEY_WEB_LOG_PATH:-/tmp/kley-canary-web.log}"
REMOTE_STAGE_ROOT="${REMOTE_STAGE_ROOT:-/tmp/kley-canary-${CANARY_HOST}-prestaged}"
PROXMOX_HOST="${PROXMOX_HOST:-saga-proxmox}"
PERISCOPE_CT_ID="${PERISCOPE_CT_ID:-100}"
PERISCOPE_API_URL="${PERISCOPE_API_URL:-http://10.0.0.1:9000/slots}"
BASELINE_SSH_TARGET="${BASELINE_SSH_TARGET:-agent@10.0.0.50}"
BASELINE_SSH_JUMP="${BASELINE_SSH_JUMP:-saga-proxmox}"

usage() {
  cat <<EOF
Usage: agent-vm/scripts/deploy-agent-vm.sh [saga-dev|saga-dev2]

Explicit target required. Use saga-dev for the normal rollout target.

Behavior:
  saga-dev   Apply canary, validate canary, promote baseline, verify both hosts
  saga-dev2  Apply canary, validate canary, verify canary, leave saga-dev cold

Environment overrides:
  KLEY_REPO_ROOT       Local checkout root (default: repo root)
  TARGET_HOST          Explicit deploy target if positional arg omitted
  CANARY_HOST          Canary host identity (default: saga-dev2)
  BASELINE_HOST        Baseline flake host identity (default: saga-dev)
  AGENT_USER           Agent SSH user for repo-native scripts (default: agent)
  KLEY_WEB_BIND        Web smoke bind address (default: 127.0.0.1:3210)
  KLEY_WEB_LOG_PATH    Remote log path for web smoke
  REMOTE_STAGE_ROOT    Remote path used for explicit canary prestaging
  PROXMOX_HOST         Proxmox SSH target used for Periscope verification
  PERISCOPE_CT_ID      Gateway CT id for Periscope service (default: 100)
  PERISCOPE_API_URL    Periscope slot API URL inside gateway CT
  BASELINE_SSH_TARGET  Baseline target for remote switch (default: agent@10.0.0.50)
  BASELINE_SSH_JUMP    SSH jump host for baseline target (default: saga-proxmox)
EOF
}

if [[ "${TARGET_HOST}" == "--help" || "${TARGET_HOST}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ -z "${TARGET_HOST}" ]]; then
  printf 'Missing deploy target. Choose saga-dev for a full rollout or saga-dev2 for an explicit canary-only run.\n' >&2
  usage >&2
  exit 1
fi

case "${TARGET_HOST}" in
  saga-dev2 | saga-dev) ;;
  *)
    printf 'Unsupported target: %s\n' "${TARGET_HOST}" >&2
    usage >&2
    exit 1
    ;;
esac

log() {
  printf '[deploy-agent-vm] %s\n' "$*"
}

load_generated_vault_env() {
  if [[ ! -f "${VAULT_ENV_FILE}" ]]; then
    return
  fi

  eval "$({
    VAULT_ENV_FILE="${VAULT_ENV_FILE}" python3 - <<'PY'
import json
import os
import pathlib
import shlex

path = pathlib.Path(os.environ["VAULT_ENV_FILE"])
data = json.loads(path.read_text())
for key in ("VAULT_ADDR", "VAULT_TOKEN"):
    value = data.get(key)
    if isinstance(value, str) and value:
        print(f'export {key}={shlex.quote(value)}')
PY
  })"
}

NIX_BUILD_ARGS=()
if [[ -f "${VAULT_ENV_FILE}" ]]; then
  load_generated_vault_env
  NIX_BUILD_ARGS+=(--impure)
fi

require_command() {
  local command_name="$1"
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    printf 'Required command not found: %s\n' "${command_name}" >&2
    exit 1
  fi
}

verify_periscope() {
  log "Verifying Periscope service on ${PROXMOX_HOST} (CT ${PERISCOPE_CT_ID})"
  ssh "${PROXMOX_HOST}" -- env \
    PERISCOPE_CT_ID="${PERISCOPE_CT_ID}" \
    PERISCOPE_API_URL="${PERISCOPE_API_URL}" \
    bash -s <<'EOF'
set -euo pipefail
pct exec "$PERISCOPE_CT_ID" -- systemctl is-active periscope >/dev/null
pct exec "$PERISCOPE_CT_ID" -- curl -fsS "$PERISCOPE_API_URL" >/dev/null
EOF
}

prestaged_cleanup() {
  ssh "${CANARY_HOST}" -- env \
    REMOTE_STAGE_ROOT="${REMOTE_STAGE_ROOT}" \
    KLEY_WEB_LOG_PATH="${KLEY_WEB_LOG_PATH}" \
    bash -s >/dev/null 2>&1 <<'EOF' || true
set -euo pipefail
rm -rf "$REMOTE_STAGE_ROOT" "$KLEY_WEB_LOG_PATH"
EOF
}

trap prestaged_cleanup EXIT

prestaged_validate_canary() {
  local canary_host="${CANARY_HOST}"

  log "Prestaging checkout on ${canary_host}:${REMOTE_STAGE_ROOT}"
  ssh "${canary_host}" -- env REMOTE_STAGE_ROOT="${REMOTE_STAGE_ROOT}" bash -s <<'EOF'
set -euo pipefail
rm -rf "$REMOTE_STAGE_ROOT"
mkdir -p "$REMOTE_STAGE_ROOT"
EOF
  rsync -a --delete \
    --exclude=.git \
    --exclude=result \
    --exclude=target \
    --exclude=node_modules \
    --exclude=.direnv \
    --exclude=playwright-report \
    --exclude=test-results \
    "${ROOT_DIR}/" "${canary_host}:${REMOTE_STAGE_ROOT}/"

  log "Running canary validator against explicit remote checkout"
  REMOTE_KLEY_REPO_ROOT="${REMOTE_STAGE_ROOT}" \
    KLEY_REPO_ROOT="${ROOT_DIR}" \
    CANARY_HOST="${canary_host}" \
    FLAKE_HOST="${canary_host}" \
    AGENT_USER="${AGENT_USER}" \
    KLEY_WEB_BIND="${KLEY_WEB_BIND}" \
    KLEY_WEB_LOG_PATH="${KLEY_WEB_LOG_PATH}" \
    "${ROOT_DIR}/agent-vm/scripts/validate-canary-kley.sh"
}

build_baseline() {
  log "Building baseline flake output for ${BASELINE_HOST}" >&2
  nix build "${NIX_BUILD_ARGS[@]}" "${ROOT_DIR}#nixosConfigurations.${BASELINE_HOST}.config.system.build.toplevel"
  readlink -f "${ROOT_DIR}/result"
}

promote_baseline() {
  local store_path="$1"

  log "Importing baseline closure on ${BASELINE_SSH_TARGET} via ${BASELINE_SSH_JUMP}"
  nix-store -qR "${store_path}" | xargs nix-store --export | ssh -J "${BASELINE_SSH_JUMP}" "${BASELINE_SSH_TARGET}" "sudo nix-store --import"

  log "Switching baseline system profile on ${BASELINE_SSH_TARGET}"
  ssh -J "${BASELINE_SSH_JUMP}" "${BASELINE_SSH_TARGET}" "sudo nix-env -p /nix/var/nix/profiles/system --set \"${store_path}\" && sudo \"${store_path}\"/bin/switch-to-configuration switch"
}

verify_host() {
  local label="$1"
  shift
  local ssh_command=("$@")

  log "Verifying ${label} host state"
  "${ssh_command[@]}" "command -v kley && kley --version && readlink /nix/var/nix/profiles/system && sudo nix-env --list-generations -p /nix/var/nix/profiles/system && cat /etc/kley-agent-vm-build.json && ip -4 addr show dev eth0 && ip route"
}

require_command nix
require_command nix-store
require_command rsync
require_command ssh

verify_periscope

log "Applying canary checkout to ${CANARY_HOST}"
KLEY_REPO_ROOT="${ROOT_DIR}" CANARY_HOST="${CANARY_HOST}" AGENT_USER="${AGENT_USER}" \
  "${ROOT_DIR}/agent-vm/scripts/apply-local-checkout-canary.sh"

prestaged_validate_canary

verify_host canary ssh "${CANARY_HOST}"

if [[ "${TARGET_HOST}" == "${CANARY_HOST}" ]]; then
  log "Target ${TARGET_HOST} requested; leaving ${BASELINE_HOST} cold"
  exit 0
fi

baseline_store_path="$(build_baseline)"
promote_baseline "${baseline_store_path}"

verify_host baseline ssh -J "${BASELINE_SSH_JUMP}" "${BASELINE_SSH_TARGET}"
log "Deployment complete for ${TARGET_HOST}"
