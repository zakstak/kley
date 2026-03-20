# Axum + Askama + Testing Patterns

## Sources

- Axum official docs: https://docs.rs/axum/latest/axum/
- Axum GitHub: https://github.com/tokio-rs/axum
- Askama docs: https://askama.readthedocs.io/
- Axum WebSocket testing example: https://github.com/tokio-rs/axum/blob/main/examples/testing-websockets/src/main.rs
- Axum DeepWiki (indexed 2026-02-22): https://deepwiki.com/tokio-rs/axum

---

## 1. Axum WebSocket Upgrade Pattern

### Core Handler Pattern
```rust
use axum::{
    extract::{ws::{WebSocket, WebSocketUpgrade, Message}, State},
    response::Response,
    routing::any,
    Router,
};

async fn ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    while let Some(Ok(msg)) = socket.recv().await {
        if let Message::Text(text) = msg {
            let _ = socket.send(Message::Text(format!("Echo: {text}")));
        }
    }
}

fn app() -> Router {
    Router::new()
        .route("/ws", any(ws_handler))  // Use `any` to support HTTP/1.1 GET and HTTP/2 CONNECT
        .with_state(state)
}
```

**Key points**:
- Use `routing::any` (not `get`) for WebSocket routes to support both HTTP/1.1 and HTTP/2
- `WebSocketUpgrade` is extracted from request parts, returns `Response` via `on_upgrade()`
- Handler is async and spawns separate task for socket processing

**Source**: https://context7.com/tokio-rs/axum/llms.txt

### WebSocketUpgrade Config Options
```rust
ws.protocols(["graphql-ws", "graphql-transport-ws"])
    .max_message_size(64 * 1024 * 1024)  // default: 64MB
    .max_frame_size(16 * 1024 * 1024)      // default: 16MB
    .max_write_buffer_size(128 * 1024)     // default: 128KB
    .on_failed_upgrade(|error| { /* log */ })
```

**Source**: https://docs.rs/axum/latest/axum/extract/ws/struct.WebSocketUpgrade.html

### Origin Checking (Same-Origin)
CORS layers don't apply to WebSocket upgrades. Check origin in handler:
```rust
use axum::http::{HeaderMap, header::ORIGIN};

async fn ws_handler(ws: WebSocketUpgrade, headers: HeaderMap) -> Response {
    let origin = headers.get(ORIGIN);
    // Validate origin matches allowed domain
    // if invalid, return 403 Response
    ws.on_upgrade(handle_socket)
}
```

**Source**: https://stackoverflow.com/questions/79702988/check-origin-for-websockets-in-axum

---

## 2. Axum Router with HTTP + WebSocket (Same-Origin)

### Combined Router Pattern
```rust
use axum::{
    routing::{get, any},
    Router,
};

fn app() -> Router {
    Router::new()
        .route("/", get(root_handler))
        .route("/health", get(health_handler))
        .route("/ws", any(ws_handler))  // WebSocket on same port/origin
        .layer(/* CORS, compression, etc. */)
        .with_state(state)
}

async fn root_handler() -> &'static str { "OK" }
async fn health_handler() -> &'static str { "healthy" }
```

**Source**: https://github.com/tokio-rs/axum/blob/main/README.md

### State Management
```rust
#[derive(Clone)]
struct AppState {
    db: Database,
    ws_sender: broadcast::Sender<String>,
}

fn app() -> Router {
    Router::new()
        .route("/ws", any(ws_handler))
        .with_state(AppState { db, ws_sender })
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}
```

**Source**: https://context7.com/tokio-rs/axum/llms.txt (broadcast chat example)

---

## 3. Askama Template Rendering in Axum

### Basic Pattern (Manual Integration)
```rust
use axum::{
    response::{Html, IntoResponse, Response},
    http::StatusCode,
};
use askama::Template;
use thiserror::Error;

#[derive(Debug, Error)]
enum AppError {
    #[error("could not render template")]
    Render(#[from] askama::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = StatusCode::INTERNAL_SERVER_ERROR;
        (status, Html("Template error".to_string())).into_response()
    }
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    title: String,
    items: Vec<String>,
}

async fn handler() -> Result<impl IntoResponse, AppError> {
    let tmpl = IndexTemplate {
        title: "Hello".to_string(),
        items: vec!["a".to_string(), "b".to_string()],
    };
    Ok(Html(tmpl.render()?))
}
```

### Simplified Pattern (askama_web)
```rust
// Add to Cargo.toml: askama_web = "0.1" (or latest)

use askama::Template;
use askama_web::WebTemplate;

#[derive(Template, WebTemplate)]
#[template(path = "index.html")]
struct IndexTemplate<'a> {
    name: &'a str,
}

// Automatically implements IntoResponse for Axum
async fn handler() -> IndexTemplate<'static> {
    IndexTemplate { name: "World" }
}
```

**Source**: https://askama.readthedocs.io/en/stable/frameworks.html

### Recommended Error Handling
```rust
// Use thiserror + displaydoc for clean error types
use thiserror::Error;
use displaydoc::Display;

#[derive(Debug, Error)]
enum AppError {
    #[error("could not render template: {0}")]
    Render(#[from] askama::Error),
    #[error("resource not found")]
    NotFound,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::Render(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::NotFound => StatusCode::NOT_FOUND,
        };
        (status, self.to_string()).into_response()
    }
}
```

**Source**: https://askama.readthedocs.io/en/stable/frameworks.html#axum

---

## 4. Rust Testing Patterns for Axum

### Unit Testing Handlers (Direct Call)
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_health_handler() {
        let result = health_handler().await;
        assert_eq!(result, "healthy");
    }
    
    #[tokio::test]
    async fn test_with_state() {
        let state = AppState { /* ... */ };
        let result = handler(State(state)).await;
        assert!(result.is_ok());
    }
}
```

### HTTP Testing with tower::ServiceExt::oneshot
```rust
use axum::{routing::get, Router};
use tower::ServiceExt;

#[tokio::test]
async fn test_http_route() {
    let app = Router::new()
        .route("/", get(root_handler));
    
    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::OK);
}
```

**Source**: https://deepwiki.com/tokio-rs/axum/12.2-testing-strategies

### WebSocket Testing - Integration Style
```rust
use tokio_tungstenite::connect_async;

