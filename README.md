# kley

Kley is a minimal coding agent with both terminal and web entry points. It is
intentionally small: a Rust CLI, a shared session runtime, a websocket-driven
web shell, and a Docker workflow aimed at Linux development environments.

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

The supported path is the Docker wrapper:

```bash
./preflight.sh
./docker-session.sh login openai
./docker-session.sh chat
```

Docker runs set `KLEY_PASSPHRASE` automatically to `kley-dev-passphrase` unless
you override it from your shell. To use your own passphrase instead, export it
before running login/web/chat:

```bash
export KLEY_PASSPHRASE="your-passphrase"
```

`./docker-session.sh` now reuses the existing image and rebuilds only the Rust
binary inside the container before running `kley`.

To force a full image rebuild:

```bash
KLEY_DOCKER_FULL_REBUILD=1 ./docker-session.sh web
```

To force a full image rebuild without layer cache:

```bash
KLEY_DOCKER_FULL_REBUILD=1 KLEY_DOCKER_NO_CACHE=1 ./docker-session.sh web
```

If you pass no arguments, `./docker-session.sh` defaults to `chat`.

If you see:

```text
auth storage unavailable: decryption failed (wrong passphrase?): Decryption failed
```

your current `KLEY_PASSPHRASE` does not match the passphrase used when
credentials were created.

In normal Docker flow this now auto-recovers by resetting stale encrypted auth
storage (`KLEY_WEB_AUTH_AUTO_RESET=1` by default), so you should not need to run
manual reset commands.

If you disabled auto-reset, fix by either exporting the original passphrase, or
recreating credentials for disposable Docker state:

```bash
./docker-session.sh auth-reset
./docker-session.sh login openai
```

To launch the web UI instead:

```bash
./docker-session.sh web
```

Then open `http://127.0.0.1:3210` in a browser.

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

Docker is the default workflow, but you can also run the binary directly if you
already have a Rust toolchain set up:

```bash
cargo run --bin kley -- <subcommand>
```

`./preflight.sh` will run `kley preflight` through Cargo or an installed `kley`
binary, depending on what is available.

## Development notes

- `./docker-session.sh` rebuilds the image before launching a fresh session.
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
