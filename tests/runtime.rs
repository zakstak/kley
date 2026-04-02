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
use kley::compact::{CompactConfig, HANDOFF_SUMMARY_PREFIX};
use kley::events::{AgentEvent, Transport, event_channel};
use kley::provider::test::{CONTROL_BLOCK_END, CONTROL_BLOCK_START};
use kley::runtime::manager::{RuntimeManager, spawn_autonomous_child_task_with_policy};
use kley::runtime::session::{
    ChildSessionBootstrapMode, DelegatedChildBootstrapOutcome, bootstrap_delegated_child_session,
};
use kley::runtime::{AbortResult, RuntimeEvent, RuntimeHooks, SessionRuntime, SubmitResult};
use kley::store::{
    AttemptLifecycleState, NewSession, NewTaskAttemptRecord, NewTaskEdgeRecord, NewTaskEventRecord,
    NewTaskRecord, Session, SessionStatus, SharedStore, Store, TaskAttemptRecord, TaskEdgeRecord,
    TaskEventRecord, TaskLifecycleState, TaskRecord, Turn,
};
use kley::tools::{Tool, ToolRegistry};
use serde_json::Value;

#[allow(clippy::too_many_arguments)]
fn delegation_policy_json(
    allow_autonomous_spawn: bool,
    current_depth: u32,
    max_depth: u32,
    max_concurrency: u32,
    budget: u64,
    allowed_providers: &[&str],
    allowed_models: &[&str],
    approved_tools: &[&str],
    tool_approval_mode: &str,
    parent_close_policy: &str,
) -> String {
    serde_json::json!({
        "allow_autonomous_spawn": allow_autonomous_spawn,
        "current_depth": current_depth,
        "max_depth": max_depth,
        "max_concurrency": max_concurrency,
        "budget": budget,
        "allowed_providers": allowed_providers,
        "allowed_models": allowed_models,
        "approved_tools": approved_tools,
        "tool_approval_mode": tool_approval_mode,
        "parent_close_policy": parent_close_policy,
    })
    .to_string()
}

mod runtime {
    use super::*;
    use kley::runtime::ToolCall;

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
            None,
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
                    if matches!(event, RuntimeEvent::ToolCallStarted { .. }) {
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
            None,
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

    #[tokio::test]
    async fn on_tool_approval_denies_execution() {
        let store = Store::open_memory().unwrap();
        let (emitter, receiver) = event_channel();
        let (executed, execution_happened) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SlowTool {
            executed: execution_happened.clone(),
        }));

        let hooks = RuntimeHooks {
            on_tool_approval: Some(Arc::new({
                let executed = executed.clone();
                move |_: &ToolCall| {
                    executed.store(true, Ordering::Relaxed);
                    false
                }
            })),
            ..RuntimeHooks::default()
        };

        let store = Arc::new(Mutex::new(store));
        let mut runtime = SessionRuntime::new_with_shared_store_and_abort_signal(
            store.clone(),
            test_auth(),
            Some("test-model"),
            None,
            emitter,
            CompactConfig::default(),
            registry,
            "system".to_string(),
            hooks,
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .unwrap();

        let result = runtime
            .submit_prompt("please use a tool".to_string())
            .await
            .unwrap();

        assert!(matches!(result, SubmitResult::Completed { .. }));
        assert!(executed.load(Ordering::Relaxed));
        assert!(!execution_happened.load(Ordering::Relaxed));

        let session = runtime.session_id().to_string();
        let turns = Turn::list_for_session(&store.lock().unwrap(), &session).unwrap();
        let function_output = turns
            .iter()
            .find(|turn| turn.kind == "function_call_output")
            .expect("function_call_output turn expected");
        let payload: serde_json::Value = serde_json::from_str(&function_output.content).unwrap();
        assert!(
            payload["output"]
                .as_str()
                .unwrap_or_default()
                .contains("Tool execution denied by user")
        );
        assert!(payload.get("edit_observation").is_none());

        let events = receiver.drain();
        let denied_tool_event = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolCallCompleted {
                    success,
                    edit_observation,
                    ..
                } => Some((*success, edit_observation)),
                _ => None,
            })
            .expect("expected ToolCallCompleted event");
        assert!(!denied_tool_event.0);
        assert!(denied_tool_event.1.is_none());
    }
}

#[tokio::test]
async fn task_schema_round_trips_canonical_identity() {
    let store = Store::open_memory().unwrap();
    let child_session_a = Session::create(
        &store,
        NewSession {
            model: "test-model".to_string(),
            provider: "test".to_string(),
        },
    )
    .unwrap();
    let child_session_b = Session::create(
        &store,
        NewSession {
            model: "test-model".to_string(),
            provider: "test".to_string(),
        },
    )
    .unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-root".to_string(),
            parent_task_id: None,
            title: Some("root task".to_string()),
            priority: 7,
            policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: Some(r#"{"cursor":1}"#.to_string()),
            owner_session_id: None,
        },
    )
    .unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-child".to_string(),
            parent_task_id: Some("task-root".to_string()),
            title: Some("child task".to_string()),
            priority: 3,
            policy_snapshot: r#"{"mode":"ask"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    TaskEdgeRecord::create(
        &store,
        NewTaskEdgeRecord {
            task_id: "task-child".to_string(),
            depends_on_task_id: "task-root".to_string(),
        },
    )
    .unwrap();

    let attempt_a = TaskAttemptRecord::create(
        &store,
        NewTaskAttemptRecord {
            attempt_id: "attempt-a".to_string(),
            task_id: "task-root".to_string(),
            session_id: Some(child_session_a.id.clone()),
            status: "running".to_string(),
            recovery_checkpoint: Some(r#"{"step":"tool-call"}"#.to_string()),
        },
    )
    .unwrap();

    let attempt_b = TaskAttemptRecord::create(
        &store,
        NewTaskAttemptRecord {
            attempt_id: "attempt-b".to_string(),
            task_id: "task-root".to_string(),
            session_id: Some(child_session_b.id.clone()),
            status: "running".to_string(),
            recovery_checkpoint: Some(r#"{"step":"resume"}"#.to_string()),
        },
    )
    .unwrap();

    let event_a = TaskEventRecord::append(
        &store,
        NewTaskEventRecord {
            task_id: "task-root".to_string(),
            attempt_id: attempt_a.attempt_id.clone(),
            session_id: Some(child_session_a.id.clone()),
            event_type: "attempt.started".to_string(),
            payload: r#"{"phase":"start"}"#.to_string(),
        },
    )
    .unwrap();
    let event_b = TaskEventRecord::append(
        &store,
        NewTaskEventRecord {
            task_id: "task-root".to_string(),
            attempt_id: attempt_b.attempt_id.clone(),
            session_id: Some(child_session_b.id.clone()),
            event_type: "attempt.started".to_string(),
            payload: r#"{"phase":"restart"}"#.to_string(),
        },
    )
    .unwrap();

    let fetched_task = TaskRecord::get(&store, "task-root").unwrap();
    assert_eq!(fetched_task.task_id, "task-root");
    assert_eq!(fetched_task.priority, 7);
    assert_eq!(
        fetched_task.parent_close_policy,
        "request_cancel_descendants".to_string()
    );

    let fetched_attempts = TaskAttemptRecord::list_for_task(&store, "task-root").unwrap();
    assert_eq!(fetched_attempts.len(), 2);
    assert_eq!(fetched_attempts[0].task_id, "task-root");
    assert_eq!(fetched_attempts[1].task_id, "task-root");
    assert_ne!(
        fetched_attempts[0].attempt_id,
        fetched_attempts[1].attempt_id
    );

    let child_edges = TaskEdgeRecord::list_for_task(&store, "task-child").unwrap();
    assert_eq!(child_edges.len(), 1);
    assert_eq!(child_edges[0].task_id, "task-child");
    assert_eq!(child_edges[0].depends_on_task_id, "task-root");

    let events = TaskEventRecord::list_for_task(&store, "task-root", 0).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].task_id, "task-root");
    assert_eq!(events[1].task_id, "task-root");
    assert_eq!(events[0].attempt_id, "attempt-a");
    assert_eq!(events[1].attempt_id, "attempt-b");
    assert!(event_a.sequence < event_b.sequence);
}

