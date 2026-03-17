# Repo Cleanup for Rapid Iteration

## TL;DR
> **Summary**: Clean up the current Rust CLI before adding more features, but keep the pass behavior-preserving and narrowly aimed at faster iteration. Prioritize repeatable quality checks, characterization coverage for risky runtime paths, and smaller module boundaries in the current hotspots.
> **Deliverables**:
> - repeatable local quality loop and minimal CI gate
> - characterization tests for CLI, auth, store, and agent runtime behavior
> - `src/agent.rs` split into smaller responsibility-based modules without adding `src/lib.rs`
> - `src/auth/mod.rs` split into clearer backend/resolver modules
> - proven-unused code and allowances removed, while speculative schema scaffolding remains in place
> **Effort**: Medium
> **Parallel**: YES - 2 waves
> **Critical Path**: 1/2/3/4/5 -> 6/7/8 -> F1/F2/F3/F4

## Context
### Original Request
Determine whether the repo should be cleaned up before moving to the next features in a greenfield learning project, with broad changes allowed and rapid iteration as the goal.

### Interview Summary
- Decision: do cleanup before adding features.
- Direction: broader refactor is in scope, but only if it improves iteration speed and keeps behavior stable.
- Test policy: `tests-after` overall, reconciled here as characterization-first for risky refactors, then green verification.
- Scope guardrail: no net-new end-user features during cleanup.

### Metis Review (gaps addressed)
- Corrected stale assumption: `.gitignore` already exists at `.gitignore:1` and already ignores `target/`.
- Locked defaults to avoid open choices: keep the crate binary-only for this pass (no `src/lib.rs`), keep speculative schema/runtime scaffold unless a reference audit proves it is both unused and explicitly safe to remove, and use a repo-native shell quality loop plus CI instead of adding a new task-runner dependency.
- Added characterization-first coverage requirements for `src/agent.rs` before structural extraction.
- Added explicit acceptance criteria and edge-case QA for callback, parser, and failed-turn behavior.

## Work Objectives
### Core Objective
Create a cleanup-first baseline that makes this repo easier to evolve safely: one command for local quality checks, one minimal CI gate, characterization coverage around risky flows, and smaller file/module boundaries in the current hotspots.

### Deliverables
- local quality loop script at `scripts/qa.sh`
- minimal CI workflow at `.github/workflows/ci.yml`
- new characterization tests for CLI help flows, auth resolution, store behavior, and agent parsing/resume/error paths
- responsibility split for `src/agent.rs` into submodules under `src/agent/`
- responsibility split for `src/auth/mod.rs` into submodules under `src/auth/`
- dead-code allowance/dependency cleanup backed by reference audits

### Definition of Done (verifiable conditions with commands)
- `bash scripts/qa.sh` exits `0`
- `cargo fmt --all -- --check` exits `0`
- `cargo clippy --all-targets -- -D warnings` exits `0`
- `cargo test` exits `0`
- `cargo run -- --help` exits `0` and output contains `login` and `chat`
- `cargo run -- chat --help` exits `0` and output contains `--last` and `--resume`
- `glob(".github/workflows/ci.yml")` returns exactly one file
- `read(".github/workflows/ci.yml")` shows `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`

### Must Have
- preserve runtime behavior while refactoring
- add characterization tests before changing `src/agent.rs` structure
- keep `src/main.rs` as a thin coordinator
- keep crate shape binary-only during this pass
- preserve `contexts`, `artifacts`, `rate_limits`, `policy`, and `settings` unless reference audit plus migration reasoning proves removal is safe; default is preserve
- use atomic commits with green checks after each task marked `Commit: YES`

### Must NOT Have (guardrails, AI slop patterns, scope boundaries)
- no new providers, commands, flags, or product features
- no trait-heavy transport abstraction or plugin architecture
- no migration that deletes speculative schema tables/fields in this pass by default
- no manual-only verification steps
- no partial quality automation that omits lint, format, or tests

## Verification Strategy
> ZERO HUMAN INTERVENTION — all verification is agent-executed.
- Test decision: `tests-after` delivery with characterization-first coverage for risky refactors; framework is Rust built-in test harness plus `#[tokio::test]`
- QA policy: Every task has agent-executed scenarios
- Evidence: `.sisyphus/evidence/task-{N}-{slug}.{ext}`

## Execution Strategy
### Parallel Execution Waves
> Target: 5-8 tasks per wave. <3 per wave (except final) = under-splitting.
> Extract shared dependencies as Wave-1 tasks for max parallelism.

Wave 1: quality loop, CI gate, CLI characterization, agent characterization, auth/store characterization
Wave 2: agent split, auth split, proven-unused cleanup

