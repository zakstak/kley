# Bindery-Inspired Web UI Integration

## TL;DR
> **Summary**: Add a same-origin web mode to `kley`, port the useful Bindery shell/UI structure into that mode, and expose a kley-owned WebSocket protocol plus persisted session replay so the copied UI can render streaming agent activity, history, tool execution, and inspector data.
> **Deliverables**:
> - In-repo `kley web` server with `/`, `/healthz`, and `/ws`
> - Kley-owned browser command/event contract inspired by Bindery but scoped to core workspace parity
> - Adapted Bindery template/assets rewritten against kley's runtime and store
> - Playwright and Rust integration coverage for mock, real, and CLI-regression paths
> **Effort**: Large
> **Parallel**: YES - 3 waves
> **Critical Path**: 1 -> 2 -> 3 -> 4 -> 5 -> 6 -> 7 -> 8 -> 9

## Context
### Original Request
Use the UI from `/home/zack/git/Bindery`, copy the pieces we need into this project, and use its richer UI as the model for the kinds of events `kley` should emit.

### Interview Summary
- Host the first implementation as an in-repo web UI served by `kley`.
- Target core workspace parity only: transcript, composer, session history, tool activity, inspector/status surfaces, reconnect/history replay, and abort.
- Add browser automation with Playwright and verify in tests-after mode.
- Exclude Bindery-only features from initial scope: forking/session tree, task sessions, extension runtime, image attachments, model switching UI, and subprocess-RPC parity.

### Metis Review (gaps addressed)
- Added a strict MVP boundary so “Bindery-inspired” does not become “Bindery-complete”.
- Chose a kley-owned versioned web protocol instead of porting Bindery’s full TypeScript RPC surface.
- Added bootstrap/replay semantics separate from live stream semantics so refresh/reconnect works.
- Added CLI-regression requirements so web work stays additive.
- Added deterministic mock protocol testing before real-agent wiring and Playwright.

## Work Objectives
### Core Objective
Build a kley-native web mode that reuses Bindery’s visual shell and interaction patterns for the core workspace while preserving existing CLI behavior and emitting a stable, testable event stream to the browser.

### Deliverables
- `kley web` entrypoint and Axum-based same-origin server surface.
- Versioned WebSocket command/event schema with stable IDs and explicit error envelopes.
- Reusable runtime/session boundary extracted from the CLI-owned loop.
- Session bootstrap, transcript replay, active-turn replay buffer, and single-controller session ownership.
- Adapted Bindery HTML/CSS/JS shell plus copied icon asset.
- Playwright and Rust integration tests for health, protocol, replay, streaming, abort, invalid commands, reconnect, and CLI parity.

### Definition of Done (verifiable conditions with commands)
- `cargo test web::healthz_returns_ok` exits `0` and asserts `GET /healthz` returns HTTP `200` with body `ok`.
- `cargo test web::ws_connect_receives_bootstrap_state` exits `0` and asserts the first server push is `state.snapshot` with concrete session metadata.
- `cargo test web::prompt_stream_emits_ordered_events` exits `0` and asserts ordered events `turn.started -> message.started -> message.delta+ -> message.completed -> turn.completed`.
- `cargo test web::tool_events_round_trip` exits `0` and asserts tool start/end events include `tool_call_id`, tool name, and success/failure.
- `cargo test web::invalid_command_returns_error_without_disconnect` exits `0` and asserts `response.error` with `code="invalid_command"` while the socket remains open.
- `cargo test web::reconnect_replays_active_turn` exits `0` and asserts refresh/reconnect restores in-progress assistant output from the replay buffer plus persisted history.
- `cargo test cli::existing_interactive_flow_still_persists_turns` exits `0` and proves CLI behavior still persists sessions/turns.
- `npx playwright test playwright/core-workspace.spec.ts --grep "core workspace parity"` exits `0` and verifies page load, session list, transcript replay, streamed response rendering, tool-card expansion, and abort UX.
- `cargo build --release` exits `0`.

### Must Have
- Same-binary `kley web` mode served from one origin for UI, WebSocket, and auth-related callbacks if needed later.
- WebSocket-first browser protocol with explicit commands for bootstrap, prompt submission, session load, and abort.
- Stable correlation fields on all relevant events: `session_id`, `turn_id`, `message_id`, `tool_call_id`, `event_id`, and timestamp.
- Distinct bootstrap/replay contract for persisted transcript plus in-memory replay for the active turn.
- Single active browser controller per session with explicit attach/lease rejection semantics.
- CLI and web both consume the same runtime events rather than maintaining separate agent flows.
- Bindery shell/template reused as inspiration and copy source only where it fits the chosen scope.

