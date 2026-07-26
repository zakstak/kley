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
manifest_entries = {line.strip() for line in manifest.read_text().splitlines() if line.strip()}
allowed_missing = {"kley", "origin", "upstream", "user.email", "user.name"}
missing = sorted(name for name in command_names if name not in manifest_entries and name not in allowed_missing)
if missing:
    print("preflight commands missing from manifest:", ", ".join(missing))
    sys.exit(1)
print("developer-heavy manifest covers the required preflight commands")
PY

echo "2. Verifying agent-vm docs markers"
python3 <<'PY'
import os
import pathlib

root = pathlib.Path(os.environ["ROOT_DIR"])
agent_vm_readme = root / "agent-vm" / "README.md"

if not agent_vm_readme.exists():
    raise SystemExit("agent-vm/README.md missing")

readme_text = agent_vm_readme.read_text()
removed_host = "saga" + "-dev"
required_markers = [
    "agent-vm/scripts/deploy-agent-vm.sh saga-runtime",
    "agent-vm/scripts/apply-local-checkout.sh",
    "agent-vm/scripts/validate-kley.sh",
    "agent-vm/scripts/recover-after-failed-update.sh",
    "does not provision `/var/lib/kley/opencode`",
    "@earendil-works/pi-coding-agent",
    "pi-coding-agent-npm-install",
    "default non-wrapper runtime host",
]
missing = [marker for marker in required_markers if marker not in readme_text]
if missing:
    raise SystemExit("agent-vm single-host docs missing markers: " + ", ".join(missing))
forbidden_markers = [
    removed_host,
    "kley-web-opencode",
    "isolated harness",
    "@mariozechner/pi-coding-agent",
]
present = [marker for marker in forbidden_markers if marker in readme_text]
if present:
    raise SystemExit("agent-vm runtime docs still mention removed markers: " + ", ".join(present))
print("agent-vm documentation markers are present")
PY

echo "3. Verifying top-level README pi contract markers"
python3 <<'PY'
import os
import pathlib

root = pathlib.Path(os.environ["ROOT_DIR"])
readme = root / "README.md"

if not readme.exists():
    raise SystemExit("README.md missing")

readme_text = readme.read_text()
removed_host = "saga" + "-dev"
required_markers = [
    "default Saga login target",
    "non-Nix-managed `pi`, `codex`, and `opencode` CLIs on PATH.",
  ]
missing = [marker for marker in required_markers if marker not in readme_text]
if missing:
    raise SystemExit("top-level README missing markers: " + ", ".join(missing))
forbidden_markers = [
    removed_host,
    "isolated `opencode` harness",
]
present = [marker for marker in forbidden_markers if marker in readme_text]
if present:
    raise SystemExit("top-level README still mentions removed markers: " + ", ".join(present))
print("top-level README pi contract markers are present")
PY

echo "4. Verifying agent-vm security markers"
python3 <<'PY'
import os
import pathlib

root = pathlib.Path(os.environ["ROOT_DIR"])
base_nix = root / "agent-vm" / "modules" / "base.nix"
vault_script = root / "agent-vm" / "scripts" / "write-generated-vault-env.sh"
apply_script = root / "agent-vm" / "scripts" / "apply-local-checkout.sh"

if not base_nix.exists() or not vault_script.exists() or not apply_script.exists():
    raise SystemExit("agent-vm security files missing")

base_text = base_nix.read_text()
vault_text = vault_script.read_text()
apply_text = apply_script.read_text()

if "VAULT_TOKEN = builtins.getEnv" in base_text:
    raise SystemExit("base.nix should not export VAULT_TOKEN into the deployed host environment")
if "ssh-keyscan" in base_text:
    raise SystemExit("base.nix should not trust github.com via live ssh-keyscan")
for marker in [
    "github.com ssh-ed25519 ",
    "github.com ecdsa-sha2-nistp256 ",
    "github.com ssh-rsa ",
]:
    if marker not in base_text:
        raise SystemExit(f"base.nix missing pinned GitHub host key marker: {marker}")
if '"VAULT_TOKEN": os.environ["VAULT_TOKEN"]' in vault_text or "VAULT_TOKEN must be set" in vault_text:
    raise SystemExit("write-generated-vault-env.sh should not persist or require VAULT_TOKEN")
if 'for key in ("VAULT_ADDR", "VAULT_TOKEN")' in apply_text:
    raise SystemExit("apply-local-checkout.sh should not load VAULT_TOKEN into the build environment")

print("agent-vm security markers are present")
PY

echo "5. Verifying agent shell experience markers"
python3 <<'PY'
import os
import pathlib

root = pathlib.Path(os.environ["ROOT_DIR"])
base_nix = root / "agent-vm" / "modules" / "base.nix"
profile_nix = root / "agent-vm" / "profiles" / "developer-heavy.nix"

if not base_nix.exists() or not profile_nix.exists():
    raise SystemExit("agent shell baseline files missing")

base_text = base_nix.read_text()
profile_text = profile_nix.read_text()

for marker in [
    "users.users.agent = {",
    "shell = pkgs.zsh;",
    "programs.zsh.enable = true;",
    "system.activationScripts.kleyShellExperience.text",
    '"pi-coding-agent-npm-install"',
    '@earendil-works/pi-coding-agent',
    'NPM_CONFIG_PREFIX',
    '.zshrc',
    'starship.toml',
    'share/fzf/key-bindings.zsh',
    'share/zsh-autosuggestions/zsh-autosuggestions.zsh',
    'share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh',
]:
    if marker not in base_text:
        raise SystemExit(f"base.nix missing shell experience marker: {marker}")

for marker in [
    "pkgs.zsh",
    "pkgs.starship",
    "pkgs.fzf",
    "pkgs.oh-my-zsh",
]:
    if marker not in profile_text:
        raise SystemExit(f"developer-heavy.nix missing shell package marker: {marker}")

print("agent shell experience markers are present")
PY

if [ -n "${out:-}" ]; then
  mkdir -p "$out"
  touch "$out/.vm-baseline-check"
fi