### Dependency Matrix (full, all tasks)
- 1: no blockers; foundation for 6, 7, 8
- 2: no blockers; foundation for 6, 7, 8
- 3: no blockers; informs 6
- 4: no blockers; blocks 6
- 5: no blockers; informs 7 and 8
- 6: blocked by 1, 2, 3, 4
- 7: blocked by 1, 2, 5
- 8: blocked by 1, 2, 5 and should run after 6/7 symbol/reference audits complete

### Agent Dispatch Summary (wave → task count → categories)
- Wave 1 -> 5 tasks -> `quick`, `unspecified-low`
- Wave 2 -> 3 tasks -> `deep`, `unspecified-high`
- Final Verification -> 4 tasks -> `oracle`, `unspecified-high`, `unspecified-high`, `deep`

## TODOs
> Implementation + Test = ONE task. Never separate.
> EVERY task MUST have: Agent Profile + Parallelization + QA Scenarios.

- [ ] 1. Add a repo-native local quality loop

  **What to do**: Create `scripts/qa.sh` as the single local validation entrypoint. Make it `bash` with `set -euo pipefail`, resolve the repo root from the script location so it works from any current directory, and run exactly `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` in that order. Do not add `justfile`, `Makefile`, or `src/lib.rs` in this task.
  **Must NOT do**: Do not add extra lint sets, coverage tooling, multi-command wrappers, or docs-only placeholders.

  **Recommended Agent Profile**:
  - Category: `quick` — Reason: one small automation artifact with deterministic commands
  - Skills: `[]` — no specialized skill required
  - Omitted: `[]` — keep the task minimal and repo-native

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 6, 7, 8 | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `Cargo.toml:1` — project is a single Cargo binary crate; use Cargo-native verification commands
  - Pattern: `.gitignore:1` — repo already ignores `target/`; this task is about quality automation, not ignore hygiene
  - Test: `src/store/mod.rs:81` — existing unit/integration style shows the repo already relies on `cargo test`
  - Test: `src/auth/mod.rs:421` — current behavioral tests are compatible with a single repo-wide QA script

  **Acceptance Criteria** (agent-executable only):
  - [ ] `glob("scripts/qa.sh")` returns exactly one file
  - [ ] `read("scripts/qa.sh")` shows `set -euo pipefail`, repo-root resolution, `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`
  - [ ] `bash scripts/qa.sh` exits `0`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Happy path local quality loop
    Tool: Bash
    Steps: Run `bash scripts/qa.sh` from `/home/zack/git/kley`
    Expected: Exit code `0`; output includes `cargo fmt`, `cargo clippy`, and `cargo test` sections finishing successfully
    Evidence: .sisyphus/evidence/task-1-local-quality-loop.txt

  Scenario: Script works outside repo cwd
    Tool: Bash
    Steps: Run `bash -lc 'cd /tmp && /home/zack/git/kley/scripts/qa.sh'`
    Expected: Exit code `0`; script still validates `/home/zack/git/kley` rather than `/tmp`
    Evidence: .sisyphus/evidence/task-1-local-quality-loop-edge.txt
  ```

  **Commit**: YES | Message: `chore: add local quality loop` | Files: `scripts/qa.sh`

- [ ] 2. Add a minimal Rust CI gate

  **What to do**: Create `.github/workflows/ci.yml` with one Linux job on `ubuntu-latest`. Use `actions/checkout@v4`, install stable Rust, and run exactly `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`. Trigger on `push` and `pull_request`. Keep the workflow intentionally small: one job, no matrix, no cache tuning, no deploy/release steps.
  **Must NOT do**: Do not add multi-OS jobs, release automation, artifact publishing, coverage upload, or extra services.

  **Recommended Agent Profile**:
  - Category: `quick` — Reason: one workflow file with fixed commands
  - Skills: `[]` — no specialized skill required
  - Omitted: `[]` — keep CI intentionally lightweight

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 6, 7, 8 | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `Cargo.toml:1` — repo is standard Cargo; CI should call Cargo directly
  - Pattern: `scripts/qa.sh` — reuse the same command order if helpful, but do not make the workflow depend on optional external tooling
  - Test: `src/store/mod.rs:81` — tests already run under Cargo's built-in harness
  - Test: `src/auth/mod.rs:502` — async tests already exist and must remain green in CI

  **Acceptance Criteria** (agent-executable only):
  - [ ] `glob(".github/workflows/ci.yml")` returns exactly one file
  - [ ] `read(".github/workflows/ci.yml")` shows triggers for `push` and `pull_request`
  - [ ] `read(".github/workflows/ci.yml")` shows `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`
  - [ ] `read(".github/workflows/ci.yml")` does not contain `windows-latest`, `macos-latest`, `cargo publish`, or release/deploy steps

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Happy path CI definition exists and matches local loop
    Tool: Read
    Steps: Read `.github/workflows/ci.yml` and verify the workflow contains one Linux job with the exact three Cargo commands
    Expected: Workflow file exists, uses `ubuntu-latest`, and includes the exact commands in the defined order
    Evidence: .sisyphus/evidence/task-2-ci-workflow.md

  Scenario: CI stays intentionally lightweight
    Tool: Read
    Steps: Read `.github/workflows/ci.yml` and verify it lacks a matrix, release, deploy, or publish stages
    Expected: No multi-OS matrix and no non-quality jobs are present
    Evidence: .sisyphus/evidence/task-2-ci-workflow-edge.md
  ```

  **Commit**: YES | Message: `ci: add rust quality workflow` | Files: `.github/workflows/ci.yml`

