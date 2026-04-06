# Web Search Tool

## TL;DR

> **Summary**: Add a built-in Rust `web_search` tool to Kley’s existing tool
> registry, using a provider-agnostic internal contract and a free-first Tavily
> backend for v1. The tool must always be registered, return a normalized JSON
> string with summary + citations, and degrade to a structured `unavailable`
> result when no backend is configured. **Deliverables**:
>
> - new built-in `src/tools/web_search.rs`
> - registry wiring in `src/tools/mod.rs`
> - deterministic unit + runtime integration tests
> - minimal README/config docs for enablement and limits **Effort**: Medium
>   **Parallel**: YES - 2 waves **Critical Path**: Task 1 → Task 3 → Task 5 →
>   Task 6

## Context

### Original Request

Add web search as a tool, using OpenAI’s web-search docs and opencode’s
`websearch.ts` as references, while preferring free options and not requiring
MCP.

### Interview Summary

- Cost posture: **free-first**.
- Tool shape: **provider-agnostic internal design**.
- V1 capability: **search only**.
- Default output: **summary + citations**.
- Test strategy: **tests-after**.

### Metis Review (gaps addressed)

- Treat this as an architecture feature, not a one-file addition.
- Keep v1 search-only: no fetch/open-page/find-in-page.
- Lock a repo-owned output contract before backend code.
- Prefer deterministic fake-backend tests over live provider tests.
- Decide registration/config behavior explicitly: always register, return
  structured `unavailable` when disabled.

## Work Objectives

### Core Objective

Ship a built-in `web_search` tool that works through Kley’s existing synchronous
tool path for both CLI and web sessions, using a normalized JSON-string output
contract and a single free-first real backend in v1.

### Deliverables

- `src/tools/web_search.rs` implementing `Tool`
- `src/tools/mod.rs` export + default registry registration + built-in tests
  updated
- deterministic backend abstraction with Tavily v1 adapter
- runtime integration tests proving execution + persistence via `SessionRuntime`
- README note covering configuration, limits, and v1 scope

### Definition of Done (verifiable conditions with commands)

- `cargo test web_search_ -- --nocapture`
- `cargo test default_registry_has_builtins -- --exact`
- `cargo test default_registry_tool_schemas_match_strict_mode_requirements -- --exact`
- `cargo test runtime_includes_web_search_in_provider_tool_payload -- --exact`
- `cargo test runtime_executes_web_search_tool_via_session_manager -- --exact`
- `cargo test runtime_persists_web_search_function_call_output -- --exact`

### Must Have

- Always-registered built-in tool named `web_search`
- Public schema limited to `query` + `max_results`
- Strict JSON schema compatible with current OpenAI tool serialization
- Internal provider abstraction, but only **one real backend** in v1
- V1 backend: **Tavily Search API** using `POST https://api.tavily.com/search`
- Structured JSON-string result:
  - `status`: `ok | no_results | unavailable`
  - `query`: original query
  - `summary`: string or `null`
  - `citations`: array of `{ index, title, url, snippet }`
  - `message`: string or `null`
- Execution-time backend resolution:
  - if `TAVILY_API_KEY` exists and is non-empty, use Tavily
  - otherwise return `status="unavailable"` with empty citations and a clear
    `message`
- Hard limits:
  - `query` max 400 chars after trim
  - `max_results` default 5, cap 5
  - timeout 15s
  - max 5 citations returned
  - snippet cap 280 chars each
  - summary cap 1600 chars

### Must NOT Have (guardrails, AI slop patterns, scope boundaries)

- No MCP transport
- No `web_fetch`, `open_page`, `find_in_page`, crawling, or extraction tool in
  v1
- No OpenAI-native web-search backend in v1
- No Brave or SearXNG integration in v1
- No provider-specific request fields in the public tool schema
- No caching, ranking knobs, domain filters, country filters, pagination,
  streaming, or UI-only rendering work
- No Playwright additions unless the implementation changes web UI behavior

## Verification Strategy

> ZERO HUMAN INTERVENTION - all verification is agent-executed.

- Test decision: tests-after + Rust unit/integration framework
- QA policy: Every task has agent-executed scenarios
- Evidence: `.sisyphus/evidence/task-{N}-{slug}.{ext}`
- Live provider calls are optional/non-default; default verification uses fake
  HTTP handlers or fake backends only

