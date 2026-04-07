2026-04-06: `cargo test <unit_test_name> -- --exact` reports zero matched tests
for module-scoped unit tests in this repo because libtest uses the full path
(for example `tools::web_search::tests::web_search_scope_excludes_fetch_fields`)
as the exact name. 2026-04-06: Root cause for the failed Task 1 verification was
that the five contract tests lived under `src/tools/web_search.rs` as nested
unit tests, so bare exact-name commands did not match them, and
`src/tools/mod.rs` also prematurely registered `web_search` in the default
registry. Fixed by moving the contract tests into a flat exact-matchable test
target, reverting the default-registry exposure, and aligning the unavailable
message to `Set TAVILY_API_KEY to enable web_search.` 2026-04-06: Maintaining
module-level wrappers still failed to satisfy the bare exact-name commands
because `cargo test default_registry_* -- --exact` requires a top-level test
with that exact name. Added `tests/default_registry.rs` with flat
`default_registry_*` tests so those commands now run real assertions over the
built-in registry and serialized provider tools.
