#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
MANIFEST_PATH="$SCRIPT_DIR/Cargo.toml"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
CREDENTIALS_PATH="$CONFIG_HOME/kley/credentials.age"

run_kley() {
  if command -v cargo >/dev/null 2>&1 && [ -f "$MANIFEST_PATH" ]; then
    exec cargo run --quiet --manifest-path "$MANIFEST_PATH" --bin kley -- "$@"
  fi

  if command -v kley >/dev/null 2>&1; then
    exec kley "$@"
  fi

  echo "error: could not find 'cargo' with repo-local Cargo.toml or 'kley' in PATH" >&2
  exit 1
}

if [ "$#" -eq 0 ]; then
  set -- chat
fi

case "$1" in
  auth-reset)
    rm -f "$CREDENTIALS_PATH"
    printf 'Reset credentials file: %s\n' "$CREDENTIALS_PATH"
    exit 0
    ;;
esac

export KLEY_PASSPHRASE="${KLEY_PASSPHRASE:-kley-dev-passphrase}"

run_kley "$@"
