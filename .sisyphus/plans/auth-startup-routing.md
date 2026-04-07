# Auth Startup Routing

## TL;DR

> **Summary**: Move env-vs-stored auth selection to startup, driven by explicit
> startup parameters for CLI and web, then freeze that choice for the lifetime
> of the process/session. Introduce a trait-based auth-source layer above
> credential storage so downstream runtime code stops branching on
> `OPENAI_API_KEY` ad hoc. **Deliverables**:
>
> - shared startup auth policy + trait-based auth-source resolver
> - CLI `--auth-source` startup plumbing
> - web startup config carrying the same policy
> - removal of duplicate runtime/hashline env shortcuts
> - deterministic tests for precedence, propagation, and recovery behavior
>   **Effort**: Medium **Parallel**: YES - 2 waves **Critical Path**: Task 1 →
>   Task 2 → Task 3 → Task 5

## Context

### Original Request

Make env-vs-auth selection a startup concern, based on startup params, with
transparent behavior after startup.

### Interview Summary

- This is a **separate plan** from web search.
- Auth-source policy should be **auto with override**.
- Startup surface should be **CLI flag + web config**.
- Runtime should not re-decide env-vs-stored auth later.

### Metis Review (gaps addressed)

- Keep this separate from secret-storage backend selection (`Vault` vs
  age-file).
- Freeze auth-source policy at startup; do not lazily re-check env in runtime
  workers.
- Define resume/recovery mismatch behavior explicitly.
- Include `hashline` because it duplicates auth resolution today.
- Avoid scope creep into login UX, credential schema changes, or web UI
  controls.

## Work Objectives

### Core Objective

Introduce a trait-based auth-source resolution layer that is selected once at
startup from explicit params/config (`auto | env | stored`), then used
consistently by CLI, web, runtime recovery, and harness flows without further
direct env-vs-store branching.

### Deliverables

- `src/auth/mod.rs` startup auth policy + auth-source trait/implementations
- `src/main.rs` chat/web startup flags and policy parsing
- `src/web/config.rs` and `src/web/mod.rs` policy plumbing
- `src/runtime/manager.rs` and `src/harness/hashline.rs` aligned to the frozen
  startup policy
- test coverage for precedence, propagation, mismatch handling, and
  duplicate-branch removal

### Definition of Done (verifiable conditions with commands)

- `cargo test auth_source_policy_ -- --nocapture`
- `cargo test resolve_auth_ -- --nocapture`
- `cargo test chat_startup_auth_source_ -- --nocapture`
- `cargo test web_startup_auth_source_ -- --nocapture`
- `cargo test runtime_recovery_auth_source_ -- --nocapture`
- `cargo test runtime_worker_auth_ -- --nocapture`
- `cargo test hashline_auth_source_ -- --nocapture`

### Must Have

- New startup policy enum with exactly: `auto`, `env`, `stored`
- New trait-based auth-source layer, e.g. `AuthSourceResolver`, with concrete
  `Auto`, `Env`, and `Stored` implementations
- Startup policy selected from explicit startup surfaces:
  - CLI chat: `--auth-source <auto|env|stored>`
  - CLI web: `--auth-source <auto|env|stored>` feeding `WebConfig`
- `auto` semantics fixed to:
  - OpenAI: prefer `OPENAI_API_KEY` if present and non-empty, otherwise stored
    credentials
  - ZAI: stored credentials only
- `env` semantics fixed to:
  - OpenAI: require `OPENAI_API_KEY`; fail fast if absent/empty
  - ZAI: unsupported in v1; fail fast with a clear error
- `stored` semantics fixed to:
  - ignore `OPENAI_API_KEY` entirely
  - use stored credentials through `CredentialStore`
- The chosen auth-source policy is frozen after startup and reused everywhere
  in-process
- Resume/recovery mismatch behavior:
  - if startup policy resolves a provider different from the requested/resumed
    provider, fail fast with a deterministic error
- `hashline` must use the same shared startup auth-source logic rather than its
  own shortcut

### Must NOT Have (guardrails, AI slop patterns, scope boundaries)

