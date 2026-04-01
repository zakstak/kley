# Task / Subagent Delegation Workflow Engine

## TL;DR

> **Summary**: Add a durable task-graph orchestration layer on top of the
> existing session/runtime system so the main agent can delegate fresh-context
> work to child sessions without losing parent context, while exposing live task
> state through CLI and API surfaces. **Deliverables**:
>
> - durable task/edge/attempt/event persistence
> - runtime scheduler with guarded autonomous delegation
> - full lifecycle controls: observe, cancel, retry, resume, reprioritize
> - restart-safe recovery and cursor-based watch/reconnect
> - TDD-backed Rust + browser verification **Effort**: XL **Parallel**: YES - 3
>   waves **Critical Path**: 1 → 2 → 3 → 8 → 10 → 12 → 13 → 15

## Context

### Original Request

Enable handing tasks off to a subagent so the main agent preserves context,
delegated work gets a fresh context, the system behaves intelligently like
omo/codex-style task systems, and running work is easily exposed.

### Interview Summary

- Canonical model: workflow engine.
- Running work must be exposed via CLI + API; a dedicated dashboard is not
  required in v1.
- Lifecycle controls required in v1: observe, cancel, retry, resume,
  reprioritize.
- Delegated work is a first-class task record; execution may attach to a child
  session.
- Restart requirement: unfinished work must resume automatically after process
  restart.
- Spawn policy: users may spawn tasks and the main agent may spawn tasks within
  explicit guardrails.
- Graph depth: arbitrary depth is allowed.
- Test strategy: TDD.

### Metis Review (gaps addressed)

- Use the store, not runtime memory, as the source of truth for task state.
- Separate stable `task_id` from per-run `attempt_id`; child session ids are
  linked execution artifacts, not canonical identity.
- Define restart semantics explicitly: on restart, nonterminal attempts are
  recovered from durable state and re-enter execution through deterministic
  recovery, not hidden transient memory.
- Add durable event sequencing/cursors for watch/reconnect instead of relying
  only on transient broadcasts.
- Treat task graphs as validated DAG data; cycles are rejected at write time
  even though arbitrary depth is allowed.

## Work Objectives

### Core Objective

Implement a durable task/subagent workflow engine that allows the main agent to
delegate work into fresh child-session contexts while preserving parent context,
tracking graph execution, and exposing real-time and recoverable task state
through CLI + API surfaces.

### Deliverables

- Task persistence model: tasks, task edges, task attempts, task events, watcher
  cursors/sequence.
- Runtime scheduler that executes DAG nodes via child sessions, enforces
  dependency readiness, and supports restart recovery.
- Parent↔child context contract using bounded handoff summaries/artifacts
  instead of raw transcript injection.
- Guardrails for autonomous delegation: policy inheritance,
  depth/concurrency/budget limits, and no silent permission escalation.
- CLI + API visibility and control surfaces for listing, watching, inspecting,
  canceling, retrying, resuming, and reprioritizing tasks.
- Automated verification covering lifecycle, concurrency, reconnect, restart
  recovery, and failure paths.

### Definition of Done (verifiable conditions with commands)

- `cargo test --test runtime -- --exact task_state_machine_is_durable`
- `cargo test --test runtime -- --exact scheduler_executes_ready_graph_nodes_via_child_sessions`
- `cargo test --test runtime -- --exact restart_recovery_resumes_nonterminal_tasks_automatically`
- `cargo test --test web -- --exact task_snapshot_includes_graph_and_attempt_state`
- `cargo test --test web -- --exact task_watch_reconnect_from_cursor_recovers_missed_events`
- `cargo test --test store_concurrency -- --exact task_claim_is_single_winner_under_race`
- `cargo test --test store_concurrency -- --exact cancel_retry_resume_and_reprioritize_are_serialized`
- `npx playwright test playwright/core-workspace.spec.ts --grep "task delegation lifecycle"`
- `npx playwright test playwright/core-workspace.spec.ts --grep "task watch survives reconnect"`

### Must Have

- Stable `task_id`, separate `attempt_id`, optional child `session_id` linkage.
- Arbitrary-depth DAG execution with cycle rejection.
- Automatic restart recovery for all nonterminal attempts.
- Cursor-based task event watching for reconnect/replay.
- Guarded autonomous spawning with inherited-or-narrower permissions only.
- Parent close policy: request-cancel descendants, mark attempts interrupted,
  and allow scheduler recovery to reconcile final state.

### Must NOT Have (guardrails, AI slop patterns, scope boundaries)

- No hidden “task = session” shortcut.
- No raw child transcript streaming into parent context by default.
- No permission widening in child runs unless an explicit policy override is
  persisted and approved.
- No cycle-supporting graph model.
- No manual verification or human-only acceptance criteria.
- No separate orchestration stack that bypasses `SessionRuntime`,
  `RuntimeManager`, existing event plumbing, or the SQLite store.

## Verification Strategy

> ZERO HUMAN INTERVENTION — all verification is agent-executed.

- Test decision: TDD using Rust integration tests, store concurrency tests, web
  protocol tests, and Playwright browser tests.
- QA policy: Every task below includes happy-path and failure-path
  agent-executed scenarios.
- Evidence: `.sisyphus/evidence/task-{N}-{slug}.{ext}`
- User approval policy: the user's explicit "okay" is release approval after
  evidence review, not a substitute for automated acceptance criteria.

## Execution Strategy

### Parallel Execution Waves

> Target: 5-8 tasks per wave. <3 per wave (except final) = under-splitting.
> Extract shared dependencies as Wave-1 tasks for max parallelism.