#[tokio::test]
async fn integration_test() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    
    tokio::spawn(axum::serve(listener, app()));
    
    let (mut socket, _response) = 
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    
    socket.send(Message::text("hello")).await.unwrap();
    let msg = socket.next().await.unwrap().unwrap();
    assert_eq!(msg.into_text().unwrap(), "You said: hello");
}
```

**Source**: https://github.com/tokio-rs/axum/blob/main/examples/testing-websockets/src/main.rs

### WebSocket Testing - Unit Style (Mocking)
```rust
use futures_channel::mpsc;
use futures_util::{SinkExt, StreamExt};

#[tokio::test]
async fn unit_test() {
    // Use futures channels (implement Sink + Stream traits)
    let (socket_write, mut test_rx) = mpsc::channel(1024);
    let (mut test_tx, socket_read) = mpsc::channel(1024);
    
    tokio::spawn(handle_socket(socket_write, socket_read));
    
    test_tx.send(Ok(Message::Text("foo".into()))).await.unwrap();
    let msg = test_rx.next().await.unwrap();
    assert_eq!(msg, Message::Text("You said: foo".into()));
}

// Handler using generic Sink + Stream bounds
async fn handle_socket<W, R>(mut write: W, mut read: R)
where
    W: Sink<Message> + Unpin,
    R: Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    while let Some(Ok(msg)) = read.next().await {
        // ...
    }
}
```

**Source**: https://github.com/tokio-rs/axum/blob/main/examples/testing-websockets/src/main.rs (lines 87-120)

### Testing Best Practices
1. **Use random ports**: `TcpListener::bind("127.0.0.1:0")` for integration tests
2. **Separate concerns**: Handlers that accept generic `Sink+Stream` are unit-testable
3. **Test HTTP routes separately**: Use `tower::ServiceExt::oneshot()` for fast HTTP tests
4. **Use `futures_channel::mpsc`** for WebSocket unit tests (implements Sink/Stream)

**Source**: https://deepwiki.com/tokio-rs/axum/12.2-testing-strategies

---

## 5. Gotchas and Important Notes

### WebSocket Testing Limitation (Open Issue)
As of 2026-03-11, there's an **open issue** (#3688) requesting better WebSocket testing support. Currently `WebSocketUpgrade` requires `hyper::upgrade::OnUpgrade` which has no public constructor.

**Workaround**: Use integration tests (run real server) or mock with generic `Sink+Stream`.

**Source**: https://github.com/tokio-rs/axum/issues/3688

### Middleware Order with ServiceBuilder
```rust
use tower::ServiceBuilder;

let app = Router::new()
    .layer(ServiceBuilder::new()
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        .layer(CorsLayer::new())
    );
```

Middleware executes in **top-to-bottom** order with ServiceBuilder.

**Source**: https://context7.com/tokio-rs/axum/llms.txt

### askama_web Limitations
- Does NOT handle custom/stylized error messages
- Best for apps where templates won't have rendering errors
- Falls back to plain error responses on render failure

**Source**: https://askama.readthedocs.io/en/stable/frameworks.html#simplified-alternative

### Shared State in WebSocket Handlers
For broadcast patterns, use `tokio::sync::broadcast`:
```rust
use tokio::sync::broadcast;

let (tx, _rx) = broadcast::channel(100);

// In handler
let rx = tx.subscribe();
let tx_clone = tx.clone();
tokio::spawn(async move {
    // receive from rx, send to socket
    // broadcast to tx_clone
});
```

**Source**: https://context7.com/tokio-rs/axum/llms.txt

---

## 6. Recommended Dependencies for Testing

```toml
[dev-dependencies]
tokio = { version = "1", features = ["full"] }
tower = "0.5"  # For ServiceExt::oneshot
http-body-util = "0.1"  # For Body::empty()
tokio-tungstenite = "0.28"  # WebSocket client for integration tests
futures-util = { version = "0.3", features = ["sink", "std"] }
futures-channel = "0.3"  # For unit testing WebSockets

# Optional helpers
axum-test = "15"  # Higher-level test helpers (alternative to raw tower)
```

## Reconnaissance Findings (Atlas, pre-Task-1-2)

### Module Layout
- `src/main.rs` — 180-line CLI entrypoint. Uses clap with `#[derive(Parser)]` on a `Cli` struct that holds a `Command` enum. Current variants: `Login`, `Chat`. This is **exactly** where `Web` subcommand must be added.
- `src/lib.rs` — exposes 7 public modules: `agent`, `auth`, `compact`, `events`, `skills`, `store`, `tools`. A new `web` module must be added here for `tests/` access.
- `src/agent.rs` — 1006 lines. The `chat_loop` async fn is the main runtime entry. Accepts `model_override`, `resume_session_id`, `store`, `emitter`, `run_mode`, `compact_config`. This is the extraction target for Task 3.
- `src/events.rs` — 146 lines. `AgentEvent` enum (7 variants: TransportSelected, TransportFallback, TokenRefreshed, TurnStart, TurnComplete, TurnError, StatusReport, HistoryCompacted). mpsc-based `EventEmitter`/`EventReceiver` channel. `event_channel()` factory.
- `src/store/mod.rs` — 262 lines. `Store` wraps `rusqlite::Connection`. `SharedStore = Arc<Mutex<Store>>`. `store_run<F, T>()` async fn runs blocking store ops in `spawn_blocking` — has a comment explicitly saying "Use from async `axum` handlers". This is the exact pattern web handlers will use.
- `src/store/session.rs` — 223 lines. `Session`, `SessionStatus`, `NewSession`. Key methods: `create`, `get`, `get_latest`, `list`, `update_status`, `update_title`, `update_settings`. `settings: Option<String>` field is available for web/runtime config.
- `src/store/turn.rs` — 128 lines. `Turn`, `NewTurn`. Key methods: `append` (atomic turn_number via subquery), `list_for_session`. Turn kinds: "message", "function_call", "function_call_output".
- `src/store/schema.rs` — 116 lines. Two migrations (v1: sessions/turns/contexts; v2: settings/artifacts/rate_limits).
- `src/auth/openai.rs` lines 267–371 — **THE EXISTING AXUM SERVER PATTERN**. `axum::Router::new().route("/auth/callback", axum::routing::get(handler))`. Handler uses `axum::extract::Query`, returns `(StatusCode, Html<String>)`. `axum::serve(listener, app).await` in a spawned task. This is the **exact template** for the web scaffold.
- `src/compact.rs` — 355 lines. `CompactConfig`, `maybe_compact` async fn. Unit tests use `Store::open_memory()` and `event_channel()`.