- No redesign of `SecretBackend`, `VaultBackend`, or `AgeFileBackend`
- No login-flow redesign
- No credential persistence schema changes
- No browser/UI control for auth-source switching
- No per-session or mid-process auth-source mutation
- No implicit fallback in `env` mode
- No remaining direct `OPENAI_API_KEY` branching outside the approved
  startup-auth code path and tests

## Verification Strategy

> ZERO HUMAN INTERVENTION - all verification is agent-executed.

- Test decision: tests-after + Rust unit/integration framework
- QA policy: Every task has agent-executed scenarios
- Evidence: `.sisyphus/evidence/task-{N}-{slug}.{ext}`
- Structural verification must include a content search proving duplicate
  `OPENAI_API_KEY` branches were removed from runtime consumers

## Execution Strategy

### Parallel Execution Waves

> Target: 5-8 tasks per wave. <3 per wave (except final) = under-splitting.
> Extract shared dependencies as Wave-1 tasks for max parallelism.

Wave 1: policy + startup entrypoints

- Task 1 Lock startup auth semantics with tests
- Task 2 Add shared policy and trait-based resolvers
- Task 3 Wire CLI startup auth policy
- Task 4 Wire web startup auth policy

Wave 2: consumers + cleanup

- Task 5 Remove runtime duplication and lock recovery behavior
- Task 6 Align hashline and docs

### Dependency Matrix (full, all tasks)

- 1: blocks 2, 3, 4, 5, 6
- 2: blocked by 1; blocks 3, 4, 5, 6
- 3: blocked by 1, 2; blocks 5
- 4: blocked by 1, 2; blocks 5
- 5: blocked by 1, 2, 3, 4; blocks 6
- 6: blocked by 1, 2, 5

### Agent Dispatch Summary (wave → task count → categories)

- Wave 1 → 4 tasks → unspecified-high, quick
- Wave 2 → 2 tasks → unspecified-high, quick
- Final Verification → 4 tasks → oracle, unspecified-high, deep

## TODOs

> Implementation + Test = ONE task. Never separate. EVERY task MUST have: Agent
> Profile + Parallelization + QA Scenarios.

- [ ] 1. Lock startup auth-source semantics with deterministic tests

  **What to do**: Add failing tests that define the exact contract for startup
  auth routing before refactoring production code. Cover `auto`, `env`, and
  `stored`; provider mismatch behavior; and web/CLI parsing expectations. Create
  exact test names with these prefixes: `auth_source_policy_`,
  `chat_startup_auth_source_`, `web_startup_auth_source_`,
  `runtime_recovery_auth_source_`, and `hashline_auth_source_`. **Must NOT do**:
  Do not modify `SecretBackend` behavior. Do not add production auth-source
  parsing logic beyond what tests need to compile.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` - Reason: semantics must be frozen before
    touching multiple startup and runtime entrypoints.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 2, 3, 4, 5, 6 |
  Blocked By: none

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/main.rs:297-345` - existing CLI startup precedence helper
    pattern for tool approval.
  - Pattern: `src/auth/mod.rs:425-516` - current auth resolution behavior that
    the tests must preserve or redefine explicitly.
  - Pattern: `src/runtime/manager.rs:172-204` - duplicate runtime env shortcut
    that must become test-covered before removal.
  - Pattern: `src/harness/hashline.rs:823-849` - third auth-resolution path that
    must be brought under the shared policy.
  - Pattern: `src/web/config.rs:6-35` - current web startup config shape that
    will need policy coverage.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test auth_source_policy_ -- --nocapture`
  - [ ] `cargo test chat_startup_auth_source_ -- --nocapture`
  - [ ] `cargo test web_startup_auth_source_ -- --nocapture`
  - [ ] `cargo test runtime_recovery_auth_source_ -- --nocapture`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Startup policy semantics are locked
    Tool: Bash
    Steps: Run `cargo test auth_source_policy_ -- --nocapture`.
    Expected: Tests pass only when `auto`, `env`, and `stored` semantics match the plan exactly, including fail-fast cases.
    Evidence: .sisyphus/evidence/task-1-auth-policy.txt

  Scenario: Recovery mismatch behavior is deterministic
    Tool: Bash
    Steps: Run `cargo test runtime_recovery_auth_source_ -- --nocapture`.
    Expected: Tests prove resume/recovery mismatch fails explicitly rather than silently switching auth source or provider.
    Evidence: .sisyphus/evidence/task-1-auth-policy-error.txt
  ```

  **Commit**: YES | Message: `test(auth): lock startup auth source semantics` |
  Files:
  `["src/auth/mod.rs", "src/main.rs", "src/web/config.rs", "src/runtime/manager.rs", "src/harness/hashline.rs"]`