## Execution Strategy

### Parallel Execution Waves

> Target: 5-8 tasks per wave. <3 per wave (except final) = under-splitting.
> Extract shared dependencies as Wave-1 tasks for max parallelism.

Wave 1: contract + scaffolding

- Task 1 Contract tests and golden output
- Task 2 Result model and validation helpers
- Task 3 Tool skeleton and execution contract
- Task 4 Backend resolver and config semantics

Wave 2: real backend + integration

- Task 5 Registry wiring and provider exposure
- Task 6 Tavily backend + normalization
- Task 7 Runtime integration and docs

### Dependency Matrix (full, all tasks)

- 1: blocks 3, 5, 6, 7
- 2: blocks 3, 4, 6
- 3: blocked by 1, 2; blocks 5, 6, 7
- 4: blocked by 2; blocks 6
- 5: blocked by 1, 3; blocks 7
- 6: blocked by 1, 2, 3, 4; blocks 7
- 7: blocked by 3, 5, 6

### Agent Dispatch Summary (wave → task count → categories)

- Wave 1 → 4 tasks → unspecified-high, quick
- Wave 2 → 3 tasks → unspecified-high, quick
- Final Verification → 4 tasks → oracle, unspecified-high, deep

## TODOs

> Implementation + Test = ONE task. Never separate. EVERY task MUST have: Agent
> Profile + Parallelization + QA Scenarios.

- [ ] 1. Lock the public contract with deterministic tests

  **What to do**: Create the contract-first test surface for `web_search`. Add
  exact tests for strict schema shape, required nullable fields, accepted
  statuses, empty-result behavior, unavailable behavior, and v1 scope
  boundaries. Define these exact test names so later tasks can drive them green:
  `web_search_schema_is_strict`, `web_search_rejects_unknown_fields`,
  `web_search_returns_unavailable_without_tavily_api_key`,
  `web_search_returns_no_results_shape`,
  `web_search_scope_excludes_fetch_fields`. **Must NOT do**: Do not call any
  live search provider. Do not introduce OpenAI/Brave/SearXNG code here.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` - Reason: multiple files and exact contract
    decisions must be locked before code.
  - Skills: `[]` - no special project skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 3, 5, 6, 7 | Blocked
  By: none

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/tools/mod.rs:206-219` - API tool serialization requires
    `strict: true` and passes each tool schema unchanged.
  - Pattern: `src/tools/mod.rs:319-367` - built-in schema and registry
    assertions already exist and must be extended.
  - Pattern: `src/tools/read_file.rs:19-41` - strict schema with nullable
    optional fields encoded in `required`.
  - Pattern: `src/tools/read_file.rs:157-181` - unit test style for strict
    schema assertions.
  - Pattern: `src/tools/shell.rs:100-117` - schema style for tool args with
    nullable optional field.
  - External: `https://developers.openai.com/api/docs/guides/tools-web-search` -
    citation-oriented result behavior reference.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web_search_schema_is_strict -- --exact`
  - [ ] `cargo test web_search_rejects_unknown_fields -- --exact`
  - [ ] `cargo test web_search_returns_unavailable_without_tavily_api_key -- --exact`
  - [ ] `cargo test web_search_returns_no_results_shape -- --exact`
  - [ ] `cargo test web_search_scope_excludes_fetch_fields -- --exact`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Contract tests define v1 shape
    Tool: Bash
    Steps: cargo test web_search_ -- --nocapture
    Expected: The exact new web_search contract tests compile and pass; failures clearly indicate missing shape fields or v1 guardrail regressions.
    Evidence: .sisyphus/evidence/task-1-web-search-contract.txt

  Scenario: Scope boundary rejects fetch-style fields
    Tool: Bash
    Steps: cargo test web_search_scope_excludes_fetch_fields -- --exact
    Expected: Test passes only when the public schema excludes fetch/open_page/find_in_page-style parameters.
    Evidence: .sisyphus/evidence/task-1-web-search-contract-error.txt
  ```

  **Commit**: YES | Message: `test(tools): lock web search v1 contract` | Files:
  `["src/tools/web_search.rs", "src/tools/mod.rs"]`

