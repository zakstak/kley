#!/usr/bin/env bash

set -euo pipefail

TARGET_HOST="${TARGET_HOST:-saga-runtime}"
AGENT_USER="${AGENT_USER:-agent}"
REMOTE_TARGET="${REMOTE_TARGET:-${AGENT_USER}@${TARGET_HOST}}"

case "${TARGET_HOST}" in
  saga-runtime)
    ssh "${REMOTE_TARGET}" "bash -lc 'command -v pi >/dev/null 2>&1 && command -v codex >/dev/null 2>&1 && codex --version >/dev/null 2>&1 && command -v opencode >/dev/null 2>&1 && opencode --version >/dev/null 2>&1 && ! command -v hermes >/dev/null 2>&1'"
    ssh "${REMOTE_TARGET}" "bash -lc 'pi --version >/dev/null 2>&1 && NPM_CONFIG_PREFIX=/home/agent/.npm-global npm list -g @earendil-works/pi-coding-agent --depth=0 >/dev/null 2>&1'"
    # shellcheck disable=SC2029
    ssh "${REMOTE_TARGET}" bash <<'EOF'
NPM_CONFIG_PREFIX=/home/agent/.npm-global npm list -g @openai/codex --depth=0 >/dev/null 2>&1
codex_path=$(readlink -f "$(command -v codex)")
opencode_path=$(readlink -f "$(command -v opencode)")
case "$codex_path" in
  /home/agent/.npm-global/*) ;;
  *) exit 1 ;;
esac
case "$opencode_path" in
  /home/agent/.opencode/bin/*) ;;
  *) exit 1 ;;
esac
test -x /home/agent/.opencode/bin/opencode
systemctl is-enabled pi-coding-agent-npm-install.service >/dev/null 2>&1
systemctl is-enabled runtime-cli-tools-install.service >/dev/null 2>&1
test "$(systemctl show -p Result --value pi-coding-agent-npm-install.service)" = success
test "$(systemctl show -p Result --value runtime-cli-tools-install.service)" = success
EOF
    ssh "${REMOTE_TARGET}" "bash -lc '! command -v kley-opencode >/dev/null 2>&1 && ! command -v kley-hermes >/dev/null 2>&1'"
    ssh "${REMOTE_TARGET}" "test ! -d /var/lib/kley/opencode"
    ssh "${REMOTE_TARGET}" "test ! -e /usr/local/bin/hermes && test ! -e /home/agent/.local/bin/hermes && test ! -d /usr/local/lib/hermes-agent"
    ssh "${REMOTE_TARGET}" "bash -lc 'command -v tmux >/dev/null 2>&1 && systemctl is-enabled tailscaled.service >/dev/null 2>&1 && systemctl is-active tailscaled.service >/dev/null 2>&1'"
    # shellcheck disable=SC2029
    ssh "${REMOTE_TARGET}" "python3 - <<'PY'
import json
import pathlib
import subprocess

tmux_conf = pathlib.Path('/home/agent/.tmux.conf')
if tmux_conf.read_text(encoding='utf-8') != 'set -g extended-keys on\n':
    raise SystemExit('/home/agent/.tmux.conf does not match the expected tmux contract')

tailscale_status = json.loads(subprocess.check_output(['tailscale', 'status', '--json'], text=True))
backend_state = tailscale_status.get('BackendState')
if backend_state != 'Running':
    raise SystemExit(f'tailscale BackendState is {backend_state!r}, expected Running')

if pathlib.Path('/var/lib/kley/tailscale-auth-key').exists():
    raise SystemExit('staged tailscale auth key still exists after bootstrap')
PY"
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
