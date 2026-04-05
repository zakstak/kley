use axum::Router;
use axum::body::Bytes;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::post;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kley::compact::CompactConfig;
use kley::events::event_channel;
use kley::lsp::{LspClient, LspClientError, LspClientFactory, LspManager, TestingServerState};
use kley::runtime::{RuntimeHooks, SessionRuntime, SubmitResult};
use kley::store::{Store, Turn};
use kley::test_openai::{self, ControlledResponse, TEST_MODEL};
use kley::tools::lsp::LspDiagnosticsTool;
use kley::tools::{ToolRegistry, default_registry};
use serde_json::{Value, json};

struct TestServer {
    addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_app(app: Router) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer { addr, task }
}

async fn controlled_openai_handler(body: Bytes) -> impl IntoResponse {
    let payload: Value = serde_json::from_slice(&body).unwrap_or_default();
    let prompt = payload
        .get("input")
        .and_then(Value::as_array)
        .and_then(|items| items.last())
        .and_then(|item| item.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let response =
        test_openai::parse_controlled_prompt(prompt).unwrap_or(ControlledResponse::Text {
            content: "No diagnostics found".to_string(),
        });

    let body = match response {
        ControlledResponse::ToolCall { name, arguments } => {
            test_openai::tool_call_sse(&name, &arguments)
        }
        ControlledResponse::Text { content } => test_openai::text_sse(&content),
    };

    ([(header::CONTENT_TYPE, "text/event-stream")], body)
}

fn function_call_outputs(store: &Store, session_id: &str) -> Vec<String> {
    Turn::list_for_session(store, session_id)
        .unwrap()
        .into_iter()
        .filter(|turn| turn.kind == "function_call_output")
        .map(|turn| {
            let payload: Value = serde_json::from_str(&turn.content).unwrap();
            payload
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

struct NoDiagnosticsClient;

impl LspClient for NoDiagnosticsClient {
    fn request(&self, _method: &str, _params: Value) -> Result<Value, LspClientError> {
        Ok(json!({ "items": [] }))
    }
}

struct CountingFactory {
    create_calls: AtomicUsize,
}

impl CountingFactory {
    fn new() -> Self {
        Self {
            create_calls: AtomicUsize::new(0),
        }
    }

    fn create_calls(&self) -> usize {
        self.create_calls.load(Ordering::Relaxed)
    }
}

impl LspClientFactory for CountingFactory {
    fn create(
        &self,
        _command: &[String],
        _workspace_root: &Path,
    ) -> Result<Arc<dyn LspClient>, String> {
        self.create_calls.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(NoDiagnosticsClient))
    }
}

struct MissingBinaryFactory {
    create_calls: AtomicUsize,
}

impl MissingBinaryFactory {
    fn new() -> Self {
        Self {
            create_calls: AtomicUsize::new(0),
        }
    }

    fn create_calls(&self) -> usize {
        self.create_calls.load(Ordering::Relaxed)
    }
}

impl LspClientFactory for MissingBinaryFactory {
    fn create(
        &self,
        command: &[String],
        _workspace_root: &Path,
    ) -> Result<Arc<dyn LspClient>, String> {
        self.create_calls.fetch_add(1, Ordering::Relaxed);
        let binary = command
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown-lsp".to_string());
        Err(format!("missing binary: {binary}"))
    }
}

#[tokio::test]
async fn runtime_executes_lsp_tools_via_session_manager() {
    let fixture = tempfile::tempdir().unwrap();
    let file_path = fixture.path().join("sample.rs");
    std::fs::write(&file_path, "fn main() {}\n").unwrap();

    let store = Store::open_memory().unwrap();
    let (events, _receiver) = event_channel();
    let server =
        spawn_app(Router::new().route("/responses", post(controlled_openai_handler))).await;

    let factory = Arc::new(CountingFactory::new());
    let manager = Arc::new(LspManager::with_test_factory(factory.clone()));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(LspDiagnosticsTool::new(
        fixture.path().to_path_buf(),
        "runtime-lsp-placeholder",
        manager.clone(),
    )));

    let mut runtime = SessionRuntime::new(
        &store,
        test_openai::auth(format!("http://{}", server.addr)),
        Some(TEST_MODEL),
        None,
        events,
        CompactConfig::default(),
        registry,
        "system".to_string(),
        RuntimeHooks::default(),
    )
    .unwrap();

    let args = json!({
        "file_path": file_path.display().to_string(),
        "severity": "all",
    });

    let result_one = runtime
        .submit_prompt(test_openai::controlled_tool_prompt(
            "lsp_diagnostics",
            args.clone(),
        ))
        .await
        .unwrap();
    let result_two = runtime
        .submit_prompt(test_openai::controlled_tool_prompt("lsp_diagnostics", args))
        .await
        .unwrap();

    assert!(matches!(result_one, SubmitResult::Completed { .. }));
    assert!(matches!(result_two, SubmitResult::Completed { .. }));

    let outputs = function_call_outputs(&store, runtime.session_id());
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0], "No diagnostics found");
    assert_eq!(outputs[1], "No diagnostics found");

    let workspace_root = fixture.path().to_path_buf();
    assert_eq!(factory.create_calls(), 1);
    assert_eq!(
        manager.lifecycle_state(runtime.session_id(), "rust-analyzer", &workspace_root),
        Some(TestingServerState::Ready)
    );
    assert_eq!(
        manager.lifecycle_state("runtime-lsp-placeholder", "rust-analyzer", &workspace_root),
        None
    );
}