#[tokio::test]
async fn task_schema_rejects_missing_attempt_identity() {
    let store = Store::open_memory().unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-root".to_string(),
            parent_task_id: None,
            title: Some("root task".to_string()),
            priority: 1,
            policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    let err = TaskEventRecord::append(
        &store,
        NewTaskEventRecord {
            task_id: "task-root".to_string(),
            attempt_id: "missing-attempt".to_string(),
            session_id: None,
            event_type: "attempt.started".to_string(),
            payload: "{}".to_string(),
        },
    )
    .unwrap_err();

    let error_text = format!("{err:#}").to_lowercase();
    assert!(
        error_text.contains("foreign key") || error_text.contains("constraint"),
        "expected foreign-key/constraint error, got: {error_text}"
    );
}

#[tokio::test]
async fn task_graph_persists_arbitrary_depth_dag() {
    let store = Store::open_memory().unwrap();

    for (task_id, priority) in [
        ("task-0", 10),
        ("task-1", 20),
        ("task-2", 30),
        ("task-3", 40),
        ("task-4", 50),
        ("task-5", 60),
        ("task-6", 70),
    ] {
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: task_id.to_string(),
                parent_task_id: None,
                title: Some(format!("title-{task_id}")),
                priority,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )
        .unwrap();
    }

    TaskRecord::create_or_update(
        &store,
        NewTaskRecord {
            task_id: "task-6".to_string(),
            parent_task_id: Some("task-2".to_string()),
            title: Some("task-six-updated".to_string()),
            priority: 99,
            policy_snapshot: r#"{"mode":"ask"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: Some(r#"{"checkpoint":"graph-only"}"#.to_string()),
            owner_session_id: None,
        },
    )
    .unwrap();

    TaskEdgeRecord::replace_for_task(&store, "task-1", &["task-0".to_string()]).unwrap();
    TaskEdgeRecord::replace_for_task(&store, "task-2", &["task-1".to_string()]).unwrap();
    TaskEdgeRecord::replace_for_task(&store, "task-3", &["task-2".to_string()]).unwrap();
    TaskEdgeRecord::replace_for_task(&store, "task-4", &["task-3".to_string()]).unwrap();
    TaskEdgeRecord::replace_for_task(&store, "task-5", &["task-4".to_string()]).unwrap();
    TaskEdgeRecord::replace_for_task(
        &store,
        "task-6",
        &["task-2".to_string(), "task-5".to_string()],
    )
    .unwrap();

    let tasks = TaskRecord::list(&store).unwrap();
    assert_eq!(tasks.len(), 7);
    let task_6 = tasks.iter().find(|task| task.task_id == "task-6").unwrap();
    assert_eq!(task_6.priority, 99);
    assert_eq!(task_6.parent_task_id.as_deref(), Some("task-2"));
    assert_eq!(task_6.title.as_deref(), Some("task-six-updated"));
    assert_eq!(task_6.policy_snapshot, r#"{"mode":"ask"}"#.to_string());
    assert_eq!(
        task_6.recovery_checkpoint,
        Some(r#"{"checkpoint":"graph-only"}"#.to_string())
    );

    let edge_pairs = TaskEdgeRecord::list(&store)
        .unwrap()
        .into_iter()
        .map(|edge| (edge.task_id, edge.depends_on_task_id))
        .collect::<Vec<_>>();
    assert_eq!(
        edge_pairs,
        vec![
            ("task-1".to_string(), "task-0".to_string()),
            ("task-2".to_string(), "task-1".to_string()),
            ("task-3".to_string(), "task-2".to_string()),
            ("task-4".to_string(), "task-3".to_string()),
            ("task-5".to_string(), "task-4".to_string()),
            ("task-6".to_string(), "task-2".to_string()),
            ("task-6".to_string(), "task-5".to_string()),
        ]
    );
}

#[tokio::test]
async fn task_graph_rejects_cycles_at_write_time() {
    let store = Store::open_memory().unwrap();

    for task_id in ["task-a", "task-b", "task-c"] {
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: task_id.to_string(),
                parent_task_id: None,
                title: Some(task_id.to_string()),
                priority: 1,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )
        .unwrap();
    }

    TaskEdgeRecord::create(
        &store,
        NewTaskEdgeRecord {
            task_id: "task-b".to_string(),
            depends_on_task_id: "task-a".to_string(),
        },
    )
    .unwrap();
    TaskEdgeRecord::create(
        &store,
        NewTaskEdgeRecord {
            task_id: "task-c".to_string(),
            depends_on_task_id: "task-b".to_string(),
        },
    )
    .unwrap();

    let err = TaskEdgeRecord::create(
        &store,
        NewTaskEdgeRecord {
            task_id: "task-a".to_string(),
            depends_on_task_id: "task-c".to_string(),
        },
    )
    .unwrap_err();

    let error_text = format!("{err:#}").to_lowercase();
    assert!(
        error_text.contains("task graph cycle detected"),
        "expected cycle error, got: {error_text}"
    );

    let task_a_edges = TaskEdgeRecord::list_for_task(&store, "task-a").unwrap();
    assert!(
        task_a_edges.is_empty(),
        "cycle edge should not be persisted"
    );

    let all_edge_pairs = TaskEdgeRecord::list(&store)
        .unwrap()
        .into_iter()
        .map(|edge| (edge.task_id, edge.depends_on_task_id))
        .collect::<Vec<_>>();
    assert_eq!(
        all_edge_pairs,
        vec![
            ("task-b".to_string(), "task-a".to_string()),
            ("task-c".to_string(), "task-b".to_string()),
        ]
    );
}

