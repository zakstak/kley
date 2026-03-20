# Bindery-UI Integration Problems / Issues / Gotchas

## 1. No Abort Mechanism Exists
- **Severity**: High — this is a core requirement for Tasks 3, 4, 5, and 6
- **Location**: `src/agent.rs:126-441` — `chat_loop()` has no cancellation channel
- **Current exit paths**: Ctrl+D (EOF), autonomous error limit, autonomous turn limit
- **`SessionStatus::Aborted`** is defined in `src/store/session.rs:28,38,53` but NEVER written
- **Fix required**: Runtime must accept a cancellation signal and return a typed abort result. The plan's acceptance criteria for Task 3 explicitly requires `runtime::abort_returns_typed_result`.

## 2. No Structured Events for Streaming/Tool Lifecycle
- **Severity**: High — Tasks 5 and 6 depend on this
- **Location**: `src/agent.rs:676-681` (WS delta), `854-861` (SSE delta), `980-998` (ZAI delta) — all just `print!()` + `full_response.push_str()`
- **Tool lifecycle**: `src/agent.rs:287-353` — only `eprintln!()` for tool calls, no events
- **`src/events.rs:12-41`**: `AgentEvent` has NO `MessageDelta`, `MessageStarted`, `MessageComplete`, `ToolStarted`, `ToolComplete` variants
- **Fix required**: Expand `AgentEvent` enum and emit structured events from the streaming parsers and tool execution path.

## 3. rusqlite Connection is !Send
- **Severity**: Medium — relevant for Task 4 runtime manager with async
- **Location**: `src/store/mod.rs:21` — `SharedStore = Arc<Mutex<Store>>` workaround
- **Implication**: The `store_run()` async helper already exists and works correctly (tested in `tests/store_concurrency.rs`), but the agent itself uses sync `Store` inside async context. After runtime extraction, the runtime manager may need to use `store_run()` for async-safe access from async contexts.
- **Risk**: Low — pattern is already established and tested.

## 4. Stdout/Stderr Coupling in Transport Functions
- **Severity**: Medium — extraction complexity
- **Location**: `src/agent.rs:678-680` (WS: `print!{delta}`, `stdout().flush()`), `857-860` (SSE), `995-997` (ZAI)
- **Issue**: Streaming functions directly print to stdout instead of returning deltas or emitting events
- **Fix**: Refactor streaming parsers to return deltas via a callback or channel, not print statements. The accumulated text is already available for persistence.

