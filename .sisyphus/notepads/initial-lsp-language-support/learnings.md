## 2026-04-03 Research: LSP catalog and root rules

- OpenCode`s `Config.Info`schema exposes an`lsp`entry that can be`false`(disabled) or a record keyed by server ID. Built-in servers are tracked elsewhere, but custom entries must include`command`(string array) plus`extensions`or be marked`disabled`. Optional fields such as `env`and`initialization`follow the same shape, and while the schema accepts either`LSPServer`ids or custom keys, it enforces that any non-built-in server declares`extensions`. (Source: `packages/opencode/src/config/config.ts`
  lines 961-996)
- The upstream `LSPServerConfig` TypeScript interface used by oh-my-openagent
  mirrors this Rust-friendly shape: each entry lists `id`, `command[]`, and
  `extensions[]`, with `disabled`, `env`, and `initialization` as optional
  helpers. (Source: `src/tools/lsp/types.ts` lines 1-8 in oh-my-openagent commit
  `53eeac3f31ee2218ad54c4c8b62d171a8045409a`)
- In `packages/opencode/src/lsp/server.ts`, the `NearestRoot` helper (lines
  35-57) walks upward from a given file toward `Instance.directory`, looking for
  `includePatterns` while optionally aborting early when `excludePatterns` are
  found. It returns the directory containing the first marker or falls back to
  `Instance.directory` if nothing is found. Concrete servers (e.g., `Deno` at
  lines 67-93 and `Typescript` at lines 95-121) feed specific marker lists into
  `NearestRoot` or custom searches, so their workspace roots derive from the
  nearest config files. We can follow this pattern when implementing the
  Rust/Go/Bash/Nix/Python detection rules, keeping the same deterministic
  targets and fallback hierarchy. (Source: `packages/opencode/src/lsp/server.ts`
  lines 35-121)

## 2026-04-03 Repository inspection: root and fixture patterns

- `src/preflight.rs` lines 303-329 walk filesystem ancestors to locate a
  `Cargo.toml` that declares `kley`, returning that manifest path before falling
  back to the `kley` binary path. This is the only git-aware root check
  currently in the repo, so our new root-resolution module should reuse similar
  tree-ascending helpers before calling out to git or parent fallbacks.
- `src/tools/editing/io.rs` lines 15-58 implement `resolve_final_target`, which
  follows symlinks (max 64 hops), normalizes via
  `parent().unwrap_or_else(|| Path::new("."))`, and rejects loops. That utility
  shows how to interleave `fs::read_link`, explicit parent handling, and
  deterministic fallback paths safely.
- `src/harness/hashline.rs` lines 718-746 materialize fixture workspaces by
  validating relative paths, running `fs::create_dir_all` on each file's parent,
  and writing file contents. The helper ensures no absolute or `..` components
  sneak in, so we can mirror this approach when building root-resolution tests
  that need file layouts.
- `tests/hashline_harness.rs` lines 286-404 create temporary workspaces with
  `tempfile::tempdir()`, call `fs::create_dir_all` for nested `src/`
  directories, and execute the CLI via `Command::current_dir(&workspace)` with
  explicit env vars. Those tests rely on deterministic temporary roots and env
  guards, which our root-resolution tests should mimic to keep path expectations
  stable.
- `src/skills/mod.rs` lines 270-285 compute discovery directories as
  `project_dir/.agents/...` and (optionally) `$HOME/.kley/...`, demonstrating
  how layered fallbacks (project-local first, home second) keep behavior
  predictable. Our new root logic should follow that priority ordering: explicit
  markers first, git root second, file parent last.
- Gotcha: current code never inspects `.git` directly outside of git commands
  invoked in preflight, so the git-root fallback for the new module must be
  implemented from scratch rather than piggybacking on existing helpers. The
  deterministic fallback order will be marker directories → git root (via
  `git rev-parse --show-toplevel`) → direct file parent, mirroring the plan’s
  requirements.

## 2026-04-03 LSP command validation research

- `rust-analyzer` ships as the `rust-analyzer` binary and is expected on `$PATH`
  before editors invoke it (source:
  [rust-analyzer binary](https://rust-analyzer.github.io/book/rust_analyzer_binary.html)).
  `lsp/rust_analyzer.lua` also starts it via `cmd = { 'rust-analyzer' }` for the
  `rust` filetype, so the plan’s command and extension mapping match the
  canonical client configs.
- `gopls` is invoked simply as `gopls` and exposes multiple subcommands for
  debugging (`gopls references ...`), confirming the single-word server name
  (source: [gopls command-line interface](https://go.dev/gopls/command-line)).
  `lsp/gopls.lua` lists `cmd = { 'gopls' }` with `go`, `gomod`, `gowork`, and
  `gotmpl` filetypes, so the extension mapping is consistent with upstream
  usage.
- `bash-language-server` requires the `start` verb when editors run it through
  the CLI, and the README shows multiple Vim/Neovim helpers setting
  `cmd = { 'bash-language-server', 'start' }` for the `sh`/`bash` filetypes
  (source:
  [bash-language-server README](https://raw.githubusercontent.com/bash-lsp/bash-language-server/master/README.md)).
  No alternate verbs are needed in practice, so the plan-string is canonical.
- `nixd` is the executable documented in the official editor setup guide and is
  registered in `coc-settings`, `eglot`, `lsp-mode`, Helix, and Kate configs
  simply as `nixd` for the `nix` filetype (source:
  [nixd/editor-setup.md](https://raw.githubusercontent.com/nix-community/nixd/main/nixd/docs/editor-setup.md)).
  That matches the plan’s command and extension assumptions exactly.
- `yaml-language-server --stdio` appears in `lsp/yamlls.lua` as
  `cmd = { 'yaml-language-server', '--stdio' }`, so the plan’s invocation keeps
  the same stdio transport flag upstream uses for editors without native
  JSON-RPC transports (source: `lsp/yamlls.lua`).
- `pyright-langserver --stdio` likewise appears in `lsp/pyright.lua` as the
  command used for the `python` filetype, confirming both the binary name and
  the required `--stdio` switch are what real clients ship (source:
  `lsp/pyright.lua`).

## 2026-04-03 Repository inspection: builtin hooks & extensions

- `src/tools/mod.rs` lines 109-193 define the `ToolRegistry` trait/struct and
  the `default_registry` helper that registers the builtin tools (`shell`,
  `read_file`, `patch`, `hashline_edit`, `read_skill`, `delegate_task`,
  `report_status`). That registry is consumed by the CLI/agent pipeline
  (`src/agent.rs` 41-75), the async runtime manager (`src/runtime/manager.rs`
  188-264), the session runtime/tests (`src/runtime/session.rs` 2008-2132), and
  delegation-policy tests (`src/runtime/settings.rs` 513-545), so a new LSP
  catalog should follow the same registration pattern.
- `src/preflight.rs` lines 172-238 currently hard-codes the LSP/binary shelf:
  optional checks for `rust-analyzer`, `gopls`, `typescript-language-server`,
  `bash-language-server`, `yaml-language-server`, `tsgo`, plus
  linters/formatters. The same file’s test block near lines 774-804 exercises
  those commands, showing how the repo already enumerates server binaries in a
  single place.
- Extension handling today is ad-hoc: `tests/hashline_contract.rs` 35-41 and
  `src/harness/hashline.rs` 922-930 limit fixtures to `.json`, while
  `src/skills/mod.rs` 124-212 only loads `.md` rules/skills. There is no
  centralized extension map to associate filetypes with LSPs yet, so the planned
  `src/lsp/mod.rs` will be the first such catalog.
- Instrumentation tests instantiate `ToolRegistry` with fake tools
  (`tests/runtime.rs` 478-516 & 623-677, `tests/hashline_observability.rs`
  349-399 & 402-465) and rely on registry lookups, so any new builtin catalog
  must expose the same registry interface they exercise.

## 2026-04-03 Implementation: task 1 built-in LSP catalog

- Added `src/lsp/mod.rs` with a static builtin catalog API (`builtin_catalog`)
  and deterministic lookup helpers (`builtin_server_for_extension`,
  `builtin_server_for_path`).
- Catalog v1 now hardcodes six logical IDs and exact runtime commands:
  `rust-analyzer`, `gopls`, `bash-language-server start`, `nixd`,
  `yaml-language-server --stdio`, and `pyright` with runtime
  `pyright-langserver --stdio`.
- Extension coverage is explicit and exact-only: `rs`, `go`, `sh`, `bash`,
  `zsh`, `ksh`, `nix`, `yaml`, `yml`, `py`, `pyi`; unsupported extensions return
  `None` without fallback behavior.
- Lookup normalizes extension input to lowercase before matching (including
  path-derived extensions), so uppercase paths like `script.BASH` resolve
  deterministically.
- Rust test filter nuance: with unit tests under `lsp::tests`,
  `cargo test <name> -- --exact` returns success with zero matches because exact
  names include module paths. The target test functions still execute and pass
  via non-exact name filters.

## 2026-04-03 Implementation: task 2 LSP root resolution

- Added `src/lsp/root.rs` with a single public
  `resolve_workspace_root(file_path, server_id)` helper that keeps rule
  selection local to `src/lsp` and returns a deterministic `PathBuf`.
- Marker precedence is encoded as explicit marker groups: Rust searches the
  nearest ancestor containing either `Cargo.toml` or `rust-project.json`; Go
  searches `go.work` across ancestors before considering any `go.mod`; Nix and
  Python search the nearest ancestor containing any marker from their bounded v1
  lists; Bash and YAML skip marker lookup entirely.
- Git fallback is implemented without shelling out: the resolver walks ancestors
  looking for a `.git` entry (file or directory), then falls back to the file
  parent using the same empty-parent guard pattern already used elsewhere in the
  repo.
- Reused the existing `src/lsp/catalog_exact_tests.rs` integration target so
  `cargo test lsp_root_resolution_matches_language_rules -- --exact` and
  `cargo test lsp_root_resolution_falls_back_without_markers -- --exact` each
  execute one real top-level test instead of succeeding with zero matches.

## 2026-04-04 Task 3 LSP runtimes inventory

- Updated `flake.nix`, `agent-vm/profiles/developer-heavy.nix`, and the
  `agent-vm/developer-heavy-tool-manifest.txt` to include `rust-analyzer`,
  `gopls`, `bash-language-server`, `yaml-language-server`, `nixd`, and `pyright`
  so the dev shell and VM baseline expose the exact binaries listed in the
  manifest.
- Verified the shell sees each binary via
  `nix develop -c bash -lc 'command -v rust-analyzer gopls bash-language-server yaml-language-server nixd pyright-langserver'`
  and confirmed `pyright-langserver` launches along with `pyright 1.1.408` from
  `nix develop -c bash -lc 'command -v pyright-langserver && pyright --version'`.

## 2026-04-04 Research: session-scoped lazy LSP manager seams

- `SessionRuntime` emulates per-session tooling with `RuntimeHooks`
  (`src/runtime/session.rs:541-625`) and sends every turn/tool event through
  `events.emit(AgentEvent::...)` (`src/runtime/session.rs:964-1210`) so an LSP
  manager can hook into the same channel with minimal changes. The runtime also
  records tool deltas (`emit_runtime_event` at `src/runtime/session.rs:541-545`)
  so listeners can update assistant-context metrics incrementally before
  `ToolCallCompleted` arrives.
- `RuntimeManager` keeps a `ManagedSession` per session
  (`src/runtime/manager.rs:226-317`) that tracks `active_turn`, context/token
  usage, `events` broadcaster, and runtime metadata. Its `publish_runtime_event`
  helper (`src/runtime/manager.rs:920-1017`) mutates the cached
  `ActiveTurnReplay`, reuses `last_context_usage`, and forwards every
  `AgentEvent` via `RuntimeEventEnvelope` to subscribers while clearing state on
  `TurnCompleted`/`TurnFailed`. That is the precise per-session lifecycle seam
  we need to align the new LSP `LspService` with.
- `src/web/ws/event_map.rs:9-239` iterates over `AgentEvent` variants, emits
  `UiEvent` records (including `StatusReport`) with new UUID `event_id`s, and
  maps `AgentEvent::StatusReport`/`HistoryCompacted` into `StatusReport` events
  with human-readable detail. Extending this mapping with
  `lsp.detected`/`lsp.starting`/`lsp.ready`/`lsp.failed` status strings should
  be straightforward once the service emits the corresponding `AgentEvent`s.
- Child-process helpers in `src/tools/shell.rs:476-687` show how the repo
  launches subprocesses with `setsid`+`sh` (fallback via `spawn_shell_child`,
  `build_setsid_spawn_command`, `build_direct_spawn_command`), captures
  stdout/stderr, and signals descendants (`signal_processes`). We can reuse the
  same pattern for the stdio JSON-RPC LSP clients and their lifecycle controls.
- Test doubles already exist: `CommandRunner`/`FakeRunner` in
  `src/preflight.rs:521-577` let tests prime expected `CommandSpec`s with canned
  `CommandOutput`s, so future LSP manager tests could stub process spawns
  through a similar trait instead of launching real binaries.
  `provider::test::TestProvider` (`src/provider/test.rs:14-185`) uses
  `CONTROL_BLOCK` markers to drive tool-call/results and could serve as the fake
  transport/stub for the planned manager tests. `tests/runtime.rs:207-374`
  already builds a `SessionRuntime` with
  `event_channel()`/`runtime_with_abort_signal` and inspects `AgentEvent`s from
  the channel, providing a template for asserting new LSP `StatusReport`
  transitions.

## 2026-04-03 Research: session-scoped lazy manager behavior

- Upstream `packages/opencode/src/lsp/index.ts` builds its `State` through
  `InstanceState.make`, keeping `clients`, `servers`, `broken`, and `spawning`
  in a per-session service and adding a finalizer that shuts down every client
  when the session closes, so each session owns its own server lifecycles.
  (lines 166-225)
- `getClients` only runs when files are touched: it filters servers by
  extension, resolves the workspace root, skips `<root, server>` keys already
  marked in `broken`, reuses any ready client for that pair, uses the `spawning`
  map to dedupe concurrent spawns, and publishes `Event.Updated` when a new
  client becomes available, so lazy startup happens purely on demand while ready
  clients are reused. (lines 227-309)
- The `schedule` helper is the sole launcher: it spawns via the server-specific
  command, logs, builds an `LSPClient`, checks for duplicates, and, on spawn or
  initialization failure, adds the `<root, server>` key to `broken` and stops
  the process so later calls observe a terminal failed state rather than retried
  attempts. (lines 233-270)
- Upstream still allows config-driven enables/disables plus an experimental
  `pyright` toggle (`cfg.lsp` block, `filterExperimentalServers`) but our plan
  intentionally freezes the catalog and lifecycle behavior, so we mirror only
  the lazy-start, per-session reuse, and failure-state model without the runtime
  config knobs. (lines 170-210)

## 2026-04-04 Implementation: task 4 lazy session-scoped LSP manager + stdio JSON-RPC

- Added `src/lsp/service.rs` and exported a hidden `LspService` seam in
  `src/lsp/mod.rs`, with `LspManager` keyed by
  `(session_id, server_id, workspace_root)` and explicit lifecycle states
  `Idle`, `Starting`, `Ready`, and `Failed`.
- Startup is lazy and deduped: `LspManager::request` resolves the builtin server
  command by `server_id`, then acquires/starts per-key state only on first
  matching call; concurrent callers block on a `Condvar` while state is
  `Starting`.
- Ready-process reuse is deterministic: once state is `Ready`, subsequent
  same-session/same-server/same-root requests reuse the same client without
  respawning; different sessions produce separate spawns even for the same
  server/root.
- Terminal failure semantics are explicit: startup errors set `Failed`
  immediately, and runtime terminal errors (including unexpected process exit
  surfaced by the stdio client) permanently flip the key to `Failed` for the
  session/root lifetime with no auto-retry.
- Implemented a concrete stdio JSON-RPC client (`StdioLspClient`) that writes
  `Content-Length` framed requests, increments JSON-RPC ids, reads framed
  responses from stdout, and maps protocol/process failures into retryable vs
  terminal manager outcomes.
- Extended `src/lsp/catalog_exact_tests.rs` with exact-name tests
  `lsp_manager_starts_once_per_session_server` and
  `lsp_manager_marks_failed_servers_terminal` using fake factory/client doubles
  so assertions are deterministic and do not depend on real LSP binaries.

## 2026-04-04 Implementation: task 5 read-only/query LSP tool surface

- Added first-class builtin registrations for `lsp_diagnostics`, `lsp_symbols`,
  `lsp_goto_definition`, and `lsp_find_references` in `src/tools/mod.rs`, all
  sharing one `Arc<dyn LspService>` per registry instance and a stable
  `tool-registry-lsp` session id until runtime task 7 wires true session scope.
- `src/tools/lsp.rs` already contained task-6 rename helpers, so task-5 work
  extended that module in place rather than creating a second LSP-tool file; the
  new read-only tools keep snake_case request fields (`file_path`,
  `include_declaration`, etc.) even though the preexisting rename surface is
  still camelCase.
- Read-only outputs normalize raw LSP responses into stable JSON summaries:
  definition/reference results become path+range records, symbols become
  normalized symbol summaries across document/workspace variants, and
  diagnostics flatten both document and workspace diagnostic shapes while
  filtering by requested severity.
- Unsupported extensions and failed/terminal server states stay recoverable
  tool-domain outputs via `Error: ...` strings, and exact-name coverage lives in
  the existing `src/lsp/catalog_exact_tests.rs` target so
  `cargo test ... -- --exact` executes real task-5 tests.

## 2026-04-04 Implementation: task 6 guarded rename tools

- Added `src/tools/lsp.rs` with injectable `LspPrepareRenameTool` and
  `LspRenameTool` implementations that stay on the existing `LspService` seam
  instead of introducing runtime wiring early.
- `lsp_rename` now performs its own `textDocument/prepareRename` request on the
  same request path before sending `textDocument/rename`; when prepare returns
  `null`/unsupported, it returns `Error: Cannot rename at this position` and
  does not apply edits.
- Tool-domain failures are deterministic and recoverable: missing files return
  `Error: File not found: ...`, unsupported extensions return
  `Error: No LSP server available for this file type.`, and manager/server
  failures are surfaced through the stable `LspManagerError` display text.
- The rename tool mirrors the pinned argument shape from
  upstream/oh-my-openagent rename tools (`filePath`, `line` 1-based, `character`
  0-based, `newName`) and formats prepare/apply results with the same
  user-visible strings (`Rename available ...`, `Applied N edit(s) ...`).
- Workspace edit application stays bounded to the upstream rename contract:
  `changes` and `documentChanges` are applied directly in tool code, while the
  exact-name tests verify both the prepare→rename call order and the no-edit
  behavior when precheck fails.

## 2026-04-04 Implementation: task 9 LSP enforcement

- `src/preflight.rs` now runs a deterministic `LSP_REQUIREMENTS` table instead
  of optional checks, so the v1 catalog enforces `rust-analyzer`, `gopls`,
  `bash-language-server`, `nixd`, `yaml-language-server`, and `pyright` via
  `report.required` plus a helper that exposes each missing binary by name.
- Added `preflight_requires_all_v1_lsp_servers` and
  `preflight_reports_each_missing_lsp_binary_by_name` unit tests to keep the new
  helper auditable under `--exact` filters and ensure the render output lists
  every missing server.
- The baseline manifest parity lane no longer allows `bash-language-server`,
  `yaml-language-server`, `tsgo`, or `typescript-language-server` to be absent
  and now mirrors the preflight commands plus the new `pyright` command entry.
- `agent-vm/developer-heavy-tool-manifest.txt` now lists both `pyright` and
  `pyright-langserver`, keeping the manifest aligned with preflight and the
  actual LSP package.
- Fixed `src/web/ws/event_map.rs` to consume the real `status`/`detail` fields,
  and reused `src/tools/lsp.rs`′s `path_to_file_uri` helper in
  `src/lsp/service.rs` so that helper is no longer duplicated.
- Attempted verification
  (`cargo test preflight_requires_all_v1_lsp_servers -- --exact` /
  `cargo test preflight_reports_each_missing_lsp_binary_by_name -- --exact`)
  still fails because the crate currently lacks `runtime_registry` and
  `apply_lsp_status_report` helpers in `src/runtime/manager.rs`. The runtime
  module is missing those functions, so the compile fails before the new tests
  can run.
- Added a `preflight_test_support` module that exposes the `FakeRunner`,
  `LspRequirement` specs, and the new `run_required_lsp_checks_with_runner`
  helper so the exact `cargo test preflight_* -- --exact` commands can execute
  real checks without hitting private internals.
- Created integration tests (`tests/preflight_requires_all_v1_lsp_servers.rs`
  and `tests/preflight_reports_each_missing_lsp_binary_by_name.rs`) that re-use
  the new helpers, run once each, and assert the strict enum of LSP IDs,
  satisfying the acceptance commands exactly.

## 2026-04-04 Implementation: task 7 runtime LSP integration

- `SessionRuntime::new_with_abort_signal` and
  `new_with_shared_store_and_abort_signal` now call
  `registry.bind_session_context(&session.id)` immediately after session
  initialization, so runtime execution keeps the existing `ToolRegistry`
  dispatch path while rebinding LSP tools off the static `tool-registry-lsp` id.
- Added `Tool::bind_session_context` (default no-op) plus
  `ToolRegistry::bind_session_context`; all six LSP tools (`diagnostics`,
  `symbols`, `goto_definition`, `find_references`, `prepare_rename`, `rename`)
  implement it by updating their internal `session_id` field.
- `default_registry` now registers the rename tools alongside read-only tools
  and shares the same `Arc<dyn LspService>` across all LSP tool instances in
  that registry.
- LSP missing-binary handling is now deterministic and recoverable:
  `StdioLspClientFactory` emits a structured startup reason
  (`missing binary: <cmd>`), `LspManager` surfaces it as
  `LspManagerError::MissingBinary`, and repeated calls return the same stable
  tool output (`Error: required lsp binary not found on PATH: ...`) instead of
  path/OS-specific spawn text.
- Added exact-name runtime integration tests in `tests/runtime_lsp_exact.rs`:
  `runtime_executes_lsp_tools_via_session_manager` verifies runtime session-id
  rebinding + single factory startup across repeated calls, and
  `runtime_returns_deterministic_lsp_missing_binary_error` verifies repeated
  calls produce the same missing-binary output string.

## 2026-04-04 Task 7 regression repair: managed runtime session-scope reuse

- `RuntimeManager` now keeps a single `Arc<LspManager>` inside each
  `RuntimeWorker`; `runtime_registry(...)` receives that shared service instead
  of constructing a new manager per prompt submission.
- `ensure_managed_session` now refreshes the existing `RuntimeWorker` in place
  to preserve the session-scoped LSP manager across repeated controller
  attaches/prompt submissions.
- `LspManager` event emission is now mutable per submission
  (`set_event_emitter`/`clear_event_emitter`) so task-8 status visibility keeps
  working without leaking emitter clones across prompts.
- Added
  `runtime::manager::tests::runtime_worker_reuses_lsp_manager_across_prompt_submissions`
  to lock the regression: two prompt submissions on the same worker/session
  trigger exactly one LSP factory startup.

## 2026-04-04 Implementation: task 8 web/runtime LSP visibility

- `StateSnapshotData` now carries a structured `lsp` block with builtin support
  metadata (`supported`) plus per-session active server state (`active`), and
  `src/web/ws/snapshot.rs` builds that shape from
  `RuntimeManager::lsp_snapshot(...)` rather than inspecting tool instances
  directly.
- LSP lifecycle visibility is routed through the existing runtime event channel:
  `SessionRuntime` emits `lsp.detected` when an LSP tool call matches a
  supported file, `LspManager` emits `lsp.starting` / `lsp.ready` / `lsp.failed`
  around spawn+initialize, and `src/web/ws/event_map.rs` forwards those exact
  status codes through `UiEvent::StatusReport`.
- Duplicate websocket status transitions are suppressed in
  `RuntimeManager::publish_runtime_event`: the manager keeps a per-session
  `(server_id, workspace_root)` state map, updates `last_file`/`last_error` for
  snapshots, but only forwards a `status.report` event when the visible state
  actually changes (for example, repeated startup failures stay a single
  `lsp.failed` transition).
- Exact-name websocket coverage lives in `tests/web.rs`:
  `lsp_status_is_exposed_in_web_snapshot_and_events` verifies
  `lsp.detected -> lsp.starting -> lsp.ready` plus snapshot shape, and
  `lsp_failed_server_emits_failed_status_once` verifies terminal failed-state
  dedupe while the snapshot still retains `last_error`.

## 2026-04-04 Lint cleanup

- Resolved the pending `clippy::single_match` by guarding
  `terminate_child_process` with `matches!` and bundled the request/session
  identifiers so `RuntimeManager::execute_reserved_prompt` now needs one fewer
  argument. Both fixes kept behavior intact while making the lint tree clean.

## 2026-04-06 Task 9 enforcement repair

- Shifted the preflight LSP requirement for Python to
  `pyright-langserver --version` so the strict runtime command surface matches
  the v1 catalog and the VM baseline's `command -v pyright-langserver`
  expectation; the helper still reports `id: "pyright"` while pointing at the
  actual server binary and the fake-runner tests now mimic the updated command.
- Baseline parity already listed `pyright-langserver`, so no manifest edits were
  needed beyond ensuring the preflight check no longer invokes the `pyright` CLI
  binary.
- Verification: `cargo test preflight_requires_all_v1_lsp_servers -- --exact`,
  `cargo test preflight_reports_each_missing_lsp_binary_by_name -- --exact`,
  `bash tests/vm-baseline-check.sh`.