- [ ] 2. Add shared startup policy and trait-based auth-source resolvers

  **What to do**: In `src/auth/mod.rs`, add the shared auth-source abstraction
  above credential storage. Define the startup policy enum
  (`auto | env | stored`), parsing helpers, and a trait-based resolver layer
  such as `AuthSourceResolver`. Provide concrete implementations for
  `EnvAuthSourceResolver`, `StoredAuthSourceResolver`, and
  `AutoAuthSourceResolver`. Keep `SecretBackend` unchanged. Centralize all
  env-vs-stored auth selection here, and expose a single policy-driven
  resolution entrypoint consumed by callers. **Must NOT do**: Do not move or
  redesign OAuth refresh logic. Do not let downstream callers manually inspect
  `OPENAI_API_KEY` anymore once this shared entrypoint exists.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` - Reason: this is the architectural core that
    all startup paths must share.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 3, 4, 5, 6 | Blocked
  By: 1

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/auth/mod.rs:53-57` - existing trait pattern (`SecretBackend`)
    for pluggable implementations.
  - Pattern: `src/auth/mod.rs:294-402` - `CredentialStore` owns storage concerns
    and must stay separate from auth-source concerns.
  - Pattern: `src/auth/mod.rs:425-516` - current `resolve_auth` logic to split
    into policy-driven resolver implementations.
  - Pattern: `src/test_openai.rs:15-21` - shape of `ResolvedAuth` used in tests.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test auth_source_policy_ -- --nocapture`
  - [ ] `cargo test resolve_auth_ -- --nocapture`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Shared auth resolver preserves OpenAI and ZAI behavior
    Tool: Bash
    Steps: Run `cargo test resolve_auth_ -- --nocapture`.
    Expected: Existing auth behavior remains correct for stored OpenAI, stored ZAI, and OpenAI API-key cases, except where the new startup policy intentionally changes semantics.
    Evidence: .sisyphus/evidence/task-2-auth-trait.txt

  Scenario: Stored mode ignores env by construction
    Tool: Bash
    Steps: Run `cargo test auth_source_policy_stored_ignores_openai_env -- --exact`.
    Expected: Test passes only if stored mode does not branch to `OPENAI_API_KEY`.
    Evidence: .sisyphus/evidence/task-2-auth-trait-error.txt
  ```

  **Commit**: YES | Message: `refactor(auth): add startup auth source resolver`
  | Files: `["src/auth/mod.rs"]`

