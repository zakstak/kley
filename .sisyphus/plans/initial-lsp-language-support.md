# Initial LSP Language Support

## TL;DR

> **Summary**: Add first-class, built-in LSP support for Rust, Go, Bash, Nix,
> YAML, and Python by introducing a session-scoped LSP subsystem, wiring an
> initial OpenCode-style tool surface, exposing deterministic load/status
> visibility, and making the required servers available through the repo’s Nix
> flake and VM baseline. **Deliverables**:
>
> - Built-in LSP server catalog and exact language-detection rules for the
>   initial language set
> - Session-scoped LSP manager plus initial LSP tool suite
> - Runtime/web visibility for detection and load state without adding
>   user-configurable settings
> - Strict preflight/runtime enforcement and Nix/VM baseline parity for required
>   server binaries **Effort**: Large **Parallel**: NO **Critical Path**: 1 → 2
>   → 4 → 5/6 → 7 → 8/9

## Context

### Original Request

Add LSP support for the file types being used, using OpenCode and
oh-my-openagent as references, with an initial limited set of: rust, golang,
bash, nixd, yaml, and pyright.

### Interview Summary

- `tsgo` was explicitly removed from scope.
- V1 must use **bundled built-in defaults only**; no user/project/session LSP
  override system.
- V1 must **expose usage and when LSPs are loaded** via language detection and
  status visibility, but **must not** add user-configurable LSP settings.
- Missing servers are **strictly required**.
- Test strategy is **tests-after**.
- Keep the full requested language set even though this checkout does not
  currently track `.go` or `.py` source files.
- Because the project ships a Nix flake, the supported LSP binaries should be
  installed through the repo-owned Nix/dev baseline.

### Metis Review (gaps addressed)

- Locked v1 to **built-in registry + exact extension detection + strict
  required-server validation + lazy runtime lifecycle + visible status**.
- Explicitly kept **full LSP feature explosion** out of scope: no config
  layering, no auto-download/install, no fuzzy detection, no generic plugin
  system.
- Chose **lazy startup**: start a server on the first LSP tool call for a
  matching file, not at session attach.
- Chose **terminal failed state per session/server**: if a server fails to start
  or crashes unexpectedly, mark it failed for that session and return the
  recorded error on later calls instead of silently retrying.
- Chose **snapshot + event visibility**: add structured LSP state to
  `state.snapshot` and reuse `status.report` events for human-readable
  transition updates.

## Work Objectives

### Core Objective

Ship a first-class LSP subsystem that can serve the initial built-in languages
through OpenCode-style LSP tools, with deterministic detection, deterministic
loading, deterministic failure behavior, and repo-native installation of all
required server binaries.

### Deliverables

- New `src/lsp/` subsystem with built-in server definitions, root resolution,
  client/manager, and tests.
- Registered built-in tool surface:
  - `lsp_diagnostics`
  - `lsp_symbols`
  - `lsp_goto_definition`
  - `lsp_find_references`
  - `lsp_prepare_rename`
  - `lsp_rename`
- Runtime integration so LSP tools execute through a session-scoped manager.
- Web/runtime visibility contract for language detection and server state.
- Updated `preflight`, Nix dev shell, VM baseline, and manifest parity checks.

### Definition of Done (verifiable conditions with commands)

- `cargo test lsp_builtin_catalog_matches_initial_languages -- --exact`
- `cargo test lsp_root_resolution_matches_language_rules -- --exact`
- `cargo test lsp_manager_starts_once_per_session_server -- --exact`
- `cargo test lsp_read_tools_match_opencode_contracts -- --exact`
- `cargo test lsp_rename_requires_prepare_success -- --exact`
- `cargo test runtime_executes_lsp_tools_via_session_manager -- --exact`
- `cargo test lsp_status_is_exposed_in_web_snapshot_and_events --test web -- --exact`
- `cargo test preflight_requires_all_v1_lsp_servers -- --exact`
- `bash tests/vm-baseline-check.sh`
- `nix develop -c bash -lc 'command -v rust-analyzer gopls bash-language-server yaml-language-server nixd pyright-langserver'`
- `cargo test`

### Must Have

- Exact built-in mappings for:
  - `.rs` → `rust-analyzer`
  - `.go` → `gopls`
  - `.sh`, `.bash`, `.zsh`, `.ksh` → `bash-language-server`
  - `.nix` → `nixd`
  - `.yaml`, `.yml` → `yaml-language-server`
  - `.py`, `.pyi` → `pyright`
