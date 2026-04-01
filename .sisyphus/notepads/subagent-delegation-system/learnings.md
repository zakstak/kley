# Learnings

- Runtime integration tests instantiate an in-memory `Store` via
  `Store::open_memory()` (or a shared `Arc<Mutex<_>>`) and wire it through a
  `SessionRuntime::{new,new_with_shared_store_and_abort_signal}` helper plus
  `event_channel`. After runtime actions they query
  `Session::get`/`Turn::list_for_session` to assert messages, tool calls, and
  persisted assistant output, making these lookups the canonical way to verify
  schema persistence and status updates.
- Failure-path coverage lives inside `tests/runtime.rs`: abort signals yield
  `SubmitResult::Aborted`, `AgentEvent::TurnFailed { error: "aborted" }`, and
  long-tool checks ensure denials never run the slow tool; `on_tool_approval`
  returns `false` so turns include a `function_call_output` record and
  `AgentEvent::ToolCallCompleted` with `success == false`, which can be re-used
  when schema rejects requests.
- Store concurrency tests rely on `SharedStore = Arc<Mutex<Store>>` plus
  `store::store_run(&shared, |s| … )` wrappers to keep SQLite threadsafe. They
  spawn tokio tasks (e.g., `tokio::spawn` loops) to create sessions in parallel,
  then re-run `store_run` queries to confirm consistent session counts and safe
  reads, offering a pattern for durable-state and schema concurrency doors.
- `src/store/schema.rs` keeps migrations in the `MIGRATIONS` array and tracks
  progress via the `_schema_version` table, so every schema change (new tables +
  indexes) is a positional entry. `migrate` iterates migrations in order,
  applies each `CREATE TABLE` batch, and records the version, which is the
  template to follow for durable task tables that must ship with
  `task_id`/`attempt_id` columns plus policy/recovery metadata.
- `src/store/session.rs` and `src/store/turn.rs` show the row-mapping
  conventions: each persisted struct has a `from_row` helper that parses RFC3339
  timestamps and turns raw strings into enums (e.g., `SessionStatus` via
  `RowParseError`). `Session::create` seeds a stable UUID `id` while
  `Turn::append` assigns `turn_number` with
  `SELECT COALESCE(MAX(turn_number), 0) + 1` to keep per-run ordering. The same
  pattern (stable canonical id + execution-scoped sequence) should guide
  `task_id` vs `attempt_id`, and any new `policy`/`settings` JSON fields can
  reuse the `settings` column treatment.
- Runtime/store tests (`tests/runtime.rs`, `tests/store_concurrency.rs`, and the
  store module tests in `src/store/mod.rs`) all rely on `Store::open_memory()`
  plus `Session::get`/`Session::list`/`Turn::list_for_session` after runtime
  actions to assert persistence. Notably, `tests/runtime.rs` verifies
  `submit_prompt_persists_messages`,
  `context_overflow_retries_with_harder_compaction`, and
  `on_tool_approval_denies_execution` by reading back `turns` and
  `function_call_output`, while `tests/store_concurrency.rs` and the store
  module `test_turn_round_trip` show how to wrap `store::store_run` for
  thread-safe reads/writes. These are the go-to references for verifying new
  rows, indexes, and concurrency invariants once the durable task tables land.
- Durable task persistence follows the same pattern cleanly in
  `src/store/session.rs`: each record type (`TaskRecord`, `TaskEdgeRecord`,
  `TaskAttemptRecord`, `TaskEventRecord`) has a typed constructor and `from_row`
  timestamp parsing, while schema-level integrity (FK + NOT NULL + sequence
  AUTOINCREMENT) handles invalid-link rejection and durable event ordering
  without runtime-only assumptions.
- Task 4 replay safety depends on validating the cursor against the requested
  task, not just filtering `sequence > ?`: because `task_events.sequence` is
  globally monotonic, reconnect replay must reject missing task streams and any
  `after_sequence` that does not belong to that task, otherwise a watcher can
  silently skip or misapply events after reconnect.