- [ ] 2. Add normalized result model and validation helpers

  **What to do**: In `src/tools/web_search.rs`, define the repo-owned result
  model and helper functions before any network code. Use a serialized JSON
  string output with this exact shape: `status`, `query`, `summary`,
  `citations`, `message`. Define `citation` as `{ index, title, url, snippet }`,
  with `snippet` nullable in Rust but serialized as either string or `null`. Add
  validation helpers for query trimming, query length, result-count caps,
  summary truncation, snippet truncation, and stable 1-based citation indexing.
  **Must NOT do**: Do not expose provider score fields, raw provider ids,
  images, answer metadata, usage, or request ids.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` - Reason: this is the contract core reused by
    the tool, backend, and tests.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 3, 4, 6 | Blocked
  By: none

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/tools/mod.rs:153-166` - tools return string outputs;
    recoverable domain errors should not throw.
  - Pattern: `src/tools/shell.rs:23-30` - precedent for hard caps/timeouts as
    module constants.
  - Pattern: `src/runtime/session.rs:1410-1448` - tool output is persisted as
    `function_call_output` string content and replayed back into history.
  - External: `https://developers.openai.com/api/docs/guides/tools-web-search` -
    output inspiration for summary + citations.
  - External:
    `https://docs.tavily.com/documentation/api-reference/endpoint/search` -
    source fields available for normalization (`results[].title`, `url`,
    `content`, optional `answer`).

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web_search_normalizes_ok_result_shape -- --exact`
  - [ ] `cargo test web_search_caps_summary_and_snippets -- --exact`
  - [ ] `cargo test web_search_assigns_stable_citation_indexes -- --exact`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Normalized JSON shape is stable
    Tool: Bash
    Steps: cargo test web_search_normalizes_ok_result_shape -- --exact
    Expected: The tool serializes a JSON string with only status/query/summary/citations/message in the agreed shape.
    Evidence: .sisyphus/evidence/task-2-web-search-shape.txt

  Scenario: Overlong snippets are bounded
    Tool: Bash
    Steps: cargo test web_search_caps_summary_and_snippets -- --exact
    Expected: Summary and citation snippets are truncated to the plan limits and remain valid JSON output.
    Evidence: .sisyphus/evidence/task-2-web-search-shape-error.txt
  ```

  **Commit**: YES | Message: `refactor(tools): add web search result contract` |
  Files: `["src/tools/web_search.rs"]`

- [ ] 3. Implement the `WebSearchTool` skeleton and synchronous execution
     contract

  **What to do**: Implement `WebSearchTool` in `src/tools/web_search.rs` using
  the existing synchronous `Tool` trait. Public schema must be exactly:
  - `query`: required string
  - `max_results`: required nullable integer, min 1, max 5, default `null` The
    tool must trim `query`, reject empty queries with a recoverable tool output,
    resolve `max_results` to default 5, and return JSON strings using the Task-2
    result model. Keep execution synchronous and use blocking HTTP only in
    backend adapters. **Must NOT do**: Do not add async trait changes, runtime
    special-casing, or alternate UI formatting.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` - Reason: central feature file with validation
    and tool semantics.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 5, 6, 7 | Blocked
  By: 1, 2

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/tools/read_file.rs:8-44` - minimal tool implementation shape.
  - Pattern: `src/tools/shell.rs:91-129` - argument parsing with recoverable
    error strings.
  - Pattern: `src/tools/mod.rs:143-166` - current `Tool` trait is sync and must
    remain unchanged in v1.
  - Pattern: `src/runtime/session.rs:880-907` - registry tools are executed
    through `execute_with_result`; `Err(...)` is treated as tool failure.
  - Oracle decision: keep v1 synchronous and bounded rather than refactoring
    tool execution architecture.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web_search_schema_is_strict -- --exact`
  - [ ] `cargo test web_search_empty_query_returns_recoverable_error -- --exact`
  - [ ] `cargo test web_search_default_max_results_is_five -- --exact`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Empty input fails gracefully
    Tool: Bash
    Steps: cargo test web_search_empty_query_returns_recoverable_error -- --exact
    Expected: Empty or whitespace-only query produces a normal tool output, not a panic or anyhow error.
    Evidence: .sisyphus/evidence/task-3-web-search-tool.txt

  Scenario: Nullable max_results obeys strict mode
    Tool: Bash
    Steps: cargo test web_search_schema_is_strict -- --exact
    Expected: Schema includes both fields in required, keeps additionalProperties false, and encodes max_results as integer|null.
    Evidence: .sisyphus/evidence/task-3-web-search-tool-error.txt
  ```

  **Commit**: YES | Message: `feat(tools): add web search tool skeleton` |
  Files: `["src/tools/web_search.rs"]`

