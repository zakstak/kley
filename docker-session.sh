#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="${KLEY_DOCKER_SERVICE:-kley}"

if [ "$#" -eq 0 ]; then
  set -- chat
fi

printf 'Rebuilding %s image before starting a new session...\n' "$SERVICE_NAME"

if [ -n "${KLEY_AGE_MAX_WORK_FACTOR:-}" ]; then
  exec docker compose run --rm --build \
    -e "KLEY_AGE_MAX_WORK_FACTOR=${KLEY_AGE_MAX_WORK_FACTOR}" \
    "$SERVICE_NAME" "$@"
fi

exec docker compose run --rm --build "$SERVICE_NAME" "$@"
