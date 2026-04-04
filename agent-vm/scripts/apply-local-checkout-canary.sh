#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="${KLEY_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd -P)}"
CANARY_HOST="${CANARY_HOST:-saga-dev2}"
AGENT_USER="${AGENT_USER:-agent}"
REMOTE_TARGET="${AGENT_USER}@${CANARY_HOST}"
FLAKE_ATTR="nixosConfigurations.${CANARY_HOST}.config.system.build.toplevel"

echo "[1/5] Building ${FLAKE_ATTR} from ${ROOT_DIR}"
nix build "${ROOT_DIR}#${FLAKE_ATTR}"

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
