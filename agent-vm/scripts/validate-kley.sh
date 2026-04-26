#!/usr/bin/env bash

set -euo pipefail

TARGET_HOST="${TARGET_HOST:-saga-runtime}"
AGENT_USER="${AGENT_USER:-agent}"
REMOTE_TARGET="${REMOTE_TARGET:-${AGENT_USER}@${TARGET_HOST}}"
KLEY_WEB_BIND_OPENCODE="${KLEY_WEB_BIND_OPENCODE:-127.0.0.1:3210}"
KLEY_WEB_BIND_HERMES="${KLEY_WEB_BIND_HERMES:-127.0.0.1:3211}"

case "${TARGET_HOST}" in
  saga-dev)
    ssh "${REMOTE_TARGET}" "command -v kley-opencode && command -v kley-hermes && command -v pi && command -v codex && codex --version >/dev/null 2>&1"
    ssh "${REMOTE_TARGET}" "test -d /var/lib/kley/opencode && test -d /var/lib/kley/hermes"
    ssh "${REMOTE_TARGET}" "systemctl is-active kley-web-opencode.service >/dev/null && systemctl is-active kley-web-hermes.service >/dev/null && systemctl is-active nginx.service >/dev/null"
    # shellcheck disable=SC2029
    ssh "${REMOTE_TARGET}" "python3 - <<'PY'
import json
import urllib.request

for bind in ('${KLEY_WEB_BIND_OPENCODE}', '${KLEY_WEB_BIND_HERMES}'):
    with urllib.request.urlopen(f'http://{bind}/healthz') as resp:
        body = resp.read().decode().strip()
    if body != 'ok':
        raise SystemExit(f'healthz for {bind} returned {body!r}')

req = urllib.request.Request('http://127.0.0.1/', headers={'Host': '${TARGET_HOST}'})
with urllib.request.urlopen(req) as resp:
    body = resp.read().decode()
if 'Kley web' not in body:
    raise SystemExit('nginx root response missing Kley web marker')

with open('/etc/kley-agent-vm-build.json', 'r', encoding='utf-8') as handle:
    metadata = json.load(handle)
assert metadata['hostName'] == '${TARGET_HOST}'
assert sorted(metadata['harnesses']) == ['hermes', 'opencode']
assert metadata['publicRuntime'] == 'opencode'
PY"
    ssh "${REMOTE_TARGET}" "command -v pi >/dev/null 2>&1"
    ;;
  saga-runtime)
    ssh "${REMOTE_TARGET}" "bash -lc 'command -v pi >/dev/null 2>&1 && command -v codex >/dev/null 2>&1 && codex --version >/dev/null 2>&1 && command -v opencode >/dev/null 2>&1 && opencode --version >/dev/null 2>&1 && command -v hermes >/dev/null 2>&1 && hermes --help >/dev/null 2>&1'"
    ssh "${REMOTE_TARGET}" "bash -lc '! command -v kley-opencode >/dev/null 2>&1 && ! command -v kley-hermes >/dev/null 2>&1'"
    ssh "${REMOTE_TARGET}" "test ! -d /var/lib/kley/opencode && test ! -d /var/lib/kley/hermes"
    # shellcheck disable=SC2029
    ssh "${REMOTE_TARGET}" "python3 - <<'PY'
import json

with open('/etc/kley-agent-vm-build.json', 'r', encoding='utf-8') as handle:
    metadata = json.load(handle)
assert metadata['hostName'] == '${TARGET_HOST}'
assert metadata['harnesses'] == []
assert metadata['publicRuntime'] is None
PY"
    ;;
  *)
    printf 'Unsupported target: %s\n' "${TARGET_HOST}" >&2
    exit 1
    ;;
esac