- [ ] 3. Wire CLI startup auth policy through chat startup

  **What to do**: Extend `Command::Chat` in `src/main.rs` with
  `--auth-source <auto|env|stored>`. Parse the startup policy there, using the
  same argument style as existing startup flags. Create the startup-selected
  resolver/policy before entering `chat_loop`, and thread that policy or frozen
  resolver into the runtime startup path so `src/agent.rs` no longer decides
  env-vs-stored ad hoc. Keep the choice process-wide for that chat invocation.
  **Must NOT do**: Do not add a persistent config file or database setting. Do
  not permit mid-session auth-source switching.

  **Recommended Agent Profile**:
  - Category: `quick` - Reason: bounded startup arg plumbing once the shared
    resolver exists.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 5 | Blocked By: 1, 2

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/main.rs:33-78` - current `Chat` startup args.
  - Pattern: `src/main.rs:297-345` - helper style for startup-mode parsing and
    defaults.
  - Pattern: `src/agent.rs:27-75` - current chat startup path that resolves auth
    before runtime creation.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test chat_startup_auth_source_ -- --nocapture`
  - [ ] `cargo test auth_source_policy_ -- --nocapture`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: CLI startup flag selects stored mode
    Tool: Bash
    Steps: Run `cargo test chat_startup_auth_source_cli_override_beats_default -- --exact`.
    Expected: Test proves `--auth-source stored` freezes stored-mode behavior for the chat startup path.
    Evidence: .sisyphus/evidence/task-3-auth-cli.txt

  Scenario: CLI env mode fails fast when unsupported or missing
    Tool: Bash
    Steps: Run `cargo test chat_startup_auth_source_env_mode_fail_fast -- --exact`.
    Expected: Test proves `env` mode errors clearly when required env auth is unavailable or unsupported for the selected provider.
    Evidence: .sisyphus/evidence/task-3-auth-cli-error.txt
  ```

  **Commit**: YES | Message: `feat(cli): add startup auth source flag` | Files:
  `["src/main.rs", "src/agent.rs", "src/auth/mod.rs"]`

- [ ] 4. Wire web startup auth policy through `WebConfig` and `WebAppState`

  **What to do**: Expand `src/web/config.rs::WebConfig` to carry the auth-source
  policy in addition to `bind_addr`, and extend `Command::Web` in `src/main.rs`
  with the same `--auth-source <auto|env|stored>` flag. Update `src/web/mod.rs`
  and `src/web/state.rs` so `WebAppState::for_web_mode(...)` receives the
  startup policy/config rather than implicitly defaulting to current env/store
  behavior. Keep this server-startup only; do not add a browser control. **Must
  NOT do**: Do not add websocket/session commands for changing auth source after
  the server is running. Do not persist the policy in app state beyond the
  startup-config object unless needed for read-only debugging.

  **Recommended Agent Profile**:
  - Category: `quick` - Reason: mostly startup config plumbing with one existing
    config type.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 5 | Blocked By: 1, 2

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/main.rs:79-82` - current `Web` startup args.
  - Pattern: `src/main.rs:253-256` - web startup config construction.
  - Pattern: `src/web/config.rs:6-35` - current `WebConfig` shape.
  - Pattern: `src/web/mod.rs:32-55` - web startup path currently calling
    `WebAppState::for_web_mode()` with no startup policy.
  - Pattern: `src/web/state.rs:531-567` - `WebAppState` constructors and current
    `for_web_mode()` behavior.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web_startup_auth_source_ -- --nocapture`
  - [ ] `cargo test auth_source_policy_ -- --nocapture`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: WebConfig carries startup auth policy
    Tool: Bash
    Steps: Run `cargo test web_startup_auth_source_config_carries_policy -- --exact`.
    Expected: Test proves `WebConfig` preserves the startup auth-source choice from `Command::Web` into web startup.
    Evidence: .sisyphus/evidence/task-4-auth-web.txt

  Scenario: Web startup freezes policy once
    Tool: Bash
    Steps: Run `cargo test web_startup_auth_source_freezes_policy_for_state -- --exact`.
    Expected: Test proves `WebAppState::for_web_mode(...)` receives one policy and does not re-decide env-vs-stored later.
    Evidence: .sisyphus/evidence/task-4-auth-web-error.txt
  ```

  **Commit**: YES | Message: `feat(web): add startup auth source config` |
  Files:
  `["src/main.rs", "src/web/config.rs", "src/web/mod.rs", "src/web/state.rs"]`