Wave 1: persistence and invariants — Tasks 1-5 Wave 2: runtime execution and
recovery — Tasks 6-10 Wave 3: events, surfaces, delegation entrypoints,
end-to-end coverage — Tasks 11-15

### Dependency Matrix (full, all tasks)

| Task | Depends On           |
| ---- | -------------------- |
| 1    | —                    |
| 2    | 1                    |
| 3    | 1, 2                 |
| 4    | 1                    |
| 5    | 1, 2, 3              |
| 6    | 2, 3                 |
| 7    | 2, 3                 |
| 8    | 3, 5, 6, 7           |
| 9    | 3, 5, 7, 8           |
| 10   | 4, 5, 8, 9           |
| 11   | 4, 8, 9, 10          |
| 12   | 4, 10, 11            |
| 13   | 11, 12               |
| 14   | 7, 8, 11, 12         |
| 15   | 8, 9, 10, 12, 13, 14 |

### Agent Dispatch Summary (wave → task count → categories)

- Wave 1 → 5 tasks → `deep`, `unspecified-high`
- Wave 2 → 5 tasks → `deep`, `unspecified-high`
- Wave 3 → 5 tasks → `unspecified-high`, `quick`

## TODOs

> Implementation + Test = ONE task. Never separate. EVERY task MUST have: Agent
> Profile + Parallelization + QA Scenarios.

- [x] 1. Add durable task domain schema

  **What to do**: Add SQLite migrations and Rust store types for `tasks`,
  `task_edges`, `task_attempts`, and `task_events`. Make `task_id` canonical,
  `attempt_id` per execution, and child `session_id` optional. Persist priority,
  policy snapshot, parent-close policy, recovery checkpoint, and durable event
  sequence. **Must NOT do**: Do not overload existing `turns` rows as task truth
  or collapse task and attempt identities.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: schema design drives every downstream invariant.
  - Skills: `[]` — repo-local design only.
  - Omitted: `find-docs` — no external API dependency.

  **Parallelization**: Can Parallel: NO | Wave 1 | Blocks: 2, 3, 4, 5 | Blocked
  By: —

  **References**:
  - Pattern: `src/store/schema.rs` — existing migration style and table
    definitions.
  - Pattern: `src/store/session.rs` — session persistence patterns to mirror.
  - Pattern: `src/store/turn.rs` — row mapping and persistence conventions.
  - API/Type: `src/runtime/settings.rs` — persistence of runtime-scoped settings
    snapshots.

  **Acceptance Criteria**:
  - [ ] `cargo test --test runtime -- --exact task_schema_round_trips_canonical_identity`
  - [ ] `cargo test --test store_concurrency -- --exact task_rows_persist_policy_and_recovery_metadata`

  **QA Scenarios**:

  ```
  Scenario: Schema stores canonical task identity
    Tool: Bash
    Steps: cargo test --test runtime -- --exact task_schema_round_trips_canonical_identity
    Expected: Exit code 0; task row keeps stable task_id while attempts/session links vary separately.
    Evidence: .sisyphus/evidence/task-1-task-schema.txt

  Scenario: Invalid schema migration fails closed
    Tool: Bash
    Steps: cargo test --test runtime -- --exact task_schema_rejects_missing_attempt_identity
    Expected: Exit code 0; test proves inserts missing required attempt/task linkage are rejected.
    Evidence: .sisyphus/evidence/task-1-task-schema-error.txt
  ```

  **Commit**: YES | Message: `feat(store): add durable task domain schema` |
  Files: `src/store/schema.rs`, `src/store/session.rs`, `src/store/turn.rs`,
  `tests/runtime.rs`, `tests/store_concurrency.rs`

- [x] 2. Implement task graph repository and DAG validation

  **What to do**: Add repository APIs to create/update tasks, attach edges,
  validate arbitrary-depth DAG structure, reject cycles at write time, and
  persist graph metadata independently of execution state. **Must NOT do**: Do
  not permit cyclic graphs, implicit parent-child links without stored edges, or
  in-memory-only graph validation.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: graph invariants and store APIs are
    safety-critical.
  - Skills: `[]` — repo-local domain logic.
  - Omitted: `find-docs` — no library research needed.

  **Parallelization**: Can Parallel: NO | Wave 1 | Blocks: 3, 5, 6, 7 | Blocked
  By: 1

  **References**:
  - Pattern: `src/store/mod.rs` — async `store_run` execution boundary.
  - Pattern: `src/store/session.rs` — repository method organization.
  - Pattern: `tests/store_concurrency.rs` — store-focused integration style.
  - API/Type: `src/store/schema.rs` — new task tables introduced by Task 1.

  **Acceptance Criteria**:
  - [ ] `cargo test --test runtime -- --exact task_graph_rejects_cycles_at_write_time`
  - [ ] `cargo test --test runtime -- --exact task_graph_persists_arbitrary_depth_dag`

  **QA Scenarios**:

  ```
  Scenario: Arbitrary-depth DAG persists successfully
    Tool: Bash
    Steps: cargo test --test runtime -- --exact task_graph_persists_arbitrary_depth_dag
    Expected: Exit code 0; multi-level dependency graph is persisted and read back unchanged.
    Evidence: .sisyphus/evidence/task-2-task-graph.txt

  Scenario: Cycle insertion is rejected
    Tool: Bash
    Steps: cargo test --test runtime -- --exact task_graph_rejects_cycles_at_write_time
    Expected: Exit code 0; repository rejects cyclic edge creation with explicit error.
    Evidence: .sisyphus/evidence/task-2-task-graph-error.txt
  ```

  **Commit**: YES | Message:
  `feat(store): add task graph repository and dag validation` | Files:
  `src/store/mod.rs`, `src/store/session.rs`, `src/store/schema.rs`,
  `tests/runtime.rs`, `tests/store_concurrency.rs`

