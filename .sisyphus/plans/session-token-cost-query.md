# Session Token + Cost Totals Query

## TL;DR
> **Summary**: Add a store-layer session usage query that derives token totals from the existing `sessions`/`turns` schema and computes session cost from models.dev API pricing without migrating storage to upstream OpenCode tables.
> **Deliverables**:
> - Public store query contract for session token + cost totals
> - SQL aggregation over persisted `turns.tokens_in` / `turns.tokens_out`
> - models.dev pricing catalog parser + live fetch helper
> - TDD coverage for empty sessions, null tokens, model fallback, unpriced models, and `store_run` integration
> **Effort**: Short
> **Parallel**: YES - 3 waves
> **Critical Path**: Task 1 → Task 2 + Task 3 → Task 4 → Task 6

## Context
### Original Request
- Query the DB for token usage.
- Follow OpenCode’s schema expectations.
- Final scope narrowed to **session totals only** for tokens and cost.
- Use **models.dev** for cost.
- Always assume **API pricing**, even when execution used a sub-provider/sub-agent path.

### Interview Summary
- Delivery surface is the **store/query layer**, not CLI/UI/websocket output.
- TDD is required.
- The repo currently persists token counts on `turns.tokens_in` / `turns.tokens_out` and stores `sessions.model` / `sessions.provider` plus optional `turns.model`.
- Upstream OpenCode stores usage/cost in message JSON and derives aggregates at query time; this work should match that **derived-view behavior**, not replicate upstream storage.

### Metis Review (gaps addressed)
- Explicitly defined pricing lookup precedence: `turn.model` first, then `sessions.model`; provider comes from canonical `sessions.provider`.
- Explicitly defined unknown pricing behavior: return token totals and `cost_usd_micros: None`, plus sorted `unpriced_models`.
- Explicitly defined scope guardrails: no schema migration, no UI/CLI work, no per-turn reporting, no fuzzy model-family matching.
- Explicitly defined cost precision: calculate in **micro-USD** with integer math; do not return floating totals as the source of truth.
- Explicitly defined aggregation semantics: count only `kind = 'message'` rows with non-null token fields; treat null token cells as zero.

## Work Objectives
### Core Objective
Provide a decision-complete store-layer API that returns session-level token totals and derived session cost from the existing SQLite store, using models.dev pricing and OpenCode-style derived totals semantics.

### Deliverables
- `SessionUsageTotals` public result contract with token totals, derived cost, and unpriced-model reporting.
- Store aggregation query over persisted message turns.
- `ModelsDevCatalog` parser/resolver from `https://models.dev/api.json`.
- Thin live pricing fetch helper for callers that want current models.dev data.
- Focused integration tests covering session totals and failure modes.

### Definition of Done (verifiable conditions with commands)
- `cargo test --test store_session_usage`
- `cargo test --test pricing_models_dev`
- `cargo test`
- `git diff --exit-code -- src/store/schema.rs`

### Must Have
- Query contract returns: `session_id`, `provider`, `input_tokens`, `output_tokens`, `total_tokens`, `cost_usd_micros`, `unpriced_models`.
- `cost_usd_micros` is `Some(u64)` only when every priced slice resolves cleanly in models.dev.
- Aggregation uses persisted DB values only; no runtime event replay.
- Cost lookup uses current models.dev API pricing and assumes canonical API pricing even if execution routing used a sub-provider path.
- Tests use fixtures for pricing data; no live network dependency inside test runs.

### Must NOT Have (guardrails, AI slop patterns, scope boundaries)
- Must NOT migrate to upstream OpenCode `message` / `part` tables.
- Must NOT add new DB columns or persistence writes for cost in v1.
- Must NOT expose this feature to CLI, websocket, or web UI in this plan.
- Must NOT add fuzzy model aliasing, family matching, or billing infrastructure beyond session totals.
- Must NOT silently convert unknown-priced models to zero cost.

