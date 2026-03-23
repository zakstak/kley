#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
MANIFEST_PATH="$SCRIPT_DIR/Cargo.toml"

check_repo_path_ownership() {
  local path="$1"
  local expected_uid="$2"
  local expected_gid="$3"

  if [ ! -e "$path" ]; then
    return 0
  fi

  if [ ! -w "$path" ]; then
    echo "error: preflight requires writable repo path: $path" >&2
    echo "hint: rerun with ./docker-session.sh self-improve.sh so docker-entrypoint.sh can repair mount ownership, or restore the path to the workspace owner uid:gid $expected_uid:$expected_gid on the host" >&2
    return 1
  fi

  local mismatch
  mismatch=$(find "$path" \( ! -uid "$expected_uid" -o ! -gid "$expected_gid" \) -print -quit 2>/dev/null || true)
  if [ -n "$mismatch" ]; then
    echo "error: preflight requires repo ownership repair: $mismatch" >&2
    echo "hint: rerun with ./docker-session.sh self-improve.sh so docker-entrypoint.sh can repair mount ownership, or restore the path to the workspace owner uid:gid $expected_uid:$expected_gid on the host" >&2
    return 1
  fi
}

if [ -e "/.dockerenv" ]; then
  expected_uid="$(stat -c '%u' "$SCRIPT_DIR" 2>/dev/null || true)"
  expected_gid="$(stat -c '%g' "$SCRIPT_DIR" 2>/dev/null || true)"
  if [ -n "$expected_uid" ] && [ -n "$expected_gid" ]; then
    check_repo_path_ownership "$SCRIPT_DIR/.git/refs" "$expected_uid" "$expected_gid"
    check_repo_path_ownership "$SCRIPT_DIR/.git/worktrees" "$expected_uid" "$expected_gid"
    check_repo_path_ownership "$SCRIPT_DIR/.sisyphus/notepads" "$expected_uid" "$expected_gid"
  fi
fi

if command -v cargo >/dev/null 2>&1 && [ -f "$MANIFEST_PATH" ]; then
  exec cargo run --quiet --manifest-path "$MANIFEST_PATH" --bin kley -- preflight "$@"
fi

if command -v kley >/dev/null 2>&1; then
  exec kley preflight "$@"
fi

echo "error: could not find 'kley' in PATH and no repo-local Cargo manifest next to preflight.sh" >&2
exit 1