- [x] 3. Add durable task and attempt state machines

  **What to do**: Define explicit state machines for task lifecycle and attempt
  lifecycle, including queued, ready, running, blocked, cancel_requested,
  cancelled, failed, completed, interrupted, and retryable transitions. Enforce
  valid transitions in repository/runtime code and persist every transition.
  **Must NOT do**: Do not infer lifecycle from session status alone or let
  retry/resume mutate terminal states ambiguously.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: lifecycle semantics affect all controls and
    recovery.
  - Skills: `[]` — domain modeling only.
  - Omitted: `find-docs` — no external API research.

  **Parallelization**: Can Parallel: NO | Wave 1 | Blocks: 5, 6, 7, 8, 9 |
  Blocked By: 1, 2

  **References**:
  - Pattern: `src/runtime/manager.rs` — current active-turn state orchestration.
  - Pattern: `src/runtime/session.rs` — runtime-driven status transitions.
  - Pattern: `tests/runtime.rs` — runtime lifecycle test patterns.
  - API/Type: `src/provider/mod.rs` — terminal/nonterminal turn result
    semantics.

  **Acceptance Criteria**:
  - [ ] `cargo test --test runtime -- --exact task_state_machine_is_durable`
  - [ ] `cargo test --test runtime -- --exact attempt_state_machine_rejects_invalid_transitions`

  **QA Scenarios**:

  ```
  Scenario: Valid task and attempt transitions persist
    Tool: Bash
    Steps: cargo test --test runtime -- --exact task_state_machine_is_durable
    Expected: Exit code 0; all defined legal transitions persist and replay correctly.
    Evidence: .sisyphus/evidence/task-3-state-machine.txt

  Scenario: Invalid transition fails closed
    Tool: Bash
    Steps: cargo test --test runtime -- --exact attempt_state_machine_rejects_invalid_transitions
    Expected: Exit code 0; invalid transitions are rejected with deterministic errors.
    Evidence: .sisyphus/evidence/task-3-state-machine-error.txt
  ```

  **Commit**: YES | Message:
  `feat(runtime): add durable task and attempt state machines` | Files:
  `src/runtime/manager.rs`, `src/runtime/session.rs`, `src/store/session.rs`,
  `tests/runtime.rs`

- [x] 4. Add durable task event sequencing and watch cursors

  **What to do**: Persist append-only task event rows with monotonic sequence
  numbers and implement cursor-based replay primitives so watchers can reconnect
  without losing task lifecycle events. **Must NOT do**: Do not rely exclusively
  on broadcast channels or transient in-memory replay for watch recovery.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: focused persistence + replay work
    with cross-surface implications.
  - Skills: `[]` — repo-local event modeling.
  - Omitted: `find-docs` — no external dependency.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 10, 11, 12 | Blocked
  By: 1

  **References**:
  - Pattern: `src/events.rs` — existing event naming and payload conventions.
  - Pattern: `src/web/ws/event_map.rs` — downstream consumer of event streams.
  - Pattern: `src/web/ws/snapshot.rs` — snapshot + replay model for reconnect.
  - Test: `tests/web.rs` — reconnect and replay test patterns.

  **Acceptance Criteria**:
  - [ ] `cargo test --test web -- --exact task_event_cursor_replays_from_last_seen_sequence`
  - [ ] `cargo test --test web -- --exact task_event_cursor_rejects_gaps_for_unknown_task`

  **QA Scenarios**:

  ```
  Scenario: Cursor replay returns missed events
    Tool: Bash
    Steps: cargo test --test web -- --exact task_event_cursor_replays_from_last_seen_sequence
    Expected: Exit code 0; reconnecting watcher receives only events after supplied sequence.
    Evidence: .sisyphus/evidence/task-4-event-cursor.txt

  Scenario: Invalid cursor request fails gracefully
    Tool: Bash
    Steps: cargo test --test web -- --exact task_event_cursor_rejects_gaps_for_unknown_task
    Expected: Exit code 0; unknown task/cursor combinations return explicit error without crashing stream.
    Evidence: .sisyphus/evidence/task-4-event-cursor-error.txt
  ```

  **Commit**: YES | Message:
  `feat(store): add task event sequencing and cursors` | Files: `src/events.rs`,
  `src/store/schema.rs`, `src/store/session.rs`, `tests/web.rs`

