# F2 Code Quality Review (Re-review)

Verdict: APPROVE

Reviewed modules:

- `src/runtime/session.rs`
- `src/runtime/manager.rs`
- `src/runtime/settings.rs`
- `src/store/session.rs`
- `src/store/mod.rs`
- `src/main.rs`
- `src/web/ws.rs`
- `src/web/ws/snapshot.rs`
- `tests/runtime.rs`
- `tests/store_concurrency.rs`

Reassessment of prior blockers:

1. Durable recovery settings source: fixed.

- `src/runtime/session.rs:189-216` now prefers `inherited_settings_override`,
  then durable checkpoint data via `inherited_settings_from_checkpoint(...)`,
  and only falls back to a live parent session when durable settings are absent.
- `src/runtime/manager.rs:1154-1180` and `src/runtime/manager.rs:1275-1322` now
  recover `handoff.inherited_settings` from
  `task_attempts.recovery_checkpoint.child_bootstrap` and pass it back into
  bootstrap.
- `tests/runtime.rs:2379-2505` specifically verifies recovery uses durable
  inherited settings instead of changed live parent-session settings.

2. Atomic delegation creation / orphan-row safety: fixed.

- `src/runtime/session.rs:321-339` adds `run_immediate_transaction(...)` using
  `BEGIN IMMEDIATE` with rollback on error.
- `src/runtime/session.rs:704-785` wraps child task creation, DAG edge creation,
  attempt creation, and bootstrap/session-link work in that transaction, so a
  downstream failure does not leave a committed orphan task row.
- `src/runtime/session.rs:2015-2035` includes a rollback regression test proving
  failed delegate steps do not leave the inserted task behind.

3. `max_concurrency` race safety: fixed enough for the current SQLite design.

- `src/runtime/settings.rs:377-439` removes the separate count-then-insert
  window and folds admission into a single
  `INSERT ... SELECT ... WHERE COUNT(*) < max_concurrency` write.
- That makes the policy check and child-row insert happen at one database write
  boundary instead of two separate operations.
- `tests/store_concurrency.rs:582-659` exercises concurrent spawns across
  separate store connections and expects exactly one winner when
  `max_concurrency = 1`.

Additional review notes:

- Identity separation still looks correct: `task_id` remains canonical,
  `attempt_id` remains per execution, and `session_id` is still only an optional
  execution artifact rather than task identity.
- The fixes are localized and maintainable: durable recovery parsing lives in
  one place, transactional delegation is explicit, and concurrency admission
  logic is now narrower than the previous read-then-write path.
- I did not find remaining hidden `task=session` coupling in the reviewed
  recovery, scheduler, CLI, or websocket task-watch paths.

Conclusion:

- The three blocking issues from the prior review have been addressed in code
  and covered by targeted regression tests.
- Based on the reviewed implementation, this wave is acceptable from a
  maintainability, concurrency-safety, and identity-separation standpoint.