#[tokio::test]
async fn task_state_machine_is_durable() {
    let store = Store::open_memory().unwrap();
    let child_session = Session::create(
        &store,
        NewSession {
            model: "test-model".to_string(),
            provider: "test".to_string(),
        },
    )
    .unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-sm-root".to_string(),
            parent_task_id: None,
            title: Some("task-sm-root".to_string()),
            priority: 10,
            policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-sm-retry".to_string(),
            parent_task_id: None,
            title: Some("task-sm-retry".to_string()),
            priority: 5,
            policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    let root_attempt = TaskAttemptRecord::create(
        &store,
        NewTaskAttemptRecord {
            attempt_id: "attempt-sm-root".to_string(),
            task_id: "task-sm-root".to_string(),
            session_id: Some(child_session.id.clone()),
            status: AttemptLifecycleState::Queued.to_string(),
            recovery_checkpoint: None,
        },
    )
    .unwrap();

    let retry_attempt = TaskAttemptRecord::create(
        &store,
        NewTaskAttemptRecord {
            attempt_id: "attempt-sm-retry".to_string(),
            task_id: "task-sm-retry".to_string(),
            session_id: Some(child_session.id.clone()),
            status: AttemptLifecycleState::Queued.to_string(),
            recovery_checkpoint: None,
        },
    )
    .unwrap();

    assert_eq!(
        TaskRecord::current_state(&store, "task-sm-root").unwrap(),
        TaskLifecycleState::Queued
    );
    assert_eq!(
        TaskRecord::current_state(&store, "task-sm-retry").unwrap(),
        TaskLifecycleState::Queued
    );

    TaskRecord::transition_state(
        &store,
        "task-sm-root",
        &root_attempt.attempt_id,
        TaskLifecycleState::Ready,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &root_attempt.attempt_id,
        AttemptLifecycleState::Ready,
    )
    .unwrap();

    TaskRecord::transition_state(
        &store,
        "task-sm-root",
        &root_attempt.attempt_id,
        TaskLifecycleState::Running,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &root_attempt.attempt_id,
        AttemptLifecycleState::Running,
    )
    .unwrap();

    TaskRecord::transition_state(
        &store,
        "task-sm-root",
        &root_attempt.attempt_id,
        TaskLifecycleState::Blocked,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &root_attempt.attempt_id,
        AttemptLifecycleState::Blocked,
    )
    .unwrap();

    TaskRecord::transition_state(
        &store,
        "task-sm-root",
        &root_attempt.attempt_id,
        TaskLifecycleState::Ready,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &root_attempt.attempt_id,
        AttemptLifecycleState::Ready,
    )
    .unwrap();

    TaskRecord::transition_state(
        &store,
        "task-sm-root",
        &root_attempt.attempt_id,
        TaskLifecycleState::Running,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &root_attempt.attempt_id,
        AttemptLifecycleState::Running,
    )
    .unwrap();

    TaskRecord::transition_state(
        &store,
        "task-sm-root",
        &root_attempt.attempt_id,
        TaskLifecycleState::Completed,
    )
    .unwrap();
    let root_attempt = TaskAttemptRecord::transition_state(
        &store,
        &root_attempt.attempt_id,
        AttemptLifecycleState::Completed,
    )
    .unwrap();

    TaskRecord::transition_state(
        &store,
        "task-sm-retry",
        &retry_attempt.attempt_id,
        TaskLifecycleState::Ready,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &retry_attempt.attempt_id,
        AttemptLifecycleState::Ready,
    )
    .unwrap();
    TaskRecord::transition_state(
        &store,
        "task-sm-retry",
        &retry_attempt.attempt_id,
        TaskLifecycleState::Running,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &retry_attempt.attempt_id,
        AttemptLifecycleState::Running,
    )
    .unwrap();
    TaskRecord::transition_state(
        &store,
        "task-sm-retry",
        &retry_attempt.attempt_id,
        TaskLifecycleState::Failed,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &retry_attempt.attempt_id,
        AttemptLifecycleState::Failed,
    )
    .unwrap();
    TaskRecord::transition_state(
        &store,
        "task-sm-retry",
        &retry_attempt.attempt_id,
        TaskLifecycleState::Retryable,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &retry_attempt.attempt_id,
        AttemptLifecycleState::Retryable,
    )
    .unwrap();
    TaskRecord::transition_state(
        &store,
        "task-sm-retry",
        &retry_attempt.attempt_id,
        TaskLifecycleState::Queued,
    )
    .unwrap();
    let retry_attempt = TaskAttemptRecord::transition_state(
        &store,
        &retry_attempt.attempt_id,
        AttemptLifecycleState::Queued,
    )
    .unwrap();

    assert_eq!(
        TaskRecord::current_state(&store, "task-sm-root").unwrap(),
        TaskLifecycleState::Completed
    );
    assert_eq!(
        TaskRecord::current_state(&store, "task-sm-retry").unwrap(),
        TaskLifecycleState::Queued
    );
    assert_eq!(
        root_attempt.status,
        AttemptLifecycleState::Completed.to_string()
    );
    assert_eq!(
        retry_attempt.status,
        AttemptLifecycleState::Queued.to_string()
    );

    let root_events = TaskEventRecord::list_for_task(&store, "task-sm-root", 0).unwrap();
    assert_eq!(root_events.len(), 12);
    assert_eq!(
        root_events
            .iter()
            .filter(|event| event.event_type == "task.state.transition")
            .count(),
        6
    );
    assert_eq!(
        root_events
            .iter()
            .filter(|event| event.event_type == "attempt.state.transition")
            .count(),
        6
    );

    let retry_events = TaskEventRecord::list_for_task(&store, "task-sm-retry", 0).unwrap();
    assert_eq!(retry_events.len(), 10);
    assert_eq!(
        retry_events
            .iter()
            .filter(|event| event.event_type == "task.state.transition")
            .count(),
        5
    );
    assert_eq!(
        retry_events
            .iter()
            .filter(|event| event.event_type == "attempt.state.transition")
            .count(),
        5
    );
}

#[tokio::test]
async fn attempt_state_machine_rejects_invalid_transitions() {
    let store = Store::open_memory().unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-invalid-attempt".to_string(),
            parent_task_id: None,
            title: Some("task-invalid-attempt".to_string()),
            priority: 1,
            policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    let attempt = TaskAttemptRecord::create(
        &store,
        NewTaskAttemptRecord {
            attempt_id: "attempt-invalid-attempt".to_string(),
            task_id: "task-invalid-attempt".to_string(),
            session_id: None,
            status: AttemptLifecycleState::Queued.to_string(),
            recovery_checkpoint: None,
        },
    )
    .unwrap();

    let err = TaskAttemptRecord::transition_state(
        &store,
        &attempt.attempt_id,
        AttemptLifecycleState::Completed,
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("invalid attempt transition: queued -> completed"),);

    let persisted_after_invalid = TaskAttemptRecord::get(&store, &attempt.attempt_id).unwrap();
    assert_eq!(
        persisted_after_invalid.status,
        AttemptLifecycleState::Queued.to_string()
    );

    TaskAttemptRecord::transition_state(
        &store,
        &attempt.attempt_id,
        AttemptLifecycleState::Running,
    )
    .unwrap();
    TaskAttemptRecord::transition_state(
        &store,
        &attempt.attempt_id,
        AttemptLifecycleState::Completed,
    )
    .unwrap();

    let err = TaskAttemptRecord::transition_state(
        &store,
        &attempt.attempt_id,
        AttemptLifecycleState::Retryable,
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("invalid attempt transition: completed -> retryable"),);

    let final_attempt = TaskAttemptRecord::get(&store, &attempt.attempt_id).unwrap();
    assert_eq!(
        final_attempt.status,
        AttemptLifecycleState::Completed.to_string()
    );

    let events = TaskEventRecord::list_for_task(&store, "task-invalid-attempt", 0).unwrap();
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| event.event_type == "attempt.state.transition")
    );
}

#[tokio::test]
async fn child_session_bootstrap_uses_handoff_summary_not_parent_transcript() {
    let store = Store::open_memory().unwrap();

    let parent = Session::create(
        &store,
        NewSession {
            model: "parent-model".to_string(),
            provider: "test".to_string(),
        },
    )
    .unwrap();
    Session::update_settings(
        &store,
        &parent.id,
        &serde_json::json!({
            "model": "parent-model",
            "provider": "test",
            "compact_threshold": 32123,
        })
        .to_string(),
    )
    .unwrap();
    Turn::append(
        &store,
        kley::store::NewTurn {
            session_id: parent.id.clone(),
            kind: "message".to_string(),
            role: "user".to_string(),
            content: "PARENT RAW TRANSCRIPT SECRET".to_string(),
            model: None,
            tokens_in: None,
            tokens_out: None,
        },
    )
    .unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-handoff-contract".to_string(),
            parent_task_id: None,
            title: Some("task-handoff-contract".to_string()),
            priority: 1,
            policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    TaskAttemptRecord::create(
        &store,
        NewTaskAttemptRecord {
            attempt_id: "attempt-handoff-contract".to_string(),
            task_id: "task-handoff-contract".to_string(),
            session_id: None,
            status: AttemptLifecycleState::Ready.to_string(),
            recovery_checkpoint: None,
        },
    )
    .unwrap();

    let outcome = bootstrap_delegated_child_session(
        &store,
        "task-handoff-contract",
        "attempt-handoff-contract",
        Some(&parent.id),
        None,
        "Bounded handoff summary for child execution",
        vec!["artifact://plan/123".to_string()],
        ChildSessionBootstrapMode::CreateNew,
    )
    .unwrap();

    let child_session_id = match outcome {
        DelegatedChildBootstrapOutcome::Started {
            child_session_id: Some(session_id),
            ..
        } => session_id,
        other => panic!("unexpected outcome: {other:?}"),
    };

    let child_turns = Turn::list_for_session(&store, &child_session_id).unwrap();
    assert_eq!(child_turns.len(), 1);
    assert!(child_turns[0].content.contains(HANDOFF_SUMMARY_PREFIX));
    assert!(
        child_turns[0]
            .content
            .contains("Bounded handoff summary for child execution")
    );
    assert!(child_turns[0].content.contains("artifact://plan/123"));
    assert!(
        !child_turns[0]
            .content
            .contains("PARENT RAW TRANSCRIPT SECRET")
    );

    let child_session = Session::get(&store, &child_session_id).unwrap();
    let settings_json = child_session.settings.expect("child settings must persist");
    let settings: serde_json::Value = serde_json::from_str(&settings_json).unwrap();
    assert_eq!(
        settings.get("compact_threshold").and_then(|v| v.as_u64()),
        Some(32_123)
    );
}

