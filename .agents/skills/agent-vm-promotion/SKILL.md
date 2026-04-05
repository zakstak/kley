---
name: agent-vm-promotion
description:
  Use when deploying, validating, promoting, or recovering the repo-owned agent
  VMs (`saga-dev2` canary and `saga-dev` baseline). Covers the revision-guarded
  rollout, staged checkout validation, target-aware host selection, and baseline
  promotion flow.
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
- Target selection follows the user's request:
  - deploy `saga-dev2` → canary-only lane
  - deploy `saga-dev` → canary first, then baseline promotion
  - ambiguous deploy target → default to `saga-dev2` and leave `saga-dev` cold
- Any rollout that touches `saga-dev` must apply canary first, validate from the
  same checkout, then promote baseline
- Validation must confirm the deployed host revision matches the local checkout
  before any smoke checks run
- Do not assume `/home/agent/kley` exists on the VM
- A healthy deployed VM should expose `kley` directly in the login shell via
  `/run/current-system/sw/bin/kley`
- Canary validation still uses a staged checkout plus `./kley-run.sh`, because
  that lane proves the exact local checkout works end-to-end on the remote host
- Use `agent-vm/scripts/deploy-agent-vm.sh` as the canonical deploy entrypoint.
- Use Periscope for gateway/UI exposure checks and browser-facing passthrough.
- If Periscope access is unavailable, stop and report that as the blocker for
  browser/UI validation rather than silently falling back to SSH local port
  forwarding.

## Target selection rules

- If the user explicitly names `saga-dev2`, operate on `saga-dev2` only unless
  they also explicitly ask to promote baseline.
- If the user explicitly names `saga-dev`, run the full canary-first promotion
  lane: `saga-dev2` apply → `saga-dev2` validate → `saga-dev` promote.
- If the user says only "deploy" or gives an ambiguous environment name, default
  to `saga-dev2`.
- Keep `saga-dev` cold unless the user explicitly requests baseline deployment
  or promotion.
- When reporting progress, always state which target was selected and why.

## Wrapper + Periscope contract

- `agent-vm/scripts/deploy-agent-vm.sh` is the preferred operator entrypoint.
- Use the wrapper instead of manually reconstructing apply/validate/promote
  steps in the skill.
- For web validation or browser inspection, establish a Periscope UI
  passthrough/port-forward to the remote bind address instead of relying on a
  local SSH tunnel.
- Keep flake host selection separate from UI passthrough selection when needed:
  - wrapper target = deploy target chosen for the machine
  - flake target = `nixosConfigurations.saga-dev` or
    `nixosConfigurations.saga-dev2`
- If the operator environment requires a Periscope-specific wrapper or session
  bootstrap step for browser/UI access, do that before any validation that
  requires a browser.
- Do not document or teach SSH local-port-forward fallback paths in responses
  produced from this skill.

## Default operator flow

Preferred entrypoint for this skill:

```bash
agent-vm/scripts/deploy-agent-vm.sh [saga-dev2|saga-dev]
```

- No argument → defaults to `saga-dev2`
- `saga-dev2` → canary apply + validate + verify, leaves `saga-dev` cold
- `saga-dev` → canary apply + validate + baseline promotion + verify
- The wrapper also checks that the Periscope gateway service is reachable before
  continuing and works around the validator's broken auto-staging path by using
  an explicit pre-staged remote checkout.

### Deploy `saga-dev2` (canary-only)

```bash
agent-vm/scripts/deploy-agent-vm.sh saga-dev2
```

### Deploy `saga-dev` (explicit baseline request only)

```bash
agent-vm/scripts/deploy-agent-vm.sh saga-dev
```

The wrapper is the canonical entrypoint. The lower-level scripts below remain
useful for debugging or surgical recovery, but the skill should prefer the
wrapper unless there is a concrete reason not to.

### 1) Apply canary from the local checkout

```bash
agent-vm/scripts/apply-local-checkout-canary.sh
```

This builds `nixosConfigurations.saga-dev2`, exports the closure, imports it on
`saga-dev2`, sets `/nix/var/nix/profiles/system`, and switches the system.

Let the wrapper own the apply transport details for the import and switch steps.
Keep Periscope focused on gateway service checks and browser/UI passthrough.

### 2) Validate canary from the same local checkout

```bash
agent-vm/scripts/validate-canary-kley.sh
```

What the validator does:

- compares the local checkout's recorded build revision against the deployed
  host's `/etc/kley-agent-vm-build.json`
