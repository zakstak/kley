#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="${KLEY_DOCKER_SERVICE:-kley}"
NO_CACHE_BUILD="${KLEY_DOCKER_NO_CACHE:-0}"
FULL_IMAGE_REBUILD="${KLEY_DOCKER_FULL_REBUILD:-0}"
PROJECT_NAME="${COMPOSE_PROJECT_NAME:-$(basename "$(pwd)")}"
AUTH_VOLUME_NAME="${PROJECT_NAME}_kley-config"

if [ "$#" -gt 0 ]; then
  case "$1" in
    auth-reset)
      shift
      docker volume rm "$AUTH_VOLUME_NAME" >/dev/null 2>&1 || true
      printf 'Reset auth volume: %s\n' "$AUTH_VOLUME_NAME"
      exit 0
      ;;
  esac
fi

if [ "$#" -eq 0 ]; then
  set -- chat
fi

KLEY_PASSPHRASE_VALUE="${KLEY_PASSPHRASE-kley-dev-passphrase}"
BUILD_ARGS=()
IMAGE_ID="$(docker compose images -q "$SERVICE_NAME" 2>/dev/null || true)"

if [ "$NO_CACHE_BUILD" = "1" ]; then
  BUILD_ARGS+=(--no-cache)
fi

if [ -z "$IMAGE_ID" ] || [ "$FULL_IMAGE_REBUILD" = "1" ]; then
  printf 'Building %s image before starting a new session...\n' "$SERVICE_NAME"
  docker compose build "${BUILD_ARGS[@]}" "$SERVICE_NAME"
else
  printf 'Using existing %s image and rebuilding Rust binary only...\n' "$SERVICE_NAME"
fi

if [ -n "${KLEY_AGE_MAX_WORK_FACTOR:-}" ]; then
  exec docker compose run --rm \
    -e "KLEY_PASSPHRASE=${KLEY_PASSPHRASE_VALUE}" \
    -e "KLEY_RUST_ONLY_REBUILD=1" \
    -e "KLEY_WEB_AUTH_AUTO_RESET=${KLEY_WEB_AUTH_AUTO_RESET-1}" \
    -e "KLEY_AGE_MAX_WORK_FACTOR=${KLEY_AGE_MAX_WORK_FACTOR}" \
    "$SERVICE_NAME" "$@"
fi

exec docker compose run --rm \
  -e "KLEY_PASSPHRASE=${KLEY_PASSPHRASE_VALUE}" \
  -e "KLEY_RUST_ONLY_REBUILD=1" \
  -e "KLEY_WEB_AUTH_AUTO_RESET=${KLEY_WEB_AUTH_AUTO_RESET-1}" \
  "$SERVICE_NAME" "$@"
