#!/usr/bin/env bash
set -euo pipefail

WORKSPACE_DIR="${WORKSPACE_DIR:-/workspace}"
HOST_UID="${LOCAL_UID:-}"
HOST_GID="${LOCAL_GID:-}"
IMAGE_SOURCE_STAMP="${IMAGE_SOURCE_STAMP:-/usr/local/bin/kley-image-source.sha256}"

detect_host_ids() {
  if [ -n "$HOST_UID" ] && [ -n "$HOST_GID" ]; then
    return 0
  fi

  if [ -e "$WORKSPACE_DIR" ]; then
    HOST_UID="${HOST_UID:-$(stat -c '%u' "$WORKSPACE_DIR" 2>/dev/null || true)}"
    HOST_GID="${HOST_GID:-$(stat -c '%g' "$WORKSPACE_DIR" 2>/dev/null || true)}"
  fi
}

setup_github_https_credentials() {
  if ! command -v gh >/dev/null 2>&1; then
    printf 'warning: gh not installed; HTTPS git fallback may fail\n' >&2
    return 0
  fi

  if ! gh auth status >/dev/null 2>&1; then
    printf 'warning: gh is not authenticated; HTTPS git fallback may fail\n' >&2
    return 0
  fi

  if ! gh auth setup-git >/dev/null 2>&1; then
    printf 'warning: failed to configure git credential helper with gh\n' >&2
  fi
}

compute_workspace_source_stamp() {
  if [ ! -f "$IMAGE_SOURCE_STAMP" ] || [ ! -d "$WORKSPACE_DIR" ]; then
    return 1
  fi

  if ! command -v git >/dev/null 2>&1 || ! command -v sha256sum >/dev/null 2>&1; then
    return 1
  fi

  local tracked_files=()
  local path

  while IFS= read -r path; do
    if [ -n "$path" ] && [ -f "$WORKSPACE_DIR/$path" ]; then
      tracked_files+=("$path")
    fi
  done < <(
    git -C "$WORKSPACE_DIR" ls-files --cached --others --exclude-standard -- \
      Cargo.toml Cargo.lock Dockerfile src templates assets 2>/dev/null | LC_ALL=C sort -u
  )

  if [ "${#tracked_files[@]}" -eq 0 ]; then
    return 1
  fi

  for path in "${tracked_files[@]}"; do
    sha256sum "$WORKSPACE_DIR/$path" | cut -d ' ' -f1
  done | sha256sum | cut -d ' ' -f1
}

warn_if_image_is_stale() {
  if [ ! -f "$IMAGE_SOURCE_STAMP" ]; then
    return 0
  fi

  local image_stamp
  local workspace_stamp

  image_stamp="$(tr -d '[:space:]' < "$IMAGE_SOURCE_STAMP" 2>/dev/null || true)"
  if [ -z "$image_stamp" ]; then
    return 0
  fi

  if ! workspace_stamp="$(compute_workspace_source_stamp)"; then
    return 0
  fi

  if [ "$workspace_stamp" = "$image_stamp" ]; then
    return 0
  fi

  printf 'warning: workspace source differs from the baked Docker image; rebuild before the next session to use the latest kley binary\n' >&2
  printf 'hint: start the next session with `./docker-session.sh` to rebuild first\n' >&2
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
    "$WORKSPACE_DIR/.git/COMMIT_EDITMSG" \
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
warn_if_image_is_stale

if [ "$#" -eq 0 ]; then
  set -- chat
fi

setup_github_https_credentials

if kley "$@"; then
  status=0
else
  status=$?
fi

fix_git_mount_ownership
exit "$status"
