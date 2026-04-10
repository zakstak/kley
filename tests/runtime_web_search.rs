use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use kley::compact::CompactConfig;
use kley::events::event_channel;
use kley::runtime::{RuntimeHooks, SessionRuntime, SubmitResult};
use kley::store::{SharedStore, Store, Turn};
use kley::test_openai::{self, ControlledResponse, TEST_MODEL};
use kley::tools::default_registry;
use kley::tools::web_search::{WebSearchCitationInput, WebSearchResult};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Mutex as AsyncMutex;

struct TestServer {
    addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedSearchRequest {
    authorization: Option<String>,
    body: Value,
}

#[derive(Clone, Default)]
struct FakeOpenAiState {
    recorded_requests: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Default)]
struct FakeTavilyState {
    recorded_requests: Arc<Mutex<Vec<RecordedSearchRequest>>>,
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

fn env_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => unsafe { std::env::set_var(self.key, previous) },
            None => unsafe { std::env::remove_var(self.key) },
        }
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

async fn fake_openai_handler(
    State(state): State<FakeOpenAiState>,
    body: Bytes,
) -> impl IntoResponse {
    let payload: Value = serde_json::from_slice(&body).unwrap_or_default();
    state
        .recorded_requests
        .lock()
        .unwrap()
        .push(payload.clone());

    let prompt = payload
        .get("input")
        .and_then(Value::as_array)
        .and_then(|items| items.last())
        .and_then(|item| item.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let response =
        test_openai::parse_controlled_prompt(prompt).unwrap_or(ControlledResponse::Text {
            content: "web_search handled".to_string(),
        });

    let body = match response {
        ControlledResponse::ToolCall { name, arguments } => {
            test_openai::tool_call_sse(&name, &arguments)
        }
        ControlledResponse::Text { content } => test_openai::text_sse(&content),
    };

    ([(header::CONTENT_TYPE, "text/event-stream")], body)
}

async fn fake_tavily_search_handler(
    State(state): State<FakeTavilyState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = serde_json::from_slice(&body).unwrap();
    state
        .recorded_requests
        .lock()
        .unwrap()
        .push(RecordedSearchRequest {
            authorization,
            body,
        });

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        tavily_response_body(),
    )
}

fn run_async_test<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn tavily_response_body() -> String {
    json!({
        "answer": "Runtime web search summary",
        "results": [
            {
                "title": "Rust Testing",
                "url": "https://example.com/rust-testing",
                "content": "Runtime result one.",
            },
            {
                "title": "Axum SSE",
                "url": "https://example.com/axum-sse",
                "content": "Runtime result two.",
            }
        ]
    })
    .to_string()
}