- [x] 5. Add scheduler claim and lease concurrency primitives

  **What to do**: Implement durable claim/lease semantics so only one scheduler
  worker owns a runnable attempt at a time, with lease expiry/interruption
  markers that support restart recovery and race-safe lifecycle updates. **Must
  NOT do**: Do not permit double-execution under concurrent claims or leave
  running attempts without durable ownership metadata.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: concurrency correctness is central to
    restart-safe orchestration.
  - Skills: `[]` — no extra skills needed.
  - Omitted: `find-docs` — repo-local concurrency work.

  **Parallelization**: Can Parallel: NO | Wave 1 | Blocks: 8, 9, 10 | Blocked
  By: 1, 2, 3

  **References**:
  - Pattern: `src/runtime/manager.rs` — current session ownership/active-turn
    rules.
  - Pattern: `tests/store_concurrency.rs` — race-focused testing patterns.
  - Pattern: `tests/web.rs` — busy-session semantics to preserve.
  - API/Type: `src/web/ws/session.rs` — controller attachment/busy-session
    behavior.

  **Acceptance Criteria**:
  - [ ] `cargo test --test store_concurrency -- --exact task_claim_is_single_winner_under_race`
  - [ ] `cargo test --test store_concurrency -- --exact expired_task_lease_is_recoverable_without_double_run`

  **QA Scenarios**:

  ```
  Scenario: Concurrent claim chooses a single winner
    Tool: Bash
    Steps: cargo test --test store_concurrency -- --exact task_claim_is_single_winner_under_race
    Expected: Exit code 0; exactly one worker owns the runnable attempt.
    Evidence: .sisyphus/evidence/task-5-lease.txt

  Scenario: Lease expiry recovers safely
    Tool: Bash
    Steps: cargo test --test store_concurrency -- --exact expired_task_lease_is_recoverable_without_double_run
    Expected: Exit code 0; interrupted lease becomes recoverable without duplicate execution.
    Evidence: .sisyphus/evidence/task-5-lease-error.txt
  ```

  **Commit**: YES | Message:
  `feat(runtime): add task claim and lease concurrency primitives` | Files:
  `src/runtime/manager.rs`, `src/store/session.rs`,
  `tests/store_concurrency.rs`, `tests/web.rs`

- [x] 6. Implement parent↔child handoff contract and child-session bootstrap

  **What to do**: Define the execution contract for delegated work: bounded
  handoff summary, referenced artifacts, inherited settings snapshot, and
  optional child `session_id` creation at attempt start. Bootstrap child
  sessions from durable task context rather than raw parent transcript replay.
  **Must NOT do**: Do not inject full parent transcript into child context or
  treat child session creation as the canonical task creation event.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: context isolation is the core product behavior.
  - Skills: `[]` — internal architecture only.
  - Omitted: `find-docs` — no library lookup needed.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: 8, 10, 14, 15 |
  Blocked By: 2, 3

  **References**:
  - Pattern: `src/runtime/session.rs` — session initialization/resume flow.
  - Pattern: `src/compact.rs` — existing handoff summary framing to reuse.
  - Pattern: `src/skills/mod.rs` — parent-context shaping patterns.
  - API/Type: `src/runtime/settings.rs` — settings snapshot structure to
    inherit.

  **Acceptance Criteria**:
  - [ ] `cargo test --test runtime -- --exact child_session_bootstrap_uses_handoff_summary_not_parent_transcript`
  - [ ] `cargo test --test runtime -- --exact delegated_task_links_child_session_after_attempt_start`

  **QA Scenarios**:

  ```
  Scenario: Child session starts from bounded handoff
    Tool: Bash
    Steps: cargo test --test runtime -- --exact child_session_bootstrap_uses_handoff_summary_not_parent_transcript
    Expected: Exit code 0; child runtime receives summary/artifact payload without full raw parent history.
    Evidence: .sisyphus/evidence/task-6-handoff.txt

  Scenario: Child creation failure leaves task recoverable
    Tool: Bash
    Steps: cargo test --test runtime -- --exact delegated_task_child_session_failure_marks_attempt_interrupted
    Expected: Exit code 0; failed child-session creation leaves deterministic interrupted/retryable state.
    Evidence: .sisyphus/evidence/task-6-handoff-error.txt
  ```

  **Commit**: YES | Message: `feat(runtime): add child-session handoff contract`
  | Files: `src/runtime/session.rs`, `src/compact.rs`,
  `src/runtime/settings.rs`, `tests/runtime.rs`

- [x] 7. Add delegation policy inheritance and autonomous-spawn guardrails

  **What to do**: Persist and enforce policy snapshots covering depth,
  concurrency, budget, provider/model/tool approvals, and parent-close behavior.
  Allow the main agent to spawn only when the current policy permits it; child
  tasks inherit equal-or-narrower permissions. **Must NOT do**: Do not allow
  silent permission escalation, unbounded recursive spawning, or policy
  evaluation only in memory.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: focused policy enforcement with
    moderate cross-cutting changes.
  - Skills: `[]` — internal policy work.
  - Omitted: `find-docs` — no external docs needed.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: 8, 9, 14, 15 |
  Blocked By: 2, 3

  **References**:
  - Pattern: `src/tools/mod.rs` — tool registration and approval surfaces.
  - Pattern: `src/runtime/settings.rs` — persisted execution settings.
  - Pattern: `tests/runtime.rs` — approval-denial test patterns.
  - API/Type: `src/provider/mod.rs` — provider/model selection boundaries.

  **Acceptance Criteria**:
  - [ ] `cargo test --test runtime -- --exact autonomous_spawn_respects_depth_and_budget_limits`
  - [ ] `cargo test --test runtime -- --exact child_task_cannot_widen_parent_permissions`

  **QA Scenarios**:

  ```
  Scenario: Guarded autonomous spawn succeeds within policy
    Tool: Bash
    Steps: cargo test --test runtime -- --exact autonomous_spawn_respects_depth_and_budget_limits
    Expected: Exit code 0; allowed autonomous spawn creates child task within configured limits.
    Evidence: .sisyphus/evidence/task-7-policy.txt

  Scenario: Escalation attempt is denied
    Tool: Bash
    Steps: cargo test --test runtime -- --exact child_task_cannot_widen_parent_permissions
    Expected: Exit code 0; child spawn requesting broader permissions fails closed with explicit denial.
    Evidence: .sisyphus/evidence/task-7-policy-error.txt
  ```

  **Commit**: YES | Message:
  `feat(runtime): add delegation policy inheritance and guardrails` | Files:
  `src/runtime/settings.rs`, `src/tools/mod.rs`, `src/runtime/manager.rs`,
  `tests/runtime.rs`

