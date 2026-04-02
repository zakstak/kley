# F4 Scope Fidelity Review

## Verdict: APPROVE

The shipped implementation stays within the planned delegation-engine scope. I
did not find evidence of a dashboard-first UI, permission widening, a hidden
orchestration stack outside the existing runtime/store/web plumbing, or non-DAG
execution behavior.

## Plan basis checked

Reviewed `.sisyphus/plans/subagent-delegation-system.md`, especially the stated
scope and guardrails at:

- lines 27-30, 58-76 (delegation engine with CLI + API visibility, not a
  dashboard-first v1)
- lines 51-52 and 93-96 (validated DAG with cycle rejection and durable
  watch/reconnect)
- lines 71-74 and 96-98 (guarded delegation, inherited-or-narrower permissions,
  lifecycle controls)
- lines 100-109 (no permission widening, no cycle-supporting model, no separate
  orchestration stack bypassing `SessionRuntime`, `RuntimeManager`, event
  plumbing, or SQLite)

## Implementation surfaces inspected

- Store: `src/store/schema.rs:80-129`, `src/store/session.rs:543-1457`
- Runtime: `src/runtime/settings.rs:304-438`, `src/runtime/session.rs:189-297`,
  `src/runtime/session.rs:674-785`, `src/runtime/session.rs:1069-1086`,
  `src/runtime/manager.rs:446-592`, `src/runtime/manager.rs:955-1455`
- Web/API: `src/web/protocol.rs:56-88`, `src/web/protocol.rs:395-430`,
  `src/web/ws.rs:911-1510`, `src/web/ws/snapshot.rs:124-365`,
  `src/web/ws/event_map.rs:220-239`
- CLI/tooling: `src/main.rs:338-427`, `src/tools/mod.rs:20-76`,
  `src/tools/report_status.rs:19-46`
- UI shell: `src/web/ui.rs:10-27`, `templates/index.html` (searched for
  `task|delegate|dashboard`, no matches)
- Tests: `tests/runtime.rs:868-1022`, `tests/runtime.rs:1667-1935`,
  `tests/runtime.rs:2656-3062`, `tests/store_concurrency.rs:277-372`,
  `tests/store_concurrency.rs:662-1020`, `tests/web.rs:1856-2485`,
  `tests/web.rs:2487-3154`, `tests/cli.rs:487-613`,
  `playwright/core-workspace.spec.ts:595-817`

## Scope fidelity checks

### 1. No dashboard-first UI shipped

- The web additions are protocol/event surfaces (`task.watch`, `task.cancel`,
  `task.retry`, `task.resume`, `task.reprioritize`) in
  `src/web/protocol.rs:56-88`, with watch snapshots/events in
  `src/web/ws.rs:1135-1255` and `src/web/ws/snapshot.rs:124-365`.
- `src/web/ui.rs:10-27` still renders the existing shell template only, and the
  main HTML template has no `task`, `delegate`, or `dashboard` matches.
- Browser coverage in `playwright/core-workspace.spec.ts:636-642` and `:723-729`
  drives task watch through websocket probe commands, not through a new
  dashboard UI.

### 2. No permission widening

- Child policy derivation in `src/runtime/settings.rs:304-349` and `:461-483`
  enforces subset-only providers/models/tools, forbids widening
  `tool_approval_mode`, and forbids changing `parent_close_policy`;
  `src/runtime/settings.rs:390-438` persists the derived child snapshot onto the
  child task row.
- Delegation entrypoint usage in `src/runtime/session.rs:703-745` passes
  requested policy through that narrowing path before task creation/bootstrap.
- Tests explicitly cover rejected widening attempts in
  `tests/runtime.rs:2779-2853` and successful narrowed delegation in
  `tests/runtime.rs:2856-2985`.

### 3. No hidden orchestration stack

- `delegate_task` is not implemented as a separate tool executor;
  `src/tools/mod.rs:74-75` marks it runtime-handled, and
  `src/runtime/session.rs:1069-1086` intercepts it inside existing
  `SessionRuntime` tool handling.
- Delegated task creation/bootstrap runs inside store-backed runtime code in
  `src/runtime/session.rs:189-297` and `:703-745`, using durable
  task/edge/attempt/event rows from `src/store/schema.rs:80-129` and
  `src/store/session.rs:543-1457`.
- Scheduler/control behavior lives in existing `RuntimeManager` methods
  (`src/runtime/manager.rs:446-592`, `:594-1455`), and web/CLI surfaces either
  call those helpers or read durable store state (`src/web/ws.rs:1390-1509`,
  `src/main.rs:338-427`). I did not find a parallel service, alternate
  scheduler, or non-store execution path.

### 4. No non-DAG behavior

- The persisted model is explicitly edge-based (`task_edges`) in
  `src/store/schema.rs:95-103`.
- Writes reject cycles in `src/store/session.rs:746-952` via
  `validate_task_graph_acyclic`, so cyclic edges fail before persistence.
- Scheduler execution in `src/runtime/manager.rs:1081-1185` only advances tasks
  when all dependencies are `completed`; otherwise runnable nodes are
  forced/stay `blocked`.
- Tests cover arbitrary-depth DAG persistence and cycle rejection
  (`tests/runtime.rs:868-1022`), ready-node scheduling plus blocked-node
  withholding (`tests/runtime.rs:1667-1935`), and serialized descendant control
  behavior across DAG edges (`tests/store_concurrency.rs:662-1020`).

## Conclusion

The implementation matches the intended delegation-engine scope: durable task
graph + attempts/events persistence, runtime scheduling/recovery, delegated
child bootstrap, CLI/API visibility and controls, and test coverage around
lifecycle/reconnect/recovery. No implementation-level scope violation was found,
so this re-review is **APPROVE**.