- [ ] 3. Add CLI characterization coverage for the binary surface

  **What to do**: Add unit tests in `src/main.rs` using Clap's command metadata so the current CLI shape is locked before any refactor. Create exact tests named `cli_help_lists_login_and_chat`, `chat_help_lists_last_and_resume`, and `invalid_subcommand_returns_error`. Use `clap::CommandFactory` rather than shelling out inside the tests. Keep command names, subcommands, and option flags exactly aligned with the existing `Cli`, `Command`, and `LoginProvider` definitions.
  **Must NOT do**: Do not add new commands, rename flags, or move business logic into the tests.

  **Recommended Agent Profile**:
  - Category: `quick` — Reason: narrow test-only coverage around a stable CLI surface
  - Skills: `[]` — no specialized skill required
  - Omitted: `[]` — avoid broader refactors in this task

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 6 | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/main.rs:12` — `Cli` is the top-level parser type
  - Pattern: `src/main.rs:21` — current subcommand shape is `Login` and `Chat`
  - Pattern: `src/main.rs:43` — current login providers are `Openai` and `Zai`
  - Pattern: `src/main.rs:59` — keep runtime coordination here thin; this task is CLI contract coverage only
  - External: `https://docs.rs/clap/latest/clap/trait.CommandFactory.html` — use generated command metadata for stable CLI assertions

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test cli_help_lists_login_and_chat -- --exact` exits `0`
  - [ ] `cargo test chat_help_lists_last_and_resume -- --exact` exits `0`
  - [ ] `cargo test invalid_subcommand_returns_error -- --exact` exits `0`
  - [ ] `cargo run -- --help` exits `0` and output contains `login` and `chat`
  - [ ] `cargo run -- chat --help` exits `0` and output contains `--last` and `--resume`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Happy path help text remains stable
    Tool: Bash
    Steps: Run `cargo run -- --help` and `cargo run -- chat --help`
    Expected: First command exits `0` and prints `login` plus `chat`; second exits `0` and prints `--last` plus `--resume`
    Evidence: .sisyphus/evidence/task-3-cli-help.txt

  Scenario: Invalid subcommand still fails cleanly
    Tool: Bash
    Steps: Run `cargo run -- nope`
    Expected: Non-zero exit code; stderr contains `unrecognized subcommand` or Clap's equivalent error text for invalid command input
    Evidence: .sisyphus/evidence/task-3-cli-help-edge.txt
  ```

  **Commit**: YES | Message: `test: add cli characterization coverage` | Files: `src/main.rs`

