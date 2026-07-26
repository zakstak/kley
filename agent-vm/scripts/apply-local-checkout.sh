#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="${KLEY_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd -P)}"
TARGET_HOST="${TARGET_HOST:-saga-runtime}"
AGENT_USER="${AGENT_USER:-agent}"
REMOTE_TARGET="${REMOTE_TARGET:-${AGENT_USER}@${TARGET_HOST}}"
VAULT_ENV_FILE="${ROOT_DIR}/agent-vm/.generated/vault-environment.json"
OPERATOR_KEY_FILE="${ROOT_DIR}/agent-vm/.generated/operator-authorized-key.pub"

load_generated_vault_env() {
  if [[ ! -f "${VAULT_ENV_FILE}" ]]; then
    return
  fi

  eval "$({
    VAULT_ENV_FILE="${VAULT_ENV_FILE}" python3 - <<'PY'
import json, os, pathlib, shlex
path = pathlib.Path(os.environ["VAULT_ENV_FILE"])
data = json.loads(path.read_text())
for key in ("VAULT_ADDR",):
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

ssh "${REMOTE_TARGET}" 'sudo install -d -m 0700 /var/lib/kley && sudo tee /var/lib/kley/operator-authorized-key.pub >/dev/null && sudo chmod 600 /var/lib/kley/operator-authorized-key.pub' <"${OPERATOR_KEY_FILE}"
nix build "${NIX_BUILD_ARGS[@]}" "${ROOT_DIR}#nixosConfigurations.${TARGET_HOST}.config.system.build.toplevel"
STORE_PATH="$(readlink -f "${ROOT_DIR}/result")"
nix-store -qR "${STORE_PATH}" | xargs nix-store --export | ssh "${REMOTE_TARGET}" "sudo nix-store --import"
printf '%s\n' "${STORE_PATH}" | ssh "${REMOTE_TARGET}" 'read -r store_path && sudo nix-env -p /nix/var/nix/profiles/system --set "$store_path" && sudo "$store_path"/bin/switch-to-configuration switch'
ssh "${REMOTE_TARGET}" "readlink /nix/var/nix/profiles/system"
ssh "${REMOTE_TARGET}" "sudo nix-env --list-generations -p /nix/var/nix/profiles/system"
