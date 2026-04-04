#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="${KLEY_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd -P)}"
CANARY_HOST="${CANARY_HOST:-saga-dev2}"
FLAKE_HOST="${FLAKE_HOST:-${CANARY_HOST}}"
AGENT_USER="${AGENT_USER:-agent}"
REMOTE_TARGET="${AGENT_USER}@${CANARY_HOST}"
REMOTE_KLEY_REPO_ROOT="${REMOTE_KLEY_REPO_ROOT:-}"
REMOTE_KLEY_STAGE_ROOT="${REMOTE_KLEY_STAGE_ROOT:-/tmp/kley-canary-${CANARY_HOST}-$$}"
KLEY_WEB_BIND="${KLEY_WEB_BIND:-127.0.0.1:3210}"
KLEY_WEB_HEALTH_URL="http://${KLEY_WEB_BIND}/healthz"
KLEY_WEB_ROOT_URL="http://${KLEY_WEB_BIND}/"
KLEY_WEB_LOG_PATH="${KLEY_WEB_LOG_PATH:-/tmp/kley-canary-web.log}"
STAGED_REMOTE_CHECKOUT=0

cleanup() {
  if [[ "${STAGED_REMOTE_CHECKOUT}" == "1" ]]; then
    ssh "${REMOTE_TARGET}" -- env REMOTE_KLEY_REPO_ROOT="${REMOTE_KLEY_REPO_ROOT}" bash -s >/dev/null 2>&1 <<'EOF' || true
set -euo pipefail
rm -rf "$REMOTE_KLEY_REPO_ROOT"
EOF
  fi
}

trap cleanup EXIT

LOCAL_BUILD_REVISION="$(nix eval --raw "${ROOT_DIR}#nixosConfigurations.${FLAKE_HOST}.config.environment.etc.\"kley-agent-vm-build.json\".text" | python3 -c 'import json,sys; print(json.load(sys.stdin)["source"]["exactRevision"])')"
REMOTE_BUILD_REVISION="$(ssh "${REMOTE_TARGET}" "cat /etc/kley-agent-vm-build.json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["source"]["exactRevision"])')"

if [[ "${1:-}" == "--help" ]]; then
  cat <<EOF
Usage: agent-vm/scripts/validate-canary-kley.sh

Runs the post-apply canary kley smoke lane on ${REMOTE_TARGET}.

Environment overrides:
  KLEY_REPO_ROOT          Local checkout to stage for validation (default: repo root)
  CANARY_HOST            Canary host name (default: saga-dev2)
  FLAKE_HOST             Flake host attr to validate against (default: CANARY_HOST)
  AGENT_USER             SSH user (default: agent)
  REMOTE_KLEY_REPO_ROOT  Existing remote checkout path (default: stage local checkout to a temp dir)
  REMOTE_KLEY_STAGE_ROOT Temp dir used when staging the local checkout remotely
  KLEY_WEB_BIND          Web bind address for the smoke run (default: 127.0.0.1:3210)
  KLEY_WEB_LOG_PATH      Remote log path for the temporary web process
EOF
  exit 0
fi

echo "[0/4] Verifying deployed build revision matches the local checkout for ${CANARY_HOST}"
if [[ "${LOCAL_BUILD_REVISION}" != "${REMOTE_BUILD_REVISION}" ]]; then
  printf 'Local build revision (%s) does not match remote deployed revision (%s) on %s\n' "${LOCAL_BUILD_REVISION}" "${REMOTE_BUILD_REVISION}" "${REMOTE_TARGET}" >&2
  exit 1
fi

if [[ -n "${REMOTE_KLEY_REPO_ROOT}" ]]; then
  echo "[1/4] Verifying existing remote checkout on ${REMOTE_TARGET}:${REMOTE_KLEY_REPO_ROOT}"
