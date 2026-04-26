#!/usr/bin/env bash

set -euo pipefail

TARGET_HOST="${TARGET_HOST:-saga-runtime}"
AGENT_USER="${AGENT_USER:-agent}"
REMOTE_TARGET="${REMOTE_TARGET:-${AGENT_USER}@${TARGET_HOST}}"
SYSTEM_PROFILE="/nix/var/nix/profiles/system"

ssh "${REMOTE_TARGET}" -- sudo nix-env --rollback -p "${SYSTEM_PROFILE}"
ssh "${REMOTE_TARGET}" -- sudo "${SYSTEM_PROFILE}/bin/switch-to-configuration" switch
ssh "${REMOTE_TARGET}" -- readlink "${SYSTEM_PROFILE}"
ssh "${REMOTE_TARGET}" -- sudo nix-env --list-generations -p "${SYSTEM_PROFILE}"
