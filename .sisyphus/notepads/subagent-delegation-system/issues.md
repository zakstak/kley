# Issues

- Task 4 regressed `web::reconnect_bootstrap_skips_persisted_history_replay`
  because the test was expanded with unrelated reconnect/runtime assertions
  (`active_turn` null and post-completion abort behavior). Keeping Task-4
  changes limited to durable task-event replay primitives avoids breaking
  existing websocket bootstrap semantics.
- Task 5 test closures that pass timestamps into `store::store_run` need
  `move`-captured owned values (`'static` closure bound); borrowing `base_now`
  directly causes E0373 in integration tests.

- Initial Task-6 failure path surfaced that linking an invalid child returned a
  raw FK error; fixed by catching link errors in bootstrap flow and forcing
  deterministic interrupted/retryable handling instead of bubbling the SQL
  error.

- Initial Task-6 failure path surfaced that linking an invalid child session_id
  returned a raw FK error; fixed by catching link errors in bootstrap flow and
  forcing deterministic interrupted/retryable handling instead of bubbling the
  SQL error.

- Task-8 implementation detail: dependency withholding for unsatisfied
  prerequisites must avoid invalid queued->blocked transitions by only forcing
  blocked when task/attempt were already runnable (`ready` or `running`); queued
  nodes are withheld until they can validly move to `ready`.

- Task-9 race gotcha: repeated cancel requests against the same running task can
  accidentally collapse `cancel_requested -> cancelled` too early unless cancel
  handling treats `cancel_requested` as sticky/idempotent for still-active
  attempts.

- Task-10 ordering gotcha: parent-close recovery must request descendant
  cancellation (`request_descendant_cancellation_before_recovery`) before
  recovery scheduling claims unrelated runnable attempts, or event ordering can
  violate the expected cancellation-before-recovery guarantee.

- Task-12 gotcha: exact-match integration tests in `tests/web.rs` need top-level
  wrapper functions when the real async helpers live inside the nested `web`
  module; otherwise `cargo test -- --exact ...` filters them out as zero tests
  run.
- Task-13 gotcha: exact-match CLI acceptance tests also need top-level test
  functions in `tests/cli.rs`; nesting them under a module risks the
  `cargo test --test cli -- --exact ...` plan commands missing the intended test
  names.

- Task-14 gotcha: task event rows (`TaskEventRecord`) are not `Serialize`, so
  runtime tool responses must map rows into explicit JSON payloads before
  embedding them in tool outputs.
- Task-15 gotcha: transcript assertions can falsely pass on delegated task ids
  because the user control-block prompt echoes those ids; lifecycle assertions
  should rely on websocket `response.ok`/`task.*` frames and tool status, not
  raw transcript substrings.
- Task-15 reliability gotcha: generating fallback Playwright ports inside
  `playwright.config.ts` can cause `webServer` to bind one port while tests
  navigate another (`ERR_CONNECTION_REFUSED`) if config is evaluated in separate
  contexts.
- Task-15 harness gotcha: `reuseExistingServer: true` by itself may still fail
  if the startup script always attempts `cargo run ... --bind`; script-level
  pre-bind health detection is required to avoid `Address already in use`
  failures.

- F3 verification rerun on 2026-04-03: the required Playwright command for
  `task delegation lifecycle|task watch survives reconnect` failed because the
  browser `task.watch` request returned `response.error` instead of
  `response.ok`; both captured error contexts showed `error: task not found`
  immediately after `delegate_task` completed.

- F1 verification: the plan-named acceptance command
  `cargo test --test runtime -- --exact delegated_task_links_child_session_after_attempt_start`
  currently matches 0 tests; nearest semantic coverage is
  `delegated_task_links_child_session_without_forcing_attempt_running` in
  `tests/runtime.rs`.
- F1 verification: both Task-15 Playwright commands currently fail because
  `seedDelegationParentTask` inserts parent tasks without `owner_session_id`,
  delegated children inherit that NULL owner in
  `spawn_autonomous_child_task_with_policy`, and `task.watch` requires
  `TaskRecord::get_owned_by_session`, yielding `task not found`.

- Verification gotcha: `playwright.config.ts` uses `reuseExistingServer: true`,
  so reruns can attach to an older `kley web` process on port `3211`; when the
  code under test changes, kill the stale listener before rerunning the
  delegation lifecycle specs or the browser lane may exercise pre-patch
  behavior.
- Task-15 teardown gotcha: the previous harness leak came from two interacting
  choices in `playwright-web-server.mjs` — detaching `kley web` into its own
  session and letting it inherit/survive outside Playwright-managed process
  cleanup. Fresh-port verification is required to catch this class of leak.