## Verification Strategy
> ZERO HUMAN INTERVENTION — all verification is agent-executed.
- Test decision: **TDD** with Rust integration tests.
- QA policy: Every task includes exact `cargo test` scenarios with evidence capture.
- Evidence: `.sisyphus/evidence/task-{N}-{slug}.txt`

## Execution Strategy
### Parallel Execution Waves
> Small-scope exception: this feature has tight TDD dependencies, so waves are intentionally narrow.

Wave 1: contract + pricing foundations
- Task 1 — session usage public contract and zero/null semantics
- Task 3 — models.dev catalog parser and exact lookup semantics

Wave 2: query + live pricing access
- Task 2 — SQL aggregation over persisted turns
- Task 5 — blocking models.dev fetch helper and parse/error handling

Wave 3: composition + regression
- Task 4 — session cost composition over aggregated slices
- Task 6 — async `store_run` integration and full regression verification

### Dependency Matrix (full, all tasks)
- Task 1: no blockers
- Task 2: blocked by Task 1
- Task 3: no blockers
- Task 4: blocked by Task 2 and Task 3
- Task 5: blocked by Task 3
- Task 6: blocked by Task 4 and Task 5

### Agent Dispatch Summary (wave → task count → categories)
- Wave 1 → 2 tasks → `quick`, `quick`
- Wave 2 → 2 tasks → `unspecified-low`, `quick`
- Wave 3 → 2 tasks → `unspecified-low`, `unspecified-low`

## TODOs
> Implementation + Test = ONE task. Never separate.
> EVERY task MUST have: Agent Profile + Parallelization + QA Scenarios.

- [ ] 1. Add the public session usage totals contract

  **What to do**: In `src/store/session.rs`, add a public `SessionUsageTotals` result type with exactly these fields: `session_id: String`, `provider: String`, `input_tokens: u64`, `output_tokens: u64`, `total_tokens: u64`, `cost_usd_micros: Option<u64>`, `unpriced_models: Vec<String>`. Add the public store entry point `Session::usage_totals_with_catalog(store, session_id, catalog)` signature in the same module. Write tests first for empty-session totals and null-token handling, then implement the minimal contract/wiring needed to make those tests pass.
  **Must NOT do**: Do not add per-turn cost fields, do not change SQLite schema, and do not return `f64` as the authoritative cost type.

  **Recommended Agent Profile**:
  - Category: `quick` — Reason: small Rust type/API addition with focused tests.
  - Skills: `[]` — no extra skills required.
  - Omitted: `find-docs` — local code + already-fetched models.dev docs are sufficient.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 2, 4 | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/store/session.rs:60-80` — session record already carries canonical `model` and `provider` fields.
  - Pattern: `src/store/mod.rs:1-13` — store exports are centralized here; re-export the new public contract from this surface.
  - Pattern: `tests/harness/mod.rs:13-31` — `TestContext` for isolated in-memory store tests.
  - Test: `tests/harness/mod.rs:71-132` — `TurnBuilder` already supports optional `model`, `tokens_in`, and `tokens_out` fixtures.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test --test store_session_usage session_usage_totals_empty_session_returns_zero_totals -- --exact`
  - [ ] `cargo test --test store_session_usage session_usage_totals_null_tokens_count_as_zero -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Empty session totals
    Tool: Bash
    Steps: cargo test --test store_session_usage session_usage_totals_empty_session_returns_zero_totals -- --exact 2>&1 | tee .sisyphus/evidence/task-1-session-usage-contract.txt
    Expected: Test passes and asserts zero input/output/total tokens, `cost_usd_micros == Some(0)`, and `unpriced_models` is empty.
    Evidence: .sisyphus/evidence/task-1-session-usage-contract.txt

  Scenario: Null token fields do not break aggregation
    Tool: Bash
    Steps: cargo test --test store_session_usage session_usage_totals_null_tokens_count_as_zero -- --exact 2>&1 | tee .sisyphus/evidence/task-1-session-usage-contract-error.txt
    Expected: Test passes and asserts nullable `tokens_in` / `tokens_out` are treated as zero rather than causing an error or skipped session result.
    Evidence: .sisyphus/evidence/task-1-session-usage-contract-error.txt
  ```

  **Commit**: NO | Message: `n/a` | Files: `src/store/session.rs`, `src/store/mod.rs`, `tests/store_session_usage.rs`

