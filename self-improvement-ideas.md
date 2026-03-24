# Self-improvement ideas for this repo

I started from a pool of 30 candidate ideas across CI, docs, frontend
architecture, self-improve workflows, testing, and core runtime internals. I
filtered out ideas that were mostly cosmetic, too speculative, or too expensive
for the likely payoff.

These 10 are the ones that still look good after checking repo fit, feasibility,
reward, cost, and risk.

## 1. Add CI that mirrors the existing local quality gates

| Feasibility | Reward | Cost       | Risk |
| ----------- | ------ | ---------- | ---- |
| High        | High   | Low-Medium | Low  |

- **Why this is a good idea:** the repo already has a clear definition of
  “good”: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and
  `cargo build --release`. Right now that quality bar exists locally, but not in
  hosted automation.
- **Why it fits this repo:** this project has meaningful Rust and browser test
  coverage already, so CI would amplify discipline that already exists rather
  than invent a new process.
- **Evidence:** `hooks/pre-push`; no `.github/workflows/*` files exist.

## 2. Replace the fragmented hook setup with one portable validation entrypoint

| Feasibility | Reward | Cost   | Risk |
| ----------- | ------ | ------ | ---- |
| High        | High   | Medium | Low  |

- **Why this is a good idea:** the repo currently has overlapping hook
  definitions plus a portability problem. `lefthook.yml` points at absolute
  personal paths under `/home/zack/personal/pragma/...`, while
  `hooks/pre-commit` and `hooks/pre-push` define their own checks separately.
- **Why it fits this repo:** a single canonical validation script would make
  local hooks, self-improve runs, and CI all agree on what “pass” means.
- **Evidence:** `lefthook.yml`; `hooks/pre-commit`; `hooks/pre-push`;
  `.agents/rules/00-kley-dev.md`.

## 3. Turn the README into a real contributor and operator guide

| Feasibility | Reward | Cost | Risk |
| ----------- | ------ | ---- | ---- |
| High        | High   | Low  | Low  |

- **Why this is a good idea:** the repo is much more sophisticated than its docs
  suggest. It has a CLI, web mode, self-improve loop, SQLite persistence, hooks,
  browser tests, and Docker workflows, but the README is only a couple of lines.
- **Why it fits this repo:** this is exactly the kind of project where new
  contributors need a “how it works” map before they can make safe changes.
- **Evidence:** `README.md`; `src/main.rs`; `src/web/*`; `self-improve.sh`;
  `tests/*`; `playwright.config.ts`.

## 4. Extract and localize the web UI assets instead of keeping them inline and CDN-backed

| Feasibility | Reward | Cost   | Risk   |
| ----------- | ------ | ------ | ------ |
| Medium      | High   | Medium | Medium |

- **Why this is a good idea:** the web UI is currently hard to evolve because
  presentation and behavior are spread across a huge HTML template and raw
  JavaScript strings. It also depends on external CDN resources at runtime.
- **Why it fits this repo:** the web app is now substantial enough that it
  deserves first-class assets rather than “single template plus injected script”
  maintenance.
- **Evidence:** `templates/index.html` is ~1018 lines and includes
  `@tailwindcss/browser` plus Google Fonts; `src/web/ui.rs` injects a large
  raw-string self-improve panel script.

## 5. Add browser-level coverage for the self-improve panel

| Feasibility | Reward      | Cost   | Risk |
| ----------- | ----------- | ------ | ---- |
| High        | Medium-High | Medium | Low  |

- **Why this is a good idea:** the self-improve panel has meaningful browser
  logic, but most of its current verification is either backend/websocket
  testing or HTML-string assertions, not real browser interaction.
- **Why it fits this repo:** this project already uses Playwright, so the
  missing piece is breadth, not a new testing stack.
- **Evidence:** `playwright/core-workspace.spec.ts` covers core workspace
  behavior but not the self-improve panel; `tests/web.rs` checks self-improve
  websocket behavior and HTML markers; `src/web/ui.rs` and
  `src/web/self_improve.rs` implement substantial self-improve UI behavior.

## 6. Move the self-improve prompt/policy out of `self-improve.sh` into versioned prompt assets