- Runtime launch commands fixed in code for v1:
  - `rust-analyzer`
  - `gopls`
  - `bash-language-server start`
  - `nixd`
  - `yaml-language-server --stdio`
  - `pyright-langserver --stdio`
- Lazy startup on first matching LSP tool call.
- Structured snapshot visibility with per-server state and human-readable
  `status.report` transitions.
- Flake/dev-shell and VM baseline parity for all required binaries.

### Must NOT Have (guardrails, AI slop patterns, scope boundaries)

- No `tsgo` support.
- No `typescript-language-server` support in this change.
- No user/project/session override config for LSP servers in v1.
- No auto-download or auto-install behavior.
- No heuristic/shebang-only detection in v1; use explicit extension matching
  only.
- No eager startup at session creation.
- No generic plugin/registry system for arbitrary third-party LSPs.
- No web settings UI or CLI flags for LSP configuration in v1.
- No silent fallback when a required binary is missing or a server enters failed
  state.

## Verification Strategy

> ZERO HUMAN INTERVENTION — all verification is agent-executed.

- Test decision: tests-after via Rust unit/integration tests, web socket
  integration tests, and Nix/baseline checks
- QA policy: Every task has agent-executed scenarios
- Evidence: `.sisyphus/evidence/task-{N}-{slug}.txt`

## Execution Strategy

### Parallel Execution Waves

> This work is intentionally sequential because the tool contracts, runtime
> lifecycle, and visibility depend on a single LSP subsystem seam. Use the waves
> below as execution tranches, not as parallel dispatch batches.

Wave 1: built-in catalog, root rules, packaging baseline Wave 2: manager/client,
read-only tools, rename tools Wave 3: runtime integration, web visibility,
strict enforcement

### Dependency Matrix (full, all tasks)

| Task | Depends On | Blocks             |
| ---- | ---------- | ------------------ |
| 1    | —          | 2, 4, 5, 6, 9      |
| 2    | 1          | 4, 7               |
| 3    | —          | 9                  |
| 4    | 1, 2       | 5, 6, 7            |
| 5    | 4          | 7                  |
| 6    | 4          | 7                  |
| 7    | 2, 4, 5, 6 | 8                  |
| 8    | 7          | Final verification |
| 9    | 1, 3       | Final verification |

### Agent Dispatch Summary (wave → task count → categories)

- Wave 1 → 3 tasks → `deep`, `unspecified-high`, `quick`
- Wave 2 → 3 tasks → `deep`, `unspecified-high`, `unspecified-high`
- Wave 3 → 3 tasks → `deep`, `unspecified-high`, `quick`

## TODOs

> Implementation + Test = ONE task. Never separate. EVERY task MUST have: Agent
> Profile + Parallelization + QA Scenarios.

- [x] 1. Create the built-in LSP catalog and exact extension map

  **What to do**: Add a new `src/lsp/` module rooted at `src/lsp/mod.rs` and
  define the v1 built-in server catalog in code. The catalog must hardcode the
  six supported server IDs, launch commands, and extension lists. Normalize path
  extensions to lowercase before lookup, but do not add heuristic detection
  beyond the explicit extension list. Add exact tests for supported mappings and
  unsupported extensions. Treat `pyright` as the logical server ID and
  `pyright-langserver --stdio` as its runtime command. **Must NOT do**: Do not
  add config loading, environment overrides, or support for `tsgo`/TypeScript.
  Do not infer shell support from shebangs or executable bits.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: establishes the core domain model every later
    task consumes
  - Skills: `[]` — no extra skill required
  - Omitted: `[find-docs]` — external reference behavior is already captured in
    the planning research

  **Parallelization**: Can Parallel: NO | Wave 1 | Blocks: 2, 4, 5, 6, 9 |
  Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/tools/mod.rs:182-193` — built-in registry pattern for adding
    first-class capabilities
  - Pattern: `src/preflight.rs:172-205` — current hardcoded LSP binary list and
    naming conventions
  - External:
    `https://github.com/anomalyco/opencode/blob/c72642dd35299b9bbf910360191690212e977b56/packages/opencode/src/config/config.ts#L961-L996`
    — OpenCode LSP config/server shape
  - External:
    `https://github.com/code-yeongyu/oh-my-openagent/blob/53eeac3f31ee2218ad54c4c8b62d171a8045409a/src/tools/lsp/types.ts#L1-L8`
    — oh-my-openagent server definition fields

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test lsp_builtin_catalog_matches_initial_languages -- --exact`
  - [ ] `cargo test lsp_builtin_catalog_rejects_unsupported_extensions -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):

  ```
  Scenario: Happy path builtin mappings
    Tool: Bash
    Steps: Run `cargo test lsp_builtin_catalog_matches_initial_languages -- --exact | tee .sisyphus/evidence/task-1-builtin-catalog.txt`
    Expected: Exit code 0; test output contains `test lsp_builtin_catalog_matches_initial_languages ... ok`
    Evidence: .sisyphus/evidence/task-1-builtin-catalog.txt

  Scenario: Unsupported extension is rejected
    Tool: Bash
    Steps: Run `cargo test lsp_builtin_catalog_rejects_unsupported_extensions -- --exact | tee .sisyphus/evidence/task-1-builtin-catalog-error.txt`
    Expected: Exit code 0; test output contains `test lsp_builtin_catalog_rejects_unsupported_extensions ... ok`
    Evidence: .sisyphus/evidence/task-1-builtin-catalog-error.txt
  ```

  **Commit**: NO | Message: `feat(lsp): add builtin server catalog` | Files:
  `src/lsp/**`