- [ ] 4. Add characterization tests for agent runtime behavior

  **What to do**: Add focused tests around the current `src/agent.rs` behavior before any module split. Introduce the smallest possible module-private helpers only if needed to expose behavior for testing; do not perform the structural split yet. Add exact tests named `openai_sse_collects_delta_until_response_completed`, `zai_sse_stops_on_done_marker`, `resume_loads_existing_turn_history_before_new_input`, and `failed_turn_keeps_persisted_user_turn_but_drops_in_memory_history`. Keep the semantics exactly as they are now: failed assistant responses leave the user turn persisted, and resumed sessions load stored turns before the loop continues.
  **Must NOT do**: Do not introduce traits, new transports, retry logic, or provider behavior changes in this task.

  **Recommended Agent Profile**:
  - Category: `unspecified-low` — Reason: test-first safety net around non-trivial runtime behavior
  - Skills: `[]` — no specialized skill required
  - Omitted: `[]` — defer structural cleanup to Task 6

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 6 | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/agent.rs:71` — current chat loop owns resume, persistence, events, and response handling
  - Pattern: `src/agent.rs:235` — preserve current provider-to-default-model mapping
  - Pattern: `src/agent.rs:243` — preserve current OpenAI input item conversion behavior
  - Pattern: `src/agent.rs:258` — preserve OpenAI transport selection/fallback behavior
  - Pattern: `src/agent.rs:298` — current WebSocket response path
  - Pattern: `src/agent.rs:380` — current OpenAI SSE response path
  - Pattern: `src/agent.rs:467` — current ZAI SSE response path
  - Pattern: `src/store/session.rs:124` — current latest/resume session access path
  - Pattern: `src/store/turn.rs:89` — current turn history load behavior

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test openai_sse_collects_delta_until_response_completed -- --exact` exits `0`
  - [ ] `cargo test zai_sse_stops_on_done_marker -- --exact` exits `0`
  - [ ] `cargo test resume_loads_existing_turn_history_before_new_input -- --exact` exits `0`
  - [ ] `cargo test failed_turn_keeps_persisted_user_turn_but_drops_in_memory_history -- --exact` exits `0`
  - [ ] `cargo test` exits `0`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Happy path parser behavior is locked before refactor
    Tool: Bash
    Steps: Run `cargo test openai_sse_collects_delta_until_response_completed -- --exact` and `cargo test zai_sse_stops_on_done_marker -- --exact`
    Expected: Both commands exit `0`; tests prove streamed delta parsing completes only on the current protocol terminators
    Evidence: .sisyphus/evidence/task-4-agent-characterization.txt

  Scenario: Failed-turn and resume edge behavior stays explicit
    Tool: Bash
    Steps: Run `cargo test resume_loads_existing_turn_history_before_new_input -- --exact` and `cargo test failed_turn_keeps_persisted_user_turn_but_drops_in_memory_history -- --exact`
    Expected: Both commands exit `0`; resume uses stored turns first, and failed-turn handling preserves DB history while removing the in-memory user message
    Evidence: .sisyphus/evidence/task-4-agent-characterization-edge.txt
  ```

  **Commit**: YES | Message: `test: add agent runtime characterization coverage` | Files: `src/agent.rs`, `src/store/mod.rs`

- [ ] 5. Add auth/store preservation tests for current cleanup boundaries

  **What to do**: Add or extend tests that lock the current auth/store boundaries the cleanup must preserve. Add exact tests named `schema_migrations_create_scaffold_tables`, `credential_store_prefers_vault_when_env_present`, `credential_store_falls_back_to_age_file_when_vault_env_missing`, `openai_callback_times_out_cleanly`, and `openai_callback_bind_conflict_is_reported`. Reuse the existing in-memory and tempdir patterns already in the repo. If the current `wait_for_callback` shape blocks deterministic tests, extract a module-private helper inside `src/auth/openai.rs` that accepts bind address and timeout duration for tests only, while `login_interactive` continues to use `127.0.0.1:1455` and `60s` exactly. Preserve the current default decision that `contexts`, `artifacts`, `rate_limits`, `policy`, and `settings` remain in place during this cleanup pass.
  **Must NOT do**: Do not remove schema fields/tables, switch storage backends, or change passphrase prompting behavior in this task.

  **Recommended Agent Profile**:
  - Category: `unspecified-low` — Reason: small but important characterization coverage for later cleanup safety
  - Skills: `[]` — no specialized skill required
  - Omitted: `[]` — keep scope to tests and preservation evidence

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 7, 8 | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/store/schema.rs:35` — `contexts` table is intentional scaffold to preserve in this pass
  - Pattern: `src/store/schema.rs:57` — `artifacts` table is intentional scaffold to preserve in this pass
  - Pattern: `src/store/schema.rs:69` — `rate_limits` table is intentional scaffold to preserve in this pass
  - Pattern: `src/store/session.rs:67` — `policy` field is current session surface to preserve
  - Pattern: `src/store/session.rs:71` — `settings` field is current session surface to preserve
  - Pattern: `src/auth/mod.rs:49` — `VaultBackend` is the first-choice backend when env is present
  - Pattern: `src/auth/mod.rs:128` — `AgeFileBackend` is the fallback backend
  - Pattern: `src/auth/mod.rs:201` — `CredentialStore` owns backend selection
  - Pattern: `src/auth/mod.rs:267` — `resolve_auth` must remain compatible with current store behavior
  - Pattern: `src/auth/openai.rs:19` — public redirect URI remains `http://localhost:1455/auth/callback`
  - Pattern: `src/auth/openai.rs:22` — callback port remains `1455`
  - Pattern: `src/auth/openai.rs:205` — callback wait path needs deterministic edge-case coverage
  - Pattern: `src/auth/openai.rs:247` — bind conflict should still surface as callback server bind failure
  - Pattern: `src/auth/openai.rs:259` — timeout path should still surface as a 60s callback timeout in production code
  - Pattern: `src/auth/openai.rs:271` — keep `login_interactive` public flow unchanged

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test schema_migrations_create_scaffold_tables -- --exact` exits `0`
  - [ ] `cargo test credential_store_prefers_vault_when_env_present -- --exact` exits `0`
  - [ ] `cargo test credential_store_falls_back_to_age_file_when_vault_env_missing -- --exact` exits `0`
  - [ ] `cargo test openai_callback_times_out_cleanly -- --exact` exits `0`
  - [ ] `cargo test openai_callback_bind_conflict_is_reported -- --exact` exits `0`
  - [ ] `cargo test wrong_passphrase_is_rejected -- --exact` exits `0`
  - [ ] `cargo test resolve_auth_openai_with_valid_token -- --exact` exits `0`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Happy path scaffold and backend selection are preserved
    Tool: Bash
    Steps: Run `cargo test schema_migrations_create_scaffold_tables -- --exact` and `cargo test credential_store_prefers_vault_when_env_present -- --exact`
    Expected: Both commands exit `0`; scaffold tables exist after migration and Vault selection wins when env vars are present
    Evidence: .sisyphus/evidence/task-5-auth-store-preservation.txt

  Scenario: Fallback and auth edge paths still hold
    Tool: Bash
    Steps: Run `cargo test credential_store_falls_back_to_age_file_when_vault_env_missing -- --exact`, `cargo test wrong_passphrase_is_rejected -- --exact`, `cargo test openai_callback_times_out_cleanly -- --exact`, and `cargo test openai_callback_bind_conflict_is_reported -- --exact`
    Expected: All commands exit `0`; missing Vault env uses age-file fallback, wrong passphrases are rejected cleanly, callback timeout is handled, and bind conflicts surface as deterministic errors
    Evidence: .sisyphus/evidence/task-5-auth-store-preservation-edge.txt
  ```

  **Commit**: YES | Message: `test: add auth and store preservation coverage` | Files: `src/auth/mod.rs`, `src/store/mod.rs`, `src/store/schema.rs`

