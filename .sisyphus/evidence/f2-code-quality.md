# F2 Code Quality Review — agent-vm-baseline-update-lane

Verdict: APPROVE

Review date: 2026-04-03

## Scope reviewed

- Plan: `.sisyphus/plans/agent-vm-baseline-update-lane.md`
- Notepads: `.sisyphus/notepads/agent-vm-baseline-update-lane/learnings.md`,
  `.sisyphus/notepads/agent-vm-baseline-update-lane/issues.md`,
  `.sisyphus/notepads/agent-vm-baseline-update-lane/decisions.md`
- Root flake and docs: `flake.nix`, `README.md`
- Current VM implementation only: `agent-vm/default.nix`,
  `agent-vm/promotion-contract.nix`, `agent-vm/modules/*.nix`,
  `agent-vm/hosts/*.nix`, `agent-vm/profiles/developer-heavy.nix`,
  `agent-vm/README.md`, `agent-vm/scripts/*.sh`, `tests/vm-baseline-check.sh`
- Explicitly out of scope except for exclusion: unrelated `src/**` and
  `playwright/**` working-tree edits

## Summary

This rerun replaces the earlier stale rejection with a fresh review of the
current `agent-vm` tree after blocker remediation. The current implementation is
structurally coherent, keeps the root flake as the single source of truth, and
now clears the four previously blocking defects. The repo-local regression gate
required by Task 8 also passes end-to-end.

I am approving this code-quality review for the current repo-local `agent-vm`
implementation.

## Previously rejected blockers — recheck

### 1. Shared baseline no longer bakes the repo app package into every VM

- `agent-vm/modules/opencode-harness.nix:1-9` now only imports
  `../profiles/developer-heavy.nix` and assigns that shared package inventory to
  `environment.systemPackages`.
- The previous repo-local `kley` build coupling is gone; the shared VM baseline
  now provides tool inventory without embedding the mutable application checkout
  into every host image.
- Result: cleared.

### 2. Host-private drift is removed from tracked host overlays

- `agent-vm/hosts/saga-dev.nix:3-31` and `agent-vm/hosts/saga-dev2.nix:3-17` now
  stay focused on host-scoped machine facts plus `kley.agentVm.promotionLane`.
- The earlier private-IP literals, concrete authorized key material, and ad hoc
  `saga-generation-marker` drift are no longer present in the current host
  files.
- Shared access policy remains centralized in `agent-vm/modules/base.nix:48-59`,
  where the `agent` user and SSH key-only defaults are defined once for the
  module graph.
- Result: cleared.

### 3. Canary helper scripts are now tracked repo source

- Fresh `git status --short` during this review shows the helper scripts as
  tracked additions: `A  agent-vm/scripts/apply-local-checkout-canary.sh`,
  `A  agent-vm/scripts/recover-canary-after-failed-update.sh`, and
  `A  agent-vm/scripts/validate-canary-kley.sh`.
- `agent-vm/README.md:55-114` and `:128-174` document those same scripts as the
  canonical apply / validate / recovery lane, so the docs and tracked source now
  line up.
- Result: cleared.

### 4. Shared disk logic no longer hardcodes `/dev/sda`

- `agent-vm/modules/disko.nix:3-17` derives `diskDevice` from
  `config.boot.loader.grub.devices` and asserts that hosts must provide a
  device, which keeps the shared storage slot host-agnostic.
- `agent-vm/hosts/saga-dev.nix:10-12` and `agent-vm/hosts/saga-dev2.nix:8-13`
  still supply concrete disk facts per host, which is the correct boundary for
  machine-specific values.
- Result: cleared.

## Code quality assessment

### Structure and responsibilities

- `flake.nix:21-27` imports `agent-vm` once and `flake.nix:62-89` exposes both
  `checks.x86_64-linux` and the VM `nixosConfigurations` / `nixosModules`
  cleanly from the root flake.
- `agent-vm/default.nix:3-18` keeps host assembly small via `mkHost`, while
  `agent-vm/modules/default.nix:11-17` makes the shared import order explicit.
- `agent-vm/modules/base.nix:39-74` is the right place for the shared OS/runtime
  contract, promotion-lane assertion, and build metadata emission.
- `agent-vm/promotion-contract.nix:1-18` centralizes canary/baseline metadata
  rather than spreading update-lane logic across host files.

### Regression coverage

- `tests/vm-baseline-check.sh:7-89` verifies the developer-heavy manifest
  coverage and required rollback/source-of-truth recovery markers.
- `flake.nix:62-85` wires repo-local VM checks into `checks.x86_64-linux`, so
  `nix flake check` exercises the host-build and manifest gates before any
  remote switch step.
- The Task 8 commands passed in this rerun, which is the key repo-local
  regression requirement for F2.

## Non-blocking observations

### 1. Host overlays still differ in machine facts, which is acceptable

- `saga-dev` and `saga-dev2` intentionally diverge in host-scoped boot/disk
  details (`/dev/sda` vs `/dev/disk/by-id/virtio-0`, root labels, guest-specific
  toggles). That is consistent with the plan because those values now live in
  the host overlays instead of the shared modules.

### 2. Review confidence remains repo-local, not remote-operational

- This F2 rerun intentionally focuses on code quality and the repo-local
  regression gate.
- The inherited environment blocker still applies: remote `ssh agent@saga-dev2`
  access remains unresolved from this environment, so this review cannot
  independently confirm live canary apply/validate behavior beyond the tracked
  scripts, docs, and local Nix/test coverage.
- That limits operational confidence, but it is not a code-quality blocker for
  the current F2 scope.

## Verification run

Commands rerun for this review:

- `nix flake check /home/zack/git/kley` → passed
- `nix build /home/zack/git/kley#nixosConfigurations.saga-dev.config.system.build.toplevel`
  → passed
- `nix build /home/zack/git/kley#nixosConfigurations.saga-dev2.config.system.build.toplevel`
  → passed
- `bash /home/zack/git/kley/tests/vm-baseline-check.sh` → passed

Additional working-tree check used to re-evaluate the prior script-tracking
blocker:

- `git status --short` → confirms the canary helper scripts are tracked in the
  current tree

Notes:

- `nix flake check` emitted the existing warning that incompatible
  `aarch64-linux` checks were omitted unless `--all-systems` is used. That does
  not affect this x86_64 host review.
- Unrelated working-tree edits exist elsewhere in the repo, but they were
  excluded from this review unless needed to define scope boundaries.

## Conclusion

APPROVE.

The current `agent-vm` implementation clears the earlier F2 blockers: the shared
baseline no longer embeds the repo app build, host-private drift has been
removed from tracked host overlays, the canary helper scripts are present as
tracked repo source, and shared disk logic no longer hardcodes a machine path.
The required Task 8 repo-local regression commands also pass, so this rerun
approves the current code-quality state while noting that remote `saga-dev2`
execution confidence remains bounded outside this environment.