- [x] 2. Implement exact workspace-root resolution rules per language family

  **What to do**: Add a dedicated root-resolution module that receives a file
  path plus server ID and returns the workspace root for that tool invocation.
  Use these exact v1 rules: Rust → nearest `Cargo.toml` or `rust-project.json`,
  else git root, else file parent; Go → nearest `go.work`, then `go.mod`, else
  git root, else file parent; Bash/YAML → git root if present, else file parent;
  Nix → nearest `flake.nix`, `shell.nix`, or `default.nix`, else git root, else
  file parent; Python → nearest `pyproject.toml`, `pyrightconfig.json`,
  `setup.py`, `requirements.txt`, or `.venv`, else git root, else file parent.
  Add exact tests that prove each branch. **Must NOT do**: Do not implement
  multi-root workspaces, schema-based YAML heuristics, Deno/TypeScript overlap
  logic, or cross-session shared caches.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: bounded but logic-heavy filesystem
    decision tree
  - Skills: `[]` — no extra skill required
  - Omitted: `[find-docs]` — root rules are fixed by this plan, not open-ended
    research

  **Parallelization**: Can Parallel: NO | Wave 1 | Blocks: 4, 7 | Blocked By: 1

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/preflight.rs:1-17` — existing path/process utility style
  - External:
    `https://github.com/anomalyco/opencode/blob/c72642dd35299b9bbf910360191690212e977b56/packages/opencode/src/lsp/server.ts#L67-L111`
    — upstream root-finding conflict handling precedent

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test lsp_root_resolution_matches_language_rules -- --exact`
  - [ ] `cargo test lsp_root_resolution_falls_back_without_markers -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):

  ```
  Scenario: Marker-based roots resolve correctly
    Tool: Bash
    Steps: Run `cargo test lsp_root_resolution_matches_language_rules -- --exact | tee .sisyphus/evidence/task-2-root-rules.txt`
    Expected: Exit code 0; test output contains `test lsp_root_resolution_matches_language_rules ... ok`
    Evidence: .sisyphus/evidence/task-2-root-rules.txt

  Scenario: Fallback root is deterministic without markers
    Tool: Bash
    Steps: Run `cargo test lsp_root_resolution_falls_back_without_markers -- --exact | tee .sisyphus/evidence/task-2-root-rules-error.txt`
    Expected: Exit code 0; test output contains `test lsp_root_resolution_falls_back_without_markers ... ok`
    Evidence: .sisyphus/evidence/task-2-root-rules-error.txt
  ```

  **Commit**: NO | Message: `feat(lsp): add root resolution rules` | Files:
  `src/lsp/root.rs`, `src/lsp/**`