| Feasibility | Reward | Cost   | Risk   |
| ----------- | ------ | ------ | ------ |
| Medium      | High   | Medium | Medium |

- **Why this is a good idea:** the self-improve harness is one of the repo’s
  most distinctive features, but its prompt contract is embedded inside a long
  shell script, which makes it harder to review, evolve, diff, and test cleanly.
- **Why it fits this repo:** prompt and harness behavior are part of the product
  here, not just incidental glue code. Treating them as first-class assets would
  make future iteration safer.
- **Evidence:** `self-improve.sh` contains a very large embedded prompt block;
  `tests/self_improve_prompt.rs` asserts many exact prompt markers and ordering
  constraints.

## 7. Build a real retrospective analytics view on top of the existing JSONL records

| Feasibility | Reward | Cost   | Risk       |
| ----------- | ------ | ------ | ---------- |
| Medium      | High   | Medium | Low-Medium |

- **Why this is a good idea:** the repo already records structured self-improve
  retrospectives, but the current UI looks more like a raw feed than a
  decision-making surface. The data is there; the insight layer is not.
- **Why it fits this repo:** this is one of the few projects where “improve the
  self-improvement loop” is a core product improvement, not a side quest.
- **Evidence:** `.self-improve-logs/`; `src/bin/self-improve-retrospective.rs`
  writes structured JSONL; `src/web/self_improve.rs` and `src/web/protocol.rs`
  already surface run history and retrospectives.

## 8. Break up the biggest Rust hotspot files into smaller modules with clearer seams

| Feasibility | Reward      | Cost        | Risk   |
| ----------- | ----------- | ----------- | ------ |
| Medium      | Medium-High | Medium-High | Medium |

- **Why this is a good idea:** several core files are large enough to slow down
  understanding and safe edits. That usually means responsibilities have
  accumulated faster than module boundaries.
- **Why it fits this repo:** the project is no longer tiny. The current file
  sizes suggest it has crossed the threshold where “just keep it in one file” is
  becoming a maintenance tax.
- **Evidence:** `src/runtime/session.rs` (~1425 lines), `src/runtime/manager.rs`
  (~917 lines), `src/web/self_improve.rs` (~947 lines), and `tests/web.rs`
  (~1523 lines).

## 9. Add protocol contract tests between the Rust event schema and the browser client

| Feasibility | Reward      | Cost   | Risk |
| ----------- | ----------- | ------ | ---- |
| Medium      | Medium-High | Medium | Low  |

- **Why this is a good idea:** the browser client manually switches on many
  frame types and fields, while the Rust side defines the protocol separately.
  That makes drift possible even though both sides are in the same repo.
- **Why it fits this repo:** this is a websocket-heavy UI, so protocol
  mismatches are one of the easiest ways to get subtle regressions.
- **Evidence:** `src/web/protocol.rs` defines `WebCommand`, `WebResponse`, and
  `UiEvent`; `templates/index.html` parses JSON frames, checks
  `protocol_version`, and branches on many `frame.type` values.

## 10. Revisit the single-mutex SQLite access model before web usage grows further

| Feasibility | Reward | Cost        | Risk   |
| ----------- | ------ | ----------- | ------ |
| Medium      | Medium | Medium-High | Medium |

- **Why this is a good idea:** the current persistence layer is simple and
  reasonable, but it serializes all DB access through `Arc<Mutex<Store>>` plus
  `spawn_blocking`. That is fine early on, but it can become a hidden bottleneck
  as the web surface grows.
- **Why it fits this repo:** the repo now supports CLI sessions,
  websocket-driven web interactions, and self-improve telemetry. That is enough
  concurrency pressure to justify at least re-evaluating the store boundary.
- **Evidence:** `src/store/mod.rs` wraps `Store` in `Arc<Mutex<Store>>` and
  routes work through `store_run()` with `spawn_blocking`.

## Why these 10 survived the cut

The strongest themes in this repo are:

- **quality automation exists locally but not centrally**;
- **documentation is far behind the codebase’s actual sophistication**;
- **the web UI has outgrown its current asset structure**;
- **the self-improve system has valuable data and prompt logic that deserve
  first-class treatment**; and
- **a few core files are now large enough to justify structural cleanup.**

That is where the reward-to-cost ratio looks best right now.
