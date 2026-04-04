#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd -P)"

echo "1. Verifying developer-heavy manifest against preflight commands"
export ROOT_DIR
python3 <<'PY'
import os
import pathlib
import re
import sys

root = pathlib.Path(os.environ["ROOT_DIR"])
preflight = root / "src" / "preflight.rs"
manifest = root / "agent-vm" / "developer-heavy-tool-manifest.txt"

if not preflight.exists() or not manifest.exists():
    raise SystemExit("preflight source or manifest file missing")

command_pattern = re.compile(r'(?<![A-Za-z0-9_])command\((["\'])([^"\']+)\1\)')
command_names = {match.group(2) for match in command_pattern.finditer(preflight.read_text())}
manifest_entries = {
    line.strip() for line in manifest.read_text().splitlines() if line.strip()
}

allowed_missing = {
    "kley",
    "origin",
    "upstream",
    "user.email",
    "user.name",
}

missing = sorted(
    name for name in command_names if name not in manifest_entries and name not in allowed_missing
)

if missing:
    print("preflight commands missing from manifest:", ", ".join(missing))
    sys.exit(1)

print("developer-heavy manifest covers the required preflight commands")
PY

echo "2. Verifying canary rollback + source-of-truth recovery workflow is documented"
python3 <<'PY'
import os
import pathlib
import stat
import sys

root = pathlib.Path(os.environ["ROOT_DIR"])
agent_vm_readme = root / "agent-vm" / "README.md"
rollback_script = root / "agent-vm" / "scripts" / "recover-canary-after-failed-update.sh"

if not agent_vm_readme.exists():
    raise SystemExit("agent-vm/README.md missing")
if not rollback_script.exists():
    raise SystemExit("rollback recovery script missing")

mode = rollback_script.stat().st_mode
if not (mode & stat.S_IXUSR):
    raise SystemExit("rollback recovery script is not executable")

readme_text = agent_vm_readme.read_text()

required_markers = [
    "agent-vm/scripts/recover-canary-after-failed-update.sh",
    "sudo nix-env --list-generations -p /nix/var/nix/profiles/system",
    "sudo nix-env --rollback -p /nix/var/nix/profiles/system",
    "sudo /nix/var/nix/profiles/system/bin/switch-to-configuration switch",
    "git revert <bad-commit-sha>",
    "git restore --staged --worktree flake.lock agent-vm",
    "agent-vm/scripts/apply-local-checkout-canary.sh",
    "agent-vm/scripts/validate-canary-kley.sh",
]

missing = [marker for marker in required_markers if marker not in readme_text]
if missing:
    raise SystemExit("agent-vm rollback/recovery docs missing markers: " + ", ".join(missing))

print("rollback and source-of-truth recovery workflow markers are present")
PY

if [ -n "${out:-}" ]; then
  mkdir -p "$out"
  touch "$out/.vm-baseline-check"
fi
