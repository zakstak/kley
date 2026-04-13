#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="${KLEY_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd -P)}"
CANARY_HOST="${CANARY_HOST:-saga-dev2}"
AGENT_USER="${AGENT_USER:-agent}"
REMOTE_TARGET="${AGENT_USER}@${CANARY_HOST}"
FLAKE_ATTR="nixosConfigurations.${CANARY_HOST}.config.system.build.toplevel"
VAULT_ENV_FILE="${ROOT_DIR}/agent-vm/.generated/vault-environment.json"
OPERATOR_KEY_FILE="${ROOT_DIR}/agent-vm/.generated/operator-authorized-key.pub"

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
ensure_impure() {
  local arg
  for arg in "${NIX_BUILD_ARGS[@]:-}"; do
    if [[ "${arg}" == "--impure" ]]; then
      return
    fi
  done
  NIX_BUILD_ARGS+=(--impure)
}

if [[ -f "${VAULT_ENV_FILE}" ]]; then
  load_generated_vault_env
  ensure_impure
fi

if [[ ! -f "${OPERATOR_KEY_FILE}" ]]; then
  printf 'Missing operator key file: %s\n' "${OPERATOR_KEY_FILE}" >&2
  exit 1
fi

stage_operator_key() {
  cat "${OPERATOR_KEY_FILE}" | ssh "${REMOTE_TARGET}" "sudo install -d -m 0700 /var/lib/kley && sudo tee /var/lib/kley/operator-authorized-key.pub >/dev/null && sudo chmod 600 /var/lib/kley/operator-authorized-key.pub"
}

echo "[0/5] Staging operator SSH key on ${REMOTE_TARGET}"
stage_operator_key

echo "[1/5] Building ${FLAKE_ATTR} from ${ROOT_DIR}"
nix build "${NIX_BUILD_ARGS[@]}" "${ROOT_DIR}#${FLAKE_ATTR}"

STORE_PATH="$(readlink -f "${ROOT_DIR}/result")"

if [[ -z "${STORE_PATH}" ]]; then
  echo "Failed to resolve build output store path from ${ROOT_DIR}/result" >&2
  exit 1
fi

echo "[2/5] Exporting closure for ${STORE_PATH} and importing on ${REMOTE_TARGET}"
nix-store -qR "${STORE_PATH}" | xargs nix-store --export | ssh "${REMOTE_TARGET}" "sudo nix-store --import"

echo "[3/5] Setting system profile and switching generation on ${REMOTE_TARGET}"
printf '%s\n' "${STORE_PATH}" | ssh "${REMOTE_TARGET}" "read -r store_path && sudo nix-env -p /nix/var/nix/profiles/system --set \"\$store_path\" && sudo \"\$store_path\"/bin/switch-to-configuration switch"

echo "[4/5] Current system profile symlink on ${REMOTE_TARGET}"
ssh "${REMOTE_TARGET}" "readlink /nix/var/nix/profiles/system"

echo "[5/5] Current generation list on ${REMOTE_TARGET}"
ssh "${REMOTE_TARGET}" "sudo nix-env --list-generations -p /nix/var/nix/profiles/system"

echo "Canary apply complete for ${REMOTE_TARGET} using ${STORE_PATH}"
