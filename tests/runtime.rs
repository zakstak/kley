use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{
    State,
    ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::stream;
use kley::auth::ResolvedAuth;
use kley::compact::CompactConfig;
use kley::events::{AgentEvent, Transport, event_channel};
use kley::runtime::{AbortResult, RuntimeHooks, SessionRuntime, SubmitResult};
use kley::store::{Session, SessionStatus, SharedStore, Store, Turn};
use kley::tools::{Tool, ToolRegistry};
use serde_json::Value;

mod runtime {
    use super::*;

    struct SlowTool {
        executed: Arc<AtomicBool>,
    }

    impl Tool for SlowTool {
        fn name(&self) -> &str {
            "unknown_tool"
        }

        fn description(&self) -> &str {
            "slow test tool"
        }

        fn parameters_schema(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false,
            })
        }

        fn execute(&self, _args: Value) -> anyhow::Result<String> {
            self.executed.store(true, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(200));
            Ok("slow tool result".to_string())
        }
    }

    struct TestServer {
        addr: std::net::SocketAddr,
        task: tokio::task::JoinHandle<()>,
    }

    async fn spawn_app(app: Router) -> TestServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        TestServer { addr, task }
    }

    async fn openai_ws_handler(ws: WebSocketUpgrade) -> Response {
        ws.on_upgrade(handle_openai_ws)
    }

    async fn handle_openai_ws(mut socket: WebSocket) {
        let _ = socket.recv().await;

        for chunk in ["slow ", "provider ", "stream"] {
            tokio::time::sleep(Duration::from_millis(60)).await;
            if socket
                .send(WsMessage::Text(
                    serde_json::json!({
                        "type": "response.output_text.delta",
                        "delta": chunk,
                    })
                    .to_string(),
                ))
                .await
                .is_err()
            {
                return;
            }
        }

        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = socket
            .send(WsMessage::Text(
                serde_json::json!({ "type": "response.completed" }).to_string(),
            ))
            .await;
    }

    async fn slow_zai_sse_handler() -> impl IntoResponse {
        let body = Body::from_stream(stream::unfold(0usize, |index| async move {
            let chunk = match index {
                0 => Some(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"slow \"}}]}\n",
                )),
                1 => Some(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"provider \"}}]}\n",
                )),
                2 => Some(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"stream\"}}]}\n",
                )),
                3 => Some(Bytes::from_static(b"data: [DONE]\n")),
                _ => None,
            }?;

            tokio::time::sleep(Duration::from_millis(60)).await;
            Some((Ok::<Bytes, std::io::Error>(chunk), index + 1))
        }));

        ([(header::CONTENT_TYPE, "text/event-stream")], body)
    }

    async fn openai_overflow_then_sse_handler(
        State(request_count): State<Arc<AtomicUsize>>,
        body: Bytes,
    ) -> Response {
        request_count.fetch_add(1, Ordering::Relaxed);

        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let input_chars = payload
            .get("input")
            .map(|value| serde_json::to_string(value).unwrap_or_default().len())
            .unwrap_or(0);
        let instructions_chars = payload
            .get("instructions")
            .and_then(|value| value.as_str())
            .map(str::len)
            .unwrap_or(0);
        let tools_chars = payload
            .get("tools")
            .map(|value| serde_json::to_string(value).unwrap_or_default().len())
            .unwrap_or(0);
        let total_chars = input_chars + instructions_chars + tools_chars;

        if total_chars > 50_000 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": "Your input exceeds the context window of this model. Please adjust your input and try again.",
                    }
                })),
            )
                .into_response();
        }

        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"compacted ok\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"total_tokens\":110}}\n\n"
        );

        ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
    }

    fn shared_store() -> SharedStore {
        Arc::new(Mutex::new(Store::open_memory().unwrap()))
    }

    fn runtime_with_abort_signal(
        store: SharedStore,
        auth: ResolvedAuth,
        abort_signal: Arc<AtomicBool>,
    ) -> (SessionRuntime<'static>, kley::events::EventReceiver) {
        let (emitter, receiver) = event_channel();
        let runtime = SessionRuntime::new_with_shared_store_and_abort_signal(
            store,
            auth,
            Some("test-model"),
            None,
            emitter,
            CompactConfig::default(),
            kley::tools::default_registry(std::env::current_dir().unwrap()),
            "system".to_string(),
            RuntimeHooks::default(),
            abort_signal,
        )
        .unwrap();
        (runtime, receiver)
    }

    fn test_auth() -> ResolvedAuth {
        ResolvedAuth {
            provider: "test".to_string(),
            api_key: "test-key".to_string(),
            base_url: "http://unused".to_string(),
            account_id: None,
        }
    }

    #[tokio::test]
    async fn submit_prompt_persists_messages() {
        let store = Store::open_memory().unwrap();
        let (emitter, _receiver) = event_channel();

        let mut runtime = SessionRuntime::new(
            &store,
            test_auth(),
            Some("test-model"),
            None,
            emitter,
            CompactConfig::default(),
            kley::tools::default_registry(std::env::current_dir().unwrap()),
            "system".to_string(),
            RuntimeHooks::default(),
        )
        .unwrap();

        let result = runtime
            .submit_prompt("hello runtime".to_string())
            .await
            .unwrap();
        assert!(matches!(result, SubmitResult::Completed { .. }));

        let session = Session::get(&store, runtime.session_id()).unwrap();
        let turns = Turn::list_for_session(&store, &session.id).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].kind, "message");
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].content, "hello runtime");
        assert_eq!(turns[1].kind, "message");
        assert_eq!(turns[1].role, "assistant");
        assert!(turns[1].content.contains("hello runtime"));
    }

    #[tokio::test]
    async fn abort_returns_typed_result() {
        let store = Store::open_memory().unwrap();
        let (emitter, _receiver) = event_channel();

        let mut runtime = SessionRuntime::new(
            &store,
            test_auth(),
            Some("test-model"),
            None,
            emitter,
            CompactConfig::default(),
            kley::tools::default_registry(std::env::current_dir().unwrap()),
            "system".to_string(),
            RuntimeHooks::default(),
        )
        .unwrap();

        let abort = runtime.abort_turn().unwrap();
        assert!(matches!(abort, AbortResult::NoActiveTurn { .. }));

        let session = Session::get(&store, runtime.session_id()).unwrap();
        assert_eq!(session.status, SessionStatus::Active);

        let submit = runtime
            .submit_prompt("still usable".to_string())
            .await
            .unwrap();
        assert!(matches!(submit, SubmitResult::Completed { .. }));

        let turns = Turn::list_for_session(&store, runtime.session_id()).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].content, "still usable");
        assert_eq!(turns[1].role, "assistant");
        assert!(turns[1].content.contains("still usable"));
    }

    #[tokio::test]
    async fn transport_and_auth_events_are_exposed() {
        let store = Store::open_memory().unwrap();
        let (emitter, receiver) = event_channel();

        emitter.emit(AgentEvent::TokenRefreshed {
            session_id: Some("runtime-test-session".to_string()),
            provider: "openai".to_string(),
        });

        let mut runtime = SessionRuntime::new(
            &store,
            ResolvedAuth {
                provider: "openai".to_string(),
                api_key: "test-key".to_string(),
                base_url: "http://127.0.0.1:9".to_string(),
                account_id: None,
            },
            Some("test-model"),
            None,
            emitter,
            CompactConfig::default(),
            kley::tools::default_registry(std::env::current_dir().unwrap()),
            "system".to_string(),
            RuntimeHooks::default(),
        )
        .unwrap();

        let _ = runtime
            .submit_prompt("hello transport".to_string())
            .await
            .unwrap();

        let events = receiver.drain();
        assert!(events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::TransportSelected {
                    transport: Transport::WebSocket,
                    ..
                }
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::TransportFallback {
                    from: Transport::WebSocket,
                    to: Transport::Sse,
                    ..
                }
            )
        }));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::TokenRefreshed { .. }))
        );
    }

    #[tokio::test]
    async fn openai_websocket_stream_honors_abort_signal() {
        let server = spawn_app(Router::new().route("/responses", get(openai_ws_handler))).await;
        let store = shared_store();
        let abort_signal = Arc::new(AtomicBool::new(false));
        let (mut runtime, receiver) = runtime_with_abort_signal(
            store,
            ResolvedAuth {
                provider: "openai".to_string(),
                api_key: "test-key".to_string(),
                base_url: format!("http://{}", server.addr),
                account_id: None,
            },
            abort_signal.clone(),
        );

        let abort_task = {
            let abort_signal = abort_signal.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(110)).await;
                abort_signal.store(true, Ordering::Relaxed);
            })
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.submit_prompt("please stream slowly".to_string()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, SubmitResult::Aborted { .. }));
        abort_task.await.unwrap();

        let events = receiver.drain();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::MessageDelta { .. }))
        );
        assert!(events.iter().any(|event| {
            matches!(event, AgentEvent::TurnFailed { error, .. } if error == "aborted")
        }));

        server.task.abort();
    }

    #[tokio::test]
    async fn zai_sse_stream_honors_abort_signal() {
        let server =
            spawn_app(Router::new().route("/chat/completions", post(slow_zai_sse_handler))).await;
        let store = shared_store();
        let abort_signal = Arc::new(AtomicBool::new(false));
        let (mut runtime, receiver) = runtime_with_abort_signal(
            store,
            ResolvedAuth {
                provider: "zai".to_string(),
                api_key: "test-key".to_string(),
                base_url: format!("http://{}", server.addr),
                account_id: None,
            },
            abort_signal.clone(),
        );

        let abort_task = {
            let abort_signal = abort_signal.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(110)).await;
                abort_signal.store(true, Ordering::Relaxed);
            })
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.submit_prompt("please stream slowly".to_string()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, SubmitResult::Aborted { .. }));
        abort_task.await.unwrap();

        let events = receiver.drain();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::MessageDelta { .. }))
        );
        assert!(events.iter().any(|event| {
            matches!(event, AgentEvent::TurnFailed { error, .. } if error == "aborted")
        }));

        server.task.abort();
    }

    #[tokio::test]
    async fn abort_stops_before_long_tool_execution_begins() {
        let store = Store::open_memory().unwrap();
        let (emitter, _receiver) = event_channel();
        let abort_signal = Arc::new(AtomicBool::new(false));
        let executed = Arc::new(AtomicBool::new(false));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SlowTool {
            executed: executed.clone(),
        }));

        let hooks = RuntimeHooks {
            on_event: Some(Arc::new({
                let abort_signal = abort_signal.clone();
                move |event| {
                    if matches!(event, kley::runtime::RuntimeEvent::ToolCallStarted { .. }) {
                        abort_signal.store(true, Ordering::Relaxed);
                    }
                }
            })),
            ..RuntimeHooks::default()
        };

        let mut runtime = SessionRuntime::new_with_shared_store_and_abort_signal(
            Arc::new(Mutex::new(store)),
            test_auth(),
            Some("test-model"),
            None,
            emitter,
            CompactConfig::default(),
            registry,
            "system".to_string(),
            hooks,
            abort_signal,
        )
        .unwrap();

        let result = runtime
            .submit_prompt("please use a tool".to_string())
            .await
            .unwrap();
        assert!(matches!(result, SubmitResult::Aborted { .. }));
        assert!(!executed.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn context_overflow_retries_with_harder_compaction() {
        let request_count = Arc::new(AtomicUsize::new(0));
        let server = spawn_app(
            Router::new()
                .route("/responses", post(openai_overflow_then_sse_handler))
                .with_state(request_count.clone()),
        )
        .await;

        let store = Store::open_memory().unwrap();
        let session = Session::create(
            &store,
            kley::store::NewSession {
                model: "test-model".to_string(),
                provider: "openai".to_string(),
            },
        )
        .unwrap();
        Session::update_settings(
            &store,
            &session.id,
            &serde_json::json!({
                "model": "test-model",
                "provider": "openai",
                "compact_threshold": 80_000,
            })
            .to_string(),
        )
        .unwrap();

        for index in 0..8 {
            Turn::append(
                &store,
                kley::store::NewTurn {
                    session_id: session.id.clone(),
                    kind: "message".into(),
                    role: "user".into(),
                    content: format!("user-{index}-{}", "x".repeat(4_000)),
                    model: None,
                    tokens_in: None,
                    tokens_out: None,
                },
            )
            .unwrap();
            Turn::append(
                &store,
                kley::store::NewTurn {
                    session_id: session.id.clone(),
                    kind: "message".into(),
                    role: "assistant".into(),
                    content: format!("assistant-{index}-{}", "y".repeat(4_000)),
                    model: Some("test-model".into()),
                    tokens_in: None,
                    tokens_out: None,
                },
            )
            .unwrap();
        }

        let (emitter, _receiver) = event_channel();
        let mut runtime = SessionRuntime::new(
            &store,
            ResolvedAuth {
                provider: "openai".to_string(),
                api_key: "test-key".to_string(),
                base_url: format!("http://{}", server.addr),
                account_id: None,
            },
            Some("test-model"),
            Some(&session.id),
            emitter,
            CompactConfig {
                threshold_chars: 80_000,
                keep_recent: 20,
            },
            kley::tools::default_registry(std::env::current_dir().unwrap()),
            "system".to_string(),
            RuntimeHooks::default(),
        )
        .unwrap();

        let result = runtime
            .submit_prompt("final prompt".to_string())
            .await
            .unwrap();

        assert!(matches!(
            result,
            SubmitResult::Completed { ref response, .. } if response.contains("compacted ok")
        ));
        assert!(request_count.load(Ordering::Relaxed) >= 2);

        server.task.abort();
    }
}
