# Decisions

- Task 4 uses the existing durable `task_events.sequence` autoincrement column
  as the reconnect cursor and keeps replay scoped to
  `TaskEventRecord::list_for_task`; the repository now fails closed for unknown
  tasks and invalid per-task cursors instead of treating them as empty history.

- Implement cycle checks inside repository write APIs (not tests-only, not
  runtime-only) so every edge mutation path enforces DAG invariants against
  durable store state.
- Add explicit graph repository surfaces (`TaskRecord::list`,
  `TaskEdgeRecord::list`, `TaskEdgeRecord::replace_for_task`) to support full
  graph round-trip/readback with persisted edges, independent from attempt/event
  execution records.
- Task 3 transition enforcement is centralized at persistence boundaries
  (`TaskRecord::transition_state`, `TaskAttemptRecord::transition_state`) so
  invalid edges fail deterministically before writes, and successful transitions
  are always durable store mutations rather than runtime-memory inference.
- For Task 5 scope control, lease ownership was persisted in
  `task_attempts.recovery_checkpoint.scheduler_lease` rather than introducing
  new schema columns, keeping changes isolated to store primitives/tests.
- Claim and expiry updates were implemented as store-level atomic writes with
  optimistic CAS guards on `updated_at` and lifecycle state, preserving
  race-safe single-owner semantics from durable state.

- Implemented delegated child bootstrap in as a store-backed helper that
  transitions task/attempt into running first, then links optional child ;
  link/create failures are normalized into with in durable checkpoint metadata.

- Implemented delegated child bootstrap in runtime/session.rs as a store-backed
  helper that transitions task/attempt into running first, then links optional
  child session_id; link/create failures are normalized into Interrupted with
  retryable=true in durable checkpoint metadata.

- Task 7 exposes autonomous child-task creation through a narrow runtime helper
  that reads the parent task's persisted policy snapshot, enforces
  depth/concurrency/budget + provider/model/tool subset rules, and persists the
  derived child snapshot on the new task row; no scheduler or delegation
  entrypoint behavior was broadened.

- Task 8 scheduler execution is exposed as a focused `RuntimeManager` method
  that: selects one durable ready candidate at a time, claims ownership with
  `TaskAttemptRecord::claim_runnable_with_lease`, executes Task-6 child
  bootstrap, and maps child submit outcomes back into persisted attempt/task
  lifecycle transitions.

- Task 9 control semantics are implemented in `RuntimeManager` as explicit
  store-backed mutations: `cancel_task_graph`, `retry_task`, `resume_task`, and
  `reprioritize_task`, with reprioritize rejected outside `queued|ready` and
  retry/resume creating fresh queued attempts for the same task.

- Task 10 recovery continues to use Task-5 lease semantics
  (`mark_expired_lease_interrupted_recoverable`) as the single stale-lease
  interruption path, then creates fresh recovery attempts with carried
  checkpoint metadata so durable handoff/bootstrap state survives restart-driven
  resume.

- Task 11 exposes task lifecycle rows over the web surface as a dedicated
  `task.event` UI frame backed directly by durable `task_events` metadata, and
  only adds the minimal public mapper exposure needed for `tests/web.rs` to
  verify translation without implementing Task-12 watch/snapshot commands.

- Task 12 keeps task watch state on a dedicated websocket command (`task.watch`)
  that emits separate `task.list.snapshot` and `task.detail.snapshot` frames,
  replays durable `task.event`s from `task_events.sequence`, and suppresses
  unrelated session runtime frames while that task watch is active.
- Task 12 snapshot scoping is the connected component around the requested task
  (task-edge neighbors plus parent/child task lineage), so task watch bootstrap
  exposes relevant graph nodes/edges without leaking unrelated task rows from
  the store.
- Task 13 keeps the new CLI task surface in `src/main.rs` as a thin adapter:
  list/inspect/watch read durable task rows/events directly from the store,
  while control actions delegate to existing `RuntimeManager` control helpers
  and only add a narrow CLI-side cancel-state rejection so terminal cancels fail
  cleanly instead of silently no-oping.

- Task 14 introduces a dedicated `delegate_task` tool surface in
  `src/tools/mod.rs` but executes it in `SessionRuntime` so delegation can use
  the existing store/runtime primitives (policy checks, DAG edge validation,
  attempt lifecycle, child bootstrap) instead of creating a parallel
  orchestration path.
- `report_status` remains the progress heartbeat tool but now accepts optional
  `task_id` + `after_sequence`; runtime interception returns durable task-event
  cursor updates keyed by stable task identity.
- Task 15 Playwright tests keep the existing real web harness
  (`playwright-web-server.mjs` + `kley web`) unchanged and seed minimal
  parent/watch state from the test process so lifecycle/reconnect verification
  remains end-to-end on the production websocket protocol.
- For the Task-15 sequential-run startup fix, keep `playwright.config.ts` on a
  deterministic port and switch `webServer.reuseExistingServer` to `true` so a
  still-live prior harness instance does not fail the next invocation with
  `Address already in use`.
- Final Task-15 harness fix was implemented in `playwright-web-server.mjs`: add
  `/healthz` probing + reuse mode and robust shutdown behavior, while preserving
  real `kley web` execution for E2E coverage.