#[tokio::test]
async fn delegated_task_links_child_session_after_attempt_start() {
    let store = Store::open_memory().unwrap();

    let parent = Session::create(
        &store,
        NewSession {
            model: "parent-model".to_string(),
            provider: "test".to_string(),
        },
    )
    .unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-link-after-start".to_string(),
            parent_task_id: None,
            title: Some("task-link-after-start".to_string()),
            priority: 1,
            policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    TaskAttemptRecord::create(
        &store,
        NewTaskAttemptRecord {
            attempt_id: "attempt-link-after-start".to_string(),
            task_id: "task-link-after-start".to_string(),
            session_id: None,
            status: AttemptLifecycleState::Queued.to_string(),
            recovery_checkpoint: None,
        },
    )
    .unwrap();

    let outcome = bootstrap_delegated_child_session(
        &store,
        "task-link-after-start",
        "attempt-link-after-start",
        Some(&parent.id),
        None,
        "summary",
        vec![],
        ChildSessionBootstrapMode::CreateNew,
    )
    .unwrap();

    let child_session_id = match outcome {
        DelegatedChildBootstrapOutcome::Started {
            child_session_id: Some(session_id),
            ..
        } => session_id,
        other => panic!("unexpected outcome: {other:?}"),
    };

    let attempt = TaskAttemptRecord::get(&store, "attempt-link-after-start").unwrap();
    assert_eq!(
        attempt.session_id.as_deref(),
        Some(child_session_id.as_str())
    );

    let events = TaskEventRecord::list_for_task(&store, "task-link-after-start", 0).unwrap();
    let running_seq = events
        .iter()
        .find(|event| {
            event.event_type == "attempt.state.transition"
                && serde_json::from_str::<serde_json::Value>(&event.payload)
                    .ok()
                    .and_then(|payload| {
                        payload
                            .get("to")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    })
                    .as_deref()
                    == Some("running")
        })
        .map(|event| event.sequence)
        .expect("running transition event expected");

    let linked_event = events
        .iter()
        .find(|event| event.event_type == "attempt.child_session.linked")
        .expect("child link event expected");
    assert!(linked_event.sequence > running_seq);
    assert_eq!(
        linked_event.session_id.as_deref(),
        Some(child_session_id.as_str())
    );
}

#[tokio::test]
async fn delegated_task_child_session_failure_marks_attempt_interrupted() {
    let store = Store::open_memory().unwrap();

    let parent = Session::create(
        &store,
        NewSession {
            model: "parent-model".to_string(),
            provider: "test".to_string(),
        },
    )
    .unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-child-session-failure".to_string(),
            parent_task_id: None,
            title: Some("task-child-session-failure".to_string()),
            priority: 1,
            policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    TaskAttemptRecord::create(
        &store,
        NewTaskAttemptRecord {
            attempt_id: "attempt-child-session-failure".to_string(),
            task_id: "task-child-session-failure".to_string(),
            session_id: None,
            status: AttemptLifecycleState::Ready.to_string(),
            recovery_checkpoint: None,
        },
    )
    .unwrap();

    let outcome = bootstrap_delegated_child_session(
        &store,
        "task-child-session-failure",
        "attempt-child-session-failure",
        Some(&parent.id),
        None,
        "summary",
        vec![],
        ChildSessionBootstrapMode::LinkExisting {
            session_id: "missing-child-session".to_string(),
        },
    )
    .unwrap();

    assert!(matches!(
        outcome,
        DelegatedChildBootstrapOutcome::InterruptedRetryable { .. }
    ));

    let attempt = TaskAttemptRecord::get(&store, "attempt-child-session-failure").unwrap();
    assert_eq!(
        attempt.status,
        AttemptLifecycleState::Interrupted.to_string(),
    );

    let task_state = TaskRecord::current_state(&store, "task-child-session-failure").unwrap();
    assert_eq!(task_state, TaskLifecycleState::Interrupted);

    let checkpoint_json: serde_json::Value =
        serde_json::from_str(attempt.recovery_checkpoint.as_deref().unwrap()).unwrap();
    assert_eq!(
        checkpoint_json
            .get("child_bootstrap")
            .and_then(|child| child.get("status"))
            .and_then(|value| value.as_str()),
        Some("interrupted")
    );
    assert_eq!(
        checkpoint_json
            .get("child_bootstrap")
            .and_then(|child| child.get("retryable"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn scheduler_executes_ready_graph_nodes_via_child_sessions() {
    let shared_store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let manager = RuntimeManager::new();

    let parent_session_id = {
        let store = shared_store.lock().unwrap();
        Session::create(
            &store,
            NewSession {
                model: "parent-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap()
        .id
    };

    {
        let store = shared_store.lock().unwrap();
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "dag-root".to_string(),
                parent_task_id: None,
                title: Some("complete root delegated node".to_string()),
                priority: 10,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: Some(parent_session_id.clone()),
            },
        )
        .unwrap();
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "dag-child".to_string(),
                parent_task_id: None,
                title: Some("complete child delegated node".to_string()),
                priority: 5,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: Some(parent_session_id.clone()),
            },
        )
        .unwrap();
        TaskEdgeRecord::create(
            &store,
            NewTaskEdgeRecord {
                task_id: "dag-child".to_string(),
                depends_on_task_id: "dag-root".to_string(),
            },
        )
        .unwrap();
    }

    let completed = manager
        .execute_scheduler_ready_graph_nodes(
            Arc::clone(&shared_store),
            &parent_session_id,
            "sched-1",
            60,
        )
        .await
        .unwrap();
    assert_eq!(completed, 2);

    let store = shared_store.lock().unwrap();
    assert_eq!(
        TaskRecord::current_state(&store, "dag-root").unwrap(),
        TaskLifecycleState::Completed
    );
    assert_eq!(
        TaskRecord::current_state(&store, "dag-child").unwrap(),
        TaskLifecycleState::Completed
    );

    let root_attempts = TaskAttemptRecord::list_for_task(&store, "dag-root").unwrap();
    let child_attempts = TaskAttemptRecord::list_for_task(&store, "dag-child").unwrap();
    assert_eq!(root_attempts.len(), 1);
    assert_eq!(child_attempts.len(), 1);
    assert_eq!(
        root_attempts[0].status,
        AttemptLifecycleState::Completed.to_string()
    );
    assert_eq!(
        child_attempts[0].status,
        AttemptLifecycleState::Completed.to_string()
    );

    let root_child_session = root_attempts[0]
        .session_id
        .clone()
        .expect("root node should link child session");
    let child_child_session = child_attempts[0]
        .session_id
        .clone()
        .expect("child node should link child session");

    let root_turns = Turn::list_for_session(&store, &root_child_session).unwrap();
    let child_turns = Turn::list_for_session(&store, &child_child_session).unwrap();
    assert!(root_turns.len() >= 3);
    assert!(child_turns.len() >= 3);

    let child_events = TaskEventRecord::list_for_task(&store, "dag-child", 0).unwrap();
    let became_ready = child_events.iter().any(|event| {
        event.event_type == "task.state.transition"
            && serde_json::from_str::<serde_json::Value>(&event.payload)
                .ok()
                .and_then(|payload| {
                    payload
                        .get("to")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                })
                .as_deref()
                == Some("ready")
    });
    assert!(
        became_ready,
        "downstream node should become ready after prerequisites complete"
    );
}

#[tokio::test]
async fn scheduler_does_not_run_blocked_nodes_early() {
    let shared_store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let manager = RuntimeManager::new();

    let parent_session_id = {
        let store = shared_store.lock().unwrap();
        Session::create(
            &store,
            NewSession {
                model: "parent-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap()
        .id
    };

    {
        let store = shared_store.lock().unwrap();
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "blocked-root".to_string(),
                parent_task_id: None,
                title: Some("root running".to_string()),
                priority: 10,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: Some(parent_session_id.clone()),
            },
        )
        .unwrap();
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "blocked-child".to_string(),
                parent_task_id: None,
                title: Some("must stay blocked".to_string()),
                priority: 8,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: Some(parent_session_id.clone()),
            },
        )
        .unwrap();

        TaskEdgeRecord::create(
            &store,
            NewTaskEdgeRecord {
                task_id: "blocked-child".to_string(),
                depends_on_task_id: "blocked-root".to_string(),
            },
        )
        .unwrap();

        let root_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-blocked-root".to_string(),
                task_id: "blocked-root".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Running.to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "blocked-root",
            &root_attempt.attempt_id,
            TaskLifecycleState::Running,
        )
        .unwrap();

        let child_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-blocked-child".to_string(),
                task_id: "blocked-child".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Queued.to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "blocked-child",
            &child_attempt.attempt_id,
            TaskLifecycleState::Ready,
        )
        .unwrap();
        TaskAttemptRecord::transition_state(
            &store,
            &child_attempt.attempt_id,
            AttemptLifecycleState::Ready,
        )
        .unwrap();
    }

    let completed = manager
        .execute_scheduler_ready_graph_nodes(
            Arc::clone(&shared_store),
            &parent_session_id,
            "sched-2",
            60,
        )
        .await
        .unwrap();
    assert_eq!(completed, 0);

    let store = shared_store.lock().unwrap();
    let child_attempt = TaskAttemptRecord::get(&store, "attempt-blocked-child").unwrap();
    assert_eq!(
        child_attempt.status,
        AttemptLifecycleState::Blocked.to_string(),
        "blocked node attempt should not run while prerequisite is unfinished"
    );
    assert!(
        child_attempt.session_id.is_none(),
        "blocked node must not have a child session yet"
    );
    assert_eq!(
        TaskRecord::current_state(&store, "blocked-child").unwrap(),
        TaskLifecycleState::Blocked
    );

    let child_events = TaskEventRecord::list_for_task(&store, "blocked-child", 0).unwrap();
    let entered_running = child_events.iter().any(|event| {
        event.event_type == "attempt.state.transition"
            && serde_json::from_str::<serde_json::Value>(&event.payload)
                .ok()
                .and_then(|payload| {
                    payload
                        .get("to")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                })
                .as_deref()
                == Some("running")
    });
    assert!(
        !entered_running,
        "blocked child attempt must not transition to running before prerequisites complete"
    );
}

