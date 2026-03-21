#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="${KLEY_DOCKER_SERVICE:-kley}"

if [ "$#" -eq 0 ]; then
  set -- chat
fi

printf 'Rebuilding %s image before starting a new session...\n' "$SERVICE_NAME"

if [ "$1" = "./self-improve.sh" ]; then
  shift
  set -- self-improve.sh "$@"
fi

exec docker compose run --rm --build "$SERVICE_NAME" "$@"