- [x] 3. Install the required LSP binaries through the flake and VM baseline

  **What to do**: Update the root dev shell and the repo-owned developer-heavy
  VM profile to install the six required runtime binaries. Use Nix packages that
  provide these commands: `rust-analyzer`, `gopls`, `bash-language-server`,
  `yaml-language-server`, `nixd`, and `pyright-langserver` (via the `pyright`
  package). Update the developer-heavy manifest so the baseline check sees the
  same binary names. Keep existing packages intact. **Must NOT do**: Do not add
  `tsgo` or `typescript-language-server`. Do not remove existing development
  packages. Do not rely on ad hoc npm/pip/go installation outside Nix.

  **Recommended Agent Profile**:
  - Category: `quick` — Reason: isolated Nix/package manifest change with clear
    binary targets
  - Skills: `[]` — no extra skill required
  - Omitted: `[git]` — commit handling is deferred to the global commit strategy

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 9 | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `flake.nix:31-59` — dev shell package inventory style
  - Pattern: `agent-vm/profiles/developer-heavy.nix:1-32` — developer-heavy
    package inventory
  - Pattern: `agent-vm/developer-heavy-tool-manifest.txt:1-21` — manifest naming
    and coverage style
  - External: `nix search nixpkgs bash-language-server` — confirms
    `bash-language-server` package exists in nixpkgs
  - External: `nix search nixpkgs yaml-language-server` — confirms
    `yaml-language-server` package exists in nixpkgs
  - External: `nix search nixpkgs nixd` — confirms `nixd` package exists in
    nixpkgs
  - External: `nix search nixpkgs pyright` — confirms `pyright` package exists
    in nixpkgs

  **Acceptance Criteria** (agent-executable only):
  - [ ] `nix develop -c bash -lc 'command -v rust-analyzer gopls bash-language-server yaml-language-server nixd pyright-langserver'`

  **QA Scenarios** (MANDATORY — task incomplete without these):

  ```
  Scenario: Dev shell exposes all required binaries
    Tool: Bash
    Steps: Run `nix develop -c bash -lc 'command -v rust-analyzer gopls bash-language-server yaml-language-server nixd pyright-langserver' | tee .sisyphus/evidence/task-3-nix-packaging.txt`
    Expected: Exit code 0; all six command paths are printed
    Evidence: .sisyphus/evidence/task-3-nix-packaging.txt

  Scenario: Pyright language server binary is present via Nix package
    Tool: Bash
    Steps: Run `nix develop -c bash -lc 'command -v pyright-langserver && pyright --version' | tee .sisyphus/evidence/task-3-nix-packaging-error.txt`
    Expected: Exit code 0; `pyright-langserver` path prints and `pyright --version` succeeds
    Evidence: .sisyphus/evidence/task-3-nix-packaging-error.txt
  ```

  **Commit**: NO | Message: `build(nix): install required lsp servers` | Files:
  `flake.nix`, `agent-vm/profiles/developer-heavy.nix`,
  `agent-vm/developer-heavy-tool-manifest.txt`

- [x] 4. Build a session-scoped lazy LSP manager and stdio JSON-RPC client

  **What to do**: Introduce an internal `LspService` seam plus a concrete
  manager/client implementation that launches the correct server command lazily
  on first use, scopes one process per session+server+workspace-root, and
  communicates over stdio JSON-RPC/LSP. The manager must memoize
  `idle|starting|ready|failed` state, reuse a ready process for subsequent calls
  in the same session/root, and mark a server terminally failed for that session
  after startup failure or unexpected exit. Add tests with fake child processes
  or a stub transport to prove reuse and failed-state behavior. **Must NOT do**:
  Do not share processes across sessions. Do not auto-retry failed servers. Do
  not start servers before the first matching tool call.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: highest-risk runtime seam with process lifecycle
    and protocol ownership
  - Skills: `[]` — no extra skill required
  - Omitted: `[find-docs]` — behavior is already pinned by the plan and external
    research

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 5, 6, 7 | Blocked By:
  1, 2

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/runtime/session.rs:1050-1207` — runtime call lifecycle and
    event emission style
  - Pattern: `src/web/ws/event_map.rs:193-205` — existing status event mapping
    contract
  - External:
    `https://github.com/anomalyco/opencode/blob/c72642dd35299b9bbf910360191690212e977b56/packages/opencode/src/lsp/index.ts#L172-L274`
    — upstream built-in server activation and enable/disable loop

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test lsp_manager_starts_once_per_session_server -- --exact`
  - [ ] `cargo test lsp_manager_marks_failed_servers_terminal -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):

  ```
  Scenario: Ready server is reused in-session
    Tool: Bash
    Steps: Run `cargo test lsp_manager_starts_once_per_session_server -- --exact | tee .sisyphus/evidence/task-4-lsp-manager.txt`
    Expected: Exit code 0; test output contains `test lsp_manager_starts_once_per_session_server ... ok`
    Evidence: .sisyphus/evidence/task-4-lsp-manager.txt

  Scenario: Failed server becomes terminal for that session
    Tool: Bash
    Steps: Run `cargo test lsp_manager_marks_failed_servers_terminal -- --exact | tee .sisyphus/evidence/task-4-lsp-manager-error.txt`
    Expected: Exit code 0; test output contains `test lsp_manager_marks_failed_servers_terminal ... ok`
    Evidence: .sisyphus/evidence/task-4-lsp-manager-error.txt
  ```

  **Commit**: NO | Message: `feat(lsp): add lazy session manager` | Files:
  `src/lsp/**`

