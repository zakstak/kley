# Handoff — agent-vm-baseline-update-lane

## Current state

- F1 plan compliance, F2 code quality, and F4 scope fidelity are all approved
  for the delivered VM lane (`.sisyphus/evidence/f1-plan-compliance.md`,
  `.sisyphus/evidence/f2-code-quality.md`,
  `.sisyphus/evidence/f4-scope-fidelity.md`). The rollout is locally validated
  through the new flake checks, shared modules, scripts, and docs.
- F3 experiential QA was rejected because every remote step (apply → validate →
  recover) fails before it can reach `agent@saga-dev2`
  (`.sisyphus/evidence/f3-experiential-qa.txt`). Until the canary host is
  reachable, no live remote gate can be closed.

## Verified infrastructure facts (session data)

- Proxmox is reachable via the `saga-proxmox` SSH alias
  (`ssh saga-proxmox -o BatchMode=yes -o ConnectTimeout=5 true` succeeded).
- `qm list` on saga-proxmox shows the baseline `saga-dev` already running as
  VMID 200, while VMID 201 is free/not actively running so it can accept the new
  canary deployment (`qm list` output shows only saga-dev 200 and the stopped
  template/placeholder entries).
- `saga-dev2` is not present as a live canary host yet, and the candidate IP
  10.0.0.51 currently drops all packets (`ping -c 1 10.0.0.51` → 100% packet
  loss).
- A full-clone approach to copy saga-dev to a new host was rejected because it
  would duplicate roughly 128 GB of disk data; the desired path is the template
  deploy command instead
  (`cargo run -p saga -- infra agent deploy saga-dev2 201 10.0.0.51 --disk 128`).

## Remaining follow-up

1. **Provision saga-dev2 at VMID 201** using the saga infra CLI (do not re-clone
   the existing disk). The known working deploy command is:
   ```bash
   KLEY_FLAKE_DIR=/home/zack/git/kley cargo run -p saga -- infra agent deploy saga-dev2 201 10.0.0.51 --disk 128
   ```
   Ensure DNS/SSH for `agent@saga-dev2` resolves after deployment; the host
   spends all config in the template-based flow instead of copying the 128 GB
   baseline disk.
2. **Once saga-dev2 is reachable**, rerun the F3 experiential QA flow exactly as
   recorded in `.sisyphus/evidence/f3-experiential-qa.txt`: run
   `agent-vm/scripts/apply-local-checkout-canary.sh`, then
   `agent-vm/scripts/validate-canary-kley.sh`, and finally
   `agent-vm/scripts/recover-canary-after-failed-update.sh` (they in turn build
   via `nix` and rely on `KLEY_FLAKE_DIR` being set). All three remote commands
   must succeed before the lane can be promoted.
