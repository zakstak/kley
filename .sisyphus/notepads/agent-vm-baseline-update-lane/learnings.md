- Authoritative flake output contract: `nix flake check` validates
  `nixosConfigurations.<name>.config.system.build.toplevel` as a derivation (Nix
  manual: https://nix.dev/manual/nix/2.28/command-ref/new-cli/nix3-flake-check).
- Authoritative host-structure example: nixpkgs flake documents
  `nixosConfigurations = { <host> = nixpkgs.lib.nixosSystem { modules = [ ./<host>/configuration.nix ... ]; }; };`
  (https://github.com/NixOS/nixpkgs/blob/760014e730de12ad1affed9fa61dff32d987f4cf/flake.nix#L236-L244).
- Deployment semantics from NixOS manual source: `switch` applies now+default
  boot, `test` applies now only, `boot` sets next-boot default only
  (https://github.com/NixOS/nixpkgs/blob/760014e730de12ad1affed9fa61dff32d987f4cf/nixos/doc/manual/installation/changing-config.chapter.md#L7-L37).
- Rollback semantics: bootloader keeps prior generations not yet
  garbage-collected; runtime rollback via `nixos-rebuild switch --rollback`
  (https://github.com/NixOS/nixpkgs/blob/760014e730de12ad1affed9fa61dff32d987f4cf/nixos/doc/manual/administration/rollback.section.md#L6-L28).
- Lockfile safety: `nix flake update` updates all inputs by default, while
  `nix flake lock` does not update already-locked inputs
  (https://nix.dev/manual/nix/2.28/command-ref/new-cli/nix3-flake-update,
  https://nix.dev/manual/nix/2.28/command-ref/new-cli/nix3-flake-lock).
- Practical local-check gotcha: when root flake imports a newly created subtree
  (e.g., `./agent-vm`) inside a Git worktree, `nix flake show /repo` fails until
  those files are Git-tracked; otherwise Nix reports the path is not tracked.
- Developer-heavy inventory should mirror `src/preflight.rs` commands, and the
  manifest diff can show helper names (e.g., `remote_probe_command`,
  `git_config_command`) so document those runtime-only tokens when evaluating
  the audit.
- Added `tests/vm-baseline-check.sh` plus a `checks.vm-baseline` flake target
  that exercises `nix flake show`, both host builds, and the manifest diff so
  regressions fail locally before a remote deploy.
- The manifest diff now tolerates the helper-name noise (`kley`, `origin`,
  `upstream`, `user.name`, `user.email`) while still flagging real missing
  developer-heavy commands.
- Root flake exports can expose shared `nixosModules` from
  `agent-vm/default.nix` directly, and `nix eval ...#nixosModules.<name>`
  succeeds once newly created module files are Git-tracked.
- When a NixOS module defines `options = ...`, all plain settings must live
  under `config = { ... };` or Nix reports unsupported top-level attributes
  during module evaluation.
- Task 4 host overlays can be kept thin by moving lane semantics to a shared
  option (for example `kley.agentVm.promotionLane`) and keeping only
  hostname/disk/network facts in host files.
- Task 5 can record the exact flake checkout used for a VM build by setting
  `system.configurationRevision` from flake source metadata and emitting the
  same revision plus locked input revs into `/etc/kley-agent-vm-build.json`.
- Raw promotion metadata is safe inside `nixosConfigurations.<host>.config` and
  docs, but exposing that attrset as a top-level flake output breaks
  `nix flake show`; keep the contract internal to host/module surfaces instead.
- Flake `checks` must be system-scoped (for example
  `checks.x86_64-linux.vm-baseline`), not a flat derivation attrset, or
  `nix flake show` still fails during output introspection.
- Task 6 local-apply flow can stay fully declarative from checkout state by
  deriving `STORE_PATH` from `/home/zack/git/kley/result`, exporting the full
  closure via `nix-store -qR`, and applying with `nix-env --set` +
  `switch-to-configuration switch` over SSH.
- Post-switch identity is operator-verifiable without in-VM edits by reading
  `/nix/var/nix/profiles/system` and listing `/nix/var/nix/profiles/system`
  generations (`nix-env --list-generations -p ...`).
- Task 7 can anchor the canary smoke lane on repo-native commands plus existing
  web contract details already in-tree: `./preflight.sh`,
  `./kley-run.sh chat --help`, `./kley-run.sh web --bind 127.0.0.1:3210`,
  `/healthz -> ok`, and the root-page `Kley web` marker.
- Task 9 can remove rollback guesswork by parsing
  `nix-env --list-generations -p /nix/var/nix/profiles/system`, reading the
  `(current)` marker, and selecting the highest lower generation before running
  `nix-env --rollback` + `switch-to-configuration switch`.
- The rollback path stays aligned with the canary-first lane when docs and
  checks require rerunning `apply-local-checkout-canary.sh` and
  `validate-canary-kley.sh` only after source-of-truth (`flake.lock` /
  `agent-vm/**`) reversion. 21: - Final-wave reviewers: stage every new
  `agent-vm/**` file before running `nix flake show`/`nix eval`, keep the
  promotion metadata inside host/module configs (not as top-level outputs),
  exercise the `checks.x86_64-linux.vm-baseline` target before deployment, and
  rely on the documented Task 6-9 scripts since this environment cannot reach
  `saga-dev2` (ssh name resolution blocked) so only local script verification is
  possible here. 22: - Evidence review update: F1 confirms the local command
  checklist (flake show, host builds, manifest audit) while also noting that
  `ssh agent@saga-dev2` still fails DNS resolution, so remote
  apply/validate/rollback remain blocked on this host name. 23: - F2/F4
  code-quality and scope reviews raised a potential scope creep: the shared
  baseline currently builds the repo-specific `kley` binary and
  `agent-vm/hosts/saga-dev.nix` carries private IPs/SSH keys, which triggered
  guardrail rejections even though the rest of the VM lane stays narrow. 24: -
  Final-wave blocker remediation pass: `modules/opencode-harness.nix` now keeps
  only the shared developer-heavy inventory (no repo-local `kley` build),
  `hosts/saga-dev.nix` removed committed private IP literals + concrete SSH
  key + `saga-generation-marker`, and `modules/disko.nix` now derives disk
  target from host `boot.loader.grub.devices` with an assertion instead of
  hardcoded `/dev/sda`. 25: - Local verification for blocker remediation
  succeeded with `nix flake check /home/zack/git/kley` plus both required host
  toplevel builds; canary helper scripts were staged so
  `agent-vm/scripts/apply-local-checkout-canary.sh` and
  `agent-vm/scripts/validate-canary-kley.sh` are tracked in git working state.
  26: - Creation/reprovision of `saga-dev2` relies on the saga CLI `infra`
  workflow: run `cargo run -p saga -- infra template create-cloud` once, then
  `cargo run -p saga -- infra agent deploy saga-dev2 201 <agent-ip-2>` so
  `nixos-anywhere` can talk to Proxmox, invoke the `system.build.diskoScript`
  from `agent-vm/modules/disko.nix` (wiping `virtio-0`, labeling
  `saga-dev2-root`), and assemble the host-specific facts in
  `agent-vm/hosts/saga-dev2.nix` before the shared module graph
  (`agent-vm/default.nix`, `promotion-contract.nix`, `modules/base.nix`) writes
  `/etc/kley-agent-vm-build.json` with the canary metadata and exposes the
  `nixosConfigurations.saga-dev2` output used by
  `agent-vm/scripts/apply-local-checkout-canary.sh`, `validate-canary-kley.sh`,
  and `recover-canary-after-failed-update.sh` during bootstrap. 27: - Repo state
  still lacks concrete infrastructure values needed to actually run those flows:
  the Proxmox host (`<proxmox-host>`), saga-dev2 IP, `vmid`/resource ticket, and
  DNS/ssh entry for `agent@saga-dev2` are intentionally external; disk
  attachment must match the documented `/dev/disk/by-id/virtio-0`, and the
  network/firewall tuples depend on the running Proxmox environment that is not
  mirrored in the repo. Without those facts the documented
  `cargo run -p saga -- infra agent deploy` plus the apply/validate scripts
  cannot be executed from this workspace.