## 5. Tool Call Correlation by "Latest Function Call" is Fragile
- **Severity**: Medium — plan explicitly forbids this pattern (guardrail #8)
- **Location**: `src/agent.rs:475-485` — `history_items_from_turns()` looks backward in built items to find `call_id` for function_call_output
- **Plan guardrail**: "Do not infer tool/message correlation by 'most recent row' or 'latest function call'"
- **Fix**: Tool call events must carry explicit `tool_call_id` from the API, and output persistence must use that explicit ID. The `ToolCall` struct already has `call_id` — the fix is using it consistently and emitting it as a structured event.

## 6. Event Channel Uses std::sync::mpsc (Not tokio)
- **Severity**: Low — works fine but worth noting
- **Location**: `src/events.rs:8,104,114` — `std::sync::mpsc::channel()`
- **Current usage**: Works because CLI spawns a `std::thread` for event consumption
- **Risk for web**: In async context, if the web handler tries to `recv_blocking()` on this channel from a tokio task, it may not integrate cleanly with tokio's cooperative scheduling. Consider whether `tokio::sync::mpsc` would be better for the runtime's event channel, or keep `std::mpsc` for CLI and add a `tokio::sync::watch` for broadcast to web.
- **Note**: `EventReceiver::recv_blocking()` (line 127) calls `self.rx.recv()` which blocks the thread — this is the CLI pattern.

## 7. Turn Number Assigned After Insert
- **Severity**: Low — already handled correctly
- **Location**: `src/store/turn.rs:68-73` — reads back `turn_number` after INSERT
- **Issue**: This is actually correct (avoids TOCTOU) but adds a round-trip per turn. For high-frequency web clients, consider batched persistence. Not a blocker for initial implementation.

## 8. No Web/UI Module Exists Yet
- **Severity**: N/A — this is what Tasks 1 and 2 create
- **Note**: `Cargo.toml` already has `axum = "0.7"` so Task 1 can begin immediately
- **Note**: No `src/web/` directory exists yet

## 9. Authentication is Tightly Coupled to CLI
- **Severity**: Low — `auth::resolve_auth()` (src/auth/mod.rs:293) is async and already takes `events: &EventEmitter` for token refresh events
- **Location**: Called at `src/agent.rs:135` inside `chat_loop()`
- **Extraction consideration**: Runtime should own the auth resolution since it needs `ResolvedAuth` for API calls. The CLI adapter passes a pre-resolved auth or the runtime resolves it internally.

## 10. Autonomous Mode Re-injection is Magic String
- **Severity**: Low — cosmetic
- **Location**: `src/agent.rs:435` — hardcoded `"Acknowledged. Continue to the next improvement."`
- **Note**: Not relevant for web mode but worth documenting as a known constant if the UI needs to trigger autonomous continuations.

## Unresolved Issues and Technical Debt (2026-03-18)

### P1: Template Size — 2483 Lines Is Large for MVP

The `index.html` template is 2483 lines (928 CSS + ~2000 JS). Adapting it fully is a significant undertaking. Risk: executor spends too much time adapting unused JS (dialog, extension, fork, task, attachment flows).

**Mitigation**: Strip aggressively. Target a minimal shell with: status strip, stream/transcript area, inspector drawer shell, composer form. Remove ~1500 lines of Bindery-specific JS before adapting. Use the shell DOM structure (CSS classes) without porting the JS.

**Status**: OPEN — executor should plan aggressive template trimming.

### P2: event_id — Bindery Doesn't Have It, kley Plan Specifies It

Bindery events use implicit ordering for correlation (no `event_id`). The kley plan requires explicit `event_id` on all events. This means the mock socket needs to generate UUIDs or incrementing IDs. Not complex, but needs to be built.

**Status**: OPEN — Task 2 must implement event_id generation in mock socket.

### P3: `message.delta` vs Bindery's `message_update` — Mapping Needed

Bindery sends `message_update` events for streaming text deltas. The plan specifies `message.delta`. The plan also includes `message.started` and `message.completed`. Need to confirm: does `message.delta` replace `message_update` 1:1? Is `message_update` ever sent without `message_start` first?

From mock.rs line 975: `message_update` is sent after `message_start` with partial accumulated text. This matches `message.delta` semantics. Map `message_update` → `message.delta`.

**Status**: RESOLVED — `message.delta` = Bindery `message_update`.

### P4: `status.report` vs `extension_ui_request` (notify/setWidget) — Scope Uncertainty

The plan maps `extension_ui_request` (notify/setWidget) → `status.report`. But "status report" is vague. What specifically does it carry? From mock.rs: `notify` carries `message` string + optional `notifyType`. `setWidget` carries `widgetLines[]`.

For kley, a `status.report` event could carry: `{ key: string, value: string }` pairs. Need to define the exact shape in Task 2.

**Status**: OPEN — Task 2 must define `status.report` payload shape.

### P5: Context Percent — Bindery Has It, kley Plan Doesn't Mention

Bindery emits `contextPercent` on multiple events (tool_execution_end, message_end, agent_start/end). The kley plan doesn't mention context reporting. Should kley emit it?

If kley has access to context window stats, emitting `contextPercent` in `status.report` events would be valuable. But if the current runtime doesn't expose this, it's a new feature, not scope creep.

**Status**: OPEN — executor should determine if context stats are available in kley's runtime.

### P6: Client-Side Event Rendering vs. Inspector Metadata

Bindery's `with_bindery_meta()` adds `{kind, title, preview}` to every event before sending to browser. kley could either:
- A) Emit flat events with `kind`/`title`/`preview` directly (Decision D5)
- B) Keep events clean and let client compute display metadata from raw fields

Option B is cleaner for event consumers (CLI tools, tests, etc.). Option A is simpler for the browser client. Need to decide before Task 8.

**Status**: OPEN — recommend Option B (clean events, client computes display) for flexibility.

### P7: `sessions.list` and `session.load` — What Does Bootstrap Include?

Plan says `state.snapshot` contains "session list, selected session metadata, persisted transcript, and any active-turn replay buffer." The Bindery `get_state` response includes: `sessionName`, `sessionFile`, `sessionId`, `model`, `isStreaming`, `contextPercent`.

kley's bootstrap needs to include: session list (from store), selected session metadata, persisted transcript rows, active-turn buffer (in-memory). No model info (kley may not track it the same way).

**Status**: OPEN — Task 6 must define exact `state.snapshot` payload shape.

### P8: Icon vs. Favicon — Two Different Assets

The `bindery-icon.svg` (10-line book) and the inline data-URI favicon in the template are different. The template embeds a more detailed SVG as a data-URI. The plan references copying the SVG file.

Need to decide: copy `bindery-icon.svg` (simpler book), or use the template's data-URI as the favicon (more detailed, matches the UI brand)? Recommendation: copy the file, it's cleaner and matches the plan.

**Status**: RESOLVED — copy `bindery-icon.svg` to `static/`.

## Task 2 protocol follow-ups (2026-03-19)

### P9: `state.snapshot` payload is intentionally minimal in mock mode

