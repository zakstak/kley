# kley

Kley is a minimal coding agent with both terminal and web entry points. It is
intentionally small: a Rust CLI, a shared session runtime, a websocket-driven
web shell, and a local Linux workflow that is environment-manager agnostic.

![Kley web UI](./kley-web-ui.png)

_Current web shell prototype. Some visible controls are intentionally UI-only
while the Bindery-style shell port is still being wired through._

Linux only; other platforms are not supported.

## Why this project exists

Kley is built to keep the moving parts of a coding agent visible. Instead of
hiding everything behind a large stack, it exposes the core pieces directly:

- a CLI entry point for interactive and autonomous runs
- a web UI backed by a versioned websocket protocol
- a shared runtime that executes turns, tools, and persistence
- a small SQLite session store
- a test suite that covers both Rust behavior and browser behavior

If you want to study, extend, or debug an agent without starting from a huge
codebase, that is the niche this repo is trying to fill.

## Quick start

The supported path is local Linux execution. Use any environment manager you
prefer (for example Nix, direnv, or a plain host toolchain):

```bash
./preflight.sh
./kley-run.sh login openai
./kley-run.sh chat
```

`./kley-run.sh` sets `KLEY_PASSPHRASE` to `kley-dev-passphrase` unless you
override it from your shell. To use your own passphrase instead, export it
before running login/web/chat:

```bash
export KLEY_PASSPHRASE="your-passphrase"
```

If you pass no arguments, `./kley-run.sh` defaults to `chat`.

If you see:

```text
auth storage unavailable: decryption failed (wrong passphrase?): Decryption failed
```

your current `KLEY_PASSPHRASE` does not match the passphrase used when
credentials were created.

If credentials are stale for your current passphrase, reset and recreate them:

```bash
./kley-run.sh auth-reset
./kley-run.sh login openai
```

To launch the web UI instead:

```bash
./kley-run.sh web
```

Then open `http://127.0.0.1:3210` in a browser.

For remote web deployments, configure a stable public callback origin for OpenAI
browser login and open Kley through that same origin:

```bash
KLEY_WEB_PUBLIC_ORIGIN="https://kley.example.com" ./kley-run.sh web --bind 0.0.0.0:3210
# or
cargo run --bin kley -- web --bind 0.0.0.0:3210 --public-origin https://kley.example.com
```

The public origin must match the browser URL you use for Kley and the redirect
URI registered with OpenAI (`https://kley.example.com/auth/callback`). Raw LAN
or private IP browser origins are not reliable for OAuth.

## What you can do with it

- Start interactive coding-agent sessions in the terminal.
- Resume the latest session or reopen a session by ID.
- Run autonomously with a bounded turn limit.
- Use the web shell against the same core runtime concepts.
- Authenticate against supported providers.
- Persist sessions and turns in `~/.kley/kley.db`.
- Run preflight checks before starting work.

## Core commands

```bash
# Authenticate with a provider
cargo run --bin kley -- login openai
cargo run --bin kley -- login zai

# Start an interactive chat session
cargo run --bin kley -- chat

# Resume the most recent or a specific session
cargo run --bin kley -- chat --last
cargo run --bin kley -- chat --resume <session-id>

# Run autonomously (requires an initial prompt)
cargo run --bin kley -- chat --autonomous --prompt "Improve repo ergonomics"

# Run the web server
cargo run --bin kley -- web
cargo run --bin kley -- web --bind 127.0.0.1:3000
cargo run --bin kley -- web --bind 0.0.0.0:3210 --public-origin https://kley.example.com

# Run environment checks
cargo run --bin kley -- preflight
```

This repository contains more than one binary, so `--bin kley` is required when
you use `cargo run` locally.

## Tool approval modes

`chat` supports three tool approval modes:

- `ask` — prompt before each tool call
- `auto` — allow tool calls automatically
- `never` — deny all tool calls

In interactive mode, the default is `ask`. In autonomous mode, the default is
`auto`, and `ask` is not allowed.

## Web search tool

`web_search` is always included in the runtime tool list and the provider tool
payload. To enable live search results, set `TAVILY_API_KEY` before starting
`chat` or `web`.

V1 is search-only. The tool returns a normalized JSON string with `status`,
`query`, `summary`, `citations`, and `message`; without `TAVILY_API_KEY`, it
returns a structured `unavailable` result instead of failing the turn.

## Architecture

At a high level, the project has three main layers:

- **Entry points**: the CLI handles `login`, `chat`, `web`, and `preflight`,
  while the web app serves a browser shell over HTTP and a versioned websocket
  protocol.
- **Shared runtime**: both terminal and web flows rely on the same core turn
  engine for prompt submission, provider calls, tool execution, event emission,
  and context compaction.
- **Persistence and integrations**: sessions and turns are stored in SQLite,
  auth is resolved per provider, and the runtime is wired to built-in tools and
  discovered skills.

The important design point is that the CLI and web UI are two interfaces over
the same underlying runtime concepts, not two separate agent implementations.

## Running locally

Local execution is the default workflow:

```bash
./kley-run.sh <subcommand>
```

If you use Nix, run `nix develop` first and then the same command above.

You can still invoke Cargo directly if you already have a Rust toolchain set up:

```bash
cargo run --bin kley -- <subcommand>
```

`./preflight.sh` will run `kley preflight` through Cargo or an installed `kley`
binary, depending on what is available.

## Agent VM rolling update lane

The repo-owned agent VM baseline lives under `agent-vm/` and uses a single
canary-first promotion flow: a normal deploy targets `saga-dev`, stages and
validates the same checkout on `saga-dev2` first, then promotes `saga-dev` from
the same revision and `flake.lock`.

Every VM build records the exact resolved checkout revision in
`system.configurationRevision` and `/etc/kley-agent-vm-build.json`, so a default
`HEAD` workflow becomes an exact build record instead of a floating label. See
[`agent-vm/README.md`](./agent-vm/README.md) for the promotion contract checks,
the explicit deploy wrapper target selection, the local-checkout `saga-dev2`
apply sequence (`agent@saga-dev2`) used during canary validation, and the
post-apply kley smoke lane that must pass on canary before `saga-dev` promotion.
The same doc also defines the failed-update recovery path:
`agent-vm/scripts/recover-canary-after-failed-update.sh` for runtime generation
rollback on `saga-dev2`, followed by repo source-of-truth (`flake.lock` /
`agent-vm/**`) reversion before retrying canary apply/validation.

## Development notes

- `./kley-run.sh` is the canonical runner (Cargo from repo or installed binary).
- `./kley-session.sh` is a compatibility wrapper.
- Rust integration tests live in `tests/`.
- Browser coverage lives in `playwright/` and is driven by
  `playwright.config.ts`.
- Tool approval examples:

```bash
cargo run --bin kley -- chat --tool-approval auto
cargo run --bin kley -- chat --tool-approval never
```

- Additional implementation notes:
  - [Bindery UI port status](./BINDERY_UI_PORT_STATUS.md) — what is wired today
    vs. still UI-only, and why.

```bash
cargo test
npm run playwright:install
npm run test:browser
```
