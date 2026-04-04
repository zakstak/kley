#!/usr/bin/env bash

set -euo pipefail

CANARY_HOST="${CANARY_HOST:-saga-dev2}"
AGENT_USER="${AGENT_USER:-agent}"
REMOTE_TARGET="${AGENT_USER}@${CANARY_HOST}"
SYSTEM_PROFILE="/nix/var/nix/profiles/system"

if [[ "${1:-}" == "--help" ]]; then
  cat <<EOF
Usage: agent-vm/scripts/recover-canary-after-failed-update.sh

Recovers saga-dev2 after a bad canary update by rolling back the active NixOS
generation and switching the restored generation into runtime.

Command path executed on ${REMOTE_TARGET}:
  1) sudo nix-env --list-generations -p ${SYSTEM_PROFILE}
  2) sudo nix-env --rollback -p ${SYSTEM_PROFILE}
  3) sudo ${SYSTEM_PROFILE}/bin/switch-to-configuration switch

Use this only after the Task 6 apply + Task 7 validate lane reports a canary
failure. After runtime rollback, revert the bad source-of-truth repo change
(flake.lock or agent-vm/**), then rerun:
  agent-vm/scripts/apply-local-checkout-canary.sh
  agent-vm/scripts/validate-canary-kley.sh

Environment overrides:
  CANARY_HOST  Canary host name (default: saga-dev2)
  AGENT_USER   SSH user (default: agent)
EOF
  exit 0
fi

echo "[1/4] Listing system generations on ${REMOTE_TARGET}"
echo "      (the script auto-selects the highest generation below current as previous-good)"

ssh "${REMOTE_TARGET}" -- SYSTEM_PROFILE="${SYSTEM_PROFILE}" bash -s <<'EOF'
set -euo pipefail

mapfile -t generations < <(sudo nix-env --list-generations -p "$SYSTEM_PROFILE")

if [[ "${#generations[@]}" -eq 0 ]]; then
	printf 'No generations returned for %s\n' "$SYSTEM_PROFILE" >&2
	exit 1
fi

printf '%s\n' "${generations[@]}"

current_generation=""
for line in "${generations[@]}"; do
	generation_id="${line%% *}"
	if [[ "$line" == *"(current)"* ]]; then
		current_generation="$generation_id"
		break
	fi
done

if [[ -z "$current_generation" ]]; then
	printf 'Could not determine current generation from nix-env output\n' >&2
	exit 1
fi

previous_generation=""
for line in "${generations[@]}"; do
	generation_id="${line%% *}"
	if [[ "$generation_id" =~ ^[0-9]+$ ]] && (( generation_id < current_generation )); then
		if [[ -z "$previous_generation" ]] || (( generation_id > previous_generation )); then
			previous_generation="$generation_id"
		fi
	fi
done

if [[ -z "$previous_generation" ]]; then
	printf 'No previous generation available; rollback cannot continue\n' >&2
	exit 1
fi

printf 'Current generation: %s\n' "$current_generation"
printf 'Previous generation selected for rollback: %s\n' "$previous_generation"
EOF

echo "[2/4] Rolling back ${SYSTEM_PROFILE} on ${REMOTE_TARGET}"
ssh "${REMOTE_TARGET}" -- sudo nix-env --rollback -p "${SYSTEM_PROFILE}"

echo "[3/4] Switching runtime to the restored generation"
ssh "${REMOTE_TARGET}" -- sudo "${SYSTEM_PROFILE}/bin/switch-to-configuration" switch

echo "[4/4] Confirming active profile and generation history after rollback"
ssh "${REMOTE_TARGET}" -- readlink "${SYSTEM_PROFILE}"
ssh "${REMOTE_TARGET}" -- sudo nix-env --list-generations -p "${SYSTEM_PROFILE}"

echo "Canary runtime rollback completed on ${REMOTE_TARGET}"