- [ ] 5. Remove runtime duplication and lock recovery/resume behavior

  **What to do**: Refactor `src/runtime/manager.rs` so
  `RuntimeWorker::resolved_auth(...)` consumes the startup-selected
  policy/shared resolver rather than checking `OPENAI_API_KEY` directly. Ensure
  recovery and resume use the frozen startup policy, and fail fast if the
  resolved provider conflicts with the requested or resumed provider. Update any
  runtime construction paths needed so this policy is available where recovery
  happens. **Must NOT do**: Do not silently downgrade from `env` to `stored`,
  and do not silently switch providers to make recovery succeed.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` - Reason: this touches runtime startup, worker
    behavior, and recovery semantics.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: 6 | Blocked By: 1,
  2, 3, 4

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/runtime/manager.rs:172-204` - duplicate env shortcut and
    current provider mismatch check.
  - Pattern: `src/main.rs:205-209` - startup recovery begins before chat
    sessions start.
  - Pattern: `src/web/mod.rs:37-42` - web startup recovery path that must honor
    the same frozen policy.
  - Pattern: `src/auth/mod.rs:404-516` - `ResolvedAuth` contract and shared
    resolution behavior.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test runtime_recovery_auth_source_ -- --nocapture`
  - [ ] `cargo test runtime_worker_auth_ -- --nocapture`
  - [ ] `cargo test chat_startup_auth_source_ -- --nocapture`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Runtime worker no longer re-checks env ad hoc
    Tool: Bash
    Steps: Run `cargo test runtime_worker_auth_ -- --nocapture`.
    Expected: Tests prove runtime workers consume the startup-selected auth path and do not independently branch on `OPENAI_API_KEY`.
    Evidence: .sisyphus/evidence/task-5-auth-runtime.txt

  Scenario: Resume/recovery mismatch fails fast
    Tool: Bash
    Steps: Run `cargo test runtime_recovery_auth_source_ -- --nocapture`.
    Expected: Conflicting startup policy/provider combinations fail with deterministic errors and no silent fallback.
    Evidence: .sisyphus/evidence/task-5-auth-runtime-error.txt
  ```

  **Commit**: YES | Message: `refactor(runtime): freeze startup auth source` |
  Files:
  `["src/runtime/manager.rs", "src/auth/mod.rs", "src/main.rs", "src/web/mod.rs", "src/web/state.rs"]`

- [ ] 6. Align `hashline`, add docs, and prove duplicate env checks are gone

  **What to do**: Update `src/harness/hashline.rs` to use the shared startup
  auth-source resolver instead of its own OpenAI env shortcut. Add minimal
  documentation covering the new startup flags and exact mode semantics. Add a
  structural regression test and/or verification command proving there are no
  direct `OPENAI_API_KEY` branches left outside the approved startup-auth module
  and tests. **Must NOT do**: Do not change unrelated harness behavior or expand
  docs into login UX tutorials.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` - Reason: one more consumer plus structural
    verification and docs.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: none | Blocked By:
  1, 2, 5

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/harness/hashline.rs:823-849` - current local auth-resolution
    helper to replace.
  - Pattern: `src/main.rs:297-345` - startup-mode docs and parsing style worth
    mirroring in user-facing docs.
  - Pattern: `README.md:177-199` - current development-notes section where
    startup flag docs can be extended.
  - Pattern: `src/auth/mod.rs:427-448` - existing env-first messaging that must
    align with the new startup policy semantics.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test hashline_auth_source_ -- --nocapture`
  - [ ] `cargo test`
  - [ ] `grep -R "OPENAI_API_KEY" src --line-number`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Hashline uses the shared startup auth policy
    Tool: Bash
    Steps: Run `cargo test hashline_auth_source_ -- --nocapture`.
    Expected: Tests prove hashline no longer has independent env-vs-stored selection behavior.
    Evidence: .sisyphus/evidence/task-6-auth-hashline.txt

  Scenario: Duplicate env branching is structurally removed
    Tool: Bash
    Steps: Run `grep -R "OPENAI_API_KEY" src --line-number` and inspect the output set against the approved startup-auth files and tests.
    Expected: Only approved startup-auth code paths and tests reference `OPENAI_API_KEY`; runtime consumers no longer do.
    Evidence: .sisyphus/evidence/task-6-auth-hashline-error.txt
  ```

  **Commit**: YES | Message: `refactor(auth): align startup auth consumers` |
  Files: `["src/harness/hashline.rs", "README.md", "src/auth/mod.rs"]`

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated
> results to user and get explicit "okay" before completing. **Do NOT
> auto-proceed after verification. Wait for user's explicit approval before
> marking work complete.** **Never mark F1-F4 as checked before getting user's
> okay.** Rejection or user feedback -> fix -> re-run -> present again -> wait
> for okay.