- [ ] 6. Split `src/agent.rs` into responsibility-based submodules

  **What to do**: Keep `src/agent.rs` as the facade module and move implementation into `src/agent/chat_loop.rs`, `src/agent/openai.rs`, `src/agent/zai.rs`, and `src/agent/message.rs`. Put `chat_loop` and only loop orchestration in `chat_loop.rs`; move OpenAI transport routing plus WS/SSE handling into `openai.rs`; move ZAI SSE handling into `zai.rs`; move `Message`, `default_model`, and `build_input_items` into `message.rs`. Keep public entrypoints unchanged for `src/main.rs`. Preserve event emission, turn persistence, resume logic, default-model logic, and fallback behavior exactly as characterized in Tasks 3 and 4.
  **Must NOT do**: Do not add traits, retries, provider registries, `src/lib.rs`, or any new CLI/user-visible behavior.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: multi-file refactor with behavior-preserving constraints and test guardrails
  - Skills: `[]` — no specialized skill required
  - Omitted: `[]` — reject architecture-heavy abstraction work

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 8 | Blocked By: 1, 2, 3, 4

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/main.rs:92` — keep `agent::chat_loop(...)` call shape unchanged
  - Pattern: `src/agent.rs:71` — current loop responsibilities to isolate into `chat_loop.rs`
  - Pattern: `src/agent.rs:235` — move default model logic to `message.rs`
  - Pattern: `src/agent.rs:243` — move shared message-to-input conversion to `message.rs`
  - Pattern: `src/agent.rs:258` — move OpenAI routing/fallback entrypoint to `openai.rs`
  - Pattern: `src/agent.rs:298` — move WebSocket implementation to `openai.rs`
  - Pattern: `src/agent.rs:380` — move OpenAI SSE implementation to `openai.rs`
  - Pattern: `src/agent.rs:467` — move ZAI SSE implementation to `zai.rs`
  - Pattern: `src/events.rs:12` — preserve event enum usage and emission semantics
  - Pattern: `src/store/session.rs:82` — preserve current session create/get/update behavior
  - Pattern: `src/store/turn.rs:36` — preserve current append/list behavior

  **Acceptance Criteria** (agent-executable only):
  - [ ] `glob("src/agent/*.rs")` returns `src/agent/chat_loop.rs`, `src/agent/openai.rs`, `src/agent/zai.rs`, and `src/agent/message.rs`
  - [ ] `read("src/agent.rs")` shows a thin facade with module declarations and no transport implementation bodies
  - [ ] `cargo test openai_sse_collects_delta_until_response_completed -- --exact` exits `0`
  - [ ] `cargo test failed_turn_keeps_persisted_user_turn_but_drops_in_memory_history -- --exact` exits `0`
  - [ ] `cargo test` exits `0`
  - [ ] `bash scripts/qa.sh` exits `0`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Happy path agent refactor preserves runtime contract
    Tool: Bash
    Steps: Run `cargo test openai_sse_collects_delta_until_response_completed -- --exact`, `cargo test zai_sse_stops_on_done_marker -- --exact`, and `bash scripts/qa.sh`
    Expected: All commands exit `0`; transport parsing and full repo quality checks remain green after the split
    Evidence: .sisyphus/evidence/task-6-agent-split.txt

  Scenario: Resume and failed-turn edge behavior survives extraction
    Tool: Bash
    Steps: Run `cargo test resume_loads_existing_turn_history_before_new_input -- --exact` and `cargo test failed_turn_keeps_persisted_user_turn_but_drops_in_memory_history -- --exact`
    Expected: Both commands exit `0`; extracted modules preserve current resume ordering and failed-turn semantics
    Evidence: .sisyphus/evidence/task-6-agent-split-edge.txt
  ```

  **Commit**: YES | Message: `refactor(agent): split chat loop and transports` | Files: `src/agent.rs`, `src/agent/chat_loop.rs`, `src/agent/openai.rs`, `src/agent/zai.rs`, `src/agent/message.rs`