fn expected_web_search_output() -> String {
    WebSearchResult::ok(
        "runtime web search",
        Some("Runtime web search summary"),
        vec![
            WebSearchCitationInput {
                title: "Rust Testing".to_string(),
                url: "https://example.com/rust-testing".to_string(),
                snippet: Some("Runtime result one.".to_string()),
            },
            WebSearchCitationInput {
                title: "Axum SSE".to_string(),
                url: "https://example.com/axum-sse".to_string(),
                snippet: Some("Runtime result two.".to_string()),
            },
        ],
        2,
    )
    .unwrap()
    .to_json_string()
    .unwrap()
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

async fn run_runtime_prompts(
    shared_store: SharedStore,
    openai_base_url: String,
    prompts: Vec<String>,
) -> (String, Vec<SubmitResult>) {
    tokio::task::spawn_blocking(move || {
        let runtime_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (events, _receiver) = event_channel();
        let mut runtime = SessionRuntime::new_with_shared_store_and_abort_signal(
            shared_store,
            test_openai::auth(openai_base_url),
            Some(TEST_MODEL),
            None,
            events,
            CompactConfig::default(),
            default_registry(std::env::current_dir().unwrap()),
            "system".to_string(),
            RuntimeHooks::default(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .unwrap();

        let session_id = runtime.session_id().to_string();
        let results = prompts
            .into_iter()
            .map(|prompt| runtime_rt.block_on(runtime.submit_prompt(prompt)).unwrap())
            .collect();

        drop(runtime);
        (session_id, results)
    })
    .await
    .unwrap()
}

fn web_search_call_prompt() -> String {
    test_openai::controlled_tool_prompt(
        "web_search",
        json!({
            "query": "runtime web search",
            "max_results": 2,
        }),
    )
}

#[test]
fn runtime_executes_web_search_tool_via_session_manager() {
    run_async_test(async {
        let _env_lock = env_lock().lock().await;
        let tavily_state = FakeTavilyState::default();
        let tavily_server = spawn_app(
            Router::new()
                .route("/search", post(fake_tavily_search_handler))
                .with_state(tavily_state.clone()),
        )
        .await;
        let openai_server = spawn_app(
            Router::new()
                .route("/responses", post(fake_openai_handler))
                .with_state(FakeOpenAiState::default()),
        )
        .await;
        let _api_key = EnvVarGuard::set("TAVILY_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set(
            "TAVILY_API_BASE_URL",
            &format!("http://{}", tavily_server.addr),
        );

        let shared_store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
        let (session_id, results) = run_runtime_prompts(
            shared_store.clone(),
            format!("http://{}", openai_server.addr),
            vec![web_search_call_prompt(), web_search_call_prompt()],
        )
        .await;
        let mut results = results.into_iter();
        let result_one = results.next().unwrap();
        let result_two = results.next().unwrap();

        assert!(matches!(result_one, SubmitResult::Completed { .. }));
        assert!(matches!(result_two, SubmitResult::Completed { .. }));

        let expected_output = expected_web_search_output();
        let store_guard = shared_store.lock().unwrap();
        let outputs = function_call_outputs(&store_guard, &session_id);
        assert_eq!(outputs, vec![expected_output.clone(), expected_output]);

        assert_eq!(
            tavily_state.recorded_requests.lock().unwrap().clone(),
            vec![
                RecordedSearchRequest {
                    authorization: Some("Bearer test-key".to_string()),
                    body: json!({
                        "query": "runtime web search",
                        "search_depth": "basic",
                        "max_results": 2,
                        "include_answer": "basic",
                        "include_raw_content": false,
                    }),
                },
                RecordedSearchRequest {
                    authorization: Some("Bearer test-key".to_string()),
                    body: json!({
                        "query": "runtime web search",
                        "search_depth": "basic",
                        "max_results": 2,
                        "include_answer": "basic",
                        "include_raw_content": false,
                    }),
                },
            ]
        );
    });
}

#[test]
fn runtime_persists_web_search_function_call_output() {
    run_async_test(async {
        let _env_lock = env_lock().lock().await;
        let tavily_server = spawn_app(
            Router::new()
                .route("/search", post(fake_tavily_search_handler))
                .with_state(FakeTavilyState::default()),
        )
        .await;
        let openai_server = spawn_app(
            Router::new()
                .route("/responses", post(fake_openai_handler))
                .with_state(FakeOpenAiState::default()),
        )
        .await;
        let _api_key = EnvVarGuard::set("TAVILY_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set(
            "TAVILY_API_BASE_URL",
            &format!("http://{}", tavily_server.addr),
        );

        let shared_store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
        let (session_id, mut results) = run_runtime_prompts(
            shared_store.clone(),
            format!("http://{}", openai_server.addr),
            vec![web_search_call_prompt()],
        )
        .await;
        let result = results.remove(0);
        assert!(matches!(result, SubmitResult::Completed { .. }));

        let store_guard = shared_store.lock().unwrap();
        let turns = Turn::list_for_session(&store_guard, &session_id).unwrap();
        let output_turn = turns
            .iter()
            .rev()
            .find(|turn| turn.kind == "function_call_output")
            .expect("function_call_output turn expected");

        assert_eq!(output_turn.role, "tool");

        let payload: Value = serde_json::from_str(&output_turn.content).unwrap();
        let output = payload
            .get("output")
            .and_then(Value::as_str)
            .expect("stored tool output string expected");

        assert!(!payload
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .is_empty());
        assert_eq!(output, expected_web_search_output());
        assert_eq!(
            serde_json::from_str::<Value>(output).unwrap(),
            json!({
                "status": "ok",
                "query": "runtime web search",
                "summary": "Runtime web search summary",
                "citations": [
                    {
                        "index": 1,
                        "title": "Rust Testing",
                        "url": "https://example.com/rust-testing",
                        "snippet": "Runtime result one.",
                    },
                    {
                        "index": 2,
                        "title": "Axum SSE",
                        "url": "https://example.com/axum-sse",
                        "snippet": "Runtime result two.",
                    }
                ],
                "message": null,
            })
        );
    });
}

#[test]
fn runtime_includes_web_search_in_provider_tool_payload() {
    run_async_test(async {
        let _env_lock = env_lock().lock().await;
        let openai_state = FakeOpenAiState::default();
        let tavily_server = spawn_app(
            Router::new()
                .route("/search", post(fake_tavily_search_handler))
                .with_state(FakeTavilyState::default()),
        )
        .await;
        let openai_server = spawn_app(
            Router::new()
                .route("/responses", post(fake_openai_handler))
                .with_state(openai_state.clone()),
        )
        .await;
        let _api_key = EnvVarGuard::set("TAVILY_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set(
            "TAVILY_API_BASE_URL",
            &format!("http://{}", tavily_server.addr),
        );

        let shared_store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
        let (_session_id, mut results) = run_runtime_prompts(
            shared_store,
            format!("http://{}", openai_server.addr),
            vec![web_search_call_prompt()],
        )
        .await;
        let result = results.remove(0);
        assert!(matches!(result, SubmitResult::Completed { .. }));

        let requests = openai_state.recorded_requests.lock().unwrap().clone();
        assert!(
            requests.len() >= 2,
            "expected provider requests before and after tool execution"
        );

        for payload in requests {
            let tools = payload
                .get("tools")
                .and_then(Value::as_array)
                .expect("provider payload should include tools array");
            let web_search_tool = tools
                .iter()
                .find(|tool| tool.get("name") == Some(&json!("web_search")))
                .expect("provider payload should include web_search tool");

            assert_eq!(web_search_tool["type"], json!("function"));
            assert_eq!(web_search_tool["strict"], json!(true));
            assert_eq!(
                web_search_tool["parameters"],
                json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "maxLength": 400,
                            "description": "The web search query text after trimming surrounding whitespace."
                        },
                        "max_results": {
                            "type": ["integer", "null"],
                            "minimum": 1,
                            "maximum": 5,
                            "default": null,
                            "description": "Maximum number of citations to return. Pass null to use the default of 5."
                        }
                    },
                    "required": ["query", "max_results"],
                    "additionalProperties": false,
                })
            );
        }
    });
}