- [ ] F1. Plan Compliance Audit — oracle

  **Acceptance Criteria**:
  - [ ] Oracle reviews `.sisyphus/plans/auth-startup-routing.md` and the branch
        diff together.
  - [ ] Oracle explicitly confirms Tasks 1-6 were satisfied.
  - [ ] Oracle explicitly confirms startup auth is frozen and no forbidden scope
        items landed. **QA Scenarios**:

  ```
  Scenario: Oracle verifies plan compliance
    Tool: task(subagent_type="oracle")
    Steps: Review `.sisyphus/plans/auth-startup-routing.md` and the branch diff; compare implemented files and tests against Tasks 1-6 plus Must Have/Must NOT Have.
    Expected: Oracle returns approval confirming one startup auth policy, no secret-backend redesign, no UI controls, and no missing required deliverables.
    Evidence: .sisyphus/evidence/f1-auth-plan-compliance.md
  ```

- [ ] F2. Code Quality Review — unspecified-high

  **Acceptance Criteria**:
  - [ ] Reviewer inspects all touched auth/startup/runtime files.
  - [ ] Reviewer explicitly approves trait boundaries, naming, and failure
        messages.
  - [ ] Reviewer reports no remaining duplicated env-vs-stored selection logic.
        **QA Scenarios**:

  ```
  Scenario: Reviewer checks code quality and duplication removal
    Tool: task(category="unspecified-high")
    Steps: Review touched files for trait clarity, startup-policy cohesion, failure-path quality, and duplication removal.
    Expected: Reviewer explicitly approves code quality and confirms no runtime consumer re-introduces direct env-vs-stored branching.
    Evidence: .sisyphus/evidence/f2-auth-code-quality.md
  ```

- [ ] F3. Real Manual QA — unspecified-high

  **Acceptance Criteria**:
  - [ ] `cargo test auth_source_policy_ -- --nocapture` passes.
  - [ ] `cargo test chat_startup_auth_source_ -- --nocapture` passes.
  - [ ] `cargo test web_startup_auth_source_ -- --nocapture` passes.
  - [ ] `cargo test runtime_recovery_auth_source_ -- --nocapture` passes.
  - [ ] `cargo test hashline_auth_source_ -- --nocapture` passes. **QA
        Scenarios**:

  ```
  Scenario: Manual QA runs all startup-auth verification commands
    Tool: Bash
    Steps: Run `cargo test auth_source_policy_ -- --nocapture`; run `cargo test chat_startup_auth_source_ -- --nocapture`; run `cargo test web_startup_auth_source_ -- --nocapture`; run `cargo test runtime_recovery_auth_source_ -- --nocapture`; run `cargo test hashline_auth_source_ -- --nocapture`.
    Expected: All commands pass and show startup auth selection behaving consistently across CLI, web, runtime recovery, and hashline.
    Evidence: .sisyphus/evidence/f3-auth-manual-qa.txt
  ```

- [ ] F4. Scope Fidelity Check — deep

  **Acceptance Criteria**:
  - [ ] Reviewer compares the final diff against the Must NOT Have list.
  - [ ] Reviewer confirms `SecretBackend` storage selection was not redesigned.
  - [ ] Reviewer confirms no login UX redesign or UI controls were added. **QA
        Scenarios**:

  ```
  Scenario: Deep review checks scope fidelity
    Tool: task(category="deep")
    Steps: Compare the final diff against the Must NOT Have section and verify no secret-backend redesign, login UX redesign, credential schema change, browser control, or mid-process auth-source mutation landed.
    Expected: Reviewer explicitly confirms the work stayed limited to startup auth routing and shared resolver plumbing.
    Evidence: .sisyphus/evidence/f4-auth-scope-fidelity.md
  ```

## Commit Strategy

- `test(auth): lock startup auth source semantics`
- `refactor(auth): add startup auth source resolver`
- `feat(cli): add startup auth source flag`
- `feat(web): add startup auth source config`
- `refactor(runtime): freeze startup auth source`
- `refactor(auth): align startup auth consumers`

## Success Criteria

- Auth source is selected exactly once at startup from `auto | env | stored`.
- CLI and web startup paths use the same shared auth policy model.
- Runtime workers and recovery paths no longer perform ad hoc env shortcuts.
- `hashline` no longer duplicates auth-source selection.
- Secret-storage backend behavior remains unchanged.
