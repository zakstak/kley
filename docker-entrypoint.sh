#!/usr/bin/env bash
set -euo pipefail

WORKSPACE_DIR="${WORKSPACE_DIR:-/workspace}"
HOST_UID="${LOCAL_UID:-}"
HOST_GID="${LOCAL_GID:-}"

detect_host_ids() {
  if [ -n "$HOST_UID" ] && [ -n "$HOST_GID" ]; then
    return 0
  fi

  if [ -e "$WORKSPACE_DIR" ]; then
    HOST_UID="${HOST_UID:-$(stat -c '%u' "$WORKSPACE_DIR" 2>/dev/null || true)}"
    HOST_GID="${HOST_GID:-$(stat -c '%g' "$WORKSPACE_DIR" 2>/dev/null || true)}"
  fi
}

fix_git_mount_ownership() {
  detect_host_ids

  if [ -z "$HOST_UID" ] || [ -z "$HOST_GID" ]; then
    return 0
  fi

  if [ ! -d "$WORKSPACE_DIR/.git" ]; then
    return 0
  fi

  local git_paths=()
  local path
  for path in \
    "$WORKSPACE_DIR/.git/FETCH_HEAD" \
    "$WORKSPACE_DIR/.git/ORIG_HEAD" \
    "$WORKSPACE_DIR/.git/HEAD" \
    "$WORKSPACE_DIR/.git/index" \
    "$WORKSPACE_DIR/.git/logs" \
    "$WORKSPACE_DIR/.git/refs" \
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

if kley "$@"; then
  status=0
else
  status=$?
fi

fix_git_mount_ownership
exit "$status"