#[tokio::test]
async fn restart_recovery_resumes_nonterminal_tasks_automatically() {
    let shared_store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let manager = RuntimeManager::new();

    let (parent_session_id, existing_child_session_id) = {
        let store = shared_store.lock().unwrap();
        let parent = Session::create(
            &store,
            NewSession {
                model: "parent-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap();
        let child = Session::create(
            &store,
            NewSession {
                model: "child-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap();
        (parent.id, child.id)
    };

    {
        let store = shared_store.lock().unwrap();
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "restart-recovery-task".to_string(),
                parent_task_id: None,
                title: Some("complete recovered task".to_string()),
                priority: 25,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: Some(parent_session_id.clone()),
            },
        )
        .unwrap();

        let stale_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-restart-recovery-stale".to_string(),
                task_id: "restart-recovery-task".to_string(),
                session_id: Some(existing_child_session_id.clone()),
                status: AttemptLifecycleState::Running.to_string(),
                recovery_checkpoint: Some(
                    serde_json::json!({
                        "scheduler_lease": {
                            "owner_id": "old-runtime",
                            "leased_at": "2020-01-01T00:00:00Z",
                            "lease_expires_at": "2020-01-01T00:01:00Z",
                            "interrupted_at": null,
                            "recoverable": false
                        },
                        "child_bootstrap": {
                            "status": "ready",
                            "retryable": false,
                            "child_session_id": existing_child_session_id,
                            "handoff": {
                                "summary": "resume using durable handoff",
                                "artifact_ids": ["artifact-1", "artifact-2"],
                                "inherited_settings": {
                                    "model": "parent-model",
                                    "provider": "test",
                                    "compact_threshold": 65536
                                }
                            },
                            "error": null
                        }
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "restart-recovery-task",
            &stale_attempt.attempt_id,
            TaskLifecycleState::Running,
        )
        .unwrap();
    }

    let completed = manager
        .recover_nonterminal_attempts_on_startup(
            Arc::clone(&shared_store),
            &parent_session_id,
            "sched-restart",
            60,
        )
        .await
        .unwrap();
    assert_eq!(completed, 1);

    {
        let store = shared_store.lock().unwrap();
        let attempts = TaskAttemptRecord::list_for_task(&store, "restart-recovery-task").unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(
            attempts[0].status,
            AttemptLifecycleState::Retryable.to_string(),
            "stale running attempt should be interrupted and then marked retryable before fresh recovery"
        );
        assert_eq!(
            attempts[1].status,
            AttemptLifecycleState::Completed.to_string(),
            "recovery should automatically create and complete a fresh attempt"
        );
        assert_eq!(
            attempts[1].session_id.as_deref(),
            Some(existing_child_session_id.as_str()),
            "recovery should continue with the durable child session linkage"
        );

        let stale_checkpoint: serde_json::Value =
            serde_json::from_str(attempts[0].recovery_checkpoint.as_deref().unwrap()).unwrap();
        assert_eq!(
            stale_checkpoint["scheduler_lease"]["recoverable"].as_bool(),
            Some(true)
        );
        assert!(
            stale_checkpoint["scheduler_lease"]["interrupted_at"]
                .as_str()
                .is_some(),
            "stale lease should be marked interrupted before rescheduling"
        );

        let recovered_checkpoint: serde_json::Value =
            serde_json::from_str(attempts[1].recovery_checkpoint.as_deref().unwrap()).unwrap();
        assert_eq!(
            recovered_checkpoint["child_bootstrap"]["handoff"]["summary"].as_str(),
            Some("resume using durable handoff")
        );
        assert_eq!(
            recovered_checkpoint["child_bootstrap"]["handoff"]["artifact_ids"],
            serde_json::json!(["artifact-1", "artifact-2"])
        );

        let sessions = Session::list(&store, 20).unwrap();
        assert_eq!(
            sessions.len(),
            2,
            "recovery should reuse the existing child session instead of creating a new one"
        );

        let turns = Turn::list_for_session(&store, &existing_child_session_id).unwrap();
        assert!(turns.iter().any(|turn| {
            turn.role == "user" && turn.content.contains("complete recovered task")
        }));

        let events = TaskEventRecord::list_for_task(&store, "restart-recovery-task", 0).unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "attempt.lease.expired"),
            "recovery should emit lease-expired interruption metadata"
        );
        assert!(
            events.iter().any(|event| {
                event.event_type == "attempt.state.transition"
                    && serde_json::from_str::<serde_json::Value>(&event.payload)
                        .ok()
                        .map(|payload| {
                            payload.get("from").and_then(|value| value.as_str()) == Some("running")
                                && payload.get("to").and_then(|value| value.as_str())
                                    == Some("interrupted")
                        })
                        .unwrap_or(false)
            }),
            "recovery should mark stale leases interrupted before rescheduling"
        );

        assert_eq!(
            TaskRecord::current_state(&store, "restart-recovery-task").unwrap(),
            TaskLifecycleState::Completed,
            "recovered task should settle to terminal completed state"
        );
    }

    let second_pass_completed = manager
        .recover_nonterminal_attempts_on_startup(
            Arc::clone(&shared_store),
            &parent_session_id,
            "sched-restart-second-pass",
            60,
        )
        .await
        .unwrap();
    assert_eq!(
        second_pass_completed, 0,
        "recovery should be idempotent once nonterminal attempts are reconciled"
    );

    let store = shared_store.lock().unwrap();
    let second_pass_attempts =
        TaskAttemptRecord::list_for_task(&store, "restart-recovery-task").unwrap();
    assert_eq!(
        second_pass_attempts.len(),
        2,
        "idempotent recovery must not create duplicate replacement attempts"
    );
}

#[tokio::test]
async fn parent_close_requests_descendant_cancellation_before_recovery() {
    let shared_store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let manager = RuntimeManager::new();

    let parent_session_id = {
        let store = shared_store.lock().unwrap();
        Session::create(
            &store,
            NewSession {
                model: "parent-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap()
        .id
    };

    {
        let store = shared_store.lock().unwrap();
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "parent-close-root".to_string(),
                parent_task_id: None,
                title: Some("parent close root".to_string()),
                priority: 50,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: Some(parent_session_id.clone()),
            },
        )
        .unwrap();
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "parent-close-child".to_string(),
                parent_task_id: Some("parent-close-root".to_string()),
                title: Some("child should be cancelled first".to_string()),
                priority: 40,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: Some(parent_session_id.clone()),
            },
        )
        .unwrap();
        TaskEdgeRecord::create(
            &store,
            NewTaskEdgeRecord {
                task_id: "parent-close-child".to_string(),
                depends_on_task_id: "parent-close-root".to_string(),
            },
        )
        .unwrap();

        let root_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-parent-close-root".to_string(),
                task_id: "parent-close-root".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Queued.to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "parent-close-root",
            &root_attempt.attempt_id,
            TaskLifecycleState::CancelRequested,
        )
        .unwrap();
        TaskAttemptRecord::transition_state(
            &store,
            &root_attempt.attempt_id,
            AttemptLifecycleState::CancelRequested,
        )
        .unwrap();

        let child_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-parent-close-child".to_string(),
                task_id: "parent-close-child".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Queued.to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "parent-close-child",
            &child_attempt.attempt_id,
            TaskLifecycleState::Ready,
        )
        .unwrap();
        TaskAttemptRecord::transition_state(
            &store,
            &child_attempt.attempt_id,
            AttemptLifecycleState::Ready,
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "parent-close-child",
            &child_attempt.attempt_id,
            TaskLifecycleState::Running,
        )
        .unwrap();
        TaskAttemptRecord::transition_state(
            &store,
            &child_attempt.attempt_id,
            AttemptLifecycleState::Running,
        )
        .unwrap();

        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "recovery-independent".to_string(),
                parent_task_id: None,
                title: Some("complete independent recovery".to_string()),
                priority: 10,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: Some(parent_session_id.clone()),
            },
        )
        .unwrap();
        let interrupted_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-recovery-independent-interrupted".to_string(),
                task_id: "recovery-independent".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Interrupted.to_string(),
                recovery_checkpoint: Some(
                    serde_json::json!({
                        "child_bootstrap": {
                            "status": "interrupted",
                            "retryable": true,
                            "child_session_id": null,
                            "handoff": {
                                "summary": "resume independent recovery",
                                "artifact_ids": ["artifact-independent"],
                                "inherited_settings": {
                                    "model": "parent-model",
                                    "provider": "test",
                                    "compact_threshold": 54321
                                }
                            },
                            "error": "runtime restarted"
                        }
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "recovery-independent",
            &interrupted_attempt.attempt_id,
            TaskLifecycleState::Interrupted,
        )
        .unwrap();
    }

    let completed = manager
        .recover_nonterminal_attempts_on_startup(
            Arc::clone(&shared_store),
            &parent_session_id,
            "sched-parent-close",
            60,
        )
        .await
        .unwrap();
    assert_eq!(completed, 1);

    let store = shared_store.lock().unwrap();
    let child_task_state = TaskRecord::current_state(&store, "parent-close-child").unwrap();
    assert_eq!(child_task_state, TaskLifecycleState::CancelRequested);
    let child_attempt = TaskAttemptRecord::get(&store, "attempt-parent-close-child").unwrap();
    assert_eq!(
        child_attempt.status,
        AttemptLifecycleState::CancelRequested.to_string()
    );

    assert_eq!(
        TaskRecord::current_state(&store, "recovery-independent").unwrap(),
        TaskLifecycleState::Completed
    );

    let child_events = TaskEventRecord::list_for_task(&store, "parent-close-child", 0).unwrap();
    let cancel_requested_sequence = child_events
        .iter()
        .find(|event| {
            event.event_type == "task.state.transition"
                && serde_json::from_str::<serde_json::Value>(&event.payload)
                    .ok()
                    .and_then(|payload| {
                        payload
                            .get("to")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    })
                    .as_deref()
                    == Some("cancel_requested")
        })
        .map(|event| event.sequence)
        .expect("descendant cancellation event expected before recovery scheduling");

    let independent_events =
        TaskEventRecord::list_for_task(&store, "recovery-independent", 0).unwrap();
    let independent_claim_sequence = independent_events
        .iter()
        .find(|event| event.event_type == "attempt.lease.claimed")
        .map(|event| event.sequence)
        .expect(
            "independent recovery should be claimed after parent-close descendant cancellation",
        );

    assert!(
        cancel_requested_sequence < independent_claim_sequence,
        "descendant cancellation should be requested before recovery scheduling proceeds"
    );
}