- [x] 5. Add the read-only/query LSP tool surface and register it

  **What to do**: Implement `lsp_diagnostics`, `lsp_symbols`,
  `lsp_goto_definition`, and `lsp_find_references` as first-class built-in
  tools, and register them in `src/tools/mod.rs`. Copy the request/response
  contract shape from OpenCode’s `tool/lsp.ts` for these operations, translated
  into the repo’s snake_case Rust structs and strict JSON-schema generation.
  Route every tool through the `LspService` seam, require a supported file
  extension, and return recoverable tool-domain errors for unsupported files or
  failed servers. **Must NOT do**: Do not invent additional LSP tools. Do not
  bypass the manager/client seam. Do not return raw protocol blobs when a stable
  tool result shape can be preserved from the upstream contract.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: schema fidelity and tool-registry
    correctness matter more than raw algorithmic complexity
  - Skills: `[]` — no extra skill required
  - Omitted: `[find-docs]` — use the pinned OpenCode source reference directly

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 7 | Blocked By: 4

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/tools/mod.rs:109-193` — tool trait, schema generation, and
    registration style
  - Pattern: `src/runtime/session.rs:1123-1137` — registry lookup and tool
    execution path
  - External:
    `https://github.com/anomalyco/opencode/blob/f2d4ced8ea527dd6518e87354b886204a2819cab/packages/opencode/src/tool/lsp.ts`
    — source of truth for initial tool contract behavior

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test lsp_read_tools_match_opencode_contracts -- --exact`
  - [ ] `cargo test lsp_read_tools_error_on_unsupported_filetype -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):

  ```
  Scenario: Read-only tools expose the expected contract
    Tool: Bash
    Steps: Run `cargo test lsp_read_tools_match_opencode_contracts -- --exact | tee .sisyphus/evidence/task-5-read-tools.txt`
    Expected: Exit code 0; test output contains `test lsp_read_tools_match_opencode_contracts ... ok`
    Evidence: .sisyphus/evidence/task-5-read-tools.txt

  Scenario: Unsupported file types fail cleanly
    Tool: Bash
    Steps: Run `cargo test lsp_read_tools_error_on_unsupported_filetype -- --exact | tee .sisyphus/evidence/task-5-read-tools-error.txt`
    Expected: Exit code 0; test output contains `test lsp_read_tools_error_on_unsupported_filetype ... ok`
    Evidence: .sisyphus/evidence/task-5-read-tools-error.txt
  ```

  **Commit**: NO | Message: `feat(tools): add read-only lsp tools` | Files:
  `src/tools/mod.rs`, `src/tools/lsp.rs`, `src/lsp/**`

- [x] 6. Add guarded rename support with mandatory prepare-rename validation

  **What to do**: Implement `lsp_prepare_rename` and `lsp_rename` on the same
  `LspService` seam. `lsp_rename` must never run directly against the server
  unless `prepareRename` succeeds for the provided location during that same
  request path. Return deterministic tool-domain errors for invalid rename
  targets, unsupported files, or failed servers. Preserve the upstream OpenCode
  contract shape for arguments and results. **Must NOT do**: Do not collapse
  `prepare_rename` into `rename`. Do not apply edits if the server reports
  rename is invalid. Do not add workspace-wide refactor helpers beyond the
  upstream contract.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: write-capable LSP operations need
    tighter safety handling than read-only queries
  - Skills: `[]` — no extra skill required
  - Omitted: `[git]` — no commit work belongs in this task

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 7 | Blocked By: 4

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/tools/mod.rs:109-193` — built-in tool implementation and
    strict schema style
  - External:
    `https://github.com/anomalyco/opencode/blob/f2d4ced8ea527dd6518e87354b886204a2819cab/packages/opencode/src/tool/lsp.ts`
    — rename-related contract behavior to mirror

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test lsp_rename_requires_prepare_success -- --exact`
  - [ ] `cargo test lsp_rename_returns_precheck_failure_without_edit -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):

  ```
  Scenario: Valid rename passes through prepare gate
    Tool: Bash
    Steps: Run `cargo test lsp_rename_requires_prepare_success -- --exact | tee .sisyphus/evidence/task-6-rename-tools.txt`
    Expected: Exit code 0; test output contains `test lsp_rename_requires_prepare_success ... ok`
    Evidence: .sisyphus/evidence/task-6-rename-tools.txt

  Scenario: Invalid rename returns error and no edits
    Tool: Bash
    Steps: Run `cargo test lsp_rename_returns_precheck_failure_without_edit -- --exact | tee .sisyphus/evidence/task-6-rename-tools-error.txt`
    Expected: Exit code 0; test output contains `test lsp_rename_returns_precheck_failure_without_edit ... ok`
    Evidence: .sisyphus/evidence/task-6-rename-tools-error.txt
  ```

  **Commit**: NO | Message: `feat(tools): add guarded lsp rename tools` | Files:
  `src/tools/lsp.rs`, `src/lsp/**`

