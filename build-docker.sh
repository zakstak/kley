#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="${KLEY_DOCKER_SERVICE:-kley}"

printf 'Building Docker image for compose service %s...\n' "$SERVICE_NAME"
docker compose build "$SERVICE_NAME"

printf '\nBuild complete. Run inside Docker only when needed:\n\n'
printf '    docker compose run --rm %s chat\n\n' "$SERVICE_NAME"
printf 'Default local workflow remains:\n\n'
printf '    ./kley-run.sh chat\n'