- [x] 8. Integrate DAG scheduler execution into RuntimeManager

  **What to do**: Extend `RuntimeManager` to select ready nodes, create/claim
  attempts, launch child-session execution, respect dependency completion, and
  mark downstream nodes ready when prerequisites complete. **Must NOT do**: Do
  not execute multiple attempts for the same runnable node, bypass
  repository-backed transitions, or violate the existing
  one-active-turn-per-session invariant inside a single session.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: cross-cutting runtime orchestration with
    dependency semantics.
  - Skills: `[]` — internal runtime work.
  - Omitted: `find-docs` — no external docs needed.

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 9, 10, 11, 14, 15 |
  Blocked By: 3, 5, 6, 7

  **References**:
  - Pattern: `src/runtime/manager.rs` — current per-session worker
    orchestration.
  - Pattern: `src/runtime/session.rs` — turn submission and provider execution
    loop.
  - Pattern: `tests/runtime.rs` — execution/abort lifecycle tests.
  - API/Type: `src/provider/mod.rs` — task completion/failure mapping to
    provider outcomes.

  **Acceptance Criteria**:
  - [ ] `cargo test --test runtime -- --exact scheduler_executes_ready_graph_nodes_via_child_sessions`
  - [ ] `cargo test --test runtime -- --exact scheduler_does_not_run_blocked_nodes_early`

  **QA Scenarios**:

  ```
  Scenario: Scheduler runs ready nodes only
    Tool: Bash
    Steps: cargo test --test runtime -- --exact scheduler_executes_ready_graph_nodes_via_child_sessions
    Expected: Exit code 0; runnable nodes execute when dependencies are satisfied and completion unlocks dependents.
    Evidence: .sisyphus/evidence/task-8-scheduler.txt

  Scenario: Blocked node is withheld
    Tool: Bash
    Steps: cargo test --test runtime -- --exact scheduler_does_not_run_blocked_nodes_early
    Expected: Exit code 0; node with unsatisfied dependencies never starts.
    Evidence: .sisyphus/evidence/task-8-scheduler-error.txt
  ```

  **Commit**: YES | Message: `feat(runtime): integrate dag scheduler execution`
  | Files: `src/runtime/manager.rs`, `src/runtime/session.rs`,
  `tests/runtime.rs`

- [x] 9. Implement lifecycle mutation controls

  **What to do**: Add deterministic semantics for cancel, retry, resume, and
  reprioritize. Restrict reprioritize to queued/ready nodes, define cancel
  propagation to descendants, and ensure resume/retry operate through new
  attempts with stable task ids. **Must NOT do**: Do not mutate running/terminal
  tasks ambiguously or let control commands bypass task/attempt state checks.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: control semantics are easy to get subtly wrong.
  - Skills: `[]` — repo-local state transition work.
  - Omitted: `find-docs` — no external API research.

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 10, 11, 12, 13, 15 |
  Blocked By: 3, 5, 7, 8

  **References**:
  - Pattern: `src/web/protocol.rs` — current command/result schema to mirror.
  - Pattern: `src/web/ws.rs` — command dispatch structure.
  - Pattern: `tests/web.rs` — abort/control-command behavior patterns.
  - API/Type: `src/runtime/manager.rs` — control entrypoints to extend.

  **Acceptance Criteria**:
  - [ ] `cargo test --test store_concurrency -- --exact cancel_retry_resume_and_reprioritize_are_serialized`
  - [ ] `cargo test --test runtime -- --exact reprioritize_rejects_running_and_terminal_tasks`

  **QA Scenarios**:

  ```
  Scenario: Control mutations serialize correctly
    Tool: Bash
    Steps: cargo test --test store_concurrency -- --exact cancel_retry_resume_and_reprioritize_are_serialized
    Expected: Exit code 0; racing lifecycle mutations resolve deterministically with no state corruption.
    Evidence: .sisyphus/evidence/task-9-controls.txt

  Scenario: Invalid reprioritize is rejected
    Tool: Bash
    Steps: cargo test --test runtime -- --exact reprioritize_rejects_running_and_terminal_tasks
    Expected: Exit code 0; queued/ready-only reprioritize rule is enforced.
    Evidence: .sisyphus/evidence/task-9-controls-error.txt
  ```

  **Commit**: YES | Message:
  `feat(runtime): add task lifecycle mutation controls` | Files:
  `src/runtime/manager.rs`, `src/web/protocol.rs`, `src/web/ws.rs`,
  `tests/runtime.rs`, `tests/store_concurrency.rs`