### Must NOT Have (guardrails, AI slop patterns, scope boundaries)
- No attempt to import or mirror the entire Bindery RPC surface from `/home/zack/git/Bindery/packages/coding-agent/src/modes/rpc/rpc-types.ts`.
- No fork/session-tree support, task-session hierarchy, extension runtime, image uploads, model switching UI, collaboration, or compaction UI in this plan.
- No coupling of browser code to provider/OpenAI/ZAI specifics.
- No socket-lifetime == session-lifetime assumption.
- No ordering logic that infers tool/message correlation by “most recent row” or “latest function call”.
- No speculative REST API beyond what this plan explicitly requires.

## Verification Strategy
> ZERO HUMAN INTERVENTION — all verification is agent-executed.
- Test decision: tests-after with new Rust integration tests plus Playwright.
- QA policy: every task includes both a happy-path and a failure/edge scenario with concrete commands/selectors.
- Evidence: `.sisyphus/evidence/task-{N}-{slug}.{ext}`
- Determinism rule: primary protocol and UI tests use mock/fake runtime paths; live-model execution is not required for acceptance.

## Execution Strategy
### Parallel Execution Waves
> Target: 5-8 tasks per wave. Shared foundations are isolated in Wave 1.

Wave 1: foundation and contract (`1` web scaffold, `2` protocol+mock WS, `3` runtime extraction, `4` runtime manager/replay)

Wave 2: real integration and UI adaptation (`5` structured event emission, `6` real WS bridge/bootstrap, `7` Bindery shell port, `8` core workspace behavior wiring)

Wave 3: verification hardening (`9` Playwright + regression suite)

### Dependency Matrix (full, all tasks)
| Task | Depends On | Notes |
|---|---|---|
| 1 | none | Establishes `kley web` surface and health checks |
| 2 | 1 | Protocol and mock socket need web scaffold |
| 3 | none | Can begin once references are understood; preserves CLI |
| 4 | 2, 3 | Runtime manager depends on command/event contract and reusable runtime |
| 5 | 3, 4 | Structured runtime events need extracted runtime and manager semantics |
| 6 | 1, 2, 4, 5 | Real socket bridge depends on protocol, manager, and rich event stream |
| 7 | 1, 2 | UI shell can be ported once server/template and protocol shape exist |
| 8 | 6, 7 | Browser behavior wiring depends on real server events and adapted UI shell |
| 9 | 6, 7, 8 | Full verification depends on stable server and browser flows |

### Agent Dispatch Summary (wave → task count → categories)
- Wave 1 -> 4 tasks -> `unspecified-high`, `deep`
- Wave 2 -> 4 tasks -> `deep`, `visual-engineering`, `unspecified-high`
- Wave 3 -> 1 task -> `unspecified-high`

## TODOs
> Implementation + Test = ONE task. Never separate.
> EVERY task MUST have: Agent Profile + Parallelization + QA Scenarios.

