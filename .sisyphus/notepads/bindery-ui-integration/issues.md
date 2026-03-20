
## Issues / Gotchas Encountered During Reconnaissance

### 1. No existing `src/web/` module — everything is greenfield
There is zero web infrastructure in the repo currently. The only server-like code is the OAuth callback in `src/auth/openai.rs` (lines 267–371), which is a short-lived single-route server, not a long-running multi-route server. The new `src/web/` module must be created entirely from scratch.

### 2. No existing template/static serving infrastructure
`Cargo.toml` has no `askama`, `tower-http`, or any HTML templating dep. Task 1 explicitly requires `askama` — it needs to be added to `Cargo.toml` before the scaffold works. No `templates/` or `static/` directories exist yet.

### 3. The `AgentEvent` channel is sync mpsc, not async
`src/events.rs` uses `std::sync::mpsc::Sender/Receiver` (not `tokio::sync::mpsc`). The web layer's WebSocket handler will need to bridge this sync channel into an async stream. The existing `EventReceiver` has `recv_blocking()` and `try_recv()` but no `tokio::AsyncRecvExt` impl. A wrapper or adapter will be needed.

### 4. `TurnResult` in `src/agent.rs` has no variant for streaming deltas
The current `TurnResult` enum (line 49) only has `Text(String)` and `ToolCalls(Vec<ToolCall>)`. The streaming assistant text is accumulated internally in `send_openai_ws` / `send_openai_sse` and returned as a single string. For the browser protocol's `message.delta` events, the streaming needs to be decomposed — either by returning a stream from the agent, or by emitting events during the turn (which is what Task 5 addresses by expanding `AgentEvent`).

### 5. The OAuth callback server binds on `0.0.0.0:1455` — not on localhost
The existing axum server in `src/auth/openai.rs:329` binds on `0.0.0.0` to accept the OAuth redirect from localhost. The web server for Tasks 1–9 should bind on `127.0.0.1` only (default per plan) to enforce same-origin.

### 6. No Playwright or Node tooling exists in repo
The plan references `package.json`, `playwright.config.ts`, `playwright/` directory for Task 9 — none of these exist. Task 9 is Wave 3 and will need full Playwright setup. The repo has only Rust.

### 7. `store_run` uses blocking mpsc mutex, not tokio mutex
`src/store/mod.rs:21` defines `SharedStore = Arc<Mutex<Store>>` (std `Mutex`, not `tokio::sync::Mutex`). The `store_run` helper (line 69) acquires this inside `spawn_blocking`. This is correct and documented. Web handlers must use the same `store_run` pattern, not `.lock().unwrap()` directly in async context.

### 8. Tool calls don't have explicit `tool_call_id` on the `ToolCall` struct yet
`src/agent.rs:41` defines `ToolCall` with `call_id: String` (which is the API-level call_id). However, `AgentEvent` has no `ToolStarted`/`ToolCompleted` variants — tool execution currently only emits `eprintln!` output. Task 5 must add these to `AgentEvent`.

### 9. `RunMode` enum in `src/agent.rs` is not exposed from the library
The `RunMode` enum is defined in `src/agent.rs` but `lib.rs` only re-exports the module path (`pub mod agent`). Web sessions should not use `RunMode` — the runtime manager (Task 4) will own session lifecycle. But the existing CLI path still uses it.

### 10. No existing concurrency test for multiple active sessions with in-flight state
`tests/store_concurrency.rs` tests concurrent reads/writes to the store but does NOT test concurrent sessions with active (incomplete) turns. The runtime manager (Task 4) will need to introduce this test pattern to verify session isolation and the active-turn replay buffer.

## Bindery UI Integration — Scope Warnings and Gotchas (2026-03-18)

### Scope Warnings (Must Not Port)

1. **Fork/Session Tree UI** (`#btn-fork-picker`, fork-related JS state `forkMessages`, `knownSessionPaths`, `state.forkMessages`). Do not port the fork picker button or any fork-related JS state. kley has no fork concept.

2. **Session Picker Dialog** (`#btn-session-picker`, `dialogMode === "session"`). kley uses `sessions.list` + `session.load` commands — no session picker modal needed. Session switching is command-driven, not dialog-driven.

3. **Model Picker Dialog** (`#btn-model-picker`, `availableModels` state, `dialogMode === "model"`). kley plan explicitly excludes model switching UI. No model picker button.

4. **Task Session UI** (`#btn-task-start`, `#btn-task-complete`, `dialogMode === "task"`). kley has no task sessions. These buttons must be absent from kley's UI.

5. **Image Attachment Flow** (`#prompt-image-input`, `composerAttachments` state, `addComposerFiles()`, file-to-dataUrl, dimension reading, chip rendering, stream media cards). kley plan excludes image uploads. Do not port attachment infrastructure.

6. **HTMX and WS Extension** (`htmx.org@2.0.4`, `htmx-ext-ws@2.0.2`). Bindery uses HTMX for SSE-like interactions. kley uses vanilla JS WebSocket. Do not include HTMX CDN.

7. **Extension Runtime** (`extension_ui_request` with `select/confirm/input/editor/setStatus/setTitle/set_editor_text` methods). Bindery's extension UI system is a separate runtime. kley has no extension system in scope. Port `notify` → `status.report` and `setWidget` → collapsed status strip updates only.

8. **Mock Presets Section** (`#mock-presets`, `#mock-preset-buttons`, `MOCK_PRESETS` array, `IS_MOCK_ROUTE` detection). kley will have its own mock socket at `/ws/mock` but without the preset-button UI. The `/mock` route distinction (`IS_MOCK_ROUTE`) should not be ported.

