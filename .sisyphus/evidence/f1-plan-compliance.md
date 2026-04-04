# F1 Plan Compliance Audit — agent-vm-baseline-update-lane

Audited only `.sisyphus/plans/agent-vm-baseline-update-lane.md`. The prior
`f1-plan-compliance.md` was stale from `subagent-delegation-system`, so this
file replaces it using current repo state, fresh local command output, and an
explicit remote-blocker check.

## Fresh verification executed

I re-ran the plan-relevant local commands against the current checkout. These
completed successfully unless noted otherwise:

- `nix flake show /home/zack/git/kley`
- `nix develop /home/zack/git/kley -c bash -lc 'cargo --version && node --version'`
- `nix eval /home/zack/git/kley#nixosModules.base --apply builtins.typeOf`
- `nix eval /home/zack/git/kley#nixosModules.opencode-harness --apply builtins.typeOf`
- `git rev-parse HEAD`
- `nix eval --raw /home/zack/git/kley#nixosConfigurations.saga-dev2.config.system.configurationRevision`
- `nix eval --raw /home/zack/git/kley#nixosConfigurations.saga-dev.config.system.configurationRevision`
- `nix eval --raw /home/zack/git/kley#nixosConfigurations.saga-dev2.config.environment.etc."kley-agent-vm-build.json".text`
- `nix eval --raw /home/zack/git/kley#nixosConfigurations.saga-dev.config.environment.etc."kley-agent-vm-build.json".text`
- `nix build --no-link /home/zack/git/kley#nixosConfigurations.saga-dev.config.system.build.toplevel`
- `nix build --no-link /home/zack/git/kley#nixosConfigurations.saga-dev2.config.system.build.toplevel`
- `nix flake check /home/zack/git/kley`
- `bash /home/zack/git/kley/tests/vm-baseline-check.sh`
- `fd flake.nix /home/zack/git/kley`
- `bash -n /home/zack/git/kley/agent-vm/scripts/apply-local-checkout-canary.sh`
- `bash -n /home/zack/git/kley/agent-vm/scripts/validate-canary-kley.sh`
- `bash -n /home/zack/git/kley/agent-vm/scripts/recover-canary-after-failed-update.sh`
- `/home/zack/git/kley/agent-vm/scripts/validate-canary-kley.sh --help`
- `/home/zack/git/kley/agent-vm/scripts/recover-canary-after-failed-update.sh --help`

Blocked remote probe:

- `ssh -o BatchMode=yes -o ConnectTimeout=10 agent@saga-dev2 true` →
  `ssh: Could not resolve hostname saga-dev2: Name or service not known`

That blocker matches the lane notepad for Tasks 6, 7, and 9, so I treat those
remote-only steps as environment-blocked rather than silently passing them.

## Tasks 1-9 mapped to implementation and commands

### Task 1 — Extend the root flake for VM ownership

Implemented in `flake.nix:6-89`: root outputs still export `devShells`, and now
also export `nixosConfigurations` plus `nixosModules` from `agent-vm/`. Fresh
`nix flake show` listed `devShells`, `nixosConfigurations.{saga-dev,saga-dev2}`,
and `nixosModules.{base,disko,impermanence,opencode-harness}`; fresh
`nix develop ... -c bash -lc 'cargo --version && node --version'` succeeded.
Fresh `fd flake.nix /home/zack/git/kley` returned only
`/home/zack/git/kley/flake.nix`, so there is still a single authoritative flake.

### Task 2 — Create the shared `agent-vm/` module graph

Implemented in `agent-vm/default.nix:1-19` and
`agent-vm/modules/default.nix:1-18`, with concrete shared modules at
`agent-vm/modules/base.nix:1-75`, `opencode-harness.nix:1-16`, `disko.nix:1-51`,
and `impermanence.nix:1-5`. Fresh
`nix eval ...#nixosModules.base --apply builtins.typeOf` and
`...#nixosModules.opencode-harness --apply builtins.typeOf` both returned
`"lambda"`, confirming export/evaluation. Tool-based content search found no
secret/private-value markers in `agent-vm/modules/*.nix`.

### Task 3 — Define the developer-heavy base inventory

Implemented in `agent-vm/profiles/developer-heavy.nix:1-32`, consumed centrally
by `agent-vm/modules/opencode-harness.nix:3-15`, and audited by
`agent-vm/developer-heavy-tool-manifest.txt:1-21`. `src/preflight.rs:49-237`
remains the canonical checklist. Fresh `bash tests/vm-baseline-check.sh` passed
the manifest audit, confirming the checked-in manifest covers the required
preflight commands with documented runtime-only exclusions. A targeted
credential-marker search across `agent-vm/**/*` returned no matches.

### Task 4 — Add host overlays for `saga-dev` and `saga-dev2`

Implemented as thin host overlays in `agent-vm/hosts/saga-dev.nix:1-52` and
`agent-vm/hosts/saga-dev2.nix:1-18`, with both hosts created through the same
`mkHost` path in `agent-vm/default.nix:5-18` and the same `sharedModuleImports`
stack in `agent-vm/modules/default.nix:11-17`. Fresh `nix build --no-link`
succeeded for both host toplevels. A targeted search of `agent-vm/hosts/*.nix`
found no `environment.systemPackages`, `rustc`, or `nodejs` duplication in host
files.

### Task 5 — Implement the rolling update lane and promotion contract

