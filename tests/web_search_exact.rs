use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use kley::tools::web_search::{
    resolve_max_results, test_support::override_tavily_timeout_for_test, WebSearchCitationInput,
    WebSearchResult, WebSearchTool,
};
use kley::tools::Tool;
use serde_json::{json, Value};

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

#[derive(Clone)]
struct FakeTavilyState {
    response_status: StatusCode,
    response_body: String,
    response_delay: Duration,
    recorded_request: Arc<Mutex<Option<RecordedSearchRequest>>>,
}

async fn spawn_app(app: Router) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer { addr, task }
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

    *state.recorded_request.lock().unwrap() = Some(RecordedSearchRequest {
        authorization,
        body,
    });

    if !state.response_delay.is_zero() {
        tokio::time::sleep(state.response_delay).await;
    }

    (
        state.response_status,
        [(header::CONTENT_TYPE, "application/json")],
        state.response_body,
    )
}

fn run_async_test<F>(future: F) -> F::Output
where
    F: Future,
{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

async fn spawn_fake_tavily_server(state: FakeTavilyState) -> TestServer {
    spawn_app(
        Router::new()
            .route("/search", post(fake_tavily_search_handler))
            .with_state(state),
    )
    .await
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe { std::env::remove_var(key) };
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

fn run_web_search(args: Value) -> Value {
    let tool = WebSearchTool;
    let output = tool.execute(args).unwrap();
    serde_json::from_str(&output).unwrap()
}

#[test]
fn web_search_schema_is_strict() {
    let tool = WebSearchTool;

    assert_eq!(
        tool.parameters_schema(),
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

#[test]
fn web_search_rejects_unknown_fields() {
    let schema = WebSearchTool.parameters_schema();
    let properties = schema["properties"].as_object().unwrap();

    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(properties.len(), 2);
    assert!(properties.contains_key("query"));
    assert!(properties.contains_key("max_results"));
}

#[test]
fn web_search_returns_unavailable_without_tavily_api_key() {
    let _env_lock = env_lock().lock().unwrap();
    let _guard = EnvVarGuard::unset("TAVILY_API_KEY");
    let tool = WebSearchTool;

    let output = tool
        .execute(json!({
            "query": "rust testing",
            "max_results": null,
        }))
        .unwrap();

    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap(),
        json!({
            "status": "unavailable",
            "query": "rust testing",
            "summary": null,
            "citations": [],
            "message": "Set TAVILY_API_KEY to enable web_search.",
        })
    );
}

#[test]
fn web_search_uses_tavily_backend_when_api_key_present() {
    let _env_lock = env_lock().lock().unwrap();
    run_async_test(async {
        let recorded_request = Arc::new(Mutex::new(None));
        let server = spawn_fake_tavily_server(FakeTavilyState {
            response_status: StatusCode::OK,
            response_body: json!({
                "answer": "Rust testing summary",
                "results": [
                    {
                        "title": "Rust Book",
                        "url": "https://doc.rust-lang.org/book/",
                        "content": "Learn Rust with the official book.",
                        "score": 0.98,
                        "raw_content": "ignore me"
                    },
                    {
                        "title": "Cargo Book",
                        "url": "https://doc.rust-lang.org/cargo/",
                        "content": "Cargo is Rust's package manager.",
                        "favicon": "https://example.com/favicon.ico"
                    },
                    {
                        "title": "Rust By Example",
                        "url": "https://doc.rust-lang.org/rust-by-example/",
                        "content": "Hands-on Rust examples."
                    }
                ],
                "request_id": "req_123",
                "project_id": "proj_123",
                "usage": { "credits_used": 1 }
            })
            .to_string(),
            response_delay: Duration::ZERO,
            recorded_request: recorded_request.clone(),
        })
        .await;
        let _api_key = EnvVarGuard::set("TAVILY_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set("TAVILY_API_BASE_URL", &format!("http://{}", server.addr));

        let output = tokio::task::spawn_blocking(|| {
            run_web_search(json!({
                "query": "rust testing",
                "max_results": 2,
            }))
        })
        .await
        .unwrap();

        assert_eq!(
            output,
            json!({
                "status": "ok",
                "query": "rust testing",
                "summary": "Rust testing summary",
                "citations": [
                    {
                        "index": 1,
                        "title": "Rust Book",
                        "url": "https://doc.rust-lang.org/book/",
                        "snippet": "Learn Rust with the official book.",
                    },
                    {
                        "index": 2,
                        "title": "Cargo Book",
                        "url": "https://doc.rust-lang.org/cargo/",
                        "snippet": "Cargo is Rust's package manager.",
                    }
                ],
                "message": null,
            })
        );

        assert_eq!(
            recorded_request.lock().unwrap().clone().unwrap(),
            RecordedSearchRequest {
                authorization: Some("Bearer test-key".to_string()),
                body: json!({
                    "query": "rust testing",
                    "search_depth": "basic",
                    "max_results": 2,
                    "include_answer": "basic",
                    "include_raw_content": false,
                }),
            }
        );
    });
}

#[test]
fn web_search_tavily_maps_answer_and_results_to_summary_and_citations() {
    let _env_lock = env_lock().lock().unwrap();
    run_async_test(async {
        let recorded_request = Arc::new(Mutex::new(None));
        let server = spawn_fake_tavily_server(FakeTavilyState {
            response_status: StatusCode::OK,
            response_body: json!({
                "answer": "Rust testing summary",
                "results": [
                    {
                        "title": "Rust Book",
                        "url": "https://doc.rust-lang.org/book/",
                        "content": "Learn Rust with the official book.",
                        "score": 0.98,
                        "raw_content": "ignore me"
                    },
                    {
                        "title": "Cargo Book",
                        "url": "https://doc.rust-lang.org/cargo/",
                        "content": "Cargo is Rust's package manager.",
                        "favicon": "https://example.com/favicon.ico"
                    },
                    {
                        "title": "Rust By Example",
                        "url": "https://doc.rust-lang.org/rust-by-example/",
                        "content": "Hands-on Rust examples."
                    }
                ],
                "request_id": "req_123",
                "project_id": "proj_123",
                "usage": { "credits_used": 1 }
            })
            .to_string(),
            response_delay: Duration::ZERO,
            recorded_request: recorded_request.clone(),
        })
        .await;
        let _api_key = EnvVarGuard::set("TAVILY_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set("TAVILY_API_BASE_URL", &format!("http://{}", server.addr));

        let output = tokio::task::spawn_blocking(|| {
            run_web_search(json!({
                "query": "rust testing",
                "max_results": 2,
            }))
        })
        .await
        .unwrap();

        assert_eq!(
            output,
            json!({
                "status": "ok",
                "query": "rust testing",
                "summary": "Rust testing summary",
                "citations": [
                    {
                        "index": 1,
                        "title": "Rust Book",
                        "url": "https://doc.rust-lang.org/book/",
                        "snippet": "Learn Rust with the official book.",
                    },
                    {
                        "index": 2,
                        "title": "Cargo Book",
                        "url": "https://doc.rust-lang.org/cargo/",
                        "snippet": "Cargo is Rust's package manager.",
                    }
                ],
                "message": null,
            })
        );

        assert_eq!(
            recorded_request.lock().unwrap().clone().unwrap(),
            RecordedSearchRequest {
                authorization: Some("Bearer test-key".to_string()),
                body: json!({
                    "query": "rust testing",
                    "search_depth": "basic",
                    "max_results": 2,
                    "include_answer": "basic",
                    "include_raw_content": false,
                }),
            }
        );
    });
}

#[test]
fn web_search_returns_no_results_shape() {
    let _env_lock = env_lock().lock().unwrap();
    run_async_test(async {
        let recorded_request = Arc::new(Mutex::new(None));
        let server = spawn_fake_tavily_server(FakeTavilyState {
            response_status: StatusCode::OK,
            response_body: json!({
                "answer": "",
                "results": []
            })
            .to_string(),
            response_delay: Duration::ZERO,
            recorded_request: recorded_request.clone(),
        })
        .await;
        let _api_key = EnvVarGuard::set("TAVILY_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set("TAVILY_API_BASE_URL", &format!("http://{}", server.addr));

        let output = tokio::task::spawn_blocking(|| {
            run_web_search(json!({
                "query": "no deterministic hits",
                "max_results": 3,
            }))
        })
        .await
        .unwrap();

        assert_eq!(
            output,
            json!({
                "status": "no_results",
                "query": "no deterministic hits",
                "summary": null,
                "citations": [],
                "message": "Tavily returned no results.",
            })
        );

        assert_eq!(
            recorded_request.lock().unwrap().clone().unwrap().body,
            json!({
                "query": "no deterministic hits",
                "search_depth": "basic",
                "max_results": 3,
                "include_answer": "basic",
                "include_raw_content": false,
            })
        );
    });
}

#[test]
fn web_search_tavily_empty_results_return_no_results() {
    let _env_lock = env_lock().lock().unwrap();
    run_async_test(async {
        let recorded_request = Arc::new(Mutex::new(None));
        let server = spawn_fake_tavily_server(FakeTavilyState {
            response_status: StatusCode::OK,
            response_body: json!({
                "answer": "",
                "results": []
            })
            .to_string(),
            response_delay: Duration::ZERO,
            recorded_request: recorded_request.clone(),
        })
        .await;
        let _api_key = EnvVarGuard::set("TAVILY_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set("TAVILY_API_BASE_URL", &format!("http://{}", server.addr));

        let output = tokio::task::spawn_blocking(|| {
            run_web_search(json!({
                "query": "no deterministic hits",
                "max_results": 3,
            }))
        })
        .await
        .unwrap();

        assert_eq!(
            output,
            json!({
                "status": "no_results",
                "query": "no deterministic hits",
                "summary": null,
                "citations": [],
                "message": "Tavily returned no results.",
            })
        );

        assert_eq!(
            recorded_request.lock().unwrap().clone().unwrap().body,
            json!({
                "query": "no deterministic hits",
                "search_depth": "basic",
                "max_results": 3,
                "include_answer": "basic",
                "include_raw_content": false,
            })
        );
    });
}

#[test]
fn web_search_tavily_timeout_returns_unavailable() {
    let _env_lock = env_lock().lock().unwrap();
    run_async_test(async {
        let server = spawn_fake_tavily_server(FakeTavilyState {
            response_status: StatusCode::OK,
            response_body: json!({
                "answer": "too slow",
                "results": []
            })
            .to_string(),
            response_delay: Duration::from_millis(100),
            recorded_request: Arc::new(Mutex::new(None)),
        })
        .await;
        let _api_key = EnvVarGuard::set("TAVILY_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set("TAVILY_API_BASE_URL", &format!("http://{}", server.addr));
        let _timeout_guard = override_tavily_timeout_for_test(Duration::from_millis(5));

        let output = tokio::task::spawn_blocking(|| {
            run_web_search(json!({
                "query": "slow tavily query",
                "max_results": 1,
            }))
        })
        .await
        .unwrap();

        assert_eq!(
            output,
            json!({
                "status": "unavailable",
                "query": "slow tavily query",
                "summary": null,
                "citations": [],
                "message": "Tavily search timed out before responding.",
            })
        );
    });
}

#[test]
fn web_search_tavily_http_429_returns_unavailable() {
    let _env_lock = env_lock().lock().unwrap();
    run_async_test(async {
        let server = spawn_fake_tavily_server(FakeTavilyState {
            response_status: StatusCode::TOO_MANY_REQUESTS,
            response_body: json!({
                "detail": "rate limited"
            })
            .to_string(),
            response_delay: Duration::ZERO,
            recorded_request: Arc::new(Mutex::new(None)),
        })
        .await;
        let _api_key = EnvVarGuard::set("TAVILY_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set("TAVILY_API_BASE_URL", &format!("http://{}", server.addr));

        let output = tokio::task::spawn_blocking(|| {
            run_web_search(json!({
                "query": "rate limited tavily query",
                "max_results": 1,
            }))
        })
        .await
        .unwrap();

        assert_eq!(
            output,
            json!({
                "status": "unavailable",
                "query": "rate limited tavily query",
                "summary": null,
                "citations": [],
                "message": "Tavily search is unavailable right now (HTTP 429).",
            })
        );
    });
}

#[test]
fn web_search_empty_query_returns_recoverable_error() {
    let tool = WebSearchTool;

    let output = tool
        .execute(json!({
            "query": "   ",
            "max_results": null,
        }))
        .unwrap();

    assert_eq!(output, "Error: query is required");
}

#[test]
fn web_search_default_max_results_is_five() {
    assert_eq!(
        resolve_max_results(&json!({ "max_results": null })).unwrap(),
        5
    );
    assert_eq!(resolve_max_results(&json!({})).unwrap(), 5);
}

#[test]
fn web_search_scope_excludes_fetch_fields() {
    let schema = WebSearchTool.parameters_schema();
    let properties = schema["properties"].as_object().unwrap();

    assert_eq!(properties.len(), 2);
    assert!(!properties.contains_key("url"));
    assert!(!properties.contains_key("open_page"));
    assert!(!properties.contains_key("fetch"));
    assert!(!properties.contains_key("find_in_page"));
    assert!(!properties.contains_key("page_id"));
}

#[test]
fn web_search_normalizes_ok_result_shape() {
    let output = WebSearchResult::ok(
        "  rust search  ",
        Some("Concise summary"),
        vec![
            WebSearchCitationInput {
                title: "Rust Language".to_string(),
                url: "https://www.rust-lang.org/".to_string(),
                snippet: Some("Rust empowers everyone to build reliable software.".to_string()),
            },
            WebSearchCitationInput {
                title: "Cargo Book".to_string(),
                url: "https://doc.rust-lang.org/cargo/".to_string(),
                snippet: None,
            },
        ],
        5,
    )
    .unwrap()
    .to_json_string()
    .unwrap();

    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap(),
        json!({
            "status": "ok",
            "query": "rust search",
            "summary": "Concise summary",
            "citations": [
                {
                    "index": 1,
                    "title": "Rust Language",
                    "url": "https://www.rust-lang.org/",
                    "snippet": "Rust empowers everyone to build reliable software.",
                },
                {
                    "index": 2,
                    "title": "Cargo Book",
                    "url": "https://doc.rust-lang.org/cargo/",
                    "snippet": null,
                }
            ],
            "message": null,
        })
    );
}

#[test]
fn web_search_caps_summary_and_snippets() {
    let summary = "s".repeat(1_601);
    let snippet = "n".repeat(281);

    let output = WebSearchResult::ok(
        "rust caps",
        Some(&summary),
        vec![WebSearchCitationInput {
            title: "Long Result".to_string(),
            url: "https://example.com/long".to_string(),
            snippet: Some(snippet),
        }],
        5,
    )
    .unwrap()
    .to_json_string()
    .unwrap();

    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap(),
        json!({
            "status": "ok",
            "query": "rust caps",
            "summary": format!("{}...", "s".repeat(1_600)),
            "citations": [
                {
                    "index": 1,
                    "title": "Long Result",
                    "url": "https://example.com/long",
                    "snippet": format!("{}...", "n".repeat(280)),
                }
            ],
            "message": null,
        })
    );
}

#[test]
fn web_search_assigns_stable_citation_indexes() {
    let citations = (0..7)
        .map(|index| WebSearchCitationInput {
            title: format!("Result {index}"),
            url: format!("https://example.com/{index}"),
            snippet: Some(format!("Snippet {index}")),
        })
        .collect();

    let output = WebSearchResult::ok("rust stable", None, citations, 99)
        .unwrap()
        .to_json_string()
        .unwrap();

    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap(),
        json!({
            "status": "ok",
            "query": "rust stable",
            "summary": null,
            "citations": [
                {
                    "index": 1,
                    "title": "Result 0",
                    "url": "https://example.com/0",
                    "snippet": "Snippet 0",
                },
                {
                    "index": 2,
                    "title": "Result 1",
                    "url": "https://example.com/1",
                    "snippet": "Snippet 1",
                },
                {
                    "index": 3,
                    "title": "Result 2",
                    "url": "https://example.com/2",
                    "snippet": "Snippet 2",
                },
                {
                    "index": 4,
                    "title": "Result 3",
                    "url": "https://example.com/3",
                    "snippet": "Snippet 3",
                },
                {
                    "index": 5,
                    "title": "Result 4",
                    "url": "https://example.com/4",
                    "snippet": "Snippet 4",
                }
            ],
            "message": null,
        })
    );
}