- [ ] 7. Split auth backend and resolver responsibilities without changing provider behavior

  **What to do**: Keep `src/auth/mod.rs` as the facade for shared credential structs and reexports. Move backend definitions (`SecretBackend`, `VaultBackend`, `AgeFileBackend`, `CredentialStore`) into `src/auth/backend.rs`. Move `ResolvedAuth` and `resolve_auth` into `src/auth/resolve.rs`. Keep provider-specific code in `src/auth/openai.rs` and `src/auth/zai.rs`. Preserve backend selection order, passphrase prompting, token refresh behavior, and error messages currently exercised by tests.
  **Must NOT do**: Do not change provider names, token field names, Vault selection order, storage locations, or introduce a new abstraction layer.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: multi-file responsibility split around security-sensitive auth code
  - Skills: `[]` — no specialized skill required
  - Omitted: `[]` — avoid architectural expansion beyond module extraction

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 8 | Blocked By: 1, 2, 5

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/auth/mod.rs:41` — backend trait currently lives in the monolith and should move to `backend.rs`
  - Pattern: `src/auth/mod.rs:49` — Vault backend implementation to move intact
  - Pattern: `src/auth/mod.rs:128` — age-file backend implementation to move intact
  - Pattern: `src/auth/mod.rs:201` — `CredentialStore` currently bundles backend selection and storage access
  - Pattern: `src/auth/mod.rs:267` — move resolved auth flow to `resolve.rs` without changing behavior
  - Pattern: `src/auth/openai.rs:120` — keep OpenAI token refresh contract unchanged
  - Pattern: `src/events.rs:12` — preserve emitted auth-related runtime events

  **Acceptance Criteria** (agent-executable only):
  - [ ] `glob("src/auth/*.rs")` returns `src/auth/backend.rs`, `src/auth/resolve.rs`, `src/auth/openai.rs`, and `src/auth/zai.rs`
  - [ ] `read("src/auth/mod.rs")` shows a thin facade with module declarations, shared types, and reexports instead of full backend implementations
  - [ ] `cargo test credential_store_prefers_vault_when_env_present -- --exact` exits `0`
  - [ ] `cargo test credential_store_falls_back_to_age_file_when_vault_env_missing -- --exact` exits `0`
  - [ ] `cargo test resolve_auth_openai_with_valid_token -- --exact` exits `0`
  - [ ] `cargo test resolve_auth_rejects_unknown_provider -- --exact` exits `0`
  - [ ] `bash scripts/qa.sh` exits `0`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Happy path auth split preserves backend selection and token resolution
    Tool: Bash
    Steps: Run `cargo test credential_store_prefers_vault_when_env_present -- --exact`, `cargo test resolve_auth_openai_with_valid_token -- --exact`, and `bash scripts/qa.sh`
    Expected: All commands exit `0`; backend selection and auth resolution behavior remain unchanged after the split
    Evidence: .sisyphus/evidence/task-7-auth-split.txt

  Scenario: Auth edge paths stay stable
    Tool: Bash
    Steps: Run `cargo test credential_store_falls_back_to_age_file_when_vault_env_missing -- --exact`, `cargo test wrong_passphrase_is_rejected -- --exact`, and `cargo test resolve_auth_rejects_unknown_provider -- --exact`
    Expected: All commands exit `0`; fallback, wrong-passphrase, and unknown-provider errors still behave as before
    Evidence: .sisyphus/evidence/task-7-auth-split-edge.txt
  ```

  **Commit**: YES | Message: `refactor(auth): split backend and resolver responsibilities` | Files: `src/auth/mod.rs`, `src/auth/backend.rs`, `src/auth/resolve.rs`, `src/auth/openai.rs`, `src/auth/zai.rs`