- [ ] 4. Add backend resolver and config semantics

  **What to do**: Keep backend resolution internal to `src/tools/web_search.rs`.
  Define a small internal trait/interface, but implement only one real backend
  in v1. Resolver order must be exact: if `TAVILY_API_KEY` is set and non-empty,
  use Tavily; otherwise return `status="unavailable"` with `summary=null`,
  `citations=[]`, and `message="Set TAVILY_API_KEY to enable web_search."`.
  Register the tool unconditionally; do not hide it based on environment. Add
  tests for resolver behavior using environment guards patterned after existing
  tests. **Must NOT do**: Do not add `WEB_SEARCH_BACKEND`, per-session backend
  switching, or credential storage integration in v1.

  **Recommended Agent Profile**:
  - Category: `quick` - Reason: small internal abstraction plus deterministic
    config behavior.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 6 | Blocked By: 2

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/auth/mod.rs:427-460` - env-var-first credential resolution
    style already exists for OpenAI.
  - Pattern: `tests/runtime.rs:39-64` - `EnvVarGuard` pattern for deterministic
    env-var tests.
  - Pattern: `src/runtime/settings.rs:154-218` - stable registry membership
    matters because policy validation rejects unknown tool names.
  - Oracle decision: always register the tool; unavailable state is returned at
    execution time.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web_search_uses_tavily_backend_when_api_key_present -- --exact`
  - [ ] `cargo test web_search_returns_unavailable_without_tavily_api_key -- --exact`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Missing config returns unavailable shape
    Tool: Bash
    Steps: cargo test web_search_returns_unavailable_without_tavily_api_key -- --exact
    Expected: The tool stays registered but returns status=unavailable with empty citations and the exact enablement message.
    Evidence: .sisyphus/evidence/task-4-web-search-config.txt

  Scenario: Configured backend path wins deterministically
    Tool: Bash
    Steps: cargo test web_search_uses_tavily_backend_when_api_key_present -- --exact
    Expected: Resolver selects Tavily whenever TAVILY_API_KEY is set, without requiring any other env var.
    Evidence: .sisyphus/evidence/task-4-web-search-config-error.txt
  ```

  **Commit**: YES | Message: `feat(tools): add web search backend resolver` |
  Files: `["src/tools/web_search.rs"]`

- [ ] 5. Wire the tool into the built-in registry and provider payloads

  **What to do**: Export the module from `src/tools/mod.rs`, register
  `WebSearchTool` in `registry_with_lsp_service(...)`, and extend built-in tests
  so `web_search` is always included in `default_registry`. Place registration
  after `read_skill` and before runtime-only tools (`delegate_task`,
  `report_status`) to keep read-only lookup tools grouped together. Add/adjust
  tests proving the serialized tool schema reaches the provider payload through
  existing registry plumbing; do not add special cases to
  `src/provider/openai.rs` because it already serializes
  `ctx.registry.to_api_tools()`. Create or update only registry-focused
  assertions in `src/tools/mod.rs`; leave runtime/provider integration
  assertions for Task 7. **Must NOT do**: Do not add provider-specific
  registration branches or runtime special handling for `web_search`.

  **Recommended Agent Profile**:
  - Category: `quick` - Reason: bounded registry wiring with existing patterns.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: 7 | Blocked By: 1, 3

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `src/tools/mod.rs:223-271` - one source of truth for built-in
    registration.
  - Pattern: `src/tools/mod.rs:319-367` - built-in registry tests that must be
    extended.
  - Pattern: `src/provider/openai.rs:250-262` - provider already sends all
    registry tools in `response.create` payload.
  - Pattern: `src/agent.rs:41-45` - CLI uses `default_registry`, so new
    built-ins propagate automatically.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test default_registry_has_builtins -- --exact`
  - [ ] `cargo test default_registry_tool_schemas_match_strict_mode_requirements -- --exact`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Registry exposes web_search everywhere
    Tool: Bash
    Steps: cargo test default_registry_has_builtins -- --exact
    Expected: default_registry contains web_search alongside existing built-ins.
    Evidence: .sisyphus/evidence/task-5-web-search-registry.txt

  Scenario: Strict built-in schema regression stays green
    Tool: Bash
    Steps: cargo test default_registry_tool_schemas_match_strict_mode_requirements -- --exact
    Expected: web_search satisfies strict-mode schema requirements without weakening any existing built-in checks.
    Evidence: .sisyphus/evidence/task-5-web-search-registry-error.txt
  ```

  **Commit**: YES | Message: `feat(tools): register web search builtin` | Files:
  `["src/tools/mod.rs", "src/tools/web_search.rs"]`

- [ ] 6. Implement the Tavily backend and normalization path

  **What to do**: Implement the only real v1 backend against Tavily Search. Use
  blocking `reqwest` with the existing `blocking` feature, a 15s timeout, and
  request body:
  - `query`: validated user query
  - `search_depth`: `basic`
  - `max_results`: resolved cap (1..=5)
  - `include_answer`: `basic`
  - `include_raw_content`: `false` Send `Authorization: Bearer <TAVILY_API_KEY>`
    to `POST https://api.tavily.com/search`. Normalize `answer` into `summary`
    when present; otherwise synthesize `summary` from the first 1-3
    `results[].content` values. Normalize citations from `results[].title`,
    `url`, and `content`, preserving only the first 5. Map provider failures to
    recoverable outputs:
  - HTTP 401/403/429/432/433/5xx => `status="unavailable"`
  - empty `results` => `status="no_results"`
  - malformed JSON / transport timeout => `status="unavailable"` In all failure
    cases, keep `citations=[]` and populate `message`. **Must NOT do**: Do not
    use Tavily extract/crawl/research endpoints. Do not surface Tavily-specific
    usage, score, request_id, project_id, or raw_content fields.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` - Reason: network integration, normalization,
    and failure mapping.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: 7 | Blocked By: 1,
  2, 3, 4

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `Cargo.toml:18-31` - `reqwest` already includes the `blocking`
    feature; no new HTTP client dependency is needed.
  - Pattern: `src/tools/shell.rs:23-30` - precedent for module-level
    timeout/output limits.
  - External:
    `https://docs.tavily.com/documentation/api-reference/introduction` - base
    URL + bearer auth.
  - External:
    `https://docs.tavily.com/documentation/api-reference/endpoint/search` -
    `/search` request and response fields.
  - External: `https://docs.tavily.com/documentation/api-credits` - free tier is
    1,000 credits/month with no card; supports the free-first decision.
  - External: `https://api-dashboard.search.brave.com/documentation/pricing` -
    considered but not chosen for v1; monthly credits still cost-metered.
  - External: `https://docs.searxng.org/` - considered but rejected for v1
    because self-hosting infra is out of scope.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test web_search_tavily_maps_answer_and_results_to_summary_and_citations -- --exact`
  - [ ] `cargo test web_search_tavily_empty_results_return_no_results -- --exact`
  - [ ] `cargo test web_search_tavily_timeout_returns_unavailable -- --exact`
  - [ ] `cargo test web_search_tavily_http_429_returns_unavailable -- --exact`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Tavily success path normalizes provider payload
    Tool: Bash
    Steps: cargo test web_search_tavily_maps_answer_and_results_to_summary_and_citations -- --exact
    Expected: A mocked Tavily response yields status=ok, a non-empty summary, and numbered citations with title/url/snippet only.
    Evidence: .sisyphus/evidence/task-6-web-search-tavily.txt

  Scenario: Tavily throttling fails gracefully
    Tool: Bash
    Steps: cargo test web_search_tavily_http_429_returns_unavailable -- --exact
    Expected: HTTP 429 is mapped to status=unavailable with no panic, no raw provider payload leak, and a clear message.
    Evidence: .sisyphus/evidence/task-6-web-search-tavily-error.txt
  ```

  **Commit**: YES | Message: `feat(tools): add tavily web search backend` |
  Files: `["src/tools/web_search.rs"]`