### CLI Structure (to extend)
- `clap` v4.5 with `derive` feature. `#[derive(Debug, Parser)]` on `Cli`, `#[derive(Debug, Subcommand)]` on `Command`.
- `Command::Chat` has 8 fields (model, last, resume, yolo, autonomous, max_turns, prompt, compact_threshold).
- `Command::Login` has a nested `LoginProvider` subcommand.
- New `Command::Web` variant needed with fields: `--bind` (default 127.0.0.1:3210), `--port` (optional override), `--open` (bool for auto-open).
- The `async fn run()` match on `cli.command` is the insertion point.

### Existing HTTP/WS Patterns
- **axum 0.7**: Already in `Cargo.toml`. Used in `src/auth/openai.rs` for OAuth callback.
- **tokio-tungstenite 0.24**: Already in `Cargo.toml`. Used in `src/agent.rs` for OpenAI WS transport.
- **http 1**: Already in `Cargo.toml`.
- **`axum::Router::new().route(path, method(handler))`** — exact pattern from auth/openai.rs:288–327.
- **`axum::extract::Query<T>`** — query param extraction used in auth callback.
- **`axum::response::Html(String)`** — response type used for callback HTML.
- **`axum::http::StatusCode`** — explicit status codes.
- **`tokio::net::TcpListener::bind()`** — async TCP binding pattern (line 329 of auth/openai.rs).
- **`tokio::spawn(async move { axum::serve(...).await })`** — background server pattern (line 334).

### Store Integration for Web
- `store_run(&shared, |s| Session::list(s, 20))` — async store access pattern (tests/store_concurrency.rs:15).
- `SharedStore` type = `Arc<Mutex<Store>>` (src/store/mod.rs:21).
- `store` crate has `open_memory()` for testing.
- The `store_run` helper has an explicit comment for axum handler usage.

### Test Patterns to Mirror
- **6 integration test files** in `tests/`: `auth_backends.rs`, `event_pipeline.rs`, `session_lifecycle.rs`, `sse_parsing.rs`, `store_concurrency.rs`, plus `harness/mod.rs`.
- **`tests/harness/mod.rs`** — the definitive test infrastructure. Provides: `TestContext` (store + event channel), `SessionBuilder` (fluent, model/provider/auto-create), `TurnBuilder` (fluent, session_id/role/content/model/tokens), `EventCollector` (background thread, `collect()` returns `Vec<AgentEvent>`).
- Event pipeline tests: `#[test]` + `EventCollector` + `drop(emitter)` pattern (tests/event_pipeline.rs).
- Concurrency tests: `#[tokio::test]` + `SharedStore` + `store::store_run` pattern (tests/store_concurrency.rs).
- SSE parsing tests: pure function tests, deterministic payloads (tests/sse_parsing.rs).
- Store round-trips: `Store::open_memory()` + builder pattern (tests/session_lifecycle.rs).

### Where `kley web` Fits
1. `src/main.rs` line 17: Add `Web` to `Command` enum via `#[derive(Debug, Subcommand)]`.
2. `src/main.rs` line 79: Add `Command::Web` arm in the `run()` match block, spawning the web server.
3. Create `src/web/mod.rs` as the module root.
4. Create `src/web/router.rs` for the Axum `Router` composition.
5. Create `src/web/config.rs` for bind address, port, default host.
6. Create `src/web/ui.rs` for the minimal HTML shell.
7. Route handlers in `src/web/handlers/` or inline in router.
8. `src/lib.rs`: add `pub mod web;`.
9. New tests in `tests/web_scaffold.rs` (or `tests/web/`) mirroring `tests/event_pipeline.rs` style.

### Dependencies Needed (not yet present)
- `askama` / `askama-axum` — for template rendering (Task 1 references this).
- `tower` / `tower-http` — for static file serving and CORS (later tasks).
- `tokio-tungstenite` is already present — for the `/ws` WebSocket handler.
- `axum` is already present.
- No `playwright` or Node tooling yet — that comes in Task 9.

### Event Seams for Tasks 5–6
- `AgentEvent` enum (src/events.rs:12) is the existing runtime event vocabulary.
- `EventEmitter::emit()` is non-blocking mpsc send.
- `EventReceiver::recv_blocking()` blocks on mpsc receive.
- The web layer will need to bridge this mpsc channel to WebSocket frames.
- No async event stream yet — the channel is sync mpsc.

### Key Structural Points for Runtime Extraction (Task 3)
- `chat_loop` in `src/agent.rs` (line 126) owns the stdin read loop.
- The inner `loop` (line 200) is the turn processing — this is the extractable core.
- `RunMode` enum (Interactive/Autonomous) controls stdin vs autonomous input.
- The CLI print/display logic in `main.rs::print_event()` (lines 153–179) is the "thin adapter" the plan references.
- Tool execution is inside the turn loop (lines 286–353) — persists turn, executes tool, re-appends output.
- The web adapter must NOT depend on stdin/io — that's the seam.

## Bindery Repository Survey (2026-03-18)

### 1. File Map — Exact Bindery Paths

| Purpose | Path |
|---|---|
| Primary web shell/template | `/home/zack/git/Bindery/bindery/templates/index.html` |
| Icon asset | `/home/zack/git/Bindery/assets/bindery-icon.svg` |
| Web router (route composition) | `/home/zack/git/Bindery/bindery/src/web/router.rs` |
| UI serving (Askama template) | `/home/zack/git/Bindery/bindery/src/web/ui.rs` |
| Real WebSocket bridge | `/home/zack/git/Bindery/bindery/src/web/ws.rs` |
| Mock WebSocket (deterministic sequence) | `/home/zack/git/Bindery/bindery/src/web/mock.rs` |
| App config (TOML loading) | `/home/zack/git/Bindery/bindery/src/config.rs` |
| Web module entry | `/home/zack/git/Bindery/bindery/src/web/mod.rs` |
| RPC types (naming reference only) | `/home/zack/git/Bindery/packages/coding-agent/src/modes/rpc/rpc-types.ts` |
| RPC client (spawn/process pattern) | `/home/zack/git/Bindery/packages/coding-agent/src/modes/rpc/rpc-client.ts` |
| RPC mode (stdin/stdout handler) | `/home/zack/git/Bindery/packages/coding-agent/src/modes/rpc/rpc-mode.ts` |