- [x] 7. Integrate the LSP subsystem into runtime tool execution

  **What to do**: Wire the LSP manager into the runtime so LSP tools execute
  with session-scoped state, project-dir/workspace-root context, and
  deterministic error handling. Reuse the existing runtime tool execution path
  instead of adding a second dispatch mechanism. Ensure unsupported files,
  missing binaries, and terminal failed-server states all come back as
  recoverable tool outputs, not panics. Add runtime integration tests that prove
  repeated calls share the same session manager and that missing binaries report
  a stable message. **Must NOT do**: Do not add LSP settings persistence in
  `sessions.settings`. Do not bypass `ToolRegistry`. Do not make runtime success
  dependent on the web layer.

  **Recommended Agent Profile**:
  - Category: `deep` — Reason: touches core runtime execution path and error
    semantics
  - Skills: `[]` — no extra skill required
  - Omitted: `[find-docs]` — this is internal architecture work, not library
    lookup

  **Parallelization**: Can Parallel: NO | Wave 3 | Blocks: 8 | Blocked By: 2, 4,
  5, 6

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/runtime/session.rs:1050-1207` — existing tool lifecycle, event
    emission, and tool-domain error handling pattern
  - Pattern: `src/tools/mod.rs:145-179` — registry lookup and API schema
    production
  - Pattern: `src/store/session.rs:614-637` — existing settings persistence seam
    to explicitly avoid for v1

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test runtime_executes_lsp_tools_via_session_manager -- --exact`
  - [ ] `cargo test runtime_returns_deterministic_lsp_missing_binary_error -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):

  ```
  Scenario: Runtime reuses session-scoped LSP manager
    Tool: Bash
    Steps: Run `cargo test runtime_executes_lsp_tools_via_session_manager -- --exact | tee .sisyphus/evidence/task-7-runtime-integration.txt`
    Expected: Exit code 0; test output contains `test runtime_executes_lsp_tools_via_session_manager ... ok`
    Evidence: .sisyphus/evidence/task-7-runtime-integration.txt

  Scenario: Missing binary returns stable recoverable tool error
    Tool: Bash
    Steps: Run `cargo test runtime_returns_deterministic_lsp_missing_binary_error -- --exact | tee .sisyphus/evidence/task-7-runtime-integration-error.txt`
    Expected: Exit code 0; test output contains `test runtime_returns_deterministic_lsp_missing_binary_error ... ok`
    Evidence: .sisyphus/evidence/task-7-runtime-integration-error.txt
  ```

  **Commit**: NO | Message:
  `feat(runtime): wire lsp subsystem into tool execution` | Files:
  `src/runtime/session.rs`, `src/lsp/**`, `src/tools/**`

- [x] 8. Expose language detection and load state through snapshots and status
     events

  **What to do**: Extend the web/runtime visibility contract without adding
  settings UI. Add a structured `lsp` field to `StateSnapshotData` containing
  the built-in support list plus active per-server state (`server_id`, `status`,
  `command`, `workspace_root`, `last_file`, `last_error`). Reuse
  `UiEvent::StatusReport` for transition messages with exact status codes
  `lsp.detected`, `lsp.starting`, `lsp.ready`, and `lsp.failed`. Emit
  `lsp.detected` when a supported file is matched for a tool call,
  `lsp.starting` before process spawn, `lsp.ready` after initialize success, and
  `lsp.failed` on startup/crash failure. Add web integration tests that assert
  both snapshot shape and event order. **Must NOT do**: Do not add new web
  commands or settings update fields. Do not add a dedicated browser UI panel in
  this change. Do not emit duplicate status transitions for the same state
  change.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` — Reason: protocol changes must stay minimal
    and backward-compatible while exposing enough visibility
  - Skills: `[]` — no extra skill required
  - Omitted: `[visual-engineering]` — no UI rendering work is in scope

  **Parallelization**: Can Parallel: NO | Wave 3 | Blocks: Final verification |
  Blocked By: 7

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/web/protocol.rs:112-121` — snapshot root shape
  - Pattern: `src/web/protocol.rs:270-378` — `StateSnapshot` and `StatusReport`
    event contracts
  - Pattern: `src/web/ws/event_map.rs:193-205` — current
    runtime-to-status-report event mapping
  - Pattern: `src/web/ws/snapshot.rs:31-92` — snapshot composition path
  - Test: `tests/web.rs:433-449` — snapshot update assertions pattern
  - Test: `tests/web.rs:1190-1244` — tool event round-trip assertions pattern

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test lsp_status_is_exposed_in_web_snapshot_and_events --test web -- --exact`
  - [ ] `cargo test lsp_failed_server_emits_failed_status_once --test web -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):

  ```
  Scenario: Snapshot and status events expose LSP lifecycle
    Tool: Bash
    Steps: Run `cargo test lsp_status_is_exposed_in_web_snapshot_and_events --test web -- --exact | tee .sisyphus/evidence/task-8-web-visibility.txt`
    Expected: Exit code 0; test output contains `test lsp_status_is_exposed_in_web_snapshot_and_events ... ok`
    Evidence: .sisyphus/evidence/task-8-web-visibility.txt

  Scenario: Failed status is emitted once and captured in snapshot
    Tool: Bash
    Steps: Run `cargo test lsp_failed_server_emits_failed_status_once --test web -- --exact | tee .sisyphus/evidence/task-8-web-visibility-error.txt`
    Expected: Exit code 0; test output contains `test lsp_failed_server_emits_failed_status_once ... ok`
    Evidence: .sisyphus/evidence/task-8-web-visibility-error.txt
  ```

  **Commit**: NO | Message: `feat(web): expose lsp lifecycle visibility` |
  Files: `src/web/protocol.rs`, `src/web/ws/**`, `tests/web.rs`

- [x] 9. Enforce strict LSP availability in preflight and baseline parity checks

  **What to do**: Convert the v1 LSP server checks in `src/preflight.rs` from
  optional convenience checks into strict required checks for the six supported
  servers. Remove `tsgo` and `typescript-language-server` from the LSP support
  list for this feature. Update `tests/vm-baseline-check.sh` so the manifest
  parity lane requires `bash-language-server`, `yaml-language-server`, `nixd`,
  and `pyright` rather than allowing them to be absent. Add exact tests that
  assert missing binaries are reported by name. **Must NOT do**: Do not keep
  legacy allowlist exceptions for any v1-supported server. Do not leave v1
  server checks under the generic formatter/linter section. Do not make
  preflight pass when a supported LSP binary is absent.

  **Recommended Agent Profile**:
  - Category: `quick` — Reason: bounded enforcement change with exact
    script/report touch points
  - Skills: `[]` — no extra skill required
  - Omitted: `[git]` — commit work remains global

  **Parallelization**: Can Parallel: NO | Wave 3 | Blocks: Final verification |
  Blocked By: 1, 3

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/preflight.rs:172-238` — current LSP and toolchain reporting
    sections
  - Pattern: `tests/vm-baseline-check.sh:7-49` — manifest vs preflight parity
    logic
  - Pattern: `tests/vm-baseline-check.sh:28-38` — current allowlist that must be
    tightened for v1 LSP support
  - Pattern: `agent-vm/developer-heavy-tool-manifest.txt:1-21` — expected
    manifest entry format

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test preflight_requires_all_v1_lsp_servers -- --exact`
  - [ ] `cargo test preflight_reports_each_missing_lsp_binary_by_name -- --exact`
  - [ ] `bash tests/vm-baseline-check.sh`

  **QA Scenarios** (MANDATORY — task incomplete without these):

  ```
  Scenario: Preflight requires every v1 LSP binary
    Tool: Bash
    Steps: Run `cargo test preflight_requires_all_v1_lsp_servers -- --exact | tee .sisyphus/evidence/task-9-preflight-enforcement.txt` and then `bash tests/vm-baseline-check.sh | tee -a .sisyphus/evidence/task-9-preflight-enforcement.txt`
    Expected: Both commands exit 0; test output contains `test preflight_requires_all_v1_lsp_servers ... ok`; script output confirms manifest coverage
    Evidence: .sisyphus/evidence/task-9-preflight-enforcement.txt

  Scenario: Each missing binary name is surfaced deterministically
    Tool: Bash
    Steps: Run `cargo test preflight_reports_each_missing_lsp_binary_by_name -- --exact | tee .sisyphus/evidence/task-9-preflight-enforcement-error.txt`
    Expected: Exit code 0; test output contains `test preflight_reports_each_missing_lsp_binary_by_name ... ok`
    Evidence: .sisyphus/evidence/task-9-preflight-enforcement-error.txt
  ```

  **Commit**: NO | Message: `fix(preflight): strictly require v1 lsp binaries` |
  Files: `src/preflight.rs`, `tests/vm-baseline-check.sh`,
  `agent-vm/developer-heavy-tool-manifest.txt`

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated
> results to user and get explicit "okay" before completing. **Do NOT
> auto-proceed after verification. Wait for user's explicit approval before
> marking work complete.** **Never mark F1-F4 as checked before getting user's
> okay.** Rejection or user feedback -> fix -> re-run -> present again -> wait
> for okay.

- [x] F1. Plan Compliance Audit — oracle

  **Tool**: `task(subagent_type="oracle")` **Steps**: Review the final diff and
  changed files against `.sisyphus/plans/initial-lsp-language-support.md`;
  verify that Tasks 1-9 were completed without adding out-of-scope behavior;
  confirm built-in language list, lazy startup, snapshot/status visibility,
  strict enforcement, and Nix provisioning all match the plan. **Expected**:
  Oracle returns an explicit approval with zero critical deviations from plan
  scope or acceptance criteria. **Evidence**:
  `.sisyphus/evidence/f1-plan-compliance.txt`

- [x] F2. Code Quality Review — unspecified-high

  **Tool**: `task(category="unspecified-high")` **Steps**: Review the
  implementation for Rust code quality, error handling, process lifecycle
  safety, schema correctness, and test naming quality; cross-check that exact
  test names from the plan exist and that no panic-prone error path was
  introduced. **Expected**: Reviewer approves with zero critical correctness or
  maintainability issues. **Evidence**: `.sisyphus/evidence/f2-code-quality.txt`

- [x] F3. Real Manual QA — unspecified-high (+ playwright if UI)

  **Tool**: `task(category="unspecified-high")` plus `playwright` only if a
  visible browser UI element was added **Steps**: Execute the full verification
  lane: `cargo test`, `bash tests/vm-baseline-check.sh`, and
  `nix develop -c bash -lc 'command -v rust-analyzer gopls bash-language-server yaml-language-server nixd pyright-langserver'`;
  if the implementation added a browser-visible indicator beyond
  websocket/snapshot behavior, also run a Playwright check for that UI.
  **Expected**: All commands exit 0; any optional Playwright check passes;
  reviewer approves with no reproduction gaps. **Evidence**:
  `.sisyphus/evidence/f3-manual-qa.txt`

- [x] F4. Scope Fidelity Check — deep

  **Tool**: `task(category="deep")` **Steps**: Compare the delivered work
  against the scope boundaries in this plan and explicitly confirm that no
  user-configurable LSP settings, no `tsgo`/TypeScript support, no
  auto-install/download logic, no heuristic detection, and no generic plugin
  system were added. **Expected**: Reviewer approves and explicitly states that
  all out-of-scope items remain absent. **Evidence**:
  `.sisyphus/evidence/f4-scope-fidelity.txt`

## Commit Strategy

- Commit 1: `feat(lsp): add builtin catalog and workspace rules`
  - Includes Tasks 1-2
- Commit 2: `build(nix): provision required lsp servers`
  - Includes Tasks 3 and the manifest portion of Task 9 if needed for parity
- Commit 3: `feat(lsp): add manager and tool suite`
  - Includes Tasks 4-7
- Commit 4: `feat(web): expose lsp status and enforce preflight`
  - Includes Tasks 8-9

## Success Criteria

- Kley can detect and service `.rs`, `.go`, `.sh/.bash/.zsh/.ksh`, `.nix`,
  `.yaml/.yml`, `.py/.pyi` through the built-in LSP tool surface.
- LSP servers are installed from the repo-owned Nix/dev baseline instead of ad
  hoc local setup.
- A supported file triggers deterministic
  `lsp.detected → lsp.starting → lsp.ready` or `lsp.failed` visibility.
- Unsupported files and missing/failed servers return recoverable tool-domain
  errors, not panics.
- `preflight` fails when any v1 server binary is missing.
- `bash tests/vm-baseline-check.sh` and `cargo test` both pass.