else
  REMOTE_KLEY_REPO_ROOT="${REMOTE_KLEY_STAGE_ROOT}"
  STAGED_REMOTE_CHECKOUT=1
  echo "[1/4] Staging local checkout from ${ROOT_DIR} to ${REMOTE_TARGET}:${REMOTE_KLEY_REPO_ROOT}"
  tar \
    --exclude=.git \
    --exclude=result \
    --exclude=target \
    --exclude=node_modules \
    --exclude=.direnv \
    --exclude=playwright-report \
    --exclude=test-results \
    -C "${ROOT_DIR}" -cf - . |
    ssh "${REMOTE_TARGET}" -- env REMOTE_KLEY_REPO_ROOT="${REMOTE_KLEY_REPO_ROOT}" bash -c 'set -euo pipefail; rm -rf "$REMOTE_KLEY_REPO_ROOT"; mkdir -p "$REMOTE_KLEY_REPO_ROOT"; tar -C "$REMOTE_KLEY_REPO_ROOT" -xf -'
fi

echo "[1/4] Verifying remote checkout on ${REMOTE_TARGET}:${REMOTE_KLEY_REPO_ROOT}"
ssh "${REMOTE_TARGET}" -- env REMOTE_KLEY_REPO_ROOT="${REMOTE_KLEY_REPO_ROOT}" bash -s <<'EOF'
set -euo pipefail
test -d "$REMOTE_KLEY_REPO_ROOT"
test -x "$REMOTE_KLEY_REPO_ROOT/preflight.sh"
test -x "$REMOTE_KLEY_REPO_ROOT/kley-run.sh"
EOF

echo "[2/4] Running terminal smoke via ./kley-run.sh chat --help"
ssh "${REMOTE_TARGET}" -- env REMOTE_KLEY_REPO_ROOT="${REMOTE_KLEY_REPO_ROOT}" bash -s <<'EOF'
set -euo pipefail
cd "$REMOTE_KLEY_REPO_ROOT"
./kley-run.sh chat --help
EOF

echo "[3/4] Running web smoke via ./kley-run.sh web --bind ${KLEY_WEB_BIND}"
ssh "${REMOTE_TARGET}" -- env \
  KLEY_REPO_ROOT="${REMOTE_KLEY_REPO_ROOT}" \
  KLEY_WEB_BIND="${KLEY_WEB_BIND}" \
  KLEY_WEB_HEALTH_URL="${KLEY_WEB_HEALTH_URL}" \
  KLEY_WEB_ROOT_URL="${KLEY_WEB_ROOT_URL}" \
  KLEY_WEB_LOG_PATH="${KLEY_WEB_LOG_PATH}" \
  bash -s <<'EOF'
set -euo pipefail

cd "$KLEY_REPO_ROOT"
rm -f "$KLEY_WEB_LOG_PATH"

./kley-run.sh web --bind "$KLEY_WEB_BIND" >"$KLEY_WEB_LOG_PATH" 2>&1 &
web_pid=$!

cleanup() {
  kill "$web_pid" 2>/dev/null || true
  wait "$web_pid" 2>/dev/null || true
}

trap cleanup EXIT

for _ in $(seq 1 20); do
  if curl -fsS "$KLEY_WEB_HEALTH_URL" >/dev/null; then
    break
  fi
  sleep 1
done

health_body="$(curl -fsS "$KLEY_WEB_HEALTH_URL")"
if [[ "$health_body" != "ok" ]]; then
  printf 'Unexpected /healthz response: %s\n' "$health_body" >&2
  exit 1
fi

root_body="$(curl -fsS "$KLEY_WEB_ROOT_URL")"
case "$root_body" in
  *"Kley web"*) ;;
  *)
    printf 'Root page missing expected marker: %s\n' "$KLEY_WEB_ROOT_URL" >&2
    exit 1
    ;;
esac

printf 'healthz=%s root=%s\n' "$health_body" "$KLEY_WEB_ROOT_URL"
EOF

echo "[4/4] Canary kley smoke succeeded on ${REMOTE_TARGET}; promote saga-dev only after this lane passes"