### 2. Template Structure (2483 lines — `index.html`)

The template is a single-file HTML shell served via Askama. Key structural elements:

- **Head**: Tailwind CSS v4 CDN (`@tailwindcss/browser@4`), HTMX v2 + WS extension, Google Fonts (Outfit + Fira Code), inline `@theme` block defining design tokens (`--color-bg`, `--color-bg1`, `--color-bdr`, `--color-txt`, `--color-purple`, etc.)
- **Inline CSS**: Scrollbar styling, keyframe animations (pulse, fadeIn), stream row grid layout, inspector drawer, composer form, flame/timeline bars, responsive breakpoints at 980px and 760px
- **Top bar** (`<header>`): session name, model tag, connection pill, top-controls buttons
- **Status strip**: model label, context-meter bar, token counters, tools count, streaming badge
- **Flame/timeline section**: relative-positioned timeline bars (`#flame-bars`), time axis labels
- **Feed controls**: filter chips (All, Messages, Tools, Agent, UI), visible count
- **Stream/transcript** (`#stream`): stream rows with grid columns (56px | 12px | minmax(0,1fr)), role-colored left borders and dot rails, per-kind CSS rules for assistant/user/agent/tool/response/ui/event
- **Inspector drawer** (right side, 360px): inspector shell with meta grid, block preview, JSON renderer, event log
- **Dialog panel**: overlay for picker modals
- **Mock presets section** (`#mock-presets`): hidden by default, shows demo preset buttons
- **Composer** (`#prompt-form`): file input (hidden), attachment chips, textarea (`#prompt-input`), attach button, task-start/task-done buttons, submit (send), abort
- **Client JS**: 2000+ lines of vanilla JS — WebSocket connection management, event rendering, stream row creation, inspector population, attachment handling, timeline management, filter logic

### 3. Shell Elements — In-Scope vs. Excluded

**SAFE TO ADAPT (in-scope per plan):**
- Stream row DOM structure: `data-kind`, `.stream-row`, `.stream-dot`, `.stream-rail`, `.stream-main`, `.stream-preview`, `.stream-body`
- Status strip layout: model/context/token/tool pill pairs
- Streaming badge (`#streaming-badge` with `.idle`/`.running` classes)
- Inspector drawer shell (`.inspector-drawer`, `.inspector-shell`, `.inspector-block`)
- Composer form (`.composer-form`, `.composer-input-wrap`, `#prompt-input`)
- Connection pill (`#conn-pill` with `data-state="open|connecting|closed"`)
- Feed filter chips (`.filter-chip`)
- Session name display (`#session-name`)
- Design token palette (CSS vars: bg, bg1, txt, purple, rose, green, amber, red, cyan, lime, etc.)
- Timeline/flame bar CSS (`.flame-bar`) — optional enhancement
- Panel kicker label pattern (`.panel-kicker`)
- Status inline pair pattern (`.status-pair`)

**EXPLICITLY OUT OF SCOPE (NOT to port):**
- `#btn-fork-picker` — fork/session-tree feature
- `#btn-session-picker` — session-switching UI (kley uses session.list/load commands)
- `#btn-model-picker` — model switching UI
- `#btn-task-start`, `#btn-task-complete` — task session lifecycle UI
- `#mock-presets` section — demo/mock presets (kley uses its own mock socket)
- Image attachment flow (file input, drag-drop, chip rendering, media cards) — kley plan excludes image uploads
- Extension UI request handling (`.extension_ui_request`) — Bindery extension runtime not in scope
- HTMX (`htmx.org`, `htmx-ext-ws`) — kley will use vanilla JS WebSocket client
- `binderyMeta()` payload unwrapping — Bindery-specific event wrapping (kley events are flat)

### 4. Mock WebSocket — Deterministic Sequencing Pattern

The mock (`mock.rs`, 1044 lines) is the primary reference for Task 2's deterministic mock socket.

**Key patterns:**

```
play_sequence(socket, events)  // drives timed event emission
  → for each MockEvent { sleep(delay); send_json(socket, payload) }
```

**Boot sequence** emits (delay in ms):
1. `extension_ui_request` (setTitle, 90ms)
2. `extension_ui_request` (notify, 80ms)
3. `extension_ui_request` (setWidget, 80ms)
4. `message_start` (welcome assistant, 90ms)
5. `message_end` (welcome assistant, 110ms)

**Prompt sequence** (per prompt) emits:
1. `agent_start` (70ms)
2. `turn_start` (80ms)
3. `extension_ui_request` (notify, 60ms)
4. `extension_ui_request` (setWidget, 60ms)
5. `message_start` (user, 60ms)
6. `message_end` (user, 55ms)
7. `message_start` (assistant, 70ms)
8. `tool_execution_start` (read, 90ms)
9. `tool_execution_end` (read, 120ms)
10. `tool_execution_start` (grep, 90ms)
11. `tool_execution_end` (grep, 100ms)
12. `extension_ui_request` (notify, 70ms)
13. `extension_ui_request` (setWidget, 80ms)
14. `tool_execution_start` (cargo_check, 85ms)
15. `tool_execution_end` (cargo_check, 120ms)
16. `message_update` (progress text, 130ms) — this is the streaming delta pattern
17. `extension_ui_request` (notify, 75ms)
18. `extension_ui_request` (setWidget, 80ms)
19. `message_end` (final text, 140ms)
20. `model_select` (70ms)
21. `turn_end` (80ms)
22. `agent_end` (65ms)

**Pattern to adapt for kley**: The `MockEvent { delay: Duration, payload: Value }` struct and `play_sequence()` helper are directly portable. The event shapes need to map to kley's own protocol (turn.started → message.started → message.delta → message.completed → tool.started → tool.completed → turn.completed).

**Key naming to borrow from rpc-types.ts without copying:**
- `turn.start` / `turn.end` (kley: `turn.started` / `turn.completed`)
- `message_start` / `message_update` / `message_end` (kley: `message.started` / `message.delta` / `message.completed`)
- `tool_execution_start` / `tool_execution_end` (kley: `tool.started` / `tool.completed`)
- `model_select` (kley: `transport.selected`)
- `agent_start` / `agent_end` (omit — kley has no sub-agent concept)
- `extension_ui_request` methods: `notify`, `setWidget` → kley: `status.report`
- `response` envelope: `{ type: "response", command, success, data?, error? }` → kley: `response.ok` / `response.error`