- [x] 10. Implement automatic restart recovery and interrupted-attempt
      continuation

  **What to do**: On process startup, recover nonterminal attempts from durable
  state, mark stale leases interrupted, and automatically schedule recovery
  attempts that continue from the last durable handoff/checkpoint until the task
  reaches a terminal state. **Must NOT do**: Do not require manual operator
  intervention for normal restart recovery or claim to preserve raw in-memory
  execution threads across restart.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: restart safety is a core user requirement and
    system risk.
  - Skills: `[]` — internal recovery semantics only.
  - Omitted: `find-docs` — no external lookup required.

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 11, 12, 13, 15 |
  Blocked By: 4, 5, 8, 9

  **References**:
  - Pattern: `src/runtime/manager.rs` — startup/runtime ownership hooks.
  - Pattern: `src/store/mod.rs` — startup DB access boundary.
  - Pattern: `tests/runtime.rs` — abort/interruption test structure.
  - Pattern: `tests/web.rs` — reconnect-after-interruption expectations.

  **Acceptance Criteria**:
  - [ ] `cargo test --test runtime -- --exact restart_recovery_resumes_nonterminal_tasks_automatically`
  - [ ] `cargo test --test runtime -- --exact parent_close_requests_descendant_cancellation_before_recovery`

  **QA Scenarios**:

  ```
  Scenario: Restart resumes unfinished work automatically
    Tool: Bash
    Steps: cargo test --test runtime -- --exact restart_recovery_resumes_nonterminal_tasks_automatically
    Expected: Exit code 0; restarting runtime recovers nonterminal tasks and drives them to completion without manual commands.
    Evidence: .sisyphus/evidence/task-10-recovery.txt

  Scenario: Parent close cascades safely
    Tool: Bash
    Steps: cargo test --test runtime -- --exact parent_close_requests_descendant_cancellation_before_recovery
    Expected: Exit code 0; parent shutdown marks descendant attempts for cancellation and recovery reconciles state cleanly.
    Evidence: .sisyphus/evidence/task-10-recovery-error.txt
  ```

  **Commit**: YES | Message: `feat(runtime): add automatic restart recovery` |
  Files: `src/runtime/manager.rs`, `src/store/mod.rs`, `src/store/session.rs`,
  `tests/runtime.rs`, `tests/web.rs`

- [x] 11. Extend task lifecycle events through runtime and UI mappings

  **What to do**: Add task-specific event types and payloads for graph creation,
  node readiness, attempt claimed, child session linked, status transitions,
  control actions, and recovery. Map them through runtime broadcasts and UI
  event translation without breaking existing session/turn events. **Must NOT
  do**: Do not hide task state behind generic status strings or regress current
  turn-level event delivery.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: event surface extension with moderate
    runtime coupling.
  - Skills: `[]` — repo-local protocol work.
  - Omitted: `find-docs` — no external docs required.

  **Parallelization**: Can Parallel: NO | Wave 3 | Blocks: 12, 13, 14, 15 |
  Blocked By: 4, 8, 9, 10

  **References**:
  - Pattern: `src/events.rs` — canonical event enum/struct shape.
  - Pattern: `src/web/ws/event_map.rs` — low-level to UI event translation.
  - Pattern: `src/tools/report_status.rs` — status-event precedent for task
    progress exposure.
  - Test: `tests/web.rs` — event-stream assertions.

  **Acceptance Criteria**:
  - [ ] `cargo test --test web -- --exact task_events_include_attempt_and_child_session_metadata`
  - [ ] `cargo test --test web -- --exact existing_turn_events_remain_backward_compatible`

  **QA Scenarios**:

  ```
  Scenario: Task events emit complete lifecycle metadata
    Tool: Bash
    Steps: cargo test --test web -- --exact task_events_include_attempt_and_child_session_metadata
    Expected: Exit code 0; task event stream carries task_id, attempt_id, optional child_session_id, and lifecycle state.
    Evidence: .sisyphus/evidence/task-11-events.txt

  Scenario: Legacy turn events still work
    Tool: Bash
    Steps: cargo test --test web -- --exact existing_turn_events_remain_backward_compatible
    Expected: Exit code 0; existing session/turn consumers still receive expected event payloads.
    Evidence: .sisyphus/evidence/task-11-events-error.txt
  ```

  **Commit**: YES | Message: `feat(events): extend lifecycle events for tasks` |
  Files: `src/events.rs`, `src/web/ws/event_map.rs`,
  `src/tools/report_status.rs`, `tests/web.rs`

- [x] 12. Add task snapshots, watch APIs, and control protocol commands

  **What to do**: Extend the web protocol and websocket handlers with task
  list/detail snapshot payloads, cursor-based watch commands, and control
  commands for cancel, retry, resume, and reprioritize. Ensure snapshots include
  graph state, attempt state, child session linkage, and cursor positions.
  **Must NOT do**: Do not expose task controls without cursor-aware watch
  recovery or mix task state into unrelated session snapshot fields.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: protocol/snapshot work with multiple
    integration points.
  - Skills: `[]` — repo-local API design.
  - Omitted: `find-docs` — no external API lookup.

  **Parallelization**: Can Parallel: NO | Wave 3 | Blocks: 13, 14, 15 | Blocked
  By: 4, 10, 11

  **References**:
  - Pattern: `src/web/protocol.rs` — command/result schema.
  - Pattern: `src/web/ws.rs` — websocket command loop.
  - Pattern: `src/web/ws/snapshot.rs` — state snapshot assembly.
  - Pattern: `src/web/ws/session.rs` — observer/controller attach semantics.

  **Acceptance Criteria**:
  - [ ] `cargo test --test web -- --exact task_snapshot_includes_graph_and_attempt_state`
  - [ ] `cargo test --test web -- --exact task_watch_reconnect_from_cursor_recovers_missed_events`

  **QA Scenarios**:

  ```
  Scenario: Snapshot exposes graph and execution state
    Tool: Bash
    Steps: cargo test --test web -- --exact task_snapshot_includes_graph_and_attempt_state
    Expected: Exit code 0; snapshot includes graph nodes, edges, priorities, attempt state, and child session linkage.
    Evidence: .sisyphus/evidence/task-12-protocol.txt

  Scenario: Watch reconnect recovers missed task events
    Tool: Bash
    Steps: cargo test --test web -- --exact task_watch_reconnect_from_cursor_recovers_missed_events
    Expected: Exit code 0; reconnecting watcher with last cursor receives missed task events without duplication.
    Evidence: .sisyphus/evidence/task-12-protocol-error.txt
  ```

  **Commit**: YES | Message:
  `feat(web): add task snapshots watch apis and controls` | Files:
  `src/web/protocol.rs`, `src/web/ws.rs`, `src/web/ws/snapshot.rs`,
  `src/web/ws/session.rs`, `tests/web.rs`

