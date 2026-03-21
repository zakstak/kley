#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="${KLEY_DOCKER_SERVICE:-kley}"

printf 'Building Docker image for compose service %s...\n' "$SERVICE_NAME"
docker compose build "$SERVICE_NAME"

printf '\nBuild complete. Start a fresh rebuilt session with:\n\n'
printf '    ./docker-session.sh\n\n'
printf 'Or pass a specific command to the container:\n\n'
printf '    ./docker-session.sh web --bind 127.0.0.1:8080\n'