### 5. Protocol Naming Inspiration vs. Out-of-Scope Commands

**From rpc-types.ts — borrow event/correlation naming:**
```
Correlation fields (kley should include):
  - session_id, turn_id, message_id, tool_call_id, event_id, ts, request_id

UiEvent naming map (Bindery → kley scope):
  Bindery: agent_start/agent_end     → kley: OMIT (no sub-agent)
  Bindery: turn_start/turn_end      → kley: turn.started / turn.completed
  Bindery: message_start/update/end → kley: message.started / message.delta / message.completed
  Bindery: tool_execution_start/end  → kley: tool.started / tool.completed
  Bindery: model_select              → kley: transport.selected (per plan)
  Bindery: extension_ui_request     → kley: status.report (simplified)
  Bindery: response (envelope)       → kley: response.ok / response.error
```

**From rpc-types.ts — DO NOT port (out of scope for kley):**
- `start_task_session`, `complete_task_session` — task sessions
- `fork`, `get_fork_messages` — fork/session tree
- `new_session` with `parentSession` — session hierarchy
- `cycle_model`, `get_available_models` — model switching UI
- `set_thinking_level`, `cycle_thinking_level` — thinking levels
- `compact`, `set_auto_compaction`, `export_html` — compaction
- `bash`, `abort_bash` — direct bash via protocol
- `get_commands`, slash commands — extension command palette
- `extension_ui_request` variants: `select`, `confirm`, `input`, `editor`, `setStatus`, `setTitle`, `set_editor_text` — extension runtime UI

### 6. Real WebSocket Bridge — Architecture Pattern

`ws.rs` shows the bridge pattern: browser ↔ WebSocket ↔ Axum handler ↔ spawned subprocess (RpcClient) ↔ JSON lines on process stdin/stdout.

For kley, the architecture differs: the bridge goes browser ↔ WebSocket ↔ Axum handler ↔ runtime manager ↔ in-process runtime (no subprocess). The `handle_socket()` pattern of `tokio::select! { msg = socket.recv() => {...}, event = client.events.recv() => {...} }` is the right async pattern to adapt.

### 7. Config Loading

`config.rs` uses TOML deserialization with `${VAR}` placeholder expansion for env vars. kley's config can follow a similar pattern.

### 8. Icon

`bindery-icon.svg` is a 3-column book SVG. Plan says to copy it as-is to kley's `static/` directory.

# Bindery-UI Integration Learnings

## Runtime Architecture (Tasks 3, 4, 5, 6)

### The Core Agent Loop — `src/agent.rs`

**Central function**: `chat_loop()` at line 126.
Signature:
```rust
pub async fn chat_loop(
    model_override: Option<&str>,
    resume_session_id: Option<&str>,
    store: &Store,
    events: EventEmitter,
    run_mode: RunMode,
    compact_config: CompactConfig,
) -> Result<()>
```

This function IS the runtime. It contains:
1. Session resolve/create (lines 155-172)
2. History load from turns (lines 175-180)
3. **Outer input loop** (lines 200-438): stdin → user turn persist → TurnStart emit → inner loop → TurnComplete/TurnError emit → assistant turn persist
4. **Inner tool-call loop** (lines 250-359): API call → handle TurnResult::Text (break) or TurnResult::ToolCalls (execute each tool, loop back)
5. Autonomous re-injection (lines 413-437)

**Critical finding**: The loop has NO cancellation/abort mechanism. The only exit paths are:
- Ctrl+D (EOF on stdin, line 210) → sets `SessionStatus::Completed`, breaks
- Autonomous mode: 3 consecutive errors (line 418) or `remaining_turns == 0` (line 429)
- Any `Err(err)` propagated as `anyhow::Result`

**`SessionStatus::Aborted`** is defined in `src/store/session.rs` (lines 28, 38, 53) but is NEVER written by the runtime. This is the exact gap Task 6's `turn.abort` must fill.

### Prompt Submit — Current Location

- **CLI path**: `stdin.lock().read_line()` at `src/agent.rs:209` → `Turn::append()` at line 225 with `kind: "message"`, role `"user"` → history push at line 238.
- **Extraction seam already visible**: `pending_input.take()` at line 202 is where autonomous-mode prompts are injected. Replace this pattern with a `Receiver<String>` to decouple from stdin.

### Tool Lifecycle — Current Location

Tool calls are handled in `src/agent.rs:287-353`:
1. Line 291-307: Persist function_call turn (`kind: "function_call"`, content JSON with `call_id`, `name`, `arguments`)
2. Line 310-320: Execute via `registry.get(name).execute(args)` 
3. Line 328-339: Persist function_call_output turn (`kind: "function_call_output"`, role `"tool"`)
4. Lines 342-352: Add both to `history` for next API call

**Gap for Task 5**: No structured events emitted for tool lifecycle. `ToolCall` struct (line 41) has `call_id`, `name`, `arguments` — these can be the basis for `tool.started`/`tool.completed` events. Currently just `eprintln!()` at lines 288, 322-325.

### Streaming Deltas — Current Location

- `src/agent.rs:676-681` (WS path): `response.output_text.delta` → `print!("{delta}")`, `stdout().flush()`
- `src/agent.rs:854-861` (SSE path): same pattern via `process_openai_sse_block_with_tools()`
- `src/agent.rs:980-998` (ZAI path): `process_zai_sse_line()` → `print!()`, `stdout().flush()`
- **Gap**: `full_response.push_str(&delta)` accumulates the text (used for persistence at line 379), but NO structured events for `message.delta` or `message.completed`.

### Session Persistence — Current Location

| Concern | File | Key Symbols |
|---------|------|-------------|
| Store wrapper + async | `src/store/mod.rs` | `Store`, `SharedStore = Arc<Mutex<Store>>`, `store_run()` |
| Session CRUD | `src/store/session.rs` | `Session::create/get/get_latest/list/update_status/update_title/update_settings` |
| Turn append/list | `src/store/turn.rs` | `Turn::append()` (atomic turn_number via subquery), `Turn::list_for_session()` |
| Schema | `src/store/schema.rs` | `sessions`, `turns` (with `kind` column: `"message"`\|`"function_call"`\|`"function_call_output"`), `contexts`, `artifacts`, `rate_limits` tables |
| Settings blob | `src/store/session.rs:71` | `Session::settings: Option<String>` — freeform JSON for resume config |

