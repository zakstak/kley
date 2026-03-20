#!/usr/bin/env bash
set -euo pipefail

WORKSPACE_DIR="${WORKSPACE_DIR:-/workspace}"
HOST_UID="${LOCAL_UID:-}"
HOST_GID="${LOCAL_GID:-}"

fix_git_mount_ownership() {
  if [ -z "$HOST_UID" ] || [ -z "$HOST_GID" ]; then
    return 0
  fi

  if [ ! -d "$WORKSPACE_DIR/.git" ]; then
    return 0
  fi

  local git_paths=()
  local path
  for path in \
    "$WORKSPACE_DIR/.git/refs" \
    "$WORKSPACE_DIR/.git/logs/refs" \
    "$WORKSPACE_DIR/.git/packed-refs"
  do
    if [ -e "$path" ]; then
      git_paths+=("$path")
    fi
  done

  if [ "${#git_paths[@]}" -eq 0 ]; then
    return 0
  fi

  chown -R "$HOST_UID:$HOST_GID" "${git_paths[@]}" 2>/dev/null || true
}

fix_git_mount_ownership

if [ "$#" -eq 0 ]; then
  set -- chat
fi

kley "$@"
status=$?

fix_git_mount_ownership
exit "$status"