- [x] 13. Add CLI task visibility and control surface

  **What to do**: Add CLI commands to list tasks, inspect graph state, watch
  task progress, and issue cancel/retry/resume/reprioritize operations against
  stable task ids. Make CLI output clearly distinguish task ids, attempt ids,
  child session ids, and current lease ownership. **Must NOT do**: Do not hide
  task state behind session-only commands or require web UI access for core task
  operations.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: user-facing control surface built on
    new protocol/runtime semantics.
  - Skills: `[]` — repo-local CLI work.
  - Omitted: `find-docs` — no external docs needed.

  **Parallelization**: Can Parallel: YES | Wave 3 | Blocks: 15 | Blocked By: 11,
  12

  **References**:
  - Pattern: `src/main.rs` — CLI command/subcommand wiring.
  - Pattern: `src/agent.rs` — CLI runtime/session entrypoints.
  - Pattern: `tests/cli.rs` — CLI persistence test style.
  - Test: `tests/hashline_harness.rs` and `src/bin/hashline-harness.rs` —
    subprocess-based CLI verification patterns.

  **Acceptance Criteria**:
  - [ ] `cargo test --test cli -- --exact cli_lists_and_inspects_task_graph_state`
  - [ ] `cargo test --test cli -- --exact cli_control_commands_require_valid_task_state`

  **QA Scenarios**:

  ```
  Scenario: CLI exposes running tasks clearly
    Tool: Bash
    Steps: cargo test --test cli -- --exact cli_lists_and_inspects_task_graph_state
    Expected: Exit code 0; CLI list/inspect output shows task graph, attempt state, and child session linkage.
    Evidence: .sisyphus/evidence/task-13-cli.txt

  Scenario: Invalid CLI control is rejected
    Tool: Bash
    Steps: cargo test --test cli -- --exact cli_control_commands_require_valid_task_state
    Expected: Exit code 0; CLI returns deterministic error for invalid terminal/running control requests.
    Evidence: .sisyphus/evidence/task-13-cli-error.txt
  ```

  **Commit**: YES | Message:
  `feat(cli): add task visibility and control surface` | Files: `src/main.rs`,
  `src/agent.rs`, `tests/cli.rs`, `tests/hashline_harness.rs`,
  `src/bin/hashline-harness.rs`

- [x] 14. Add guarded main-agent delegation entrypoint

  **What to do**: Expose a first-class delegation entrypoint the main agent can
  call to create tasks, provide handoff briefs, and subscribe to outcomes.
  Prefer a dedicated runtime/tool entrypoint that records policy checks and task
  ids explicitly. **Must NOT do**: Do not let agent delegation bypass policy
  enforcement, task persistence, or graph validation.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: ties agent-facing behavior to the new
    orchestration substrate.
  - Skills: `[]` — internal tool/runtime integration.
  - Omitted: `find-docs` — no external docs required.

  **Parallelization**: Can Parallel: YES | Wave 3 | Blocks: 15 | Blocked By: 7,
  8, 11, 12

  **References**:
  - Pattern: `src/tools/mod.rs` — tool registration surface.
  - Pattern: `src/runtime/session.rs` — tool-call execution loop.
  - Pattern: `src/tools/report_status.rs` — task/progress signaling precedent.
  - Test: `tests/runtime.rs` — tool approval and autonomous execution patterns.

  **Acceptance Criteria**:
  - [ ] `cargo test --test runtime -- --exact main_agent_can_delegate_task_with_handoff_and_policy_check`
  - [ ] `cargo test --test runtime -- --exact agent_delegation_denied_when_policy_blocks_spawn`

  **QA Scenarios**:

  ```
  Scenario: Main agent delegates a task successfully
    Tool: Bash
    Steps: cargo test --test runtime -- --exact main_agent_can_delegate_task_with_handoff_and_policy_check
    Expected: Exit code 0; agent call creates task, persists handoff, and subscribes to outcome via stable task id.
    Evidence: .sisyphus/evidence/task-14-delegation.txt

  Scenario: Policy-blocked delegation fails closed
    Tool: Bash
    Steps: cargo test --test runtime -- --exact agent_delegation_denied_when_policy_blocks_spawn
    Expected: Exit code 0; blocked spawn returns explicit denial and no child task is created.
    Evidence: .sisyphus/evidence/task-14-delegation-error.txt
  ```

  **Commit**: YES | Message:
  `feat(runtime): add guarded agent delegation entrypoint` | Files:
  `src/tools/mod.rs`, `src/runtime/session.rs`, `src/tools/report_status.rs`,
  `tests/runtime.rs`