**Key**: `Turn::append()` assigns `turn_number` atomically via subquery (line 50 of `turn.rs`) — avoids TOCTOU races.

### Event Pipeline — Current Location

`src/events.rs`:
- `AgentEvent` enum (lines 12-41): Has `TransportSelected`, `TransportFallback`, `TokenRefreshed`, `TurnStart`, `TurnComplete`, `TurnError`, `StatusReport`, `HistoryCompacted`.
- **Missing for Tasks 5/6**: `MessageDelta`, `MessageStarted`, `MessageComplete`, `ToolStarted`, `ToolComplete`, `TurnFailed` (distinct from `TurnError`).
- `EventEmitter`/`EventReceiver` (lines 103-110): Cloneable emitter backed by `mpsc::Sender`.
- `event_channel()` (line 113): Constructor.
- `EventReceiver::recv_blocking()` (line 127): Blocking receive — used in CLI event thread.
- `EventReceiver::try_recv()` (line 133): Non-blocking.
- `EventReceiver::drain()` (line 139): Drain all pending.

**Transport enum** (lines 43-47): `Transport::WebSocket`, `Transport::Sse` — already has the two transport types the protocol needs.

### CLI Adapter — `src/main.rs`

The CLI adapter is thin:
1. Parse CLI args (line 80)
2. Create `Store::open()` (line 111)
3. Create `event_channel()` (line 112)
4. Spawn event printing thread (lines 115-119) — consumes `EventReceiver` with `recv_blocking()` + `print_event()`
5. Determine session_id (`resume`, `last`, or None) (lines 122-128)
6. Call `kley::agent::chat_loop(...)` (lines 135-143)
7. Join event thread (line 145)

**Extraction seam**: After Task 3, `chat_loop` becomes a runtime function. `main.rs` becomes a thin adapter that passes a channel receiver for prompts and handles events for terminal output.

### Concurrency Helpers — `src/store/mod.rs`

- `SharedStore` type alias (line 21): `Arc<Mutex<Store>>` — `rusqlite::Connection` is `!Send`, so mutex required.
- `store_run()` (lines 69-83): `async fn` that runs blocking closures on the Tokio blocking thread pool. Returns `Result<T>`.
- Used by: tests (`tests/store_concurrency.rs`) — verified safe for 10 concurrent writes and 20 concurrent reads.

### Existing Tests

| Test File | What It Validates | Relevant For |
|-----------|-------------------|--------------|
| `tests/event_pipeline.rs` | Event channel ordering, close-on-drop, drain, emit-after-close | Task 5 event expansion |
| `tests/session_lifecycle.rs` | Session CRUD, turn ordering/numbering, title/setting persistence, resume reconstruction | Task 3 runtime extraction |
| `tests/store_concurrency.rs` | 10 concurrent writes, 20 concurrent reads, sequential create-then-read | Task 4 runtime manager |
| `tests/sse_parsing.rs` | Delta accumulation, completion detection, error propagation, malformed input | Task 5 structured events |
| `tests/harness/mod.rs` | `TestContext`, `SessionBuilder`, `TurnBuilder`, `EventCollector` | All integration tests |
| `src/store/mod.rs` (internal tests) | `open_memory`, session/turn round-trip, `get_latest`, `store_run_async` | Task 4 |
| `src/compact.rs` (internal tests) | `estimate_history_chars`, `needs_compaction`, keep-recent logic | Task 3 |

### Cargo Dependencies Already Available

`Cargo.toml` already includes:
- `axum = "0.7"` — for web server (Task 1, 6)
- `tokio` with `"full"` — async runtime (Tasks 1, 4, 6)
- `http = "1"` — HTTP types (Task 6)
- `tokio-tungstenite` — WebSocket (already used for OpenAI WS transport, Task 6)
- `futures-util` — already in use

## Extraction Seams for Reusable Runtime (Task 3)

### What Must Move to `src/runtime/`
1. The `chat_loop()` function, signature modified to:
   - Accept `prompt_rx: Receiver<String>` instead of stdin
   - Return a result type that distinguishes normal/complete/abort/error
   - Emit ALL events (not just TurnStart/TurnComplete/TurnError)
2. The transport functions: `send_openai_ws()`, `send_openai_sse()`, `send_zai_sse()`
3. `process_openai_sse_block_with_tools()`, `process_zai_sse_line()`, `process_openai_sse_block()`
4. `history_items_from_turns()`, `messages_from_history()`, `history_from_turns()`
5. `ToolCall`, `TurnResult` types

### What Stays Out (CLI Adapter)
1. `main.rs` — owns CLI argument parsing and store/event_channel creation
2. `print_event()` function — terminal-specific rendering
3. stdin input collection
4. `RunMode` resolution from CLI flags

### What Stays in lib.rs
- `compact.rs` — already separate, already async, already takes `EventEmitter`
- `tools/mod.rs` — already separate, already `Send + Sync`
- `auth/mod.rs` — already separate
- `events.rs` — already separate

### Abort Mechanism to Add
The runtime needs a cancellation channel. Candidates:
1. `tokio::sync::watch` — for broadcast of abort signal
2. `tokio::sync::oneshot` — for single-shot abort per turn
3. Extend `chat_loop` to take `abort_rx: watch::Receiver<bool>` checked at key yield points

## Active-Turn Replay Buffer (Task 4)
Currently: NO in-memory replay buffer. Active assistant text is accumulated in `full_response: String` (e.g. `src/agent.rs:659`) but never buffered for recovery. On reconnect, the only source is SQLite via `Turn::list_for_session()`.

**Task 4 must add**: A `HashMap<session_id, ReplayBuffer>` in a runtime manager that accumulates `message.delta` events in-memory. Buffer cleared on `TurnComplete` or persisted turn append.

## Task 1 implementation notes (2026-03-18)