- stages the local checkout to a temporary remote directory by default
- verifies the staged checkout has `kley-run.sh`
- does not rely on the globally installed `kley` binary for the smoke gate
- runs terminal smoke with `./kley-run.sh chat --help`
- runs web smoke with `./kley-run.sh web --bind 127.0.0.1:3210`
- checks `/healthz == ok` and the root page contains `Kley web`
- removes the staged checkout on exit

Let the wrapper own remote staging for this validation lane. If the validator's
built-in staging path is incompatible with the operator environment, use its
documented `REMOTE_KLEY_REPO_ROOT` override through the wrapper's explicit
pre-staged checkout path.

For the web smoke lane, treat `KLEY_WEB_BIND` as a remote service endpoint that
must be exposed through a Periscope UI passthrough when browser access is
required. Do not use SSH `-L` port forwarding for `127.0.0.1:3210`.

### 3) Promote baseline only after canary passes

Build and apply `saga-dev` from the same checkout revision that passed canary.

Preferred explicit command path for the local build remains:

```bash
nix build /home/zack/git/kley#nixosConfigurations.saga-dev.config.system.build.toplevel
STORE_PATH=$(readlink -f /home/zack/git/kley/result)
```

Then let the wrapper perform the remote import, profile switch, and verification
steps on `saga-dev`.

## Transport target vs flake host

If your wrapper target name or Periscope passthrough target name is not the same
as the flake host name, split the operator target from the flake target:

```bash
TARGET_HOST=<periscope-target> FLAKE_HOST=saga-dev agent-vm/scripts/validate-canary-kley.sh
TARGET_HOST=<periscope-target> FLAKE_HOST=saga-dev2 agent-vm/scripts/validate-canary-kley.sh
```

- `TARGET_HOST` = wrapper/operator target for the deployment host
- UI passthrough target = Periscope port-forward/passthrough bound to the same
  deployed host context
- `FLAKE_HOST` = `nixosConfigurations.<name>` attr to evaluate locally

If a repo script only exposes `CANARY_HOST`, treat that as the host identity the
wrapper should operate on.

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

If browser verification is part of the task, also ensure the matching Periscope
UI passthrough is created for the chosen `KLEY_WEB_BIND` value.

When using this skill, prefer overrides that preserve the same local checkout
and same resolved flake revision across apply, validate, and promote.

## Verification checklist

After apply/promotion, verify with real commands:

- `command -v kley`
- `kley --version`
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
Run browser/UI validation through Periscope and use the repo-owned rollback path
for the actual canary recovery steps.

### Baseline recovery

There is no dedicated baseline rollback wrapper in this repo. If `saga-dev`
loses normal reachability after a switch but the VM is still running, recover
through the approved non-SSH control plane (for example the Periscope-connected
Proxmox/QEMU guest-agent path):

1. inspect the current generation and guest network state
2. roll back `/nix/var/nix/profiles/system` to the previous generation
3. run `switch-to-configuration switch`
4. verify the static address returns before retrying

Do not keep promoting forward while baseline reachability is broken.

## Never

- Skip canary and deploy straight to `saga-dev`
- Default an ambiguous deploy target to `saga-dev`; ambiguous requests default
  to `saga-dev2`
- Warm `saga-dev` without an explicit request for baseline deployment/promotion
- Validate against a different checkout than the one that was applied
- Assume `/home/agent/kley` exists unless you explicitly created it
- Treat a missing `kley` command after deployment as normal; that indicates the
  VM package layer drifted or the rollout is incomplete
- Bypass `agent-vm/scripts/deploy-agent-vm.sh` and manually reconstruct the
  rollout path unless debugging a specific failing step
- Use SSH local port forwarding instead of a Periscope UI passthrough for the
  web UI
- Use `./preflight.sh` as the canary smoke gate for staged checkout validation
  (it requires git remote and GitHub auth state that this lane does not
  provision)
- Promote baseline if revision checks or canary smoke fail

## Done checklist

- `saga-dev2` applied from the intended checkout
- validator passed from that same checkout
- requested target selection honored (`saga-dev2` only vs full `saga-dev` lane)
- `saga-dev` promoted only after canary success when baseline deployment was
  requested
- `saga-dev` left cold when baseline deployment was not explicitly requested
- `kley --version` works on the deployed host(s)
- active generations verified on the target host(s)
- deployed `/etc/kley-agent-vm-build.json` matches the intended revision