Implemented in `agent-vm/promotion-contract.nix:1-18`,
`agent-vm/modules/base.nix:22-74`, `agent-vm/README.md:11-47`, and the root
README section at `README.md:159-173`. Fresh `git rev-parse HEAD` returned
`b746475d31c68726d4ba4d76042f41f3bb20008a`; fresh
`nix eval --raw ...configurationRevision` for both hosts returned the same exact
dirty revision string (`b746475d31c68726d4ba4d76042f41f3bb20008a-dirty`), and
fresh `kley-agent-vm-build.json` evaluation for both hosts showed matching
source/input metadata with only `hostName`/`promotionLane` differing. This
satisfies the plan’s “same checkout, same resolved inputs” contract locally.

### Task 6 — Add the local-checkout apply workflow for canary VM updates

Implemented in `agent-vm/scripts/apply-local-checkout-canary.sh:1-33` and
documented in `agent-vm/README.md:49-80`. The script contains the required
repo-root build → closure export/import → remote `nix-env --set` →
`switch-to-configuration switch` flow, and the script is tracked, executable,
and shell-syntax-valid. I could not execute the remote apply itself here because
the fresh SSH probe to `agent@saga-dev2` fails hostname resolution.

### Task 7 — Add the push-known-changes kley test lane on canary

Implemented in `agent-vm/scripts/validate-canary-kley.sh:1-78` and documented in
`agent-vm/README.md:82-123`. The script explicitly performs the required
terminal smoke (`./preflight.sh`, `./kley-run.sh chat --help`) and web smoke
(`./kley-run.sh web --bind 127.0.0.1:3210`, `/healthz`, root-page marker) before
promotion; `--help` output also confirms the expected operator surface. Remote
execution is environment-blocked by the same `saga-dev2` DNS/SSH failure.

### Task 8 — Add automated evaluation and regression checks for the VM baseline

Implemented in `flake.nix:62-85` and `tests/vm-baseline-check.sh:1-94`. Fresh
`nix flake check /home/zack/git/kley` passed, which exercised the in-repo
`checks.x86_64-linux.{vm-baseline-host-builds,vm-baseline-manifest}` targets,
and the standalone `bash tests/vm-baseline-check.sh` run also passed. I did not
rerun the plan’s intentional-breakage scenario because that would require
modifying implementation files, which this audit must not do.

### Task 9 — Add rollback and recovery flow for failed updates

Implemented in `agent-vm/scripts/recover-canary-after-failed-update.sh:1-93`,
`agent-vm/README.md:124-177`, and reinforced by
`tests/vm-baseline-check.sh:51-89`. Fresh `--help` output confirms the exact
rollback command path (`nix-env --list-generations`, `nix-env --rollback`,
`switch-to-configuration switch`) plus the required source-of-truth recovery
path (`git revert <bad-commit-sha>` or
`git restore --staged --worktree flake.lock agent-vm`). Actual remote rollback
execution remains blocked by unresolved `saga-dev2` SSH in this environment.

## Guardrail compliance

- **Single source of truth / no second VM flake**: satisfied. Fresh
  `fd flake.nix` found only the root `flake.nix`, and all VM outputs are
  exported from it.
- **Shared base stable; repo-specific behavior narrow**: satisfied. Shared
  behavior lives in `agent-vm/modules/*` and `profiles/developer-heavy.nix`,
  while hosts stay small and host-specific.
- **`saga-dev` baseline vs `saga-dev2` canary**: satisfied. Hostnames/lanes are
  encoded in `agent-vm/hosts/*.nix` and enforced by the lane assertion in
  `agent-vm/modules/base.nix:63-69`.
- **Updates pinned by lock/input state, not mutable VM edits**: satisfied
  locally. `promotion-contract.nix` plus
  `system.configurationRevision`/`/etc/kley-agent-vm-build.json` record the
  exact applied source/input resolution, and docs/scripts keep the workflow
  repo-first.
- **Rollback explicit and documented**: satisfied. README + recovery script +
  regression check all cover runtime rollback and source-of-truth reversion.
- **No drift where canary/baseline diverge in module logic**: satisfied. Both
  hosts are built from the same shared module graph and currently evaluate to
  the same source/input metadata.
- **No committed secrets in the VM baseline**: satisfied from repo inspection. I
  found no credential material in `agent-vm/**/*`.

Nuance: `agent-vm/hosts/saga-dev.nix` does contain committed network values and
an SSH public key. I am treating that as compliant with Task 4’s explicit
“machine facts” allowance for host overlays, not as a shared-module guardrail
violation. If the plan intended “no committed host IPs anywhere,” this would
need clarification and would change the audit outcome.

## Residual concerns / blocked requirements

1. **Remote-only acceptance remains unproven here**: Tasks 5, 6, 7, and 9 each
   include remote canary execution steps. I verified the local
   build/eval/docs/script surfaces, but fresh end-to-end execution against
   `agent@saga-dev2` is blocked by environment DNS/SSH failure.
2. **Promotion evidence should be captured from the intended checkout state**:
   current `configurationRevision` resolves to a `-dirty` revision because the
   working tree is not clean. That is still an exact build record, but final
   promotion evidence should explicitly state whether the canary was applied
   from a clean commit or an intentional dirty checkout.

## Verdict: APPROVE

The current repo state matches the plan’s Tasks 1-9 structurally and passes the
local verification surface the plan requires: root flake ownership, shared
module graph, developer-heavy inventory, both host builds, promotion metadata,
regression checks, and documented canary/apply/rollback scripts are all present
and wired correctly.

This is an **F1 plan-compliance APPROVE**, not a remote promotion approval:
remote canary apply/smoke/rollback steps are still environment-blocked here and
must be exercised separately in F3 before treating the lane as fully end-to-end
proven.