- Task-2 graph safety works best in the repository layer:
  `TaskEdgeRecord::{create,replace_for_task}` now validates the full persisted
  edge set plus the proposed write before insert/delete, so cycle rejection
  happens deterministically at write time instead of relying on
  scheduler/runtime behavior.
- Keeping graph metadata independent of execution state is straightforward when
  task upserts are isolated to `tasks` rows (`TaskRecord::create_or_update`) and
  edge reads/writes stay in `task_edges`; Task-2 tests can prove DAG persistence
  without touching attempts/events lifecycle semantics.
- Task 3 uses explicit `TaskLifecycleState` / `AttemptLifecycleState` enums and
  guarded `transition_state` APIs in `src/store/session.rs`; attempt transitions
  update `task_attempts.status` and append `attempt.state.transition` events,
  while task transitions are durable via `task.state.transition` events plus
  `tasks.updated_at` updates.
- A practical durability pattern here is defaulting a task with no transition
  events to `queued`, then persisting every legal state hop as append-only
  events. This avoids session-status inference while keeping `task_id` stable
  and `attempt_id` run-scoped.
- Task 5 can add durable scheduler ownership without schema churn by embedding a
  `scheduler_lease` object inside `task_attempts.recovery_checkpoint` and
  updating it atomically alongside status transitions.
- Single-winner claims are reliably enforced by compare-and-swap updates on
  `(attempt_id, status, updated_at)` before appending lease/state events.
- Lease expiry recovery is safer when it first marks `running -> interrupted`
  with `recoverable=true` in durable metadata; this prevents immediate
  double-claims while leaving a deterministic path back to `ready`.

- Task 6 handoff bootstrap is safest when child initialization persists a
  bounded checkpoint (summary + artifact ids + inherited settings snapshot) on
  before any session link, so retry paths never depend on parent transcript
  replay.

- Task 6 handoff bootstrap is safest when child initialization persists a
  bounded child_bootstrap checkpoint (summary + artifact ids + inherited
  settings snapshot) on task_attempts.recovery_checkpoint before any session
  link, so retry paths never depend on parent transcript replay.

- Task 7 policy inheritance can stay schema-stable by treating
  `tasks.policy_snapshot` as the durable source of truth for delegation
  guardrails and parsing it into a typed runtime snapshot only at evaluation
  time; child tasks then persist their narrowed snapshot back into the same task
  field instead of inventing a memory-only cache.

- Task 8 scheduler integration can stay narrow by running DAG readiness checks
  from persisted `task_edges` + `TaskRecord::current_state`, normalizing
  runnable attempts to `ready`, and then using Task-5 CAS lease claims plus
  Task-6 `bootstrap_delegated_child_session` before launching child runtime
  execution.

- Task 9 lifecycle controls are safest when they only read/write through durable
  `TaskRecord`/`TaskAttemptRecord` transitions under the shared-store mutex;
  retry/resume should mint new `attempt_id`s while keeping stable `task_id`
  identity, and cancel propagation should traverse persisted `task_edges`
  dependents (DAG descendants) deterministically.

- Task 10 restart recovery remains store-driven by first reconciling nonterminal
  attempts (`recover_nonterminal_attempts_on_startup` ->
  `reconcile_nonterminal_attempts`), then scheduling from persisted
  graph/attempt state; the scheduler reuses
  `task_attempts.recovery_checkpoint.child_bootstrap` handoff/session linkage
  instead of replaying raw parent transcript history.

- Task 11 can stay narrow by translating durable `task_events` rows into a
  single `AgentEvent::TaskLifecycle` carrier (`sequence`, `task_id`,
  `attempt_id`, optional child session id, `event_type`, raw payload,
  recorded_at) and deferring JSON parsing to `src/web/ws/event_map.rs`; that
  keeps existing turn-event behavior untouched while exposing richer task
  metadata for graph, readiness, claim, control, and recovery events.

