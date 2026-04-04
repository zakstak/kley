## 2026-04-03 Task 1 verification repair

- Root cause: task-1 tests were unit tests under `lsp::tests::...`, so
  `cargo test <name> -- --exact` with bare function names matched zero tests.
- Fix: moved acceptance checks into a dedicated test target at
  `src/lsp/catalog_exact_tests.rs` with top-level test function names exactly
  `lsp_builtin_catalog_matches_initial_languages` and
  `lsp_builtin_catalog_rejects_unsupported_extensions`, and registered it in
  `Cargo.toml` as `[[test]]`.
- Result: both exact verification commands now run exactly one real test and
  pass; builtin catalog behavior was kept unchanged.

## 2026-04-04 Task 8 regression repair

- Task 8 added manager-side `initialize`, which changed the old task-4 fake
  terminal client path: the fake `Terminal(...)` mode started failing during
  initialization instead of on the first real request, so
  `lsp_manager_marks_failed_servers_terminal` saw a startup-path mismatch.
- Minimal repair: updated only `src/lsp/catalog_exact_tests.rs` so the fake
  terminal client returns a normal initialize response and still emits a
  terminal error for non-`initialize` methods. That preserves task-4
  terminal-failure semantics without changing task-8 runtime/web visibility
  behavior.

## 2026-04-04 F2 blocker remediation (final hardening pass)

- `src/lsp/service.rs`: request handling now tolerates interleaved JSON-RPC
  notifications and non-matching frames by continuing to read until the matching
  request id arrives, instead of failing immediately on the first non-matching
  frame.
- `src/lsp/service.rs`: added a drop cleanup path for stdio clients that
  terminates/waits child processes, preventing leaked session-scoped LSP server
  processes when manager/client instances are dropped.
- `src/tools/lsp.rs`: `textDocument/prepareRename` and `textDocument/rename`
  request paths now reject `line == 0` with deterministic `line must be >= 1`
  errors instead of silently coercing to line 0 via saturating subtraction.
- Added deterministic unit tests for all three cases
  (`reads_past_interleaved_notification_and_non_matching_response`,
  `drop_terminates_lsp_child_process`, and rename line-validation tests) and
  re-ran full `cargo test` green.

## 2026-04-04 ShellCheck hygiene

- Reworked SSH invocations in the canary recovery and validation scripts so
  remote commands now pass arguments via `ssh ... -- ...` and `env` instead of
  interpolating variables into quoted command strings, which keeps SC2029 quiet
  without changing runtime behavior.