#[tokio::test]
async fn recovery_bootstrap_uses_durable_inherited_settings_over_live_parent_session() {
    let shared_store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let manager = RuntimeManager::new();

    let parent_session_id = {
        let store = shared_store.lock().unwrap();
        let parent = Session::create(
            &store,
            NewSession {
                model: "live-parent-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap();
        Session::update_settings(
            &store,
            &parent.id,
            &serde_json::json!({
                "model": "live-parent-model",
                "provider": "test",
                "compact_threshold": 11111,
            })
            .to_string(),
        )
        .unwrap();
        parent.id
    };

    {
        let store = shared_store.lock().unwrap();
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "recovery-durable-settings".to_string(),
                parent_task_id: None,
                title: Some("complete durable inherited settings recovery".to_string()),
                priority: 10,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: Some(parent_session_id.clone()),
            },
        )
        .unwrap();

        let interrupted_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-recovery-durable-settings-interrupted".to_string(),
                task_id: "recovery-durable-settings".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Interrupted.to_string(),
                recovery_checkpoint: Some(
                    serde_json::json!({
                        "child_bootstrap": {
                            "status": "interrupted",
                            "retryable": true,
                            "child_session_id": null,
                            "handoff": {
                                "summary": "resume with durable inherited settings",
                                "artifact_ids": ["artifact-durable-1"],
                                "inherited_settings": {
                                    "model": "durable-child-model",
                                    "provider": "test",
                                    "compact_threshold": 22222
                                }
                            },
                            "error": "runtime restarted"
                        }
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "recovery-durable-settings",
            &interrupted_attempt.attempt_id,
            TaskLifecycleState::Interrupted,
        )
        .unwrap();
    }

    let completed = manager
        .recover_nonterminal_attempts_on_startup(
            Arc::clone(&shared_store),
            &parent_session_id,
            "sched-durable-settings",
            60,
        )
        .await
        .unwrap();
    assert_eq!(completed, 1);

    let store = shared_store.lock().unwrap();
    let attempts = TaskAttemptRecord::list_for_task(&store, "recovery-durable-settings").unwrap();
    assert_eq!(attempts.len(), 2);
    let recovered_attempt = &attempts[1];
    assert_eq!(
        recovered_attempt.status,
        AttemptLifecycleState::Completed.to_string()
    );

    let child_session_id = recovered_attempt
        .session_id
        .as_deref()
        .expect("recovered attempt should link a child session");
    let child_session = Session::get(&store, child_session_id).unwrap();
    assert_eq!(child_session.model, "durable-child-model");
    assert_eq!(child_session.provider, "test");

    let child_settings_json = child_session
        .settings
        .as_deref()
        .expect("child session settings should persist from durable inherited settings");
    let child_settings: serde_json::Value = serde_json::from_str(child_settings_json).unwrap();
    assert_eq!(
        child_settings.get("model").and_then(|value| value.as_str()),
        Some("durable-child-model")
    );
    assert_eq!(
        child_settings
            .get("compact_threshold")
            .and_then(|value| value.as_u64()),
        Some(22_222)
    );
}

#[tokio::test]
async fn reprioritize_rejects_running_and_terminal_tasks() {
    let shared_store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let manager = RuntimeManager::new();

    {
        let store = shared_store.lock().unwrap();

        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "reprio-running".to_string(),
                parent_task_id: None,
                title: Some("reprio-running".to_string()),
                priority: 1,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )
        .unwrap();
        let running_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-reprio-running".to_string(),
                task_id: "reprio-running".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Queued.to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "reprio-running",
            &running_attempt.attempt_id,
            TaskLifecycleState::Running,
        )
        .unwrap();
        TaskAttemptRecord::transition_state(
            &store,
            &running_attempt.attempt_id,
            AttemptLifecycleState::Running,
        )
        .unwrap();

        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "reprio-completed".to_string(),
                parent_task_id: None,
                title: Some("reprio-completed".to_string()),
                priority: 1,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )
        .unwrap();
        let completed_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-reprio-completed".to_string(),
                task_id: "reprio-completed".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Queued.to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "reprio-completed",
            &completed_attempt.attempt_id,
            TaskLifecycleState::Running,
        )
        .unwrap();
        TaskAttemptRecord::transition_state(
            &store,
            &completed_attempt.attempt_id,
            AttemptLifecycleState::Running,
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "reprio-completed",
            &completed_attempt.attempt_id,
            TaskLifecycleState::Completed,
        )
        .unwrap();
        TaskAttemptRecord::transition_state(
            &store,
            &completed_attempt.attempt_id,
            AttemptLifecycleState::Completed,
        )
        .unwrap();

        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "reprio-ready".to_string(),
                parent_task_id: None,
                title: Some("reprio-ready".to_string()),
                priority: 1,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )
        .unwrap();
        let ready_attempt = TaskAttemptRecord::create(
            &store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-reprio-ready".to_string(),
                task_id: "reprio-ready".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Ready.to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            &store,
            "reprio-ready",
            &ready_attempt.attempt_id,
            TaskLifecycleState::Ready,
        )
        .unwrap();
    }

    let running_err = manager
        .reprioritize_task(&shared_store, "reprio-running", 99)
        .unwrap_err();
    assert!(format!("{running_err:#}").contains("queued/ready"));

    let terminal_err = manager
        .reprioritize_task(&shared_store, "reprio-completed", 99)
        .unwrap_err();
    assert!(format!("{terminal_err:#}").contains("queued/ready"));

    manager
        .reprioritize_task(&shared_store, "reprio-ready", 33)
        .unwrap();
    let store = shared_store.lock().unwrap();
    let ready = TaskRecord::get(&store, "reprio-ready").unwrap();
    assert_eq!(ready.priority, 33);
}