- `kley web` now fits the existing CLI pattern by adding `Command::Web` in `src/main.rs` and delegating to `kley::web::serve(...)`, which keeps `chat` and `login` untouched.
- The minimal same-origin scaffold lives in `src/web/` with `config.rs` for the default bind (`127.0.0.1:3210`), `router.rs` for `/` and `/healthz`, and `ui.rs` for an Askama-rendered shell.
- An inline Askama template in `src/web/ui.rs` avoids adding a `templates/` directory during the scaffold phase while still satisfying the requirement to serve HTML through Askama.
- The acceptance-test filter names from the plan are easiest to preserve with an integration test file `tests/web.rs` that wraps the tests in `mod web { ... }`, so `cargo test web::healthz_returns_ok` and `cargo test web::root_serves_html_shell` match exactly.
- Real-process verification confirmed the default bind succeeds on `127.0.0.1:3210`, `/healthz` returns plain `ok`, `/` responds with `text/html`, and a second `kley web` process fails cleanly with a bind error instead of panicking.

## Task 3 runtime extraction notes (2026-03-19)

- The reusable seam is now `SessionRuntime` in `src/runtime/session.rs`; it owns session resolve/create, history reconstruction, submit execution, tool-call persistence, and assistant persistence while keeping CLI IO outside.
- `src/agent.rs::chat_loop` is reduced to a thin adapter that reads stdin/autonomous prompts and calls `SessionRuntime::submit_prompt`, preserving existing turn/session persistence semantics.
- Runtime output streaming no longer writes directly to stdout in transport code; deltas are emitted through `RuntimeHooks::on_output_delta`, which the CLI adapter maps to terminal printing.
- A typed abort path now exists via `AbortResult` and `SubmitResult::Aborted`, and runtime writes `SessionStatus::Aborted` instead of relying on panic/unwind behavior.
- Existing helper APIs used by current tests (`history_from_turns`, `history_items_from_turns`, `process_openai_sse_block`, `process_zai_sse_line`) are re-exported through `src/agent.rs` from `src/runtime/`.

## Task 3 abort semantics fix (2026-03-19)

- Idle `abort_turn()` must return `AbortResult::NoActiveTurn` without setting `abort_requested`; otherwise the next `submit_prompt()` can incorrectly persist a user message and immediately return `SubmitResult::Aborted`.
- Runtime usability after an idle abort is now enforced by test: call idle abort, then submit prompt, and assert a normal completed turn with user+assistant persistence.

## Task 2 implementation notes (2026-03-19)

- Added `src/web/protocol.rs` with a kley-owned versioned browser schema: `PROTOCOL_VERSION = 1`, command variants `state.get`, `sessions.list`, `session.load`, `prompt.submit`, and `turn.abort`, plus response envelopes `response.ok` and `response.error` keyed by `request_id`.
- Added typed `UiEvent` variants for all Task 2 event names (`state.snapshot`, `turn.started`, `message.started`, `message.delta`, `message.completed`, `tool.started`, `tool.completed`, `turn.completed`, `turn.failed`, `status.report`, `transport.selected`, `transport.fallback`, `auth.token_refreshed`) with stable correlation fields present on relevant variants.
- Added deterministic mock websocket handling in `src/web/mock.rs` and routed it at `/ws` from `src/web/router.rs` using `routing::any`, keeping the path fully isolated from the real runtime manager work.
- Mock determinism uses fixed IDs (`evt-*`, `turn-mock-*`, `msg-mock-*`, `tool-mock-*`) and fixed timestamps (`2026-01-01T00:00:SSZ`) so tests can assert exact values and ordering.
- Prompt streaming path emits the required ordered turn/message lifecycle and supports deterministic tool lifecycle frames when the submitted prompt contains `tool`.
- Added integration websocket tests in `tests/web.rs` with random-port Axum server startup + `tokio_tungstenite` client connection for realistic frame assertions.
- Required checks passed: `cargo test web::ws_connect_receives_bootstrap_state`, `cargo test web::invalid_command_returns_error_without_disconnect`, and `cargo test web::mock_prompt_stream_emits_ordered_events`.

## Task 2 ordering fix note (2026-03-19)

- Fixed mock lifecycle ordering bug in `src/web/mock.rs`: for prompt flows that include tool activity, `tool.started` and `tool.completed` now emit before `turn.completed`.
- Added focused regression in `tests/web.rs` (`web::mock_tool_lifecycle_emits_before_turn_completed`) that asserts exact event order and verifies the same websocket connection remains reusable with a follow-up `state.get` request after the prompt stream completes.

## Task 4 runtime-manager integration notes (2026-03-19)

- Added `RuntimeManager` in `src/runtime/manager.rs` keyed by `session_id`, with explicit single-controller lease semantics and typed rejection (`AttachControllerError::SessionBusy`).
- The manager now owns per-session runtime metadata (`ManagedRuntime`, sourced from `sessions.settings`) and an in-memory `ActiveTurnReplay` buffer for in-flight assistant text.
- WebSocket handling moved to `src/web/ws.rs` and now attaches a controller on connect, rejects concurrent attach with a typed `response.error` (`code = "session_busy"`), and releases lease on disconnect without mutating session completion state.
- `state.snapshot` now includes both persisted `transcript` rows (SQLite via `Turn::list_for_session`) and `active_turn` replay content (memory via `RuntimeManager`) so reconnect can hydrate durable + in-flight state separately.
- Prompt flow persists completed turns to SQLite as before, but the in-flight path (`prompt` containing `hold-open`) intentionally keeps assistant output only in memory to validate reconnect replay without treating SQLite as the live event bus.

## Task 4 scope-correction note (2026-03-19)

- Corrected `RuntimeManager` from metadata-only tracking to owning executable per-session runtime workers (`RuntimeWorker`) that run prompts through extracted `SessionRuntime`.
- Removed direct turn persistence + fabricated delta/completion generation from `src/web/ws.rs::prompt.submit`; websocket prompt handling now delegates to `RuntimeManager::submit_prompt` and renders events from runtime-produced output.
- Replay buffer remains memory-backed and is now populated by runtime submit outcomes, while completed transcript replay remains sourced from SQLite.

## Task 4 provider/auth correctness fix (2026-03-19)

- `RuntimeWorker::resolved_auth` in `src/runtime/manager.rs` now preserves provider semantics: `provider = test` still uses deterministic in-memory auth, while non-test providers resolve through the existing `CredentialStore::open` + `auth::resolve_auth` path.
- Added strict provider consistency check (`resolved.provider == session provider`) so runtime manager surfaces a truthful mismatch error instead of silently falling back to fake test auth.