- [ ] 7. Add runtime integration coverage and minimal operator docs

  **What to do**: Add a new integration file `tests/runtime_web_search.rs`. In
  that file, add runtime integration tests proving `web_search` executes through
  the shared runtime, persists `function_call_output`, and appears in outbound
  provider tool schemas. Follow the LSP runtime test style with a fake provider
  endpoint and deterministic backend stub or fake HTTP service. Also add minimal
  README documentation covering: `web_search` is built in, requires
  `TAVILY_API_KEY` for live use, returns JSON-string output with citations, and
  is intentionally search-only in v1. Do not add browser tests unless the UI
  surface changes. **Must NOT do**: Do not add Playwright tests for a
  backend-only change. Do not document Brave/OpenAI/SearXNG as shipped backends.

  **Recommended Agent Profile**:
  - Category: `unspecified-high` - Reason: multi-surface integration plus docs.
  - Skills: `[]` - no special skill needed.
  - Omitted: `["git"]` - no git operations required.

  **Parallelization**: Can Parallel: YES | Wave 2 | Blocks: none | Blocked By:
  3, 5, 6

  **References** (executor has NO interview context - be exhaustive):
  - Pattern: `tests/runtime_lsp_exact.rs:147-215` - runtime tool execution
    through `SessionRuntime` + fake server.
  - Pattern: `tests/runtime_lsp_exact.rs:217-283` - deterministic error-path
    runtime test style.
  - Pattern: `src/runtime/session.rs:1339-1448` - exact persistence path for
    `function_call` and `function_call_output` turns.
  - Pattern: `README.md:177-199` - development-notes section already documents
    test commands.
  - Pattern: `package.json:5-8` - browser test scripts exist, but are not
    required for backend-only verification.
  - Pattern: `lefthook.yml:23-27` - pre-push only runs Rust lib/bin tests, so
    runtime coverage must live in Rust tests.

  **Acceptance Criteria** (agent-executable only):
  - [ ] `cargo test runtime_executes_web_search_tool_via_session_manager -- --exact`
  - [ ] `cargo test runtime_persists_web_search_function_call_output -- --exact`
  - [ ] `cargo test runtime_includes_web_search_in_provider_tool_payload -- --exact`
  - [ ] `cargo test`

  **QA Scenarios** (MANDATORY - task incomplete without these):

  ```
  Scenario: Shared runtime executes web_search end to end
    Tool: Bash
    Steps: cargo test runtime_executes_web_search_tool_via_session_manager -- --exact
    Expected: The fake provider emits a web_search tool call, SessionRuntime executes it, and the turn completes successfully.
    Evidence: .sisyphus/evidence/task-7-web-search-runtime.txt

  Scenario: Output persistence keeps normalized JSON
    Tool: Bash
    Steps: cargo test runtime_persists_web_search_function_call_output -- --exact
    Expected: Stored function_call_output contains the normalized JSON string contract, not raw provider JSON.
    Evidence: .sisyphus/evidence/task-7-web-search-runtime-error.txt
  ```

  **Commit**: YES | Message: `test(runtime): integrate web search tool` | Files:
  `["tests/runtime_web_search.rs", "README.md"]`

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated
> results to user and get explicit "okay" before completing. **Do NOT
> auto-proceed after verification. Wait for user's explicit approval before
> marking work complete.** **Never mark F1-F4 as checked before getting user's
> okay.** Rejection or user feedback -> fix -> re-run -> present again -> wait
> for okay.

