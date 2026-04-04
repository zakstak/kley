---
name: agent-vm-promotion
description:
  Use when deploying, validating, promoting, or recovering the repo-owned agent
  VMs (`saga-dev2` canary and `saga-dev` baseline). Covers the revision-guarded
  canary-first rollout, staged checkout validation, and baseline promotion flow.
---

## Use this skill when

- The user asks to deploy to `saga-dev2` or `saga-dev`
- The user asks to validate the canary agent VM
- The user asks to promote canary changes to baseline
- The user asks why agent-VM validation is failing
- The user asks to recover from a broken agent-VM rollout

## Canonical contract

- The source of truth is the local repo checkout plus its `flake.lock`
- `saga-dev2` is always the canary; `saga-dev` is always the baseline
- Apply canary first, validate from the same checkout, then promote baseline
- Validation must confirm the deployed host revision matches the local checkout
  before any smoke checks run
- Do not assume `/home/agent/kley` exists on the VM
- Do not assume an installed `kley` binary exists on the VM

## Default operator flow

### 1) Apply canary from the local checkout

```bash
agent-vm/scripts/apply-local-checkout-canary.sh
```

This builds `nixosConfigurations.saga-dev2`, exports the closure, imports it on
`agent@saga-dev2`, sets `/nix/var/nix/profiles/system`, and switches the system.

### 2) Validate canary from the same local checkout

```bash
agent-vm/scripts/validate-canary-kley.sh
```

What the validator does:

- compares the local checkout's recorded build revision against the deployed
  host's `/etc/kley-agent-vm-build.json`
- stages the local checkout to a temporary remote directory by default
- verifies the staged checkout has `kley-run.sh`
- runs terminal smoke with `./kley-run.sh chat --help`
- runs web smoke with `./kley-run.sh web --bind 127.0.0.1:3210`
- checks `/healthz == ok` and the root page contains `Kley web`
- removes the staged checkout on exit

### 3) Promote baseline only after canary passes

Build and apply `saga-dev` from the same checkout revision that passed canary.

Preferred explicit command path:

```bash
nix build /home/zack/git/kley#nixosConfigurations.saga-dev.config.system.build.toplevel
STORE_PATH=$(readlink -f /home/zack/git/kley/result)
nix-store --export $(nix-store -qR "$STORE_PATH") | ssh agent@saga-dev "sudo nix-store --import"
ssh agent@saga-dev "sudo nix-env -p /nix/var/nix/profiles/system --set $STORE_PATH && sudo $STORE_PATH/bin/switch-to-configuration switch"
ssh agent@saga-dev "readlink /nix/var/nix/profiles/system"
ssh agent@saga-dev "sudo nix-env --list-generations -p /nix/var/nix/profiles/system"
```

## Host alias vs flake host

If you connect through an SSH alias that is not the flake host name, split the
SSH target from the flake target:

```bash
CANARY_HOST=saga-agent FLAKE_HOST=saga-dev agent-vm/scripts/validate-canary-kley.sh
CANARY_HOST=saga-agent2 FLAKE_HOST=saga-dev2 agent-vm/scripts/validate-canary-kley.sh
```

- `CANARY_HOST` = SSH host / alias
- `FLAKE_HOST` = `nixosConfigurations.<name>` attr to evaluate locally

## Existing remote checkout override

If you intentionally want to validate a preexisting remote checkout instead of a
staged local copy:

```bash
REMOTE_KLEY_REPO_ROOT=/path/to/kley agent-vm/scripts/validate-canary-kley.sh
```

Useful overrides:

- `KLEY_REPO_ROOT=/path/to/local/kley`
- `CANARY_HOST=saga-dev2`
- `FLAKE_HOST=saga-dev2`
- `AGENT_USER=agent`
- `REMOTE_KLEY_STAGE_ROOT=/tmp/kley-canary-saga-dev2-12345`
- `KLEY_WEB_BIND=127.0.0.1:3210`

## Verification checklist

After apply/promotion, verify with real commands:

- `readlink /nix/var/nix/profiles/system`
- `sudo nix-env --list-generations -p /nix/var/nix/profiles/system`
- `cat /etc/kley-agent-vm-build.json`
- `ip -4 addr show dev eth0`
- `ip route`

For `saga-dev`, the expected static address is `10.0.0.50/24`. For `saga-dev2`,
the expected static address is `10.0.0.51/24`.

## Recovery

### Canary rollback

Use the repo-owned recovery path:

```bash
agent-vm/scripts/recover-canary-after-failed-update.sh
```

Then revert the bad source-of-truth change in the repo before retrying canary.

### Baseline recovery

There is no dedicated baseline rollback wrapper in this repo. If `saga-dev`
loses SSH after a switch but the VM is still running, recover from Proxmox/QEMU
guest-agent access:

1. inspect the current generation and guest network state
2. roll back `/nix/var/nix/profiles/system` to the previous generation
3. run `switch-to-configuration switch`
4. verify the static address returns before retrying

Do not keep promoting forward while baseline reachability is broken.

## Never

- Skip canary and deploy straight to `saga-dev`
- Validate against a different checkout than the one that was applied
- Assume `/home/agent/kley` exists unless you explicitly created it
- Use `./preflight.sh` as the canary smoke gate for staged checkout validation
  (it requires git remote and GitHub auth state that this lane does not
  provision)
- Promote baseline if revision checks or canary smoke fail

## Done checklist

- `saga-dev2` applied from the intended checkout
- validator passed from that same checkout
- `saga-dev` promoted only after canary success
- active generations verified on the target host(s)
- deployed `/etc/kley-agent-vm-build.json` matches the intended revision
