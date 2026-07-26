# Agent VM module graph

`agent-vm/` is the shared NixOS baseline for the repo-owned `saga-runtime` agent
VM.

- `modules/base.nix` defines the shared host baseline.
- `modules/runtime-cli-tools.nix` bootstraps the default non-wrapper CLI runtime
  tools for `saga-runtime`.
- `hosts/` stays thin and should only describe machine facts.
- `default.nix` is where the host gets wired to its additive runtime modules.

## Host layout

The `saga-runtime` VM is the default non-wrapper runtime host. It ships the
shared non-Nix-managed `pi`, `codex`, and `opencode` CLIs on PATH. It does not
require a repo checkout inside the VM and does not provision
`/var/lib/kley/opencode`.

The build metadata is written to `/etc/kley-agent-vm-build.json` and records the
host plus any additive runtime state.

## Deploy and validate

Apply the current checkout to the runtime host:

```bash
agent-vm/scripts/deploy-agent-vm.sh saga-runtime
```

Lower-level helpers for the same single-host path:

```bash
agent-vm/scripts/apply-local-checkout.sh
agent-vm/scripts/validate-kley.sh
```

`validate-kley.sh` enforces the runtime-only contract for `saga-runtime`: normal
CLI installs on PATH, no harness wrappers, and no `/var/lib/kley` runtime roots.

`pi` is installed the standard npm way by the `pi-coding-agent-npm-install`
startup service:

```bash
npm install -g @earendil-works/pi-coding-agent
```

The service uses the agent-owned global prefix `/home/agent/.npm-global`, so
normal `ssh agent@saga-runtime` lands in an environment where `pi` is on PATH
and `pi update --self` can update the npm-managed install.

`codex` follows that same npm-managed path through the
`runtime-cli-tools-install` startup service, which installs `@openai/codex` into
the agent-owned npm prefix and keeps `/usr/local/bin/codex` pointed at that
user-managed install.

`opencode` stays off Nix too, but it avoids both npm and the generic glibc
installer build on NixOS. The same `runtime-cli-tools-install` service pulls the
official musl release artifact into `/home/agent/.opencode/bin`, then keeps
`/usr/local/bin/opencode` pointed at that managed install.

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
