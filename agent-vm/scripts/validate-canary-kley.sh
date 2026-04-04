#!/usr/bin/env bash

set -euo pipefail

CANARY_HOST="${CANARY_HOST:-saga-dev2}"
AGENT_USER="${AGENT_USER:-agent}"
REMOTE_TARGET="${AGENT_USER}@${CANARY_HOST}"
REMOTE_KLEY_REPO_ROOT="${REMOTE_KLEY_REPO_ROOT:-/home/${AGENT_USER}/kley}"
KLEY_WEB_BIND="${KLEY_WEB_BIND:-127.0.0.1:3210}"
KLEY_WEB_HEALTH_URL="http://${KLEY_WEB_BIND}/healthz"
KLEY_WEB_ROOT_URL="http://${KLEY_WEB_BIND}/"
KLEY_WEB_LOG_PATH="${KLEY_WEB_LOG_PATH:-/tmp/kley-canary-web.log}"

if [[ "${1:-}" == "--help" ]]; then
  cat <<EOF
Usage: agent-vm/scripts/validate-canary-kley.sh

Runs the post-apply canary kley smoke lane on ${REMOTE_TARGET}.

Environment overrides:
  CANARY_HOST            Canary host name (default: saga-dev2)
  AGENT_USER             SSH user (default: agent)
  REMOTE_KLEY_REPO_ROOT  Remote checkout path (default: /home/agent/kley)
  KLEY_WEB_BIND          Web bind address for the smoke run (default: 127.0.0.1:3210)
  KLEY_WEB_LOG_PATH      Remote log path for the temporary web process
EOF
  exit 0
fi

echo "[1/4] Verifying remote checkout on ${REMOTE_TARGET}:${REMOTE_KLEY_REPO_ROOT}"
ssh "${REMOTE_TARGET}" -- sh -c 'test -d "$1" && test -x "$1/preflight.sh" && test -x "$1/kley-run.sh" && git -C "$1" rev-parse --show-toplevel' _ "${REMOTE_KLEY_REPO_ROOT}"

echo "[2/4] Running terminal smoke via ./preflight.sh and ./kley-run.sh chat --help"
ssh "${REMOTE_TARGET}" -- sh -c 'cd "$1" && ./preflight.sh && ./kley-run.sh chat --help' _ "${REMOTE_KLEY_REPO_ROOT}"

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