- [ ] 1. Add `kley web` scaffold and same-origin server foundation

  **What to do**: Add a new `web` entrypoint in `src/main.rs`; create `src/web/mod.rs`, `src/web/router.rs`, `src/web/config.rs`, and `src/web/ui.rs`; add `askama` and serve a minimal HTML shell at `/` plus `GET /healthz` returning plain `ok`; set the default bind address to `127.0.0.1:3210`; keep the server in the same binary and same origin as the future WebSocket surface.
  **Must NOT do**: Do not port Bindery JS yet; do not add speculative REST APIs; do not break the existing CLI command path.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: touches Rust app entrypoints, dependencies, and server surface.
  - Skills: `[]` — no extra skill is needed for non-git implementation.
  - Omitted: `[git]` — not a git/history task.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: `2`, `7` | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/main.rs` — existing CLI entrypoint and command wiring to extend with `web` mode.
  - Pattern: `Cargo.toml` — existing dependency manifest already includes `axum`; add template/static-serving deps here.
  - Pattern: `src/store/mod.rs` — existing async-safe store wrapper for future web handlers.
  - Pattern: `/home/zack/git/Bindery/bindery/src/web/router.rs` — reference for route composition.
  - Pattern: `/home/zack/git/Bindery/bindery/src/web/ui.rs` — reference for Askama-backed HTML serving.
  - Pattern: `/home/zack/git/Bindery/bindery/src/config.rs` — reference for web/server config loading shape.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web::healthz_returns_ok` exits `0` and asserts `GET /healthz` returns `200` with body `ok`.
  - [ ] `cargo test web::root_serves_html_shell` exits `0` and asserts `GET /` returns `200` and `text/html`.
  - [ ] `cargo build --release` exits `0` after the new web-mode dependencies and entrypoint land.

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```text
  Scenario: Server boot and health route
    Tool: Bash
    Steps: Run `cargo run -- web --bind 127.0.0.1:3210`; wait for startup; run `curl -i http://127.0.0.1:3210/healthz`; run `curl -I http://127.0.0.1:3210/`
    Expected: Health returns `HTTP/1.1 200 OK` with body `ok`; root responds `200` and `Content-Type: text/html`
    Evidence: .sisyphus/evidence/task-1-web-scaffold.txt

  Scenario: Bind failure is reported cleanly
    Tool: Bash
    Steps: Start one server on `127.0.0.1:3210`; start a second server on the same bind address
    Expected: Second process exits non-zero with a clear bind/listen error and no Rust panic backtrace
    Evidence: .sisyphus/evidence/task-1-web-scaffold-error.txt
  ```

  **Commit**: YES | Message: `feat(web): add kley web server scaffold` | Files: `src/main.rs`, `src/web/*`, `Cargo.toml`

- [ ] 2. Define the versioned browser protocol and deterministic mock socket

  **What to do**: Create `src/web/protocol.rs` with a kley-owned `protocol_version = 1` contract. Define `WebCommand` variants exactly as `state.get`, `sessions.list`, `session.load`, `prompt.submit`, and `turn.abort`. Define server responses exactly as `response.ok` and `response.error` envelopes keyed by `request_id`. Define `UiEvent` variants exactly as `state.snapshot`, `turn.started`, `message.started`, `message.delta`, `message.completed`, `tool.started`, `tool.completed`, `turn.completed`, `turn.failed`, `status.report`, `transport.selected`, `transport.fallback`, and `auth.token_refreshed`. Include stable fields `session_id`, `turn_id`, `message_id`, `tool_call_id`, `event_id`, `ts`, and `request_id` where relevant. Add a deterministic mock WebSocket path that emits bootstrap, prompt streaming, tool lifecycle, and error frames without touching the real agent runtime.
  **Must NOT do**: Do not import or mirror Bindery’s full RPC types; do not wire the real agent yet; do not add unsupported commands such as forking, task sessions, model cycling, or extension UI.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: protocol design and testability are foundational and long-lived.
  - Skills: `[]` — no extra skill is needed.
  - Omitted: `[git]` — not a git/history task.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: `4`, `6`, `7` | Blocked By: `1`

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/events.rs` — current event vocabulary that must be expanded/mapped, not reused verbatim as the browser schema.
  - Pattern: `tests/event_pipeline.rs` — event ordering/assertion style already used in the repo.
  - Pattern: `/home/zack/git/Bindery/bindery/src/web/mock.rs` — reference for deterministic UI-facing event sequencing.
  - Pattern: `/home/zack/git/Bindery/bindery/src/web/ws.rs` — reference for WebSocket frame flow and response/event separation.
  - Pattern: `/home/zack/git/Bindery/packages/coding-agent/src/modes/rpc/rpc-types.ts` — reference for naming inspiration only; do not copy wholesale.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web::ws_connect_receives_bootstrap_state` exits `0` and asserts the first server push is `state.snapshot` with session metadata.
  - [ ] `cargo test web::invalid_command_returns_error_without_disconnect` exits `0` and asserts `response.error` with `code="invalid_command"` while the socket stays open.
  - [ ] `cargo test web::mock_prompt_stream_emits_ordered_events` exits `0` and asserts the exact ordered stream `turn.started -> message.started -> message.delta+ -> message.completed -> turn.completed`.

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```text
  Scenario: Mock prompt streaming contract
    Tool: Bash
    Steps: Run `cargo test web::mock_prompt_stream_emits_ordered_events -- --nocapture`
    Expected: Test exits `0` and logs the expected ordered event sequence with stable IDs present on every emitted frame
    Evidence: .sisyphus/evidence/task-2-protocol-contract.txt

  Scenario: Invalid command handling
    Tool: Bash
    Steps: Run `cargo test web::invalid_command_returns_error_without_disconnect -- --nocapture`
    Expected: Test exits `0`; server emits `response.error` with `invalid_command`; socket remains usable for the next valid request
    Evidence: .sisyphus/evidence/task-2-protocol-contract-error.txt
  ```

  **Commit**: YES | Message: `feat(web): define browser protocol and mock socket` | Files: `src/web/protocol.rs`, `src/web/*`, `tests/*`

- [ ] 3. Extract a reusable session runtime from the CLI-owned loop

  **What to do**: Refactor `src/agent.rs` so the core prompt/tool/session flow becomes a reusable runtime module consumed by both CLI and web adapters. Keep CLI printing and browser serialization outside the runtime boundary. The runtime must accept commands equivalent to `prompt.submit` and `turn.abort`, emit structured internal events, and persist turns through the existing store path. Preserve current CLI semantics and keep `chat_loop` as a thin adapter over the extracted runtime.
  **Must NOT do**: Do not introduce browser-specific types into the runtime; do not make the CLI depend on WebSocket code; do not rewrite provider/auth logic beyond what the extraction requires.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: this is the core architectural seam and easiest place to create regressions.
  - Skills: `[]` — no extra skill is needed.
  - Omitted: `[git]` — not a git/history task.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: `4`, `5`, `6` | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/agent.rs` — current chat loop, stdout streaming, tool-call handling, and turn lifecycle.
  - Pattern: `src/main.rs` — CLI adapter that must keep working after runtime extraction.
  - Pattern: `src/store/session.rs` — session lifecycle persistence.
  - Pattern: `src/store/turn.rs` — turn persistence model including function-call and function-call-output rows.
  - Pattern: `tests/session_lifecycle.rs` — current expectations around persisted sessions and turns.
  - Pattern: `tests/sse_parsing.rs` — current streaming/parsing test style.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test runtime::submit_prompt_persists_messages` exits `0` and asserts the extracted runtime persists user and assistant turns.
  - [ ] `cargo test runtime::abort_returns_typed_result` exits `0` and asserts abort is surfaced as a typed runtime result, not a panic.
  - [ ] `cargo test cli::existing_interactive_flow_still_persists_turns` exits `0` and proves CLI parity remains intact.

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```text
  Scenario: CLI parity after runtime extraction
    Tool: Bash
    Steps: Run `cargo test cli::existing_interactive_flow_still_persists_turns -- --nocapture`
    Expected: Test exits `0` and verifies sessions/turns still persist through the CLI adapter path
    Evidence: .sisyphus/evidence/task-3-runtime-extraction.txt

  Scenario: Abort before completion
    Tool: Bash
    Steps: Run `cargo test runtime::abort_returns_typed_result -- --nocapture`
    Expected: Test exits `0`; runtime returns a typed aborted/error result without unwind, panic, or orphaned DB state
    Evidence: .sisyphus/evidence/task-3-runtime-extraction-error.txt
  ```

  **Commit**: YES | Message: `refactor(runtime): extract reusable session engine` | Files: `src/agent.rs`, `src/main.rs`, `src/runtime/*`, `tests/*`

- [ ] 4. Add a server-side runtime manager, session lease policy, and active-turn replay buffer

  **What to do**: Introduce a runtime manager keyed by `session_id` that owns the extracted runtime instances, enforces exactly one active browser controller per session, and keeps an in-memory replay buffer for the active turn so refresh/reconnect can rebuild partial assistant output. Persist completed turns to SQLite as before, but do not treat SQLite as the live event bus. Reuse `sessions.settings` for resumable web/runtime configuration before adding new schema.
  **Must NOT do**: Do not mark a session completed on socket disconnect; do not allow implicit multi-client writers; do not rely on row order or “latest function call” lookups for live correlation.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: concurrency, ownership, and replay semantics are architecture-critical.
  - Skills: `[]` — no extra skill is needed.
  - Omitted: `[git]` — not a git/history task.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: `5`, `6`, `8` | Blocked By: `2`, `3`

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/store/mod.rs` — shared store access and async-safe DB wrapper.
  - Pattern: `src/store/session.rs` — session metadata and `settings` field.
  - Pattern: `src/store/turn.rs` — transcript replay source for completed turns.
  - Pattern: `src/store/schema.rs` — persistence constraints and artifact/session tables.
  - Pattern: `tests/store_concurrency.rs` — concurrency testing style already used in the repo.
  - Pattern: `/home/zack/git/Bindery/bindery/src/web/ws.rs` — attach/bridge structure inspiration only.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web::attach_second_controller_returns_session_busy` exits `0` and asserts a second active browser controller gets a typed lease rejection.
  - [ ] `cargo test web::reconnect_replays_active_turn` exits `0` and asserts reconnect restores persisted transcript plus in-flight assistant text.
  - [ ] `cargo test web::disconnect_does_not_complete_session` exits `0` and asserts socket loss does not mutate session completion state.

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```text
  Scenario: Reconnect restores active view
    Tool: Bash
    Steps: Run `cargo test web::reconnect_replays_active_turn -- --nocapture`
    Expected: Test exits `0`; reconnect receives `state.snapshot` with prior history and buffered in-flight assistant content
    Evidence: .sisyphus/evidence/task-4-runtime-manager.txt

  Scenario: Second controller is rejected
    Tool: Bash
    Steps: Run `cargo test web::attach_second_controller_returns_session_busy -- --nocapture`
    Expected: Test exits `0`; second attach gets a typed session-busy error while the first controller remains active
    Evidence: .sisyphus/evidence/task-4-runtime-manager-error.txt
  ```

  **Commit**: YES | Message: `feat(web): add runtime manager and replay buffer` | Files: `src/runtime/*`, `src/web/*`, `src/store/*`, `tests/*`

- [ ] 5. Emit structured runtime events with explicit correlation IDs

  **What to do**: Expand the extracted runtime and `src/events.rs` integration so streaming assistant text, tool execution, turn lifecycle, transport selection/fallback, and token refresh information become structured runtime events with explicit IDs. Replace stdout-only or stderr-only side effects in the runtime path with emitted events that can be consumed by both the CLI adapter and the web adapter. Ensure tool events carry a durable `tool_call_id` and are correlated explicitly rather than by scanning for the latest function call.
  **Must NOT do**: Do not serialize WebSocket frames in the runtime; do not regress current terminal output behavior for the CLI adapter; do not keep any event relationship dependent on insertion order alone.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: modifies the core event pipeline and correlation semantics.
  - Skills: `[]` — no extra skill is needed.
  - Omitted: `[git]` — not a git/history task.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: `6`, `8` | Blocked By: `3`, `4`

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/events.rs` — current typed event emitter and receiver seam.
  - Pattern: `src/agent.rs` — current stdout/stderr streaming and tool lifecycle callsites that must emit structured runtime events.
  - Pattern: `tests/event_pipeline.rs` — existing event ordering checks.
  - Pattern: `tests/sse_parsing.rs` — streaming delta behavior and parser-focused test patterns.
  - Pattern: `/home/zack/git/Bindery/bindery/src/web/mock.rs` — event cadence inspiration for assistant/tool lifecycles.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web::prompt_stream_emits_ordered_events` exits `0` and asserts `turn.started -> message.started -> message.delta+ -> message.completed -> turn.completed`.
  - [ ] `cargo test web::tool_events_round_trip` exits `0` and asserts `tool.started` and `tool.completed` include the same `tool_call_id`, tool name, and status.
  - [ ] `cargo test runtime::transport_and_auth_events_are_exposed` exits `0` and asserts transport and token-refresh notifications surface as structured runtime events.

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```text
  Scenario: Ordered assistant stream and tool lifecycle
    Tool: Bash
    Steps: Run `cargo test web::prompt_stream_emits_ordered_events -- --nocapture`; run `cargo test web::tool_events_round_trip -- --nocapture`
    Expected: Tests exit `0` and show ordered turn/message/tool events with stable IDs reused correctly across start/end pairs
    Evidence: .sisyphus/evidence/task-5-structured-events.txt

  Scenario: Transport fallback remains structured
    Tool: Bash
    Steps: Run `cargo test runtime::transport_and_auth_events_are_exposed -- --nocapture`
    Expected: Test exits `0`; transport selection/fallback and token refresh are emitted as typed runtime events instead of only terminal output
    Evidence: .sisyphus/evidence/task-5-structured-events-error.txt
  ```

  **Commit**: YES | Message: `feat(runtime): emit structured ui-ready events` | Files: `src/agent.rs`, `src/events.rs`, `src/runtime/*`, `tests/*`

- [ ] 6. Wire the real WebSocket bridge, bootstrap snapshot, and command handlers

  **What to do**: Implement the real `/ws` path using the protocol from Task 2 and the runtime manager from Task 4. On connect, send `state.snapshot` containing session list, selected session metadata, persisted transcript, and any active-turn replay buffer. Support exactly these commands: `state.get`, `sessions.list`, `session.load`, `prompt.submit`, and `turn.abort`. Map runtime events from Task 5 into the browser-facing `UiEvent` frames. Keep all browser communication same-origin and same-binary.
  **Must NOT do**: Do not add unsupported commands; do not expose provider internals to the browser; do not rely on the database alone to reconstruct active in-flight output.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: integrates Axum WebSocket handling, runtime manager, and store-backed replay.
  - Skills: `[]` — no extra skill is needed.
  - Omitted: `[git]` — not a git/history task.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: `8`, `9` | Blocked By: `1`, `2`, `4`, `5`

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `.sisyphus/plans/bindery-ui-integration.md` — canonical command/event scope and guardrails for this work.
  - Pattern: `src/store/session.rs` — session list and load semantics for `sessions.list` and `session.load`.
  - Pattern: `src/store/turn.rs` — transcript replay source for `state.snapshot` and session loads.
  - Pattern: `src/events.rs` — current emitter seam that the web bridge consumes after runtime extraction.
  - Pattern: `src/agent.rs` — current session execution path now adapted behind the runtime manager.
  - Pattern: `/home/zack/git/Bindery/bindery/src/web/ws.rs` — WebSocket bridge structure and frame flow inspiration.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web::ws_connect_receives_bootstrap_state` exits `0` and asserts the initial frame contains session list, selected session, and transcript payload.
  - [ ] `cargo test web::session_load_replays_history` exits `0` and asserts `session.load` returns the persisted transcript for the requested session.
  - [ ] `cargo test web::abort_command_emits_turn_failed_and_runtime_stops` exits `0` and asserts `turn.abort` stops active work and emits the expected failure/completion state.

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```text
  Scenario: Bootstrap and history replay
    Tool: Bash
    Steps: Run `cargo test web::ws_connect_receives_bootstrap_state -- --nocapture`; run `cargo test web::session_load_replays_history -- --nocapture`
    Expected: Tests exit `0`; snapshot contains concrete session metadata and replayed transcript rows for the selected session
    Evidence: .sisyphus/evidence/task-6-real-ws.txt

  Scenario: Abort command over live socket
    Tool: Bash
    Steps: Run `cargo test web::abort_command_emits_turn_failed_and_runtime_stops -- --nocapture`
    Expected: Test exits `0`; runtime stops active work, emits terminal state, and keeps the socket session reusable for later prompts
    Evidence: .sisyphus/evidence/task-6-real-ws-error.txt
  ```

  **Commit**: YES | Message: `feat(web): wire runtime to websocket protocol` | Files: `src/web/*`, `src/runtime/*`, `src/store/*`, `tests/*`

- [ ] 7. Port the Bindery shell, theme, and asset into kley's served UI

  **What to do**: Copy the useful shell structure, design tokens, and icon from `/home/zack/git/Bindery/bindery/templates/index.html` and `/home/zack/git/Bindery/assets/bindery-icon.svg` into kley’s served template. Keep the first implementation as an Askama-rendered HTML shell with the Bindery-inspired layout for sidebar, transcript, inspector/status panel, tool activity area, and composer. Rewrite the client-side JS around kley’s protocol shape rather than Bindery’s RPC. Add deterministic selectors exactly as `data-testid="app-shell"`, `session-list`, `transcript`, `composer`, `composer-submit`, `abort-button`, `tool-card`, `inspector-panel`, and `status-pill`. Remove or hide unsupported UI controls entirely instead of leaving dead affordances. Retain Bindery’s Tailwind-CDN-plus-custom-theme approach for this first implementation; do not introduce a bundler pipeline in this plan.
  **Must NOT do**: Do not leave visible forking/task-session/model-switch/extension controls; do not import Bindery’s unsupported command builders; do not split the UI into a separate frontend app.

  **Recommended Agent Profile**:
  - Category: `visual-engineering` — Reason: high-leverage UI adaptation with deliberate layout and interaction cleanup.
  - Skills: `[]` — no extra skill is needed.
  - Omitted: `[git]` — not a git/history task.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: `8`, `9` | Blocked By: `1`, `2`

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `.sisyphus/plans/bindery-ui-integration.md` — canonical UI scope, selectors, and excluded features.
  - Pattern: `/home/zack/git/Bindery/bindery/templates/index.html` — primary visual shell, layout, theme tokens, and DOM structure to adapt.
  - Pattern: `/home/zack/git/Bindery/assets/bindery-icon.svg` — icon asset to copy.
  - Pattern: `src/main.rs` — same-binary serving model that the copied shell must plug into.
  - Pattern: `Cargo.toml` — dependency surface for Askama/static serving.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web::root_serves_bindery_shell_markers` exits `0` and asserts the rendered HTML contains the required `data-testid` markers and no unsupported controls.
  - [ ] `cargo test web::root_serves_bindery_icon` exits `0` and asserts the copied icon asset is reachable from the served UI.
  - [ ] `cargo build --release` exits `0` with the adapted template/assets in place.

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```text
  Scenario: Bindery-inspired shell is served
    Tool: Bash
    Steps: Run `cargo test web::root_serves_bindery_shell_markers -- --nocapture`; run `cargo test web::root_serves_bindery_icon -- --nocapture`
    Expected: Tests exit `0`; HTML contains all required `data-testid` markers and excludes unsupported controls; icon route resolves successfully
    Evidence: .sisyphus/evidence/task-7-bindery-shell.txt

  Scenario: Unsupported controls are removed
    Tool: Bash
    Steps: Run `cargo test web::root_serves_bindery_shell_markers -- --nocapture`
    Expected: Test exits `0`; unsupported UI affordances are not rendered at all
    Evidence: .sisyphus/evidence/task-7-bindery-shell-error.txt
  ```

  **Commit**: YES | Message: `feat(ui): port bindery shell into kley` | Files: `templates/*`, `static/*`, `src/web/*`, `Cargo.toml`

- [ ] 8. Wire core workspace browser behaviors to the real kley protocol

  **What to do**: Finish the client-side adapter so the served UI actually consumes `state.snapshot`, renders the session list and transcript, streams assistant deltas into the active message row, renders tool cards from `tool.started`/`tool.completed`, updates inspector/status surfaces from transport/status events, submits prompts via `prompt.submit`, loads prior sessions via `session.load`, and aborts active work via `turn.abort`. Ensure reconnect/hydration reuses the bootstrap snapshot and active-turn replay data rather than assuming a blank page.
  **Must NOT do**: Do not resurrect unsupported Bindery flows; do not keep any dead buttons; do not bypass the protocol by scraping server logs or polling ad hoc endpoints.

  **Recommended Agent Profile**:
  - Category: `visual-engineering` — Reason: UI behavior wiring across DOM state, WebSocket events, and user actions.
  - Skills: `[]` — no extra skill is needed.
  - Omitted: `[git]` — not a git/history task.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: `9` | Blocked By: `4`, `5`, `6`, `7`

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `.sisyphus/plans/bindery-ui-integration.md` — canonical event names, selectors, and excluded controls.
  - Pattern: `src/events.rs` — current event seam that now feeds the browser adapter.
  - Pattern: `src/agent.rs` — current session/tool flow that must surface browser-visible updates.
  - Pattern: `src/store/session.rs` — session switching and sidebar metadata source.
  - Pattern: `src/store/turn.rs` — transcript replay source.
  - Pattern: `/home/zack/git/Bindery/bindery/templates/index.html` — interaction behavior to selectively adapt for transcript, inspector, and composer flows.
  - Pattern: `/home/zack/git/Bindery/bindery/src/web/mock.rs` — event sequencing reference for browser state updates.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web::prompt_submit_updates_transcript_and_tool_panel` exits `0` and asserts the browser-facing state model updates transcript rows and tool activity in response to live events.
  - [ ] `cargo test web::session_load_switches_visible_history` exits `0` and asserts `session.load` swaps the active transcript without losing sidebar state.
  - [ ] `cargo test web::abort_keeps_session_reusable` exits `0` and asserts abort clears active-run UI state while allowing a later prompt in the same session.

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```text
  Scenario: Core workspace state updates
    Tool: Bash
    Steps: Run `cargo test web::prompt_submit_updates_transcript_and_tool_panel -- --nocapture`; run `cargo test web::session_load_switches_visible_history -- --nocapture`
    Expected: Tests exit `0`; transcript and tool-panel state update in response to `state.snapshot`, `message.*`, and `tool.*` events without unsupported controls appearing
    Evidence: .sisyphus/evidence/task-8-core-workspace.txt

  Scenario: Abort leaves UI reusable
    Tool: Bash
    Steps: Run `cargo test web::abort_keeps_session_reusable -- --nocapture`
    Expected: Test exits `0`; abort clears active busy state and a subsequent prompt can start in the same session
    Evidence: .sisyphus/evidence/task-8-core-workspace-error.txt
  ```

  **Commit**: YES | Message: `feat(ui): wire core workspace behaviors` | Files: `templates/*`, `static/*`, `src/web/*`, `tests/*`

- [ ] 9. Add Playwright and full browser regression coverage

  **What to do**: Introduce browser-test tooling in-repo with `playwright.config.ts` and specs under `playwright/`. Run against deterministic mock or fixture-backed server paths by default, then include at least one real-runtime smoke path that still avoids external model dependencies. Cover first-page load, session sidebar render, transcript replay, streamed assistant response, tool-card expansion, abort interaction, reconnect recovery, and absence of unsupported Bindery controls.
  **Must NOT do**: Do not depend on live external providers for primary acceptance; do not mix Playwright setup with unrelated UI refactors; do not leave selectors implicit.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: cross-stack verification with new browser tooling and fixture control.
  - Skills: `[]` — no extra skill is needed.
  - Omitted: `[git]` — not a git/history task.

  **Parallelization**: Can Parallel: NO | Wave 3 | Blocks: none | Blocked By: `6`, `7`, `8`

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `.sisyphus/plans/bindery-ui-integration.md` — canonical browser acceptance targets and selectors.
  - Pattern: `tests/harness/mod.rs` — current helper style for deterministic test support.
  - Pattern: `tests/event_pipeline.rs` — existing event assertions to mirror in browser fixtures.
  - Pattern: `src/main.rs` — app entrypoint that must expose the served UI for browser tests.
  - Pattern: `/home/zack/git/Bindery/bindery/src/web/mock.rs` — source for realistic mocked UI event sequences.
  - Pattern: `/home/zack/git/Bindery/bindery/templates/index.html` — expected shell structure the browser tests should exercise.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `npx playwright test playwright/core-workspace.spec.ts --grep "core workspace parity"` exits `0` and asserts page load, session sidebar, transcript replay, streamed response rendering, tool-card expansion, and abort behavior.
  - [ ] `npx playwright test playwright/core-workspace.spec.ts --grep "reconnect recovery"` exits `0` and asserts refresh/reconnect restores the active transcript view.
  - [ ] `cargo test cli::existing_interactive_flow_still_persists_turns` exits `0` after browser tooling lands, proving CLI parity still holds.

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```text
  Scenario: Browser core workspace parity
    Tool: Playwright
    Steps: Run `npx playwright test playwright/core-workspace.spec.ts --grep "core workspace parity"`
    Expected: Spec exits `0`; `[data-testid="session-list"]`, `[data-testid="transcript"]`, `[data-testid="composer"]`, `[data-testid="tool-card"]`, and `[data-testid="abort-button"]` all function against the served app
    Evidence: .sisyphus/evidence/task-9-playwright.zip

  Scenario: Reconnect and unsupported-control guardrail
    Tool: Playwright
    Steps: Run `npx playwright test playwright/core-workspace.spec.ts --grep "reconnect recovery|unsupported controls absent"`
    Expected: Spec exits `0`; refresh restores active view and fork/task-session/model-switch/extension controls remain absent
    Evidence: .sisyphus/evidence/task-9-playwright-error.zip
  ```

  **Commit**: YES | Message: `test(ui): add playwright core workspace coverage` | Files: `package.json`, `playwright.config.ts`, `playwright/*`, `src/web/*`, `templates/*`

## Final Verification Wave (MANDATORY — after ALL implementation tasks)
> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.
> **Do NOT auto-proceed after verification. Wait for user's explicit approval before marking work complete.**
> **Never mark F1-F4 as checked before getting user's okay.** Rejection or user feedback -> fix -> re-run -> present again -> wait for okay.
- [ ] F1. Plan Compliance Audit — oracle
- [ ] F2. Code Quality Review — unspecified-high
- [ ] F3. Real Manual QA — unspecified-high (+ playwright if UI)
- [ ] F4. Scope Fidelity Check — deep

  **F1 QA Scenario**
  ```text
  Scenario: Oracle verifies implementation against the plan
    Tool: task
    Steps: Run `task(subagent_type="oracle", load_skills=[], run_in_background=false, description="Audit plan compliance", prompt="Audit the implemented changes against .sisyphus/plans/bindery-ui-integration.md. Verify every numbered task, acceptance criterion, dependency guardrail, and excluded-scope rule. Return PASS or REJECT with concrete file references and missing items.")`
    Expected: Oracle returns PASS, or returns a concrete REJECT list that must be fixed before completion
    Evidence: .sisyphus/evidence/f1-plan-compliance.md
  ```

  **F2 QA Scenario**
  ```text
  Scenario: Independent code-quality review
    Tool: task
    Steps: Run `task(category="unspecified-high", load_skills=[], run_in_background=false, description="Review code quality", prompt="Review the implemented files for correctness, duplication, maintainability, test quality, and regression risk. Inspect Rust server/runtime code, copied/adapted UI files, and browser tests. Return APPROVE or REJECT with concrete file references and remediation items.")`
    Expected: Reviewer returns APPROVE, or returns a concrete REJECT list that must be fixed before completion
    Evidence: .sisyphus/evidence/f2-code-quality.md
  ```

  **F3 QA Scenario**
  ```text
  Scenario: Full app QA with browser and runtime checks
    Tool: task
    Steps: Run `task(category="unspecified-high", load_skills=[], run_in_background=false, description="Run final QA", prompt="Run final QA for the implemented Bindery-inspired web UI. Execute the relevant Rust tests, run Playwright core-workspace and reconnect specs, and verify the served app supports page load, session history, streaming response rendering, tool-card expansion, abort, reconnect recovery, and absence of unsupported controls. Return PASS or REJECT with evidence paths.")`
    Expected: Reviewer returns PASS with explicit mention of browser and runtime checks, or a concrete REJECT list that must be fixed before completion
    Evidence: .sisyphus/evidence/f3-final-qa.md
  ```

  **F4 QA Scenario**
  ```text
  Scenario: Scope-fidelity audit
    Tool: task
    Steps: Run `task(category="deep", load_skills=[], run_in_background=false, description="Check scope fidelity", prompt="Compare the implemented changes to .sisyphus/plans/bindery-ui-integration.md and verify the delivered work stays within scope. Confirm that fork/session-tree features, task sessions, extension runtime, image uploads, model switching UI, collaboration, and full Bindery RPC parity were not added. Return PASS or REJECT with file references.")`
    Expected: Reviewer returns PASS, or returns a concrete REJECT list identifying out-of-scope additions or missing in-scope deliverables
    Evidence: .sisyphus/evidence/f4-scope-fidelity.md
  ```

## Commit Strategy
- One commit per numbered task after its acceptance criteria pass.
- Keep protocol-shape commits separate from UI-shell commits so browser/runtime mismatches stay easy to isolate.
- Preserve a dedicated CLI-regression commit boundary before real web wiring lands.
- Do not combine Playwright setup with the first UI port; land server/runtime stability first.

## Success Criteria
- A developer can run `kley web`, open the copied/adapted Bindery-style UI, load session history, submit a prompt, observe ordered streaming/tool events, abort a turn, refresh, and recover the active view.
- CLI workflows still behave as before and continue writing durable session/turn data.
- Browser automation and Rust tests validate the contract without depending on external model providers.
