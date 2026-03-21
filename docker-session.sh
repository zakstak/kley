#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="${KLEY_DOCKER_SERVICE:-kley}"
REBUILD_AFTER_RUN=0

if [ "$#" -eq 0 ]; then
  set -- chat
fi

printf 'Rebuilding %s image before starting a new session...\n' "$SERVICE_NAME"

if [ "$1" = "./self-improve.sh" ]; then
  shift
  set -- self-improve.sh "$@"
  REBUILD_AFTER_RUN=1
elif [ "$1" = "self-improve.sh" ]; then
  REBUILD_AFTER_RUN=1
fi

run_status=0
if docker compose run --rm --build "$SERVICE_NAME" "$@"; then
  :
else
  run_status=$?
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