#[tokio::test]
async fn runtime_returns_deterministic_lsp_missing_binary_error() {
    let fixture = tempfile::tempdir().unwrap();
    let file_path = fixture.path().join("missing.rs");
    std::fs::write(&file_path, "fn main() {}\n").unwrap();

    let store = Store::open_memory().unwrap();
    let (events, _receiver) = event_channel();
    let server =
        spawn_app(Router::new().route("/responses", post(controlled_openai_handler))).await;

    let factory = Arc::new(MissingBinaryFactory::new());
    let manager = Arc::new(LspManager::with_test_factory(factory.clone()));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(LspDiagnosticsTool::new(
        fixture.path().to_path_buf(),
        "runtime-lsp-placeholder",
        manager,
    )));

    let mut runtime = SessionRuntime::new(
        &store,
        test_openai::auth(format!("http://{}", server.addr)),
        Some(TEST_MODEL),
        None,
        events,
        CompactConfig::default(),
        registry,
        "system".to_string(),
        RuntimeHooks::default(),
    )
    .unwrap();

    let args = json!({
        "file_path": file_path.display().to_string(),
        "severity": "all",
    });

    let result_one = runtime
        .submit_prompt(test_openai::controlled_tool_prompt(
            "lsp_diagnostics",
            args.clone(),
        ))
        .await
        .unwrap();
    let result_two = runtime
        .submit_prompt(test_openai::controlled_tool_prompt("lsp_diagnostics", args))
        .await
        .unwrap();

    assert!(matches!(result_one, SubmitResult::Completed { .. }));
    assert!(matches!(result_two, SubmitResult::Completed { .. }));
    assert_eq!(factory.create_calls(), 1);

    let outputs = function_call_outputs(&store, runtime.session_id());
    assert_eq!(outputs.len(), 2);
    let expected = "Error: required lsp binary not found on PATH: rust-analyzer";
    assert_eq!(outputs[0], expected);
    assert_eq!(outputs[1], expected);
}

#[test]
fn default_registry_includes_lsp_rename_tools() {
    let reg = default_registry(std::env::temp_dir());
    assert!(reg.get("lsp_prepare_rename").is_some());
    assert!(reg.get("lsp_rename").is_some());
}