- [ ] 2. Implement SQL aggregation over persisted turns

  **What to do**: Add a private aggregation helper in `src/store/turn.rs` that groups usage rows by effective model using `COALESCE(turns.model, sessions.model)`, scoped to one session, counting only `turns.kind = 'message'` rows where either token column is non-null. Treat null token cells as zero within the sum. Return grouped slices that Task 4 can price correctly when a session switches models mid-stream.
  **Must NOT do**: Do not aggregate from runtime events, do not assume a single model per session, and do not include non-message rows in the pricing basis.

  **Recommended Agent Profile**:
  - Category: `unspecified-low` — Reason: SQL aggregation and row-mapping logic with low but non-trivial correctness risk.
  - Skills: `[]` — no extra skills required.
  - Omitted: `find-docs` — local schema/query patterns are authoritative.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: 4 | Blocked By: 1

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/store/schema.rs:21-33` — `turns` stores `role`, `kind`, `model`, `tokens_in`, `tokens_out`, and `turn_number`.
  - Pattern: `src/store/turn.rs:8-34` — existing persisted `Turn` / `NewTurn` data model.
  - Pattern: `src/store/turn.rs:41-100` — existing insert and list query style; keep SQL in this module aligned with current mapping style.
  - Pattern: `src/runtime/session.rs:1243-1258` — assistant responses persist `model`, `tokens_in`, and `tokens_out` here.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test --test store_session_usage session_usage_totals_sum_tokens_across_message_turns -- --exact`
  - [ ] `cargo test --test store_session_usage session_usage_totals_groups_usage_by_effective_model -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Sum session token totals across persisted turns
    Tool: Bash
    Steps: cargo test --test store_session_usage session_usage_totals_sum_tokens_across_message_turns -- --exact 2>&1 | tee .sisyphus/evidence/task-2-turn-aggregation.txt
    Expected: Test passes and proves totals equal the sum of persisted `tokens_in` and `tokens_out` across the session's token-bearing message turns.
    Evidence: .sisyphus/evidence/task-2-turn-aggregation.txt

  Scenario: Mid-session model change remains visible to pricing layer
    Tool: Bash
    Steps: cargo test --test store_session_usage session_usage_totals_groups_usage_by_effective_model -- --exact 2>&1 | tee .sisyphus/evidence/task-2-turn-aggregation-error.txt
    Expected: Test passes and proves usage is grouped by effective model rather than flattened to a single session model before cost calculation.
    Evidence: .sisyphus/evidence/task-2-turn-aggregation-error.txt
  ```

  **Commit**: NO | Message: `n/a` | Files: `src/store/turn.rs`, `tests/store_session_usage.rs`

