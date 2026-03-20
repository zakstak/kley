
## Bindery UI Integration — Architectural Decisions (2026-03-18)

### D1: Use Bindery Template as Shell, Rewrite JS Around kley Protocol

**Decision**: Copy `bindery/templates/index.html` as the starting point for kley's UI. Rewrite all client-side JS to use kley's own WebSocket protocol (`prompt.submit`, `session.load`, etc.) rather than Bindery's RPC command names.

**Rationale**: The template provides a tested layout, design tokens, responsive CSS, and visual language. Rewriting the JS from scratch is cleaner than trying to adapt Bindery's JS which is tightly coupled to its RPC protocol and extension UI system.

**Trade-off**: 2483-line template is large; adaptation is work. But the alternative (build from scratch) loses 2000+ lines of CSS/layout investment.

### D2: Use RPC Types Only for Naming Inspiration

**Decision**: Reference `rpc-types.ts` for event and command naming patterns, but do not copy any type definitions. kley defines its own protocol types in Rust.

**Rationale**: The plan explicitly prohibits mirroring the full RPC surface. The naming conventions (e.g., `message.delta` vs Bindery's `message_update`, `turn.completed` vs `turn_end`) are valuable signals for what makes a good event vocabulary.

### D3: Port Mock Socket Pattern, Not Bindery's Mock Commands

**Decision**: Adapt the `MockEvent { delay, payload }` + `play_sequence()` pattern from `mock.rs` for kley's mock WebSocket. Emit events matching kley's own protocol shapes.

**Rationale**: The sequencing pattern (timed event emission for bootstrap and prompt response) is exactly what Task 2 needs. The specific mock commands (fork, task session, model cycling) are not relevant to kley.

### D4: Adopt Tailwind CDN v4 (No Build Pipeline for MVP)

**Decision**: Use `@tailwindcss/browser@4` CDN in the template, same as Bindery.

**Rationale**: Matches Bindery's approach. Avoids introducing a bundler/PostCSS pipeline in the first implementation. The plan explicitly says "do not introduce a bundler pipeline."

### D5: Flat Event Shape, No binderyMeta Wrapper

**Decision**: kley runtime events should emit `kind`, `title`, `preview` (or equivalent display fields) directly on the event object, not behind a `binderyMeta` wrapper.

**Rationale**: Avoids the transform layer `ws.rs` uses (`with_bindery_meta`). Simpler for a first implementation. Client JS can extract display data directly from event fields.

### D6: Omit Agent-level Events Entirely

**Decision**: kley has no sub-agent concept. Do not emit `agent_start`/`agent_end` events. The outer turn lifecycle (`turn.started` → `turn.completed`) is sufficient.

**Rationale**: These events are tied to Bindery's multi-agent orchestration model. kley is single-runtime. Omitting them simplifies the event taxonomy.

### D7: Copy bindery-icon.svg as-is

**Decision**: Copy `assets/bindery-icon.svg` to kley's `static/` directory and reference it from the template as a static asset.

**Rationale**: Plan explicitly calls for copying the icon. It's a simple 10-line SVG book icon.

### D8: Route icon through Axum for same-binary serving

**Decision**: Serve the copied icon at `/assets/bindery-icon.svg` via `src/web/ui.rs::bindery_icon` and route wiring in `src/web/router.rs`.

**Rationale**: Keeps Task 7 fully same-binary and avoids introducing new static-serving middleware just for one required asset.

### D9: Keep Task 7 shell presentational-only

**Decision**: Port the Bindery-inspired layout/tokens/selectors only, with no browser behavior wiring beyond static shell markup.

**Rationale**: Task 8 owns websocket-driven interactivity; keeping Task 7 presentational avoids scope creep and runtime/protocol changes.