## Task 5 structured runtime events (2026-03-19)

- Expanded `AgentEvent` into structured turn/message/tool lifecycle events with explicit correlation fields (`session_id`, `turn_id`, `message_id`, `tool_call_id`) so consumers no longer infer relationships from row order.
- `SessionRuntime` now emits ordered lifecycle events directly (`turn.started`, `message.started`, streamed `message.delta`, `message.completed`, `turn.completed`/`turn.failed`) and includes explicit IDs in `SubmitResult`.
- Tool lifecycle now carries durable `tool_call_id` from provider response through both start and completion events; web assertions can verify same-ID round trip.
- `RuntimeManager` now captures the runtime event channel and returns ordered `AgentEvent` vectors with submit outcomes; replay state is updated from explicit `message.delta` events.
- `src/web/ws.rs` now maps runtime events to protocol `UiEvent`s instead of synthesizing assistant/tool lifecycle from DB writes, which keeps CLI and web adapters on the same event contract.
- CLI output parity is preserved by consuming structured events in `print_event`: assistant deltas stream to stdout, tool and turn lifecycle remain terminal-visible, and transport/auth/status stay typed.

## Task 6 websocket bridge notes (2026-03-19)

- `src/web/ws.rs` now drives the real `/ws` route with a `tokio::select!` loop over browser commands and per-session runtime broadcasts, so prompt streaming and command handling share one same-origin bridge instead of waiting for a completed submit.
- `src/runtime/manager.rs` now owns a per-session `broadcast::Sender<RuntimeEventEnvelope>` plus active prompt abort state, which lets reconnecting controllers re-subscribe to live runtime events without rebuilding lifecycle from persisted rows.
- Bootstrap/state hydration is centralized as `StateSnapshotData` in `src/web/protocol.rs`; both connect-time `state.snapshot` events and `state.get` responses now use the same concrete payload shape with `selected_session`, `sessions`, `transcript`, and `active_turn`.
- `turn.abort` now flips a real runtime abort flag carried into `SessionRuntime::new_with_abort_signal(...)`, and aborted turns emit runtime `TurnFailed` events that the web bridge maps straight into `turn.failed` frames.
- The deterministic test-provider path now special-cases `hold-open` and `abortable` prompts to stream in timed chunks, which keeps reconnect and abort integration tests exercising the real bridge/replay path instead of a synchronous one-shot response.

## Task 7 Bindery shell port notes (2026-03-19)

- Askama root rendering now uses a filesystem template (`templates/index.html`) instead of inline Rust source, which makes Bindery shell adaptation tractable while keeping same-binary serving.
- The Task 7 shell is intentionally stripped to scoped surfaces only: sidebar session list, transcript panel, composer, inspector/tool activity, and status pill.
- Deterministic selectors required by the plan are embedded directly as `data-testid` markers on shell anchors (`app-shell`, `session-list`, `transcript`, `composer`, `composer-submit`, `abort-button`, `tool-card`, `inspector-panel`, `status-pill`).
- Unsupported Bindery controls are omitted entirely from markup (no model/fork/session picker buttons, task controls, mock preset panel, or image attachment input), preventing dead-affordance regressions.
- The copied icon asset is served from a dedicated same-origin route (`/assets/bindery-icon.svg`) so the shell can reference a real static-like URL without introducing a bundler or separate frontend runtime.

## Task 8 core workspace wiring notes (2026-03-19)

- `templates/index.html` now includes a same-page websocket adapter that reads `data-ws-path` and `data-protocol-version` from the shell root, connects to `/ws`, and consumes only protocol frames already emitted by `src/web/ws.rs` (`state.snapshot`, `message.*`, `tool.*`, `turn.*`, `status.report`, `transport.*`, `auth.token_refreshed`, `response.*`).
- Hydration now treats `state.snapshot` as the single browser source of truth: session sidebar, transcript rows, selected session, and `active_turn` replay are rebuilt from that payload so refresh/reconnect does not assume an empty view.
- Session switching is now protocol-driven from sidebar clicks (`session.load`) and relies on the post-ack `state.snapshot` push to swap visible history while keeping session list state intact.
- Composer wiring now submits prompts through `prompt.submit` and appends streamed assistant output into the active message row using `message.started` + `message.delta` + `message.completed`; abort uses `turn.abort` and resets busy state on `turn.failed`/`turn.completed` so the same session remains reusable.
- Tool activity rendering now uses `tool.started`/`tool.completed` correlation by `tool_call_id`, creating/updating inspector cards without inferring by "latest row" heuristics.
- Added Task 8 verification tests in `tests/web.rs`: `web::prompt_submit_updates_transcript_and_tool_panel`, `web::session_load_switches_visible_history`, and `web::abort_keeps_session_reusable`, plus a full `cargo test web::` regression run.

## Task 9 Playwright coverage notes (2026-03-19)

- Added in-repo browser tooling with `package.json`, `package-lock.json`, `playwright.config.ts`, and `playwright/core-workspace.spec.ts` so browser regression coverage lives beside the Rust app instead of depending on external test harnesses.
- The Playwright `webServer` path uses `playwright/support/web-server.mjs`, which wipes a repo-local `.playwright/home` directory before launch and then starts `cargo run -- web --bind 127.0.0.1:3211`; this keeps browser acceptance deterministic by isolating the SQLite store through `HOME`.
- The existing `provider = "test"` runtime path is enough for primary acceptance: browser tests can drive the real `kley web` server and websocket bridge without live credentials while still covering streaming, tool events, abort, and reconnect behavior.
- Browser coverage now exercises the required selectors directly (`session-list`, `transcript`, `composer`, `composer-submit`, `tool-card`, `abort-button`, `inspector-panel`, `status-pill`) for page load, session rendering, transcript replay after reload, streamed assistant output, tool-card presence/expansion, abort, reconnect recovery, and unsupported-control absence.
- `templates/index.html` gained a minimal expandable tool-card rendering path using native `<details>` so the browser suite can verify interaction without introducing new unsupported UI flows or changing the websocket/runtime contract.
- Follow-up QA tightening: `playwright/core-workspace.spec.ts` now also exposes unsupported-control absence as its own named Playwright test so plan-level review can grep that behavior directly instead of only finding it embedded inside `core workspace parity`.