- Task 12 watch recovery can stay durable-only by bootstrapping snapshots from
  store state, setting the active watch cursor to the task's latest persisted
  `task_events.sequence`, and polling
  `TaskEventRecord::list_for_task(task_id, last_sequence)` for live replay; this
  avoids duplicate delivery across reconnects without needing transient
  broadcast history.
- Adding a new `WebCommand` variant also touches `src/web/mock.rs`: its
  exhaustive command match must get a narrow fallback arm, or the `web`
  integration test target fails to compile before the Task-12 assertions can
  even run.
- Task 13 CLI coverage works best as a real subprocess test seeded through
  `Store::open()` under a temp `HOME`, because the task commands read the
  durable on-disk SQLite store rather than test-only in-memory state.
- Task 13 output needs explicit field labels (`task_id`, `attempt_id`,
  `child_session_id`, `lease_owner`) in both summaries and event lines;
  otherwise task identity, execution attempt identity, and scheduler lease
  ownership are too easy to blur together in plain-text CLI output.

- Task 14 delegation entrypoint can stay first-class and durable by handling
  `delegate_task` at runtime dispatch time: spawn via policy-checked
  `spawn_autonomous_child_task_with_policy`, attach graph dependency via
  `TaskEdgeRecord::create` (DAG-validated), create a durable attempt row, then
  bootstrap child context with bounded handoff in
  `task_attempts.recovery_checkpoint.child_bootstrap`.
- Stable outcome reporting by `task_id` is cleanly reused through
  `report_status` interception that reads
  `TaskEventRecord::list_for_task(task_id, after_sequence)` and returns
  cursor-based `next_after_sequence`, avoiding session/attempt-only transient
  identifiers.
- Task 15 browser coverage is most reliable when Playwright captures real `/ws`
  frames and drives `task.watch` directly over the live socket; this validates
  request/response cursors, list/detail snapshots, and replay semantics without
  adding UI-only controls.
- After transport reconnect, `task.watch` must use the reattached
  `state.snapshot.session_id` from the new socket, not a previously captured
  session id, or the server correctly rejects with `invalid_session`.
- Playwright harness reliability for sequential Task-15 runs is better with a
  single deterministic `PLAYWRIGHT_WEB_PORT` plus `reuseExistingServer: true`;
  random fallback ports in config can diverge between config evaluation contexts
  and break `baseURL` vs `webServer` alignment.
- Harness startup reliability needed script-level handling too: if
  `127.0.0.1:<port>/healthz` is already healthy, `playwright-web-server.mjs`
  should enter reuse mode instead of spawning a second `kley web` process that
  fails bind.
- Teardown reliability matters for sequential verification: on SIGTERM/SIGINT,
  the harness script now exits deterministically after signaling the child
  process tree, preventing Playwright commands from hanging between runs.

- F2 follow-up: delegated child bootstrap should prefer durable
  `task_attempts.recovery_checkpoint.child_bootstrap.handoff.inherited_settings`
  over live parent session settings whenever present; parent-session fallback
  should only run when checkpoint settings are absent.
- F2 follow-up: making `delegate_task` creation robust against partial writes is
  straightforward with an explicit `BEGIN IMMEDIATE` transaction wrapping child
  task row creation, DAG edge write, attempt creation, and bootstrap
  checkpoint/session-link writes; any downstream error rolls back the task row
  automatically.
- F2 follow-up: `max_concurrency` admission can be made race-safe across
  separate SQLite connections by folding the active-child count predicate into
  the `INSERT ... SELECT ... WHERE count < max_concurrency` statement so
  check+insert become one atomic write boundary.

- F1 follow-up: the missing API half of Task 12 stays thin by adding
  `task.cancel`, `task.retry`, `task.resume`, and `task.reprioritize` websocket
  commands that only call `RuntimeManager` control helpers and then read back
  durable task state for the ack payload.
- F1 follow-up: API cancel needs the same narrow precheck the CLI already uses
  for completed/cancelled/failed/cancel_requested tasks; otherwise
  `cancel_task_graph` is intentionally idempotent and would silently accept
  terminal-task cancels instead of rejecting invalid state at the control
  surface.