#[tokio::test]
async fn autonomous_spawn_respects_depth_and_budget_limits() {
    let store = Store::open_memory().unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-policy-parent".to_string(),
            parent_task_id: None,
            title: Some("policy-parent".to_string()),
            priority: 1,
            policy_snapshot: delegation_policy_json(
                true,
                0,
                1,
                2,
                25,
                &["test"],
                &["parent-model"],
                &["read_file", "report_status"],
                "ask",
                "request_cancel_descendants",
            ),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    let child = spawn_autonomous_child_task_with_policy(
        &store,
        "task-policy-parent",
        "task-policy-child",
        Some("policy-child".to_string()),
        2,
        Some(
            &serde_json::json!({
                "budget": 10,
                "approved_tools": ["read_file"],
                "tool_approval_mode": "never"
            })
            .to_string(),
        ),
    )
    .unwrap();

    assert_eq!(child.parent_task_id.as_deref(), Some("task-policy-parent"));
    assert_eq!(child.parent_close_policy, "request_cancel_descendants");

    let child_policy: serde_json::Value = serde_json::from_str(&child.policy_snapshot).unwrap();
    assert_eq!(child_policy["current_depth"], 1);
    assert_eq!(child_policy["max_depth"], 1);
    assert_eq!(child_policy["budget"], 10);
    assert_eq!(
        child_policy["approved_tools"],
        serde_json::json!(["read_file"])
    );
    assert_eq!(child_policy["tool_approval_mode"], "never");
    assert_eq!(
        child_policy["parent_close_policy"],
        "request_cancel_descendants"
    );

    let depth_err = spawn_autonomous_child_task_with_policy(
        &store,
        "task-policy-child",
        "task-policy-grandchild",
        Some("policy-grandchild".to_string()),
        3,
        None,
    )
    .unwrap_err();
    assert!(format!("{depth_err:#}").contains("depth limit"));

    let budget_request = serde_json::json!({ "budget": 30 }).to_string();
    let budget_err = spawn_autonomous_child_task_with_policy(
        &store,
        "task-policy-parent",
        "task-policy-budget-too-large",
        Some("policy-budget-too-large".to_string()),
        4,
        Some(&budget_request),
    )
    .unwrap_err();
    assert!(format!("{budget_err:#}").contains("budget"));

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-no-autonomous".to_string(),
            parent_task_id: None,
            title: Some("task-no-autonomous".to_string()),
            priority: 1,
            policy_snapshot: delegation_policy_json(
                false,
                0,
                2,
                1,
                10,
                &["test"],
                &["parent-model"],
                &["read_file"],
                "ask",
                "request_cancel_descendants",
            ),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    let autonomous_err = spawn_autonomous_child_task_with_policy(
        &store,
        "task-no-autonomous",
        "task-no-autonomous-child",
        Some("task-no-autonomous-child".to_string()),
        5,
        None,
    )
    .unwrap_err();
    assert!(format!("{autonomous_err:#}").contains("does not allow autonomous spawning"));
}

