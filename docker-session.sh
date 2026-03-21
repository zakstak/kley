#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="${KLEY_DOCKER_SERVICE:-kley}"

if [ "$#" -eq 0 ]; then
  set -- chat
fi

printf 'Rebuilding %s image before starting a new session...\n' "$SERVICE_NAME"
exec docker compose run --rm --build "$SERVICE_NAME" "$@"
