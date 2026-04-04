#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
MANIFEST_PATH="$SCRIPT_DIR/Cargo.toml"

if command -v cargo >/dev/null 2>&1 && [ -f "$MANIFEST_PATH" ]; then
  exec cargo run --quiet --manifest-path "$MANIFEST_PATH" --bin kley -- preflight "$@"
fi

if command -v kley >/dev/null 2>&1; then
  exec kley preflight "$@"
fi

echo "error: could not find 'kley' in PATH and no repo-local Cargo manifest next to preflight.sh" >&2
exit 1
