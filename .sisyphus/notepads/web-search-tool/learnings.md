2026-04-06: Strict built-in tool schemas in this repo require every declared
property to appear in `required`, with nullable optionals encoded as union types
like `["integer", "null"]` plus `additionalProperties: false`. 2026-04-06:
Registering `web_search` in `default_registry()` automatically brings it under
`default_registry_tool_schemas_match_strict_mode_requirements`, so registry
wiring is enough to keep the contract covered by the existing strict-mode
guardrail. 2026-04-06: Task 2 moved the web-search output contract into
repo-owned `WebSearchResult`/`WebSearchCitation` helpers in
`src/tools/web_search.rs`, so JSON-string responses now consistently apply query
trimming, length validation, and nullable `summary`/`message` serialization
before any backend work exists. 2026-04-06: Cited web-search results should be
normalized from provider-shaped inputs into capped repo-owned citations, with
snippets truncated to 280 chars, summaries truncated to 1600 chars, and citation
indexes reassigned to stable 1-based values after limiting to the max-result
cap. 2026-04-06: Task 3 kept `WebSearchTool::execute()` synchronous by resolving
`max_results` through a reusable helper and routing configured/unconfigured
placeholder branches through JSON result helpers, which lets flat integration
tests verify the default-of-5 contract without adding backend-specific behavior
early. 2026-04-06: Task 4 introduced an internal resolver in
`src/tools/web_search.rs` that binds the tool to Tavily when `TAVILY_API_KEY` is
populated and otherwise returns the standardized unavailable payload, keeping
all backend selection logic in one place until later tasks add real
integrations. 2026-04-06: Task 5 wires `web_search` into
`registry_with_lsp_service` between `read_skill` and the runtime-only tools and
adds registry tests proving the provider-facing `to_api_tools()` array now
includes the strict `web_search` schema without any special-casing. 2026-04-06:
Task 6 keeps Tavily production behavior fixed at a private 15s blocking
`reqwest` timeout while exposing only a `feature = "testing"` guard-backed
override and test-only `TAVILY_API_BASE_URL` path, which makes local axum
`/search` integration tests deterministic without broadening the runtime config
surface.
