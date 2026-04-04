- Prefer NixOS manual + nixpkgs source permalinks over wiki/discourse for
  implementation guidance in this lane.
- Recommend canary flow using `nixos-rebuild test` before promoting with
  `switch`/`boot` to reduce blast radius.
- Kept root `flake.nix` as the single authoritative flake and wired VM outputs
  via
  `nixosConfigurations = (import ./agent-vm { inherit nixpkgs; }).nixosConfigurations;`
  so `devShells` remain unchanged.
- Final Task 1 minimal structure: root flake exports existing `devShells` plus
  `nixosConfigurations.agent-vm` from `agent-vm/default.nix` and
  `agent-vm/hosts/agent-vm.nix`; local `nix flake show /repo` requires those new
  subtree files to be Git-tracked.
- Created the shared `agent-vm/modules/base.nix` module plus a
  `profiles/developer-heavy.nix` package list so hosts can just import
  `base.nix` for the canonical developer-heavy inventory and added a checked-in
  manifest at `agent-vm/developer-heavy-tool-manifest.txt` for local audits.
- Split `agent-vm` into a shared module graph exported as
  `nixosModules.{base,opencode-harness,disko,impermanence}`; keep the
  developer-heavy inventory in `modules/opencode-harness.nix`, keep
  `modules/base.nix` as the shared OS/runtime contract, and leave `hosts/` as
  thin machine-fact overlays.
- Replaced placeholder `nixosConfigurations.agent-vm` with `saga-dev` and
  `saga-dev2` via a shared `mkHost` helper so both lanes consume the identical
  `moduleGraph.sharedModuleImports` stack.
- Preserved the saga `agent` SSH-key contract in shared config
  (`users.users.agent` plus SSH key-only auth defaults) so host overlays stay
  machine-facts-only and do not carry env-local overrides.
- Encoded the rolling update lane once in `agent-vm/promotion-contract.nix`,
  passed it through `specialArgs`, and made both hosts publish the same
  source/input metadata from `modules/base.nix` instead of introducing
  host-local promotion logic.
- Repaired Task 5 by removing the root-level `agentVmPromotion` flake output and
  keeping the promotion contract reachable only through
  `promotion-contract.nix` + per-host config metadata so normal flake
  introspection still works.
- Added a dedicated `tests/vm-baseline-check.sh` script and wired it into a
  `checks.vm-baseline` flake output so the root flake now proves the manifest
  keeps in sync with the preflight commands (noise names filtered explicitly)
  before allowing promotion.
- Task 6 implementation uses a repo-local helper
  (`agent-vm/scripts/apply-local-checkout-canary.sh`) that defaults to
  `KLEY_REPO_ROOT=/home/zack/git/kley`, `CANARY_HOST=saga-dev2`, and
  `AGENT_USER=agent`, preserving the canary source-of-truth and saga remote
  primitives without VM hand-edit steps.
- Task 7 codifies the post-apply canary gate in
  `agent-vm/scripts/validate-canary-kley.sh` and defaults the remote checkout
  path to `/home/agent/kley`, while keeping `REMOTE_KLEY_REPO_ROOT` overrideable
  so the operator uses one explicit variable instead of memory or ad hoc shell
  history.
- Added a dedicated Task 9 recovery companion script
  (`agent-vm/scripts/recover-canary-after-failed-update.sh`) instead of folding
  rollback into apply/validate scripts, so failure handling is explicit,
  host-scoped to `saga-dev2`, and reusable after either apply failures or
  post-apply smoke failures.
- Made source-of-truth recovery explicit in `agent-vm/README.md` with two
  concrete branches (`git revert <bad-commit-sha>` for committed changes,
  `git restore --staged --worktree flake.lock agent-vm` for local changes), then
  mandated retry through the same Task 6 + Task 7 scripts.