- [ ] F1. Plan Compliance Audit — oracle

  **Tool**: `task(subagent_type="oracle")` **Acceptance Criteria**:
  - [ ] Oracle reviews `.sisyphus/plans/web-search-tool.md` and the branch diff
        together.
  - [ ] Oracle explicitly confirms Tasks 1-7 were satisfied.
  - [ ] Oracle explicitly confirms the diff matches the Must Have and Must NOT
        Have lists. **QA Scenarios**:

  ```
  Scenario: Oracle verifies plan compliance
    Tool: task(subagent_type="oracle")
    Steps: Review `.sisyphus/plans/web-search-tool.md` and the actual branch diff; compare implemented files and tests against Tasks 1-7 plus Must Have/Must NOT Have.
    Expected: Oracle returns an approval that confirms no scope drift, Tavily-only v1, exact public contract fields, and no missing required deliverables.
    Evidence: .sisyphus/evidence/f1-plan-compliance.md
  ```

- [ ] F2. Code Quality Review — unspecified-high

  **Tool**: `task(category="unspecified-high")` **Acceptance Criteria**:
  - [ ] Reviewer inspects all files touched for `web_search`.
  - [ ] Reviewer explicitly approves blocking-call safety, error handling,
        naming, and test determinism.
  - [ ] Reviewer reports no provider-leakage in the public contract. **QA
        Scenarios**:

  ```
  Scenario: Reviewer checks code quality and determinism
    Tool: task(category="unspecified-high")
    Steps: Review the branch diff and touched files for error handling, naming consistency, unnecessary abstractions, blocking-call safety, and test determinism.
    Expected: Reviewer explicitly approves code quality, flags no flaky tests, and confirms the public contract does not leak provider-native fields.
    Evidence: .sisyphus/evidence/f2-code-quality.md
  ```

