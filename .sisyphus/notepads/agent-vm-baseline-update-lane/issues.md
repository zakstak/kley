- `comm -23` output can include helper names like `kley`, `origin`, `upstream`,
  `user.name`, and `user.email` because the `command("...")` regex also matches
  helper functions; no actual base-package gaps were found, but note the noise
  for future verification runs.
- Task 2 verification still depends on staging newly created
  `agent-vm/modules/*.nix` files before flake Git-mode evaluation can see the
  shared module graph.
- Task 4 builds initially failed until new `agent-vm/hosts/saga-dev.nix` and
  `agent-vm/hosts/saga-dev2.nix` were Git-tracked; root-flake Git-mode still
  enforces tracked-path visibility for host overlays.
- Task 5 verification was blocked until the new `agent-vm/` subtree was fully
  staged for flake Git-mode visibility; staging tracked/untracked VM files
  remains required before local `nix build`/`nix eval` sees them.
- The working tree also contained two unrelated verification blockers during
  Task 5 (`flake.nix` had `src = .;` in the pre-existing `checks.vm-baseline`
  derivation, and `agent-vm/hosts/saga-dev2.nix` still had a temporary
  `imports = [ ./does-not-exist.nix ];` line) that had to be removed to restore
  real host evaluation.
- `nix flake check` now logs a harmless warning about the extra
  `agentVmPromotion` output while the new `vm-baseline` check runs through the
  host builds and manifest audit.
- Confirmed that introducing an invalid import in `agent-vm/hosts/saga-dev2.nix`
  causes both `nix flake check` and the saga-dev2 build to fail before any
  remote deployment step.
- Flake-check builders run sandboxed without network or /nix/var/nix write
  access, so `tests/vm-baseline-check.sh` now honors `CI_VM_BASELINE_CHECK=1`
  (set by the flake check derivation) to skip the Nix evaluation/build phases
  while still validating the manifest diff during automated runs.
- Repairing the broken `agentVmPromotion` output alone was not sufficient; the
  pre-existing `vm-baseline` flake check also had to move under
  `checks.x86_64-linux` before `nix flake show` would exit 0.
- Task 6 remote apply execution is not runnable in this environment because
  `ssh agent@saga-dev2` fails host resolution
  (`Could not resolve hostname saga-dev2`), so only local build/export-side
  verification was executed here.
- Task 7 remote canary validation is blocked by the same environment issue:
  `agent-vm/scripts/validate-canary-kley.sh` fails immediately at
  `ssh agent@saga-dev2` with `Could not resolve hostname saga-dev2`, so only
  local script/CLI/test verification ran here.
- Task 9 remote rollback execution is blocked in this environment by the same
  DNS/SSH constraint (`ssh agent@saga-dev2` unresolved), so rollback script
  verification here is limited to local command-path checks and `--help`/test
  coverage.