#[tokio::test]
async fn child_task_cannot_widen_parent_permissions() {
    let store = Store::open_memory().unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "task-parent-permissions".to_string(),
            parent_task_id: None,
            title: Some("task-parent-permissions".to_string()),
            priority: 1,
            policy_snapshot: delegation_policy_json(
                true,
                0,
                4,
                1,
                40,
                &["test"],
                &["parent-model"],
                &["read_file"],
                "ask",
                "request_cancel_descendants",
            ),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    for (index, request, expected_fragment) in [
        (
            0usize,
            serde_json::json!({ "max_concurrency": 2 }).to_string(),
            "max_concurrency",
        ),
        (
            1usize,
            serde_json::json!({ "allowed_providers": ["test", "openai"] }).to_string(),
            "allowed_providers",
        ),
        (
            2usize,
            serde_json::json!({ "allowed_models": ["parent-model", "child-model"] }).to_string(),
            "allowed_models",
        ),
        (
            3usize,
            serde_json::json!({ "approved_tools": ["read_file", "shell"] }).to_string(),
            "approved_tools",
        ),
        (
            4usize,
            serde_json::json!({ "tool_approval_mode": "auto" }).to_string(),
            "tool_approval_mode",
        ),
        (
            5usize,
            serde_json::json!({ "parent_close_policy": "detach_descendants" }).to_string(),
            "parent_close_policy",
        ),
    ] {
        let err = spawn_autonomous_child_task_with_policy(
            &store,
            "task-parent-permissions",
            &format!("task-widen-attempt-{index}"),
            Some(format!("task-widen-attempt-{index}")),
            10 + i64::try_from(index).unwrap(),
            Some(&request),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains(expected_fragment),
            "expected error to mention {expected_fragment}, got: {err:#}"
        );
    }
}

#[tokio::test]
async fn main_agent_can_delegate_task_with_handoff_and_policy_check() {
    let store = Store::open_memory().unwrap();
    let (events, _receiver) = event_channel();

    let mut runtime = SessionRuntime::new(
        &store,
        ResolvedAuth {
            provider: "test".to_string(),
            api_key: "test-key".to_string(),
            base_url: "http://unused".to_string(),
            account_id: None,
        },
        Some("test-model"),
        None,
        events,
        CompactConfig::default(),
        kley::tools::default_registry(std::env::current_dir().unwrap()),
        "system".to_string(),
        RuntimeHooks::default(),
    )
    .unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "parent-delegation-task".to_string(),
            parent_task_id: None,
            title: Some("parent-delegation-task".to_string()),
            priority: 10,
            policy_snapshot: delegation_policy_json(
                true,
                0,
                3,
                2,
                20,
                &["test"],
                &["test-model"],
                &["delegate_task", "report_status", "read_file"],
                "ask",
                "request_cancel_descendants",
            ),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    let delegate_control = serde_json::json!({
        "type": "tool_call",
        "name": "delegate_task",
        "arguments": {
            "parent_task_id": "parent-delegation-task",
            "child_task_id": "child-delegation-task",
            "title": "delegated child",
            "priority": 7,
            "handoff_brief": "Investigate logs and return summary",
            "artifact_ids": ["artifact://logs/alpha"],
            "requested_policy_json": serde_json::json!({
                "budget": 5,
                "approved_tools": ["report_status"],
                "tool_approval_mode": "never"
            })
            .to_string(),
            "after_sequence": 0
        }
    })
    .to_string();
    let delegate_prompt = format!("{CONTROL_BLOCK_START}{delegate_control}{CONTROL_BLOCK_END}");
    runtime.submit_prompt(delegate_prompt).await.unwrap();

    let child = TaskRecord::get(&store, "child-delegation-task").unwrap();
    assert_eq!(
        child.parent_task_id.as_deref(),
        Some("parent-delegation-task")
    );
    let child_policy: serde_json::Value = serde_json::from_str(&child.policy_snapshot).unwrap();
    assert_eq!(child_policy["current_depth"], 1);
    assert_eq!(child_policy["budget"], 5);
    assert_eq!(
        child_policy["approved_tools"],
        serde_json::json!(["report_status"])
    );

    let child_edges = TaskEdgeRecord::list_for_task(&store, "child-delegation-task").unwrap();
    assert_eq!(child_edges.len(), 1);
    assert_eq!(child_edges[0].depends_on_task_id, "parent-delegation-task");

    let attempts = TaskAttemptRecord::list_for_task(&store, "child-delegation-task").unwrap();
    assert_eq!(attempts.len(), 1);
    let attempt = &attempts[0];
    let checkpoint: serde_json::Value =
        serde_json::from_str(attempt.recovery_checkpoint.as_deref().unwrap()).unwrap();
    assert_eq!(
        checkpoint
            .get("child_bootstrap")
            .and_then(|value| value.get("handoff"))
            .and_then(|value| value.get("summary"))
            .and_then(|value| value.as_str()),
        Some("Investigate logs and return summary")
    );

    let report_control = serde_json::json!({
        "type": "tool_call",
        "name": "report_status",
        "arguments": {
            "summary": "track delegated child",
            "task_id": "child-delegation-task",
            "after_sequence": 0
        }
    })
    .to_string();
    let report_prompt = format!("{CONTROL_BLOCK_START}{report_control}{CONTROL_BLOCK_END}");
    runtime.submit_prompt(report_prompt).await.unwrap();

    let turns = Turn::list_for_session(&store, runtime.session_id()).unwrap();
    let report_turn = turns
        .iter()
        .rev()
        .find(|turn| turn.kind == "function_call_output")
        .unwrap();
    let report_payload: serde_json::Value = serde_json::from_str(&report_turn.content).unwrap();
    let report_output = report_payload
        .get("output")
        .and_then(|value| value.as_str())
        .unwrap();
    let report_json: serde_json::Value = serde_json::from_str(report_output).unwrap();
    assert_eq!(report_json["task_id"], "child-delegation-task");
    assert!(report_json["events"].as_array().unwrap().len() >= 2);
    assert!(report_json["next_after_sequence"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn agent_delegation_denied_when_policy_blocks_spawn() {
    let store = Store::open_memory().unwrap();
    let (events, _receiver) = event_channel();

    let mut runtime = SessionRuntime::new(
        &store,
        ResolvedAuth {
            provider: "test".to_string(),
            api_key: "test-key".to_string(),
            base_url: "http://unused".to_string(),
            account_id: None,
        },
        Some("test-model"),
        None,
        events,
        CompactConfig::default(),
        kley::tools::default_registry(std::env::current_dir().unwrap()),
        "system".to_string(),
        RuntimeHooks::default(),
    )
    .unwrap();

    TaskRecord::create(
        &store,
        NewTaskRecord {
            task_id: "parent-policy-denied".to_string(),
            parent_task_id: None,
            title: Some("parent-policy-denied".to_string()),
            priority: 3,
            policy_snapshot: delegation_policy_json(
                false,
                0,
                3,
                1,
                10,
                &["test"],
                &["test-model"],
                &["delegate_task", "report_status"],
                "ask",
                "request_cancel_descendants",
            ),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
        },
    )
    .unwrap();

    let delegate_control = serde_json::json!({
        "type": "tool_call",
        "name": "delegate_task",
        "arguments": {
            "parent_task_id": "parent-policy-denied",
            "child_task_id": "child-policy-denied",
            "handoff_brief": "Try to delegate despite policy"
        }
    })
    .to_string();
    let prompt = format!("{CONTROL_BLOCK_START}{delegate_control}{CONTROL_BLOCK_END}");
    runtime.submit_prompt(prompt).await.unwrap();

    assert!(TaskRecord::get(&store, "child-policy-denied").is_err());

    let turns = Turn::list_for_session(&store, runtime.session_id()).unwrap();
    let output_turn = turns
        .iter()
        .rev()
        .find(|turn| turn.kind == "function_call_output")
        .unwrap();
    let output_payload: serde_json::Value = serde_json::from_str(&output_turn.content).unwrap();
    let output = output_payload
        .get("output")
        .and_then(|value| value.as_str())
        .unwrap();
    assert!(output.contains("autonomous spawn denied"));
}