- [ ] F3. Real Manual QA — unspecified-high

  **Tool**: `task(category="unspecified-high")` + Bash **Acceptance Criteria**:
  - [ ] `cargo test web_search_ -- --nocapture` passes.
  - [ ] `cargo test runtime_executes_web_search_tool_via_session_manager -- --exact`
        passes.
  - [ ] `cargo test runtime_persists_web_search_function_call_output -- --exact`
        passes.
  - [ ] `cargo test runtime_includes_web_search_in_provider_tool_payload -- --exact`
        passes. **QA Scenarios**:

  ```
  Scenario: Manual QA runs all web_search verification commands
    Tool: Bash
    Steps: Run `cargo test web_search_ -- --nocapture`; run `cargo test runtime_executes_web_search_tool_via_session_manager -- --exact`; run `cargo test runtime_persists_web_search_function_call_output -- --exact`; run `cargo test runtime_includes_web_search_in_provider_tool_payload -- --exact`.
    Expected: All commands pass, stored tool output remains normalized JSON, and no browser/UI-specific verification is needed.
    Evidence: .sisyphus/evidence/f3-manual-qa.txt
  ```

- [ ] F4. Scope Fidelity Check — deep

  **Tool**: `task(category="deep")` **Acceptance Criteria**:
  - [ ] Reviewer compares the final diff against the Must NOT Have list.
  - [ ] Reviewer confirms no fetch/open-page/find-in-page or extra backends
        landed.
  - [ ] Reviewer confirms v1 remains search-only with exactly one real backend.
        **QA Scenarios**:

  ```
  Scenario: Deep review checks scope fidelity
    Tool: task(category="deep")
    Steps: Compare the final diff against the plan’s Must NOT Have section and verify no `web_fetch`, `open_page`, `find_in_page`, OpenAI-native backend, Brave backend, SearXNG backend, caching, or UI-only changes were added.
    Expected: Reviewer explicitly confirms the implementation stayed search-only, used exactly one real backend, and landed no out-of-scope features.
    Evidence: .sisyphus/evidence/f4-scope-fidelity.md
  ```

## Commit Strategy

- `test(tools): lock web search v1 contract`
- `refactor(tools): add web search result contract`
- `feat(tools): add web search tool skeleton`
- `feat(tools): add web search backend resolver`
- `feat(tools): register web search builtin`
- `feat(tools): add tavily web search backend`
- `test(runtime): integrate web search tool`

## Success Criteria

- `web_search` is a built-in tool in the default registry for both CLI and web
  runtime paths.
- Tool output is always a normalized JSON string with only the approved fields.
- Missing config, timeout, rate-limit, and empty-result cases all produce
  recoverable outputs with stable statuses.
- V1 stays search-only and provider-agnostic at the public contract level.
- Default verification passes with Rust tests only; browser tests remain
  unchanged unless implementation scope expands.
