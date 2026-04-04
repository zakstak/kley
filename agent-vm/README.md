# Agent VM module graph

`agent-vm/` is the shared NixOS baseline for repo-owned agent VMs.

- `modules/base.nix` defines the shared OS/runtime contract.
- `modules/opencode-harness.nix` defines the shared agent runtime layer and owns
  the developer-heavy package inventory.
- `modules/disko.nix` is the shared storage-contract slot.
- `modules/impermanence.nix` is the shared persistence-policy slot.
- `hosts/` stays thin and should only describe machine facts.

## Rolling update lane

Routine VM updates are lockfile-driven and canary-first:

1. Update the repo checkout by changing `flake.lock` and/or shared `agent-vm/**`
   baseline files.
2. Build and apply `saga-dev2` first.
3. Run canary validation against that same checkout revision.
4. Promote `saga-dev` only after the canary passes, using the same checkout and
   the same resolved inputs.

`modules/base.nix` writes `/etc/kley-agent-vm-build.json` on every host and sets
`system.configurationRevision` from the exact flake checkout revision resolved
at build time. That keeps a default `HEAD` workflow reproducible because the
build records the concrete revision that was actually applied.

## Promotion contract checks

Use the root flake for both hosts; do not introduce a second flake or host-local
override path.

```bash
REF=$(git -C /home/zack/git/kley rev-parse HEAD)
nix build /home/zack/git/kley#nixosConfigurations.saga-dev2.config.system.build.toplevel
STORE_PATH=$(readlink -f /home/zack/git/kley/result)
nix eval --raw /home/zack/git/kley#nixosConfigurations.saga-dev2.config.system.configurationRevision
nix eval --raw /home/zack/git/kley#nixosConfigurations.saga-dev2.config.environment.etc."kley-agent-vm-build.json".text
```

After the canary checks pass, promote from the same checkout:

```bash
nix build /home/zack/git/kley#nixosConfigurations.saga-dev.config.system.build.toplevel
nix eval --raw /home/zack/git/kley#nixosConfigurations.saga-dev.config.system.configurationRevision
```

The `configurationRevision` values for `saga-dev2` and `saga-dev` should match
for a promotion run; only the host name and `promotionLane` fields should differ
inside `kley-agent-vm-build.json`.

## Local-checkout canary apply workflow (`saga-dev2`)

Task 6 deploys directly from the local kley checkout; do not edit files on the
VM and do not use a second registry/source of truth.

The operator path is fixed to the repo-root build + `agent@saga-dev2` apply:

```bash
REPO_ROOT=/home/zack/git/kley
REF=$(git -C "$REPO_ROOT" rev-parse HEAD)
nix build "$REPO_ROOT#nixosConfigurations.saga-dev2.config.system.build.toplevel"
STORE_PATH=$(readlink -f "$REPO_ROOT/result")
nix-store --export $(nix-store -qR "$STORE_PATH") | ssh agent@saga-dev2 "sudo nix-store --import"
ssh agent@saga-dev2 "sudo nix-env -p /nix/var/nix/profiles/system --set $STORE_PATH && sudo $STORE_PATH/bin/switch-to-configuration switch"
ssh agent@saga-dev2 "readlink /nix/var/nix/profiles/system"
ssh agent@saga-dev2 "sudo nix-env --list-generations -p /nix/var/nix/profiles/system"
```

`readlink /nix/var/nix/profiles/system` and `--list-generations` are the source
for confirming which generation is active after switch.

For a non-interactive wrapper around the same primitives, run:

```bash
agent-vm/scripts/apply-local-checkout-canary.sh
```

Optional overrides (same declarative flow):

- `KLEY_REPO_ROOT=/path/to/checkout`
- `CANARY_HOST=saga-dev2`
- `AGENT_USER=agent`

## Push-known-changes canary validation lane (`saga-dev2`)

After the canary switch completes, validate kley on `saga-dev2` before any
promotion to `saga-dev`. The default operator path stays repo-first and reuses
the repo-native entrypoints already documented at the root:

1. Apply the local checkout to `saga-dev2`.
2. SSH to the canary and run the kley smoke lane from a repo checkout on that
   host.
3. Promote `saga-dev` only if both the terminal and web smoke checks pass.

The operator sequence is:

```bash
agent-vm/scripts/apply-local-checkout-canary.sh
agent-vm/scripts/validate-canary-kley.sh
```

`validate-canary-kley.sh` makes the required post-apply checks explicit so the
promotion gate does not depend on memory:

- verifies the remote checkout has repo-local entrypoints (`preflight.sh` and
  `kley-run.sh`)
- runs terminal smoke with `./preflight.sh` and `./kley-run.sh chat --help`
- runs web smoke with `./kley-run.sh web --bind 127.0.0.1:3210`
- probes `http://127.0.0.1:3210/healthz` for `ok` and `/` for the `Kley web`
  marker before allowing promotion

If the remote checkout lives somewhere other than `/home/agent/kley`, set the
path explicitly instead of improvising:

```bash
REMOTE_KLEY_REPO_ROOT=/path/to/kley agent-vm/scripts/validate-canary-kley.sh
```

Optional overrides for the same canary lane:

- `CANARY_HOST=saga-dev2`
- `AGENT_USER=agent`
- `REMOTE_KLEY_REPO_ROOT=/home/agent/kley`
- `KLEY_WEB_BIND=127.0.0.1:3210`

## Rollback and recovery flow for failed canary updates (`saga-dev2`)

If Task 6 apply or Task 7 canary validation fails after a switch, recover
`saga-dev2` first at runtime, then recover source-of-truth before retrying. Do
not guess generation numbers or rebuild repeatedly.

### A) Runtime rollback on `saga-dev2` (restore previous good generation)

Use the explicit rollback companion script for the same canary host lane:

```bash
agent-vm/scripts/recover-canary-after-failed-update.sh
```

That script always follows this command path on `agent@saga-dev2`:

```bash
sudo nix-env --list-generations -p /nix/var/nix/profiles/system
sudo nix-env --rollback -p /nix/var/nix/profiles/system
sudo /nix/var/nix/profiles/system/bin/switch-to-configuration switch
```

To remove guesswork, it parses `--list-generations` output, identifies the
`(current)` generation, then selects the highest generation lower than current
as the rollback target before executing `--rollback`.

### B) Source-of-truth reversion in repo state (before retrying canary)

Runtime rollback only fixes the running VM state. If the breakage was caused by
the promoted repo inputs (for example `flake.lock` or shared `agent-vm/**`
baseline changes), revert that change in the checkout before retrying canary.

Revert the bad committed change on a throwaway branch:

```bash
git switch -c task9-recovery-retry
git log --oneline -n 10 -- flake.lock agent-vm
git revert <bad-commit-sha>
```

Or if the bad change is still local/uncommitted:

```bash
git restore --staged --worktree flake.lock agent-vm
```

Then rerun the same canary-first lane (no alternate workflow):

```bash
agent-vm/scripts/apply-local-checkout-canary.sh
agent-vm/scripts/validate-canary-kley.sh
```

Only promote to `saga-dev` after this rerun passes on `saga-dev2`.