9. **Diffy Integration** (`DiffyConfig` in `config.rs`, references to `diffy` in CSS comments). Bindery has optional Diffy integration for file diffs. kley plan does not mention diffs. Ignore Diffy references.

10. **Agent/Sub-agent Concept** (`agent_start`, `agent_end` events, `agentId` field, `bindery-demo-agent`). kley has a single runtime with no sub-agent concept. Omit `agent_start`/`agent_end` events entirely.

11. **Timeline/Flame Bars** (`.flame-bar`, `flameSection`, `#flame-bars`, `#flame-axis`, `timelineStartMs`, `timelineItems`, `timelineById`, `MAX_TIMELINE_ITEMS`, `BAR_ROW_HEIGHT`). This is a nice-to-have timing visualization in Bindery. Plan does not mention it; treat as optional enhancement, not MVP.

12. **Dialog Panel** (`#dialog-panel`, `dialogPanel`, `dialogMode` state). Bindery uses dialog panels for model picker, session picker, fork picker. kley only needs the status strip and inspector. Do not port dialog infrastructure.

### Gotchas

1. **Inline SVG Favicon**: The favicon in `index.html` line 7 is an inline data-URI, NOT a reference to `bindery-icon.svg`. The actual `bindery-icon.svg` file at `assets/` is a different, simpler 3-column book icon. Plan references the SVG file specifically — use that, not the data-URI from the template.

2. **Tailwind v4 CDN**: The template uses `@tailwindcss/browser@4` which is a JIT/runtime version. This is non-standard — most projects use the build-time Tailwind pipeline. For kley, either stick to the CDN version (same as Bindery) or establish a build-time pipeline. CDN approach matches Bindery and avoids build complexity for the first implementation.

3. **Responsive Breakpoints**: Bindery has two breakpoints at 980px and 760px. Mobile drawer placement shifts from right-side to bottom sheet at 760px. This is a substantial amount of CSS (~35 lines). Adapt conservatively or skip mobile for MVP.

4. **`binderyMeta` Payload Wrapping**: The real `ws.rs` injects a `binderyMeta` field into events with `{kind, title, preview}` for UI rendering. This is Bindery-specific. kley should either emit flat events with `kind`/`title`/`preview` directly, or not emit this wrapper at all (let the client construct it from event fields).

5. **HTML Template Is 2483 Lines**: The template is very large — 928 lines of CSS + 2000+ lines of JS. Copying it wholesale would be expensive to adapt. The plan's advice to "rewrite the client-side JS around kley's protocol shape" is the right call. Target a minimal shell first.

6. **`rpc-mode.ts` is 785 Lines**: This file handles stdin/stdout JSONL protocol — completely different architecture from kley's in-process runtime. Do not reference it for implementation guidance; reference only `rpc-types.ts` for naming.

7. **`rpc-client.ts` Spawns a Node Process**: Bindery's web server spawns `node dist/cli.js --mode rpc` as a subprocess. kley's web server will call an in-process runtime directly. The subprocess spawn pattern is not applicable.

8. **Mock Socket State**: `handle_mock_socket` maintains mutable state across prompts (model, prompt_index, task_index, session_name, fork_messages, active_task). A kley mock socket following this pattern would need similar state tracking for multiple sequential prompts in a session.

9. **Error Response Shape**: Bindery uses `{ type: "response", command, success: false, error }` for all errors. The plan specifies `response.error` with `code="invalid_command"`. Be consistent with the plan's envelope shape — do not use Bindery's error format.

10. **`with_bindery_meta` Function**: `ws.rs` wraps every agent event with `binderyMeta { kind, title, preview }` before sending to browser. This is a transform layer that computes display metadata from raw event data. kley may want an analogous (but simplified) layer — either in the bridge or the client — to compute `kind` and `preview` from structured runtime events.

11. **TOML Config with Env Expansion**: `config.rs` expands `${VAR}` in agent.env values. kley's config loading (if any) should consider whether to follow this pattern or use a simpler approach.

12. **Askama Template Path**: `ui.rs` uses `#[template(path = "index.html")]` which resolves relative to the configured template directory. The path `index.html` means Askama will look in `templates/index.html` at the crate root. kley will need a similar templates directory with the Askama template engine configured.

## Task 7 implementation issues (2026-03-19)

1. **Tailwind-specific style blocks trip HTML diagnostics**: `type="text/tailwindcss"` plus `@theme` raised an editor/LSP error in this repo. Switched to plain CSS variables while still keeping Tailwind CDN for utility layout classes.

2. **No static middleware route existed for icon assets**: root router had only `/`, `/healthz`, and `/ws`. Added explicit `/assets/bindery-icon.svg` route for deterministic icon serving without touching websocket/runtime paths.

## Task 8 implementation issues (2026-03-19)

1. **HTML lint warnings for optional chaining style**: `lsp_diagnostics` on `templates/index.html` reports Biome `lint/complexity/useOptionalChain` warnings in the inline script. They are warnings only (no errors) and do not affect runtime behavior or Rust test verification.

## Task 9 implementation issues (2026-03-19)

1. **Overriding `HOME` broke `cargo` resolution inside the Playwright web-server helper**: launching the Rust server from Node with a temp `HOME` made `rustup` lose its toolchain lookup, so `playwright/support/web-server.mjs` must preserve `CARGO_HOME` and `RUSTUP_HOME` from the outer environment while still isolating Kley's store path through `HOME`.
