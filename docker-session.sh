#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="${KLEY_DOCKER_SERVICE:-kley}"
REBUILD_AFTER_RUN=0
INTERRUPT_STATUS=0
ACTIVE_DOCKER_PID=

forward_signal() {
	local signal="$1"
	local status="$2"

	INTERRUPT_STATUS="$status"

	if [ -n "${ACTIVE_DOCKER_PID:-}" ]; then
		kill -s "$signal" "$ACTIVE_DOCKER_PID" 2>/dev/null || true
	fi
}

run_docker_child() {
	local status=0

	if [ "$INTERRUPT_STATUS" -ne 0 ]; then
		return "$INTERRUPT_STATUS"
	fi

	"$@" &
	ACTIVE_DOCKER_PID=$!

	while true; do
		if wait "$ACTIVE_DOCKER_PID"; then
			status=0
			break
		else
			status=$?

			if [ "$INTERRUPT_STATUS" -ne 0 ] && kill -0 "$ACTIVE_DOCKER_PID" 2>/dev/null; then
				continue
			fi

			break
		fi
	done

	ACTIVE_DOCKER_PID=

	if [ "$INTERRUPT_STATUS" -ne 0 ]; then
		return "$INTERRUPT_STATUS"
	fi

	return "$status"
}

compose_run_args() {
	local -a args=(docker compose run --rm --build)

	if [ -n "${KLEY_AGE_MAX_WORK_FACTOR:-}" ]; then
		args+=(-e "KLEY_AGE_MAX_WORK_FACTOR=${KLEY_AGE_MAX_WORK_FACTOR}")
	fi

	args+=("$SERVICE_NAME" "$@")
	printf '%s\0' "${args[@]}"
}

if [ "$#" -eq 0 ]; then
	set -- chat
fi

printf 'Rebuilding %s image before starting a new session...\n' "$SERVICE_NAME"

if [ "$1" = "./self-improve.sh" ] || [ "$1" = "/workspace/self-improve.sh" ]; then
	shift
	set -- self-improve.sh "$@"
	REBUILD_AFTER_RUN=1
elif [ "$1" = "self-improve.sh" ]; then
	REBUILD_AFTER_RUN=1
fi

if [ "$REBUILD_AFTER_RUN" -eq 0 ]; then
	mapfile -d '' -t docker_args < <(compose_run_args "$@")
	exec "${docker_args[@]}"
fi

trap 'forward_signal TERM 143' TERM
trap 'forward_signal INT 130' INT

run_status=0
mapfile -d '' -t docker_args < <(compose_run_args "$@")
if run_docker_child "${docker_args[@]}"; then
	:
else
	run_status=$?
fi

if [ "$INTERRUPT_STATUS" -ne 0 ]; then
	trap - TERM INT
	exit "$INTERRUPT_STATUS"
fi

build_status=0

if [ "$REBUILD_AFTER_RUN" -eq 1 ]; then
	printf 'Rebuilding %s image after self-improve to verify the resulting workspace...\n' "$SERVICE_NAME"
	if run_docker_child docker compose build "$SERVICE_NAME"; then
		:
	else
		build_status=$?
	fi
fi

trap - TERM INT

if [ "$INTERRUPT_STATUS" -ne 0 ]; then
	exit "$INTERRUPT_STATUS"
fi

if [ "$build_status" -ne 0 ]; then
	exit "$build_status"
fi

exit "$run_status"