- [x] 15. Add end-to-end lifecycle, reconnect, and recovery coverage

  **What to do**: Expand Rust integration and Playwright coverage to validate
  full lifecycle flows across CLI/API surfaces: create graph, observe progress,
  reconnect from cursor, cancel/retry/resume/reprioritize, restart recovery, and
  autonomous delegation outcomes. **Must NOT do**: Do not ship the feature with
  only happy-path coverage or without browser/workspace reconnect verification.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: cross-surface verification and test
    harness extension.
  - Skills: `[]` — repo-local test work.
  - Omitted: `find-docs` — no external docs needed.

  **Parallelization**: Can Parallel: NO | Wave 3 | Blocks: Final Verification
  Wave | Blocked By: 8, 9, 10, 12, 13, 14

  **References**:
  - Test: `tests/runtime.rs` — runtime lifecycle coverage patterns.
  - Test: `tests/web.rs` — websocket/session reconnect coverage.
  - Test: `tests/store_concurrency.rs` — race/serialization coverage.
  - Test: `playwright/core-workspace.spec.ts` and `playwright.config.ts` —
    browser E2E harness.
  - Test: `playwright-web-server.mjs` — real `kley web` process bootstrapping
    for E2E.

  **Acceptance Criteria**:
  - [ ] `cargo test --test runtime -- --exact restart_recovery_resumes_nonterminal_tasks_automatically`
  - [ ] `cargo test --test web -- --exact task_watch_reconnect_from_cursor_recovers_missed_events`
  - [ ] `npx playwright test playwright/core-workspace.spec.ts --grep "task delegation lifecycle"`
  - [ ] `npx playwright test playwright/core-workspace.spec.ts --grep "task watch survives reconnect"`

  **QA Scenarios**:

  ```
  Scenario: End-to-end lifecycle works across surfaces
    Tool: Bash
    Steps: npx playwright test playwright/core-workspace.spec.ts --grep "task delegation lifecycle"
    Expected: Exit code 0; browser/API surface shows task creation, progress, controls, and completion consistently.
    Evidence: .sisyphus/evidence/task-15-e2e.txt

  Scenario: Reconnect and restart recovery remain consistent
    Tool: Bash
    Steps: npx playwright test playwright/core-workspace.spec.ts --grep "task watch survives reconnect" && cargo test --test runtime -- --exact restart_recovery_resumes_nonterminal_tasks_automatically
    Expected: Exit code 0; reconnect and process restart preserve observable task continuity without orphaning work.
    Evidence: .sisyphus/evidence/task-15-e2e-error.txt
  ```

  **Commit**: YES | Message:
  `test(e2e): cover task lifecycle reconnect and recovery` | Files:
  `tests/runtime.rs`, `tests/web.rs`, `tests/store_concurrency.rs`,
  `playwright/core-workspace.spec.ts`, `playwright.config.ts`,
  `playwright-web-server.mjs`

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated
> results to user and get explicit "okay" before completing. **Do NOT
> auto-proceed after verification. Wait for the user's release approval after
> presenting evidence.** **Never mark the work complete before the user's
> okay.** Rejection or user feedback -> fix -> re-run F1-F4 -> present updated
> evidence -> wait for okay.

- [x] F1. Plan Compliance Audit — oracle
  - Prompt: Review completed implementation against
    `.sisyphus/plans/subagent-delegation-system.md`; verify every task,
    dependency, guardrail, and acceptance criterion was satisfied with evidence.
  - Expected Result: Explicit APPROVE verdict with no unresolved deviations.
  - Evidence: `.sisyphus/evidence/f1-plan-compliance.md`
- [x] F2. Code Quality Review — unspecified-high
  - Prompt: Review changed files for maintainability, edge-case handling,
    concurrency safety, and protocol/store/runtime consistency; reject any
    hidden task=session coupling or unsafe recovery logic.
  - Expected Result: Explicit APPROVE verdict with no critical or major defects.
  - Evidence: `.sisyphus/evidence/f2-code-quality.md`
- [x] F3. Agent-Executed Experiential QA — unspecified-high (+ playwright if UI)
  - Steps:
    1. Run
       `cargo test --test runtime -- --exact restart_recovery_resumes_nonterminal_tasks_automatically`
    2. Run
       `cargo test --test web -- --exact task_watch_reconnect_from_cursor_recovers_missed_events`
    3. Run
       `cargo test --test cli -- --exact cli_lists_and_inspects_task_graph_state`
    4. Run
       `npx playwright test playwright/core-workspace.spec.ts --grep "task delegation lifecycle|task watch survives reconnect"`
  - Expected Result: Exit code 0 for every command and a short narrative
    confirming the observed lifecycle matches the plan.
  - Evidence: `.sisyphus/evidence/f3-experiential-qa.txt`
- [x] F4. Scope Fidelity Check — deep
  - Prompt: Verify the delivered system includes only the planned delegation
    workflow engine scope and does not introduce dashboard-first UI work,
    permission widening, hidden orchestration stacks, or non-DAG execution
    behavior.
  - Expected Result: Explicit APPROVE verdict with all out-of-scope changes
    either absent or justified as required plumbing.
  - Evidence: `.sisyphus/evidence/f4-scope-fidelity.md`

## Commit Strategy

- Keep commits green; each commit includes tests and implementation for one
  coherent slice.
- Commit in this order: schema → repository/state machine → concurrency/claiming
  → child-session contract → scheduler/lifecycle → recovery → events/protocol →
  CLI/API → delegation entrypoint → end-to-end verification.
- Reprioritize/resume semantics must never ship before the underlying durable
  state machine and claim/lease behavior are verified.

## Success Criteria

- The main agent can delegate work without absorbing raw child transcript
  history.
- Tasks are visible and controllable through CLI + API with stable identifiers.
- Restarting the process does not orphan nonterminal work; recovery is automatic
  and deterministic.
- Graph execution remains DAG-valid, race-safe, and policy-constrained.
- All named Rust and Playwright verification commands pass.