- [ ] 8. Remove proven-unused code and dead-code allowances while preserving roadmap scaffold

  **What to do**: Perform a final cleanup pass only on code proven unused after Tasks 6 and 7 land. Remove `EventReceiver::try_recv` and `EventReceiver::drain` from `src/events.rs` because current source references show no production/test callers. Remove `CredentialStore::backend_name` and its backing field if still unused after the auth split. Convert `AgeFileBackend::new` to a test-only constructor with `#[cfg(test)]` instead of `#[allow(dead_code)]`, because current references are test-only. Keep `contexts`, `artifacts`, `rate_limits`, `policy`, and `settings` intact in this task.
  **Must NOT do**: Do not delete scaffold schema tables/fields, remove provider flows, or prune dependencies unless a direct source reference audit plus `cargo test` proves they are unused and safe to delete.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: cleanup requires careful reference audits after structural refactors
  - Skills: `[]` — no specialized skill required
  - Omitted: `[]` — do not expand into speculative architecture work

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: none | Blocked By: 1, 2, 5, 6, 7

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/events.rs:106` — `EventReceiver` owns the unused helper methods to audit/remove
  - Pattern: `src/events.rs:113` — `try_recv` is currently defined with no known callers
  - Pattern: `src/events.rs:119` — `drain` is currently defined with no known callers
  - Pattern: `src/auth/mod.rs:128` — `AgeFileBackend::new` exists mainly for tests and should become test-only if refactor preserves that fact
  - Pattern: `src/auth/mod.rs:201` — `CredentialStore` currently holds `backend_name`; remove it if no callers remain after the split
  - Pattern: `src/store/schema.rs:35` — preserve `contexts`
  - Pattern: `src/store/schema.rs:57` — preserve `artifacts`
  - Pattern: `src/store/schema.rs:69` — preserve `rate_limits`
  - Pattern: `src/store/session.rs:67` — preserve `policy`
  - Pattern: `src/store/session.rs:71` — preserve `settings`

  **Acceptance Criteria** (agent-executable only):
  - [ ] `grep("try_recv\\(", include="*.rs", path="/home/zack/git/kley/src", output_mode="content")` shows no production/test callers beyond the removed helper definition
  - [ ] `grep("drain\\(", include="*.rs", path="/home/zack/git/kley/src", output_mode="content")` shows no production/test callers beyond the removed helper definition
  - [ ] `grep("backend_name\\(", include="*.rs", path="/home/zack/git/kley/src", output_mode="content")` shows no remaining callers if the field/method are removed
  - [ ] `cargo test wrong_passphrase_is_rejected -- --exact` exits `0`
  - [ ] `cargo test resolve_auth_rejects_unknown_provider -- --exact` exits `0`
  - [ ] `bash scripts/qa.sh` exits `0`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Happy path cleanup removes only proven-unused helpers
    Tool: Bash
    Steps: Run `bash scripts/qa.sh`
    Expected: Exit code `0`; repo still formats, lints, and tests successfully after the cleanup pass
    Evidence: .sisyphus/evidence/task-8-proven-unused-cleanup.txt

  Scenario: Reference audit confirms no accidental live-code deletion
    Tool: Grep
    Steps: Search for `try_recv(`, `drain(`, and `backend_name(` across `/home/zack/git/kley/src`
    Expected: No remaining callers require the removed helpers; scaffold fields/tables remain present in `src/store/schema.rs` and `src/store/session.rs`
    Evidence: .sisyphus/evidence/task-8-proven-unused-cleanup-edge.md
  ```

  **Commit**: YES | Message: `chore: remove proven-unused code` | Files: `src/events.rs`, `src/auth/mod.rs`, `src/auth/backend.rs`, `Cargo.toml` (only if reference audit proves a dependency is unused)

## Final Verification Wave (4 parallel agents, ALL must APPROVE)
- [ ] F1. Plan Compliance Audit — oracle

  **What to do**: Verify the implemented branch against this plan only. Confirm every Task 1-8 deliverable exists, every required file path exists, every required command passed, and every task produced evidence under `.sisyphus/evidence/`.
  **Tool / Steps / Expected / Evidence**:
  ```
  Tool: Read + Glob + Bash
  Steps:
    1. Read `.sisyphus/plans/repo-cleanup-iteration-plan.md`
    2. Glob for `scripts/qa.sh`, `.github/workflows/ci.yml`, `src/agent/*.rs`, and `src/auth/*.rs`
    3. Run `bash scripts/qa.sh`
    4. Read the changed files and verify each task's acceptance criteria were satisfied
  Expected:
    - All planned deliverables exist
    - `bash scripts/qa.sh` exits `0`
    - No planned task is skipped or replaced with an unplanned alternative
  Evidence: .sisyphus/evidence/f1-plan-compliance.md
  ```

- [ ] F2. Code Quality Review — unspecified-high

  **What to do**: Review the final code for cleanup quality, correctness, and avoidable complexity. Focus on whether the refactors preserved behavior, kept `src/main.rs` thin, and avoided trait-heavy overengineering.
  **Tool / Steps / Expected / Evidence**:
  ```
  Tool: Read + Bash
  Steps:
    1. Read `src/main.rs`, `src/agent.rs`, `src/agent/*.rs`, `src/auth/mod.rs`, and `src/auth/*.rs`
    2. Run `cargo clippy --all-targets -- -D warnings`
    3. Inspect whether new modules follow the plan's intended responsibility split
  Expected:
    - `cargo clippy --all-targets -- -D warnings` exits `0`
    - `src/main.rs` remains a thin coordinator
    - No new trait hierarchy, plugin system, or user-visible feature creep is introduced
  Evidence: .sisyphus/evidence/f2-code-quality.md
  ```

- [ ] F3. Real Runtime QA — unspecified-high

  **What to do**: Execute the repo's runtime-facing smoke checks without human intervention. Use commands only; there is no UI/browser surface in this repo.
  **Tool / Steps / Expected / Evidence**:
  ```
  Tool: Bash
  Steps:
    1. Run `cargo run -- --help`
    2. Run `cargo run -- chat --help`
    3. Run `cargo test openai_sse_collects_delta_until_response_completed -- --exact`
    4. Run `cargo test zai_sse_stops_on_done_marker -- --exact`
    5. Run `cargo test openai_callback_times_out_cleanly -- --exact`
    6. Run `cargo test openai_callback_bind_conflict_is_reported -- --exact`
  Expected:
    - Help commands exit `0` and still expose `login`, `chat`, `--last`, and `--resume`
    - Targeted runtime and callback edge-case tests exit `0`
  Evidence: .sisyphus/evidence/f3-runtime-qa.txt
  ```

- [ ] F4. Scope Fidelity Check — deep

  **What to do**: Confirm the cleanup pass stayed within scope: no new product features, no schema deletions for scaffold tables/fields, and no shift from binary crate to `src/lib.rs`.
  **Tool / Steps / Expected / Evidence**:
  ```
  Tool: Read + Glob + Grep
  Steps:
    1. Glob for `src/lib.rs`
    2. Read `src/store/schema.rs` and `src/store/session.rs`
    3. Grep the workspace for new provider names, new top-level Clap subcommands, or removed scaffold identifiers (`contexts`, `artifacts`, `rate_limits`, `policy`, `settings`)
  Expected:
    - `src/lib.rs` does not exist
    - scaffold identifiers remain present
    - no new provider or CLI feature surface was added during cleanup
  Evidence: .sisyphus/evidence/f4-scope-fidelity.md
  ```

## Commit Strategy
- Use small green commits in this order: `chore: add local quality loop`, `ci: add rust quality workflow`, `test: add cli characterization coverage`, `test: add agent runtime characterization coverage`, `test: add auth and store preservation coverage`, `refactor(agent): split chat loop and transports`, `refactor(auth): split backend and resolver responsibilities`, `chore: remove proven-unused code`
- Do not squash characterization and refactor changes into one commit; keep the safety net visible in history.

## Success Criteria
- The repo can be validated locally with one command and in CI with one workflow.
- `src/agent.rs` and `src/auth/mod.rs` no longer act as multi-responsibility hotspots.
- New tests lock the current behavior of help flows, auth resolution, resume flow, transport parsing, and failed-turn handling.
- Cleanup reduces future iteration friction without changing product scope.
