# Agent VM module graph

`agent-vm/` is the shared NixOS baseline for two repo-owned agent VMs:
`saga-runtime` and `saga-dev`.

- `modules/base.nix` defines the shared host baseline.
- `modules/runtime-harness.nix` defines the shared Kley runtime package layer.
- `modules/runtime-cli-tools.nix` installs the default non-wrapper CLI runtime
  tools for `saga-runtime`.
- `modules/opencode-harness.nix` and `modules/hermes-harness.nix` add isolated
  harness-specific behavior on top of that baseline.
- `hosts/` stays thin and should only describe machine facts.
- `default.nix` is where the host gets wired to its additive harness set.

## Host layout

The `saga-runtime` VM is the default non-wrapper runtime host. It ships native
`pi`, `codex`, and `opencode` on PATH plus a normal Hermes CLI install in the
agent user's profile. It does not require a repo checkout inside the VM and it
does not provision `/var/lib/kley/opencode` or `/var/lib/kley/hermes`.

The `saga-dev` VM remains the explicit harness host. It carries two isolated
harnesses plus a shared native `pi` binary:

- `opencode` — the default public web/runtime harness
- `hermes` — a second isolated runtime harness on the same machine
- `pi` — a native CLI from the shared package layer, available on PATH

Each harness keeps its own state and config root under `/var/lib/kley`:

- `/var/lib/kley/opencode`
- `/var/lib/kley/hermes`

The build metadata is written to `/etc/kley-agent-vm-build.json` and records the
host plus the additive harness set.

## Deploy and validate

Apply the current checkout to the default runtime host:

```bash
agent-vm/scripts/deploy-agent-vm.sh saga-runtime
```

The harness host stays available explicitly:

```bash
agent-vm/scripts/deploy-agent-vm.sh saga-dev
```

Lower-level helpers for the same single-host path:

```bash
agent-vm/scripts/apply-local-checkout.sh
agent-vm/scripts/validate-kley.sh
```

Validation expects these harness-specific entrypoints to exist on `saga-dev`:

- `kley-opencode`
- `kley-hermes`

pi is installed from the shared package layer, so normal `ssh agent@saga-dev`
lands in the shared baseline environment with the native `pi` binary already on
PATH.

And these runtime services:

- `kley-web-opencode`
- `kley-web-hermes`

The shared public nginx entrypoint proxies `http://saga-dev/` to the opencode
runtime. Hermes stays isolated behind its own local bind by default.

## Rollback

For a bad update on the single host, roll back the active system generation:

```bash
agent-vm/scripts/recover-after-failed-update.sh
```

That script runs the standard NixOS rollback path on whichever host you target
(default `saga-runtime`):

```bash
sudo nix-env --rollback -p /nix/var/nix/profiles/system
sudo /nix/var/nix/profiles/system/bin/switch-to-configuration switch
```

## Vault locator on the shared host

If your operator environment already has `VAULT_ADDR`, write it into the
gitignored generated file before deploying:

```bash
agent-vm/scripts/write-generated-vault-env.sh
```

That creates `agent-vm/.generated/vault-environment.json`, and the deploy/apply
scripts build the VM configuration with `--impure` so the shared baseline can
export the non-secret Vault address into the host environment. `VAULT_TOKEN` is
intentionally not propagated to deployed hosts.
