#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="${KLEY_DOCKER_SERVICE:-kley}"
REBUILD_AFTER_RUN=0
INTERRUPT_STATUS=0

forward_signal() {
  local signal="$1"
  local status="$2"

  INTERRUPT_STATUS="$status"

  if [ -n "${RUN_PID:-}" ]; then
    kill -s "$signal" "$RUN_PID" 2>/dev/null || true
  fi
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
  exec docker compose run --rm --build "$SERVICE_NAME" "$@"
fi

trap 'forward_signal TERM 143' TERM
trap 'forward_signal INT 130' INT

run_status=0
docker compose run --rm --build "$SERVICE_NAME" "$@" &
RUN_PID=$!

if wait "$RUN_PID"; then
  :
else
  run_status=$?
fi

trap - TERM INT

if [ "$INTERRUPT_STATUS" -ne 0 ]; then
  exit "$INTERRUPT_STATUS"
fi

if [ "$REBUILD_AFTER_RUN" -eq 1 ]; then
  printf 'Rebuilding %s image after self-improve to verify the resulting workspace...\n' "$SERVICE_NAME"
  if docker compose build "$SERVICE_NAME"; then
    build_status=0
  else
    build_status=$?
    exit "$build_status"
  fi
fi

exit "$run_status"