- [ ] 3. Add a models.dev pricing catalog parser and resolver

  **What to do**: Add a new top-level `src/pricing/` module with `src/pricing/models_dev.rs` and export it from `src/lib.rs`. Implement `ModelsDevCatalog` parsing from JSON shaped like `https://models.dev/api.json`, keeping only the fields needed for this feature: provider id, model id, `cost.input`, and `cost.output`. Implement exact provider+model lookup only. Add fixture-driven tests in `tests/pricing_models_dev.rs` using a checked-in fixture under `tests/fixtures/models_dev/api.json`.
  **Must NOT do**: Do not add fuzzy alias matching, family-based lookup, or any requirement for live network access in tests.

  **Recommended Agent Profile**:
  - Category: `quick` — Reason: bounded parser/resolver module with fixture tests.
  - Skills: `[]` — no extra skills required.
  - Omitted: `find-docs` — models.dev docs are already captured; implementation is fixture-driven.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 4, 5 | Blocked By: none

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/lib.rs:6-17` — new top-level modules are exported here.
  - Pattern: `Cargo.toml:23-25` — `reqwest` + `serde_json` are already present; avoid adding new HTTP or JSON dependencies.
  - External: `https://models.dev/api.json` — canonical pricing registry shape.
  - External: `https://github.com/anomalyco/models.dev/blob/dev/README.md` — confirms model entries expose `[cost].input` and `[cost].output` in USD per 1M tokens.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test --test pricing_models_dev models_dev_catalog_resolves_exact_provider_and_model -- --exact`
  - [ ] `cargo test --test pricing_models_dev models_dev_catalog_returns_none_for_missing_price -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Exact models.dev provider/model lookup works
    Tool: Bash
    Steps: cargo test --test pricing_models_dev models_dev_catalog_resolves_exact_provider_and_model -- --exact 2>&1 | tee .sisyphus/evidence/task-3-models-dev-catalog.txt
    Expected: Test passes and resolves the fixture's exact provider/model pair to the expected input/output USD-per-1M values.
    Evidence: .sisyphus/evidence/task-3-models-dev-catalog.txt

  Scenario: Missing or partial pricing does not masquerade as free
    Tool: Bash
    Steps: cargo test --test pricing_models_dev models_dev_catalog_returns_none_for_missing_price -- --exact 2>&1 | tee .sisyphus/evidence/task-3-models-dev-catalog-error.txt
    Expected: Test passes and proves missing price data returns `None` rather than zero pricing.
    Evidence: .sisyphus/evidence/task-3-models-dev-catalog-error.txt
  ```

  **Commit**: NO | Message: `n/a` | Files: `src/lib.rs`, `src/pricing/mod.rs`, `src/pricing/models_dev.rs`, `tests/pricing_models_dev.rs`, `tests/fixtures/models_dev/api.json`

- [ ] 4. Compose session cost totals from aggregated usage + pricing catalog

  **What to do**: Implement `Session::usage_totals_with_catalog` by combining Task 2’s grouped usage slices with Task 3’s exact pricing catalog. Cost rules are fixed: compute in integer micro-USD, using `turn.model` when present and `sessions.model` as fallback; provider always comes from canonical `sessions.provider`; no fuzzy aliasing; if any grouped slice is unpriced, set `cost_usd_micros` to `None` and return sorted unique `unpriced_models`. Use `u128` intermediates for multiplication/division and convert to `u64` only after bounds checks.
  **Must NOT do**: Do not fetch the network inside this core method, do not flatten multi-model sessions into a single-price assumption, and do not silently drop unpriced slices.

  **Recommended Agent Profile**:
  - Category: `unspecified-low` — Reason: correctness-sensitive composition logic with multiple edge cases.
  - Skills: `[]` — no extra skills required.
  - Omitted: `find-docs` — implementation depends on already-captured pricing schema, not new docs.

  **Parallelization**: Can Parallel: NO | Wave 3 | Blocks: 6 | Blocked By: 2, 3

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/store/session.rs:60-80` — canonical session metadata used for provider/model fallback.
  - Pattern: `src/provider/mod.rs:34-54` — token usage semantics are input/output/total; mirror that shape in derived totals.
  - Pattern: `src/runtime/session.rs:1284-1295` — runtime emits derived token totals after persistence; this plan derives from stored rows instead of events.
  - External: `https://github.com/anomalyco/opencode/blob/00d6841f8474676052553d6278c1ad52b8ecf182/packages/opencode/src/cli/cmd/stats.ts#L170-L219`
  - External: `https://github.com/anomalyco/opencode/blob/00d6841f8474676052553d6278c1ad52b8ecf182/packages/opencode/src/cli/cmd/stats.ts#L252-L307` — upstream OpenCode aggregates persisted usage/cost at query time; align with that derived-view behavior.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test --test store_session_usage session_usage_totals_compute_cost_micros_from_models_dev_prices -- --exact`
  - [ ] `cargo test --test store_session_usage session_usage_totals_return_unpriced_models_when_catalog_entry_is_missing -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Session cost totals are computed from models.dev fixture prices
    Tool: Bash
    Steps: cargo test --test store_session_usage session_usage_totals_compute_cost_micros_from_models_dev_prices -- --exact 2>&1 | tee .sisyphus/evidence/task-4-session-cost-composition.txt
    Expected: Test passes and asserts exact micro-USD totals for known input/output token counts and known models.dev prices.
    Evidence: .sisyphus/evidence/task-4-session-cost-composition.txt

  Scenario: Unknown pricing yields explicit unpriced output
    Tool: Bash
    Steps: cargo test --test store_session_usage session_usage_totals_return_unpriced_models_when_catalog_entry_is_missing -- --exact 2>&1 | tee .sisyphus/evidence/task-4-session-cost-composition-error.txt
    Expected: Test passes and asserts `cost_usd_micros == None` plus a sorted unique `unpriced_models` list naming the unresolved model ids.
    Evidence: .sisyphus/evidence/task-4-session-cost-composition-error.txt
  ```

  **Commit**: NO | Message: `n/a` | Files: `src/store/session.rs`, `tests/store_session_usage.rs`

- [ ] 5. Add the live models.dev fetch helper without making tests network-dependent

  **What to do**: In `src/pricing/models_dev.rs`, add a thin blocking fetch helper that downloads `https://models.dev/api.json` using the existing blocking reqwest pattern, then delegates to the already-tested parser from Task 3. Expose a convenience entry point for callers that want current pricing, but keep the parser and `usage_totals_with_catalog` path usable without network access. Add tests for invalid JSON / invalid shape handling using fixture strings, not live HTTP.
  **Must NOT do**: Do not call the live models.dev endpoint during test execution, do not cache to disk, and do not move networking into the store aggregation core.

  **Recommended Agent Profile**:
  - Category: `quick` — Reason: narrow wrapper over existing parser + existing blocking reqwest pattern.
  - Skills: `[]` — no extra skills required.
  - Omitted: `find-docs` — no further doc lookup is needed.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: 6 | Blocked By: 3

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/auth/mod.rs:84-126` — canonical sync code pattern for blocking reqwest client usage.
  - Pattern: `Cargo.toml:23-25` — `reqwest` already includes the `blocking` feature.
  - External: `https://models.dev/api.json` — fetch target.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test --test pricing_models_dev models_dev_catalog_rejects_invalid_json_payload -- --exact`
  - [ ] `cargo test --test pricing_models_dev models_dev_catalog_rejects_invalid_shape_payload -- --exact`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Invalid JSON is surfaced as a parser error
    Tool: Bash
    Steps: cargo test --test pricing_models_dev models_dev_catalog_rejects_invalid_json_payload -- --exact 2>&1 | tee .sisyphus/evidence/task-5-models-dev-fetcher.txt
    Expected: Test passes and proves malformed payloads are rejected with a clear parse error path.
    Evidence: .sisyphus/evidence/task-5-models-dev-fetcher.txt

  Scenario: Structurally wrong payload is surfaced as a shape error
    Tool: Bash
    Steps: cargo test --test pricing_models_dev models_dev_catalog_rejects_invalid_shape_payload -- --exact 2>&1 | tee .sisyphus/evidence/task-5-models-dev-fetcher-error.txt
    Expected: Test passes and proves syntactically valid but schema-incompatible payloads do not produce silent empty catalogs.
    Evidence: .sisyphus/evidence/task-5-models-dev-fetcher-error.txt
  ```

  **Commit**: NO | Message: `n/a` | Files: `src/pricing/models_dev.rs`, `tests/pricing_models_dev.rs`

- [ ] 6. Verify the store query works through `store_run` and the existing suite

  **What to do**: Add an integration test that exercises the finished API through `store::store_run` with a shared in-memory store, mirroring the async access pattern already used elsewhere. Run the focused new tests first, then the full Rust suite. Keep `src/store/schema.rs` untouched unless a test proves the current schema is insufficient; if that happens, stop and open a follow-up plan instead of silently expanding scope.
  **Must NOT do**: Do not add browser tests, do not touch websocket/UI code, and do not sneak in schema changes to “make it easier.”

  **Recommended Agent Profile**:
  - Category: `unspecified-low` — Reason: regression-oriented integration verification.
  - Skills: `[]` — no extra skills required.
  - Omitted: `find-docs` — entirely local verification.

  **Parallelization**: Can Parallel: NO | Wave 3 | Blocks: Final Verification Wave | Blocked By: 4, 5

  **References** (executor has NO interview context — be exhaustive):
  - Pattern: `src/store/mod.rs:65-86` — `store_run` is the async-safe query surface.
  - Test: `tests/store_concurrency.rs:46-90` — existing pattern for `SharedStore` + `store_run` integration.
  - Test: `tests/harness/mod.rs:13-31` — in-memory store setup for isolated regression checks.
  - Pattern: `src/store/schema.rs:138-170` — schema migration path; leave untouched for this feature.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test --test store_session_usage session_usage_totals_can_run_inside_store_run -- --exact`
  - [ ] `cargo test --test store_session_usage`
  - [ ] `cargo test --test pricing_models_dev`
  - [ ] `cargo test`

  **QA Scenarios** (MANDATORY — task incomplete without these):
  ```
  Scenario: Async store_run caller can retrieve session totals
    Tool: Bash
    Steps: cargo test --test store_session_usage session_usage_totals_can_run_inside_store_run -- --exact 2>&1 | tee .sisyphus/evidence/task-6-store-run-regression.txt
    Expected: Test passes and proves the new query surface works through `store_run` with `SharedStore` instead of only direct `Store` access.
    Evidence: .sisyphus/evidence/task-6-store-run-regression.txt

  Scenario: Full Rust regression remains green
    Tool: Bash
    Steps: cargo test 2>&1 | tee .sisyphus/evidence/task-6-store-run-regression-error.txt
    Expected: Entire Rust test suite passes with no schema-migration changes introduced for this feature.
    Evidence: .sisyphus/evidence/task-6-store-run-regression-error.txt
  ```

  **Commit**: NO | Message: `n/a` | Files: `tests/store_session_usage.rs`, `tests/pricing_models_dev.rs`, optional minimal touch-ups in `src/store/mod.rs`

## Final Verification Wave (MANDATORY — after ALL implementation tasks)
> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.
> **Do NOT auto-proceed after verification. Wait for user's explicit approval before marking work complete.**
> **Never mark F1-F4 as checked before getting user's okay.** Rejection or user feedback -> fix -> re-run -> present again -> wait for okay.
- [ ] F1. Plan Compliance Audit — oracle
- [ ] F2. Code Quality Review — unspecified-high
- [ ] F3. Real Manual QA — unspecified-high (+ playwright if UI)
- [ ] F4. Scope Fidelity Check — deep

## Commit Strategy
- Default: no commit during implementation unless the user explicitly requests one.
- If a commit is requested after verification, use a single final commit:
  - `feat(store): add session token and cost totals query`

## Success Criteria
- A caller can request session totals from the store layer and receive deterministic token totals plus derived cost.
- Cost is derived from models.dev API pricing without changing the on-disk schema.
- Unknown-priced models are explicit (`cost_usd_micros: None` + `unpriced_models`), never silently free.
- Tests prove empty-session, null-token, model-fallback, unpriced-model, and `store_run` behaviors.
- Full Rust suite passes and no extra surface area (CLI/UI/schema migration) was added.