- **Current shape in mock**: `state.snapshot` includes `protocol_version`, selected `session_id`, and a compact `sessions[]` list.
- **Open detail for Task 6**: bootstrap also needs persisted transcript rows + active-turn replay content once the runtime manager is wired.
- **Status**: OPEN — expand snapshot shape during real runtime bridge work.

### P10: `status.report` schema is defined but not yet emitted by mock flow

- **Current type**: `status.report` is represented as `{ status, detail }` plus `event_id/ts/session_id`.
- **Open detail**: exact producer mapping from runtime events (`report_status`, transport/auth state, compaction summaries) has not been decided yet.
- **Status**: OPEN — lock final producer-to-payload mapping when Tasks 5/6 event bridge is implemented.

### P11: Compatibility shim required due parallel runtime extraction churn

- During Task 2 verification, `src/agent.rs` became unavailable/unstable in the workspace while protocol work was in progress, which blocked `cargo test` compilation even for websocket tests.
- Added a temporary compatibility shim in `src/agent.rs` to keep build/test paths available for Task 2 protocol validation without wiring the real web runtime path.
- **Status**: OPEN — remove/replace shim once Task 3 runtime extraction lands cleanly and stabilizes the CLI adapter surface.

## Task 3 runtime constraints (2026-03-19)

### P12: Typed abort exists, in-flight interruption still needs manager coordination

- `SessionRuntime::abort_turn()` now returns a typed `AbortResult` and persists `SessionStatus::Aborted`.
- Direct interruption of an already-running network request still needs runtime-manager orchestration in later tasks to support truly concurrent `turn.abort` while submit is executing.
- **Status**: OPEN — finalize active-turn cancellation semantics in Task 4+.

### P13: Runtime hook boundary exists; tool stderr coupling not fully removed

- Streaming/model output now crosses the runtime boundary through `RuntimeHooks::on_output_delta` instead of direct transport `print!` calls.
- `report_status` still emits stderr from the tool implementation itself, so not all user-facing progress is hook/event-driven yet.
- **Status**: OPEN — complete structured event coverage in Task 5.

## Task 4 remaining edge cases (2026-03-19)

### P14: `session_not_found` detection currently string-matched

- `src/web/ws.rs::load_session_for_controller()` currently maps missing sessions by matching `"session not found"` in the error string returned through `store_run`.
- This is stable with current `Session::get()` context text, but brittle if that error string changes.
- **Status**: OPEN — consider a typed store error variant or dedicated `Session::exists` helper in a follow-up.

### P15: Runtime manager stores runtime metadata + replay, not a long-lived async runtime task yet

- `RuntimeManager` owns per-session runtime identity (`session_id/settings`) and active-turn replay buffer, and enforces single active controller.
- It does not yet host a dedicated long-lived runtime worker loop per session; that bridge remains for Task 6 real runtime event wiring.
- **Status**: OPEN — acceptable for Task 4 scope, but full runtime-worker ownership should be finalized when real websocket-runtime bridging lands.

### P16: Legacy `function_call_output` rows may lack explicit `call_id` metadata

- New writes now persist `function_call_output` as JSON with both `call_id` and `output`, but older rows can still be plain text.
- `history_items_from_turns` now parses explicit `call_id` when present and falls back to an empty `call_id` for legacy rows.
- **Status**: OPEN — acceptable for Task 5 scope, but a backfill/migration would be needed for fully explicit historical correlation.

### P17: Runtime event IDs are generated at web-bridge mapping layer

- Runtime now emits explicit correlation IDs (`session_id`, `turn_id`, `message_id`, `tool_call_id`) but does not yet emit stable `event_id` values.
- `src/web/ws.rs` assigns `event_id`/`ts` while mapping `AgentEvent -> UiEvent` to keep protocol requirements satisfied.
- **Status**: OPEN — acceptable for Task 5 since ordering/correlation is explicit, but cross-adapter deterministic `event_id` generation can be centralized in Task 6.

## Task 6 remaining bridge risks (2026-03-19)

### P18: Abort is cooperative for real provider transports

- `turn.abort` now stops the test-provider path deterministically and emits `turn.failed` through the real websocket bridge, but OpenAI/ZAI network requests are still only observed at runtime checkpoints rather than being force-cancelled at the HTTP/WebSocket transport layer.
- This is enough for the current Task 6 websocket contract and tests, but a future pass should thread hard cancellation into the provider clients if web abort needs immediate mid-request teardown for live credentials.
- **Status**: OPEN — explicit risk to revisit when expanding live-runtime QA beyond fixture-backed coverage.

## Task 7 scope guard notes (2026-03-19)

- No new unresolved runtime problems were introduced during the shell/icon port.
- Known follow-up remains Task 8 behavior wiring; Task 7 intentionally leaves selector anchors static and protocol-agnostic.
