# Learnings

- `Session` canonical metadata comes from `sessions.model`/`sessions.provider`
  via `Session::get/find/list` row mapping (`src/store/session.rs`,
  `impl Session`, `from_row`), while runtime settings are stored separately in
  `sessions.settings`.
- Store query style keeps SQL + row mapping in the owning module
  (`src/store/turn.rs`: `Turn::list_for_session` + `Turn::from_row`); private
  helper APIs are colocated under the same `impl`.
- Async integration uses `SharedStore = Arc<Mutex<Store>>` +
  `store_run(&shared, move |s| { ... })` with owned captures, as shown in
  `tests/store_concurrency.rs` and `tests/web.rs`.
- Test fixtures for nullable usage are already standardized via
  `tests/harness/mod.rs::TurnBuilder` defaults
  (`model/tokens_in/tokens_out = None`) and direct `NewTurn` insertions in async
  tests.
- 2026-04-01: models.dev `/api.json` is a top-level provider-id map whose
  provider objects contain `id` and `models` (model-id map); pricing fields are
  `cost.input`/`cost.output` in USD per 1M tokens (README + live payload check).
  OpenCode stats derives totals at query time by reading messages and summing
  `message.info.cost` and token fields in-memory.
- 2026-04-02: Added the `ModelsDevCatalog` parser with fixture tests so catalog
  lookups stay exact to the provider/model pair and return `None` whenever
  `cost.input` or `cost.output` is missing, keeping the feature fully offline
  for now.
- Added `SessionUsageTotals` + `Session::usage_totals_with_catalog`, anchored in
  a COALESCE-based SQL sum over `message` turns so empty sessions and null
  `tokens_in`/`tokens_out` produce zero totals with `Some(0)` cost and no
  unpriced models yet.
- 2026-04-02: Added a blocking `fetch_catalog` helper that hits
  `https://models.dev/api.json` via `reqwest::blocking::Client`, delegating
  parsing to `ModelsDevCatalog::from_reader` so request/parse errors share the
  familiar context.
- 2026-04-02: Parser now requires each provider to declare `models`, preventing
  structurally invalid payloads from producing empty catalogs; offline tests
  assert `serde_json::Error::Category::Syntax` vs `Category::Data` for malformed
  JSON versus missing-model definitions.
- 2026-04-02: Added the turn-layer aggregation helper that groups persisted
  `message` turns by `COALESCE(turns.model, sessions.model)` and exposed a
  catalog hook so tests can verify both summed totals and effective-model usage
  slices before pricing composition.
- 2026-04-02: Task 4 now converts models.dev USD-per-1M prices into micro-USD
  using `u128` arithmetic, sums per-slice input/output costs, and reports sorted
  unique unpriced models while still honoring the `SessionUsageCatalog` hook and
  new pricing-focused tests.
