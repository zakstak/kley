use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use chrono::{Duration, Utc};
use futures_util::{SinkExt, StreamExt};
use kley::events::AgentEvent;
use kley::lsp::{LspClient, LspClientError, LspClientFactory};
use kley::provider::test::{CONTROL_BLOCK_END, CONTROL_BLOCK_START};
use kley::store::{
    self, AttemptLifecycleState, NewSession, NewTaskAttemptRecord, NewTaskEdgeRecord,
    NewTaskEventRecord, NewTaskRecord, NewTurn, Session, SessionStatus, SharedStore, Store,
    TaskAttemptRecord, TaskEdgeRecord, TaskEventRecord, TaskLifecycleState, TaskRecord, Turn,
};
use kley::web::state::{MockWebAuthService, WebAppState, WebAuthService};
use kley::web::ws::runtime_event_to_ui_event;
use serde_json::Value;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tower::util::ServiceExt;

mod web {
    use super::*;

    fn controlled_tool_prompt(name: &str, arguments: Value) -> String {
        let control = serde_json::json!({
            "type": "tool_call",
            "name": name,
            "arguments": arguments,
        });
        format!("invoke tool {CONTROL_BLOCK_START}{control}{CONTROL_BLOCK_END}")
    }

    struct ReadyDiagnosticsClient;

    impl LspClient for ReadyDiagnosticsClient {
        fn request(&self, method: &str, _params: Value) -> Result<Value, LspClientError> {
            match method {
                "initialize" => Ok(serde_json::json!({ "capabilities": {} })),
                _ => Ok(serde_json::json!({ "items": [] })),
            }
        }
    }

    struct ReadyDiagnosticsFactory;

    impl LspClientFactory for ReadyDiagnosticsFactory {
        fn create(
            &self,
            _command: &[String],
            _workspace_root: &Path,
        ) -> Result<Arc<dyn LspClient>, String> {
            Ok(Arc::new(ReadyDiagnosticsClient))
        }
    }

    struct FailingStartupFactory {
        message: String,
    }

    impl FailingStartupFactory {
        fn new(message: &str) -> Self {
            Self {
                message: message.to_string(),
            }
        }
    }

    impl LspClientFactory for FailingStartupFactory {
        fn create(
            &self,
            _command: &[String],
            _workspace_root: &Path,
        ) -> Result<Arc<dyn LspClient>, String> {
            Err(self.message.clone())
        }
    }

    struct TestServer {
        addr: std::net::SocketAddr,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn spawn_server() -> TestServer {
        spawn_server_with_state(test_state())
            .await
            .expect("server should start")
    }

    async fn spawn_server_with_state(state: WebAppState) -> anyhow::Result<TestServer> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let app = kley::web::router::app_with_state(state);

        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Ok(TestServer { addr, task })
    }

    fn test_state() -> WebAppState {
        let store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
        WebAppState::new(store)
    }

    fn test_state_with_auth_service(auth_service: Arc<dyn WebAuthService>) -> WebAppState {
        let store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
        WebAppState::with_auth_service(store, auth_service)
    }

    fn test_state_with_mock_openai() -> WebAppState {
        test_state_with_auth_service(Arc::new(MockWebAuthService::default()))
    }

    async fn seed_test_session(state: &WebAppState, title: &str) -> Session {
        let store = state.store.clone();
        let title = title.to_string();
        store::store_run(&store, move |s| {
            let session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &session.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;
            Session::update_title(s, &session.id, &title)?;
            Ok(session)
        })
        .await
        .unwrap()
    }

    async fn connect_ws(
        addr: std::net::SocketAddr,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        connect_ws_path(addr, "/ws").await
    }

    async fn connect_ws_path(
        addr: std::net::SocketAddr,
        path: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!("ws://{addr}{path}");
        let (socket, _) = connect_async(url).await.unwrap();
        socket
    }

    async fn connect_mock_ws(
        addr: std::net::SocketAddr,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        connect_ws_path(addr, "/ws/mock").await
    }

    async fn recv_json(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        let message = socket.next().await.unwrap().unwrap();
        let text = message.into_text().unwrap();
        serde_json::from_str(&text).unwrap()
    }

    async fn recv_until_type(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        event_type: &str,
    ) -> Value {
        loop {
            let frame = recv_json(socket).await;
            if frame["type"] == event_type {
                return frame;
            }
        }
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let response = kley::web::router::app_with_state(test_state())
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn root_serves_html_shell() {
        let response = kley::web::router::app_with_state(test_state())
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(content_type.starts_with("text/html"));

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("<!doctype html>"));
        assert!(html.contains("Kley web"));
    }

    #[tokio::test]
    async fn root_serves_bindery_shell_markers() {
        let response = kley::web::router::app_with_state(test_state())
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();

        for marker in [
            "data-testid=\"app-shell\"",
            "data-testid=\"session-list\"",
            "data-testid=\"transcript\"",
            "data-testid=\"composer\"",
            "data-testid=\"composer-submit\"",
            "data-testid=\"abort-button\"",
            "data-testid=\"tool-card\"",
            "data-testid=\"inspector-panel\"",
            "data-testid=\"status-pill\"",
            "data-testid=\"session-settings-form\"",
            "data-testid=\"provider-select\"",
            "data-testid=\"model-select\"",
            "data-testid=\"session-settings-submit\"",
            "data-testid=\"login-form\"",
            "data-testid=\"login-provider-select\"",
            "data-testid=\"login-zai-controls\"",
            "data-testid=\"login-api-key\"",
            "data-testid=\"login-submit\"",
            "data-testid=\"login-openai-controls\"",
            "data-testid=\"login-openai-start\"",
            "data-testid=\"selected-session-meta\"",
            "data-testid=\"filter-chip-all\"",
            "data-testid=\"filter-chip-messages\"",
            "data-testid=\"filter-chip-tools\"",
        ] {
            assert!(html.contains(marker), "missing marker: {marker}");
        }

        for unsupported in [
            "btn-fork-picker",
            "btn-task-start",
            "btn-task-complete",
            "btn-model-picker",
            "prompt-image-input",
            "mock-preset-buttons",
            "btn-session-picker",
        ] {
            assert!(
                !html.contains(unsupported),
                "unsupported control rendered: {unsupported}"
            );
        }
    }

    #[tokio::test]
    async fn root_serves_bindery_icon() {
        let response = kley::web::router::app_with_state(test_state())
            .oneshot(
                Request::builder()
                    .uri("/assets/bindery-icon.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(content_type.starts_with("image/svg+xml"));

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let icon = String::from_utf8(body.to_vec()).unwrap();
        assert!(icon.contains("<svg"));
        assert!(icon.contains("shape-rendering=\"crispEdges\""));
    }

    #[tokio::test]
    async fn ws_connect_receives_bootstrap_state() {
        let state = test_state();
        let store = state.store.clone();
        let seeded_session = store::store_run(&store, |s| {
            let session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &session.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;
            Session::update_title(s, &session.id, "Bootstrap Session")?;
            Turn::append(
                s,
                NewTurn {
                    session_id: session.id.clone(),
                    kind: "message".to_string(),
                    role: "user".to_string(),
                    content: "Persisted bootstrap prompt".to_string(),
                    model: None,
                    tokens_in: None,
                    tokens_out: None,
                },
            )?;
            Turn::append(
                s,
                NewTurn {
                    session_id: session.id.clone(),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: "Persisted bootstrap reply".to_string(),
                    model: Some("test-model".to_string()),
                    tokens_in: None,
                    tokens_out: None,
                },
            )?;
            Ok(session)
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", seeded_session.id),
        )
        .await;

        let frame = recv_json(&mut socket).await;
        assert_eq!(frame["type"], "state.snapshot");
        assert_eq!(frame["protocol_version"], 1);
        assert_eq!(frame["session_id"], seeded_session.id);
        assert!(frame["event_id"].as_str().unwrap().starts_with("evt-"));
        assert!(frame["sessions"].as_array().is_some());
        assert!(frame["transcript"].as_array().is_some());
        assert_eq!(frame["selected_session"]["session_id"], seeded_session.id);
        assert_eq!(frame["selected_session"]["title"], "Bootstrap Session");
        assert_eq!(frame["selected_session"]["status"], "active");
        assert!(frame["auth"].is_object());
        assert!(frame["selected_session"]["created_at"].as_str().is_some());
        assert!(frame["selected_session"]["updated_at"].as_str().is_some());
        assert_eq!(frame["selected_session"]["provider"], "test");
        assert_eq!(frame["selected_session"]["model"], "test-model");
        assert!(
            frame["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry["updated_at"].as_str().is_some())
        );
        let transcript = frame["transcript"].as_array().unwrap();
        assert!(transcript.is_empty());
    }

    #[tokio::test]
    async fn ws_connect_prefers_requested_session() {
        let state = test_state();
        let store = state.store.clone();

        let (first_session, second_session) = store::store_run(&store, |s| {
            let first = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &first.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &first.id, "Requested Session")?;

            let second = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &second.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &second.id, "Newest Session")?;

            Ok((first, second))
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket =
            connect_ws_path(server.addr, &format!("/ws?session_id={}", first_session.id)).await;

        let frame = recv_json(&mut socket).await;
        assert_eq!(frame["type"], "state.snapshot");
        assert_eq!(frame["session_id"], first_session.id);
        assert_eq!(frame["selected_session"]["title"], "Requested Session");
        assert_ne!(frame["session_id"], second_session.id);
    }

    #[tokio::test]
    async fn ws_connect_backfills_missing_settings_with_compact_threshold() {
        let state = test_state();
        let store = state.store.clone();
        let seeded_session = store::store_run(&store, |s| {
            Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", seeded_session.id),
        )
        .await;
        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");
        assert_eq!(bootstrap["session_id"], seeded_session.id);

        let stored_settings = store::store_run(&store, {
            let session_id = seeded_session.id.clone();
            move |s| Ok(Session::get(s, &session_id)?.settings)
        })
        .await
        .unwrap()
        .expect("settings should be backfilled");

        let settings: Value = serde_json::from_str(&stored_settings).unwrap();
        assert_eq!(settings["model"], "test-model");
        assert_eq!(settings["provider"], "test");
        assert_eq!(
            settings["compact_threshold"],
            serde_json::json!(kley::compact::CompactConfig::default().threshold_chars)
        );
    }

    #[tokio::test]
    async fn session_settings_update_persists_model_and_provider() {
        let state = test_state();
        let store = state.store.clone();
        let seeded_session = store::store_run(&store, |s| {
            Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", seeded_session.id),
        )
        .await;

        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");
        assert_eq!(bootstrap["selected_session"]["provider"], "test");
        assert_eq!(bootstrap["selected_session"]["model"], "test-model");

        socket
            .send(Message::Text(format!(
                r#"{{"type":"session.settings.update","request_id":"req-settings-1","session_id":"{}","provider":"openai","model":"gpt-4.1"}}"#,
                seeded_session.id
            )))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-settings-1");
        assert_eq!(ack["data"]["updated"], true);
        assert_eq!(ack["data"]["provider"], "openai");
        assert_eq!(ack["data"]["model"], "gpt-4.1");

        let snapshot = recv_json(&mut socket).await;
        assert_eq!(snapshot["type"], "state.snapshot");
        assert_eq!(snapshot["selected_session"]["provider"], "openai");
        assert_eq!(snapshot["selected_session"]["model"], "gpt-4.1");

        let stored = store::store_run(&store, {
            let session_id = seeded_session.id.clone();
            move |s| Session::get(s, &session_id)
        })
        .await
        .unwrap();

        assert_eq!(stored.provider, "openai");
        assert_eq!(stored.model, "gpt-4.1");
        let settings: Value =
            serde_json::from_str(&stored.settings.expect("settings should exist")).unwrap();
        assert_eq!(settings["provider"], "openai");
        assert_eq!(settings["model"], "gpt-4.1");
        assert_eq!(
            settings["compact_threshold"],
            serde_json::json!(kley::compact::CompactConfig::default().threshold_chars)
        );
    }

    #[tokio::test]
    async fn openai_api_key_login_is_rejected_in_web_ui() {
        let server = spawn_server_with_state(test_state_with_mock_openai())
            .await
            .unwrap();
        let mut socket = connect_mock_ws(server.addr).await;
        let _bootstrap = recv_json(&mut socket).await;

        socket
            .send(Message::Text(
                r#"{"type":"auth.login","request_id":"req-openai-api-key","provider":"openai","api_key":"sk-not-allowed"}"#
                    .to_string(),
            ))
            .await
            .unwrap();

        let rejection = recv_json(&mut socket).await;
        assert_eq!(rejection["type"], "response.error");
        assert_eq!(rejection["request_id"], "req-openai-api-key");
        assert_eq!(rejection["error"]["code"], "auth_flow_mismatch");
    }

    #[tokio::test]
    async fn openai_browser_login_start_and_complete_updates_auth_snapshot() {
        let server = spawn_server_with_state(test_state_with_mock_openai())
            .await
            .unwrap();
        let mut socket = connect_ws(server.addr).await;

        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");
        assert_eq!(bootstrap["auth"]["openai_logged_in"], false);
        assert_eq!(bootstrap["auth"]["pending_openai_login"], false);

        socket
            .send(Message::Text(
                r#"{"type":"auth.openai.start","request_id":"req-openai-start"}"#.to_string(),
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-openai-start");
        assert_eq!(ack["data"]["provider"], "openai");
        assert_eq!(ack["data"]["started"], true);
        assert!(ack["data"]["authorize_url"].as_str().is_some());

        let pending_snapshot = recv_json(&mut socket).await;
        assert_eq!(pending_snapshot["type"], "state.snapshot");
        assert_eq!(pending_snapshot["auth"]["pending_openai_login"], true);
        assert_eq!(pending_snapshot["auth"]["openai_logged_in"], false);

        socket
            .send(Message::Text(
                r#"{"type":"auth.openai.complete","request_id":"req-openai-complete","callback_input":"mock-openai-code"}"#.to_string(),
            ))
            .await
            .unwrap();

        let completion = recv_json(&mut socket).await;
        assert_eq!(completion["type"], "response.ok");
        assert_eq!(completion["request_id"], "req-openai-complete");
        assert_eq!(completion["data"]["provider"], "openai");
        assert_eq!(completion["data"]["logged_in"], true);

        let logged_in_snapshot = recv_json(&mut socket).await;
        assert_eq!(logged_in_snapshot["type"], "state.snapshot");
        assert_eq!(logged_in_snapshot["auth"]["pending_openai_login"], false);
        assert_eq!(logged_in_snapshot["auth"]["openai_logged_in"], true);
        assert_eq!(logged_in_snapshot["auth"]["active_provider"], "openai");
    }

    #[tokio::test]
    async fn openai_complete_with_verifier_state_requires_pending_login() {
        let server = spawn_server_with_state(test_state_with_mock_openai())
            .await
            .unwrap();
        let mut socket = connect_ws(server.addr).await;

        let _bootstrap = recv_json(&mut socket).await;

        socket
            .send(Message::Text(
                r#"{"type":"auth.openai.complete","request_id":"req-openai-complete-no-start","callback_input":"mock-openai-code","verifier":"client-supplied-verifier","state":"client-supplied-state"}"#.to_string(),
            ))
            .await
            .unwrap();

        let completion = recv_json(&mut socket).await;
        assert_eq!(completion["type"], "response.error");
        assert_eq!(completion["request_id"], "req-openai-complete-no-start");
        assert_eq!(completion["error"]["code"], "auth_completion_failed");
        assert!(
            completion["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("no OpenAI login is currently pending")
        );
    }

    #[tokio::test]
    async fn zai_login_updates_auth_snapshot() {
        let server = spawn_server_with_state(test_state_with_mock_openai())
            .await
            .unwrap();
        let mut socket = connect_ws(server.addr).await;
        let _bootstrap = recv_json(&mut socket).await;

        socket
            .send(Message::Text(
                r#"{"type":"auth.login","request_id":"req-zai-login","provider":"zai","api_key":"zai-test-key"}"#
                    .to_string(),
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-zai-login");
        assert_eq!(ack["data"]["provider"], "zai");
        assert_eq!(ack["data"]["logged_in"], true);

        let snapshot = recv_json(&mut socket).await;
        assert_eq!(snapshot["type"], "state.snapshot");
        assert_eq!(snapshot["auth"]["zai_logged_in"], true);
        assert_eq!(snapshot["auth"]["active_provider"], "zai");
    }

    #[tokio::test]
    async fn ws_connect_chooses_available_session_when_latest_is_busy() {
        let state = test_state();
        let store = state.store.clone();

        let (first_session, second_session) = store::store_run(&store, |s| {
            let first = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &first.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &first.id, "Available Session")?;

            let second = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &second.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &second.id, "Busy Session")?;

            Ok((first, second))
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut first_socket = connect_ws(server.addr).await;
        let first_bootstrap = recv_json(&mut first_socket).await;
        assert_eq!(first_bootstrap["session_id"], second_session.id);

        let mut second_socket = connect_ws(server.addr).await;
        let second_bootstrap = recv_json(&mut second_socket).await;
        assert_eq!(second_bootstrap["type"], "state.snapshot");
        assert_eq!(second_bootstrap["session_id"], first_session.id);
        assert_eq!(
            second_bootstrap["selected_session"]["title"],
            "Available Session"
        );
    }

    #[tokio::test]
    async fn ws_connect_returns_session_busy_for_requested_busy_session() {
        let state = test_state();
        let store = state.store.clone();

        let (first_session, second_session) = store::store_run(&store, |s| {
            let first = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &first.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &first.id, "Available Session")?;

            let second = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &second.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &second.id, "Busy Requested Session")?;

            Ok((first, second))
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut first_socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", second_session.id),
        )
        .await;
        let first_bootstrap = recv_json(&mut first_socket).await;
        assert_eq!(first_bootstrap["session_id"], second_session.id);

        let mut second_socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", second_session.id),
        )
        .await;
        let rejection = recv_json(&mut second_socket).await;
        assert_eq!(rejection["type"], "response.error");
        assert_eq!(rejection["request_id"], "attach");
        assert_eq!(rejection["error"]["code"], "session_busy");
        assert_eq!(
            rejection["error"]["details"]["session_id"],
            second_session.id
        );
        assert_ne!(
            rejection["error"]["details"]["session_id"],
            first_session.id
        );
    }

    #[tokio::test]
    async fn ws_connect_returns_session_busy_when_all_sessions_are_busy() {
        let state = test_state();
        let store = state.store.clone();

        let (first_session, second_session) = store::store_run(&store, |s| {
            let first = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &first.id, r#"{"model":"test-model","provider":"test"}"#)?;

            let second = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &second.id, r#"{"model":"test-model","provider":"test"}"#)?;

            Ok((first, second))
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();

        let mut first_socket =
            connect_ws_path(server.addr, &format!("/ws?session_id={}", first_session.id)).await;
        let first_bootstrap = recv_json(&mut first_socket).await;
        assert_eq!(first_bootstrap["session_id"], first_session.id);

        let mut second_socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", second_session.id),
        )
        .await;
        let second_bootstrap = recv_json(&mut second_socket).await;
        assert_eq!(second_bootstrap["session_id"], second_session.id);

        let mut third_socket = connect_ws(server.addr).await;
        let rejection = recv_json(&mut third_socket).await;
        assert_eq!(rejection["type"], "response.error");
        assert_eq!(rejection["request_id"], "attach");
        assert_eq!(rejection["error"]["code"], "session_busy");
        let busy_session_id = rejection["error"]["details"]["session_id"]
            .as_str()
            .unwrap();
        assert!(busy_session_id == first_session.id || busy_session_id == second_session.id);
    }

    #[tokio::test]
    async fn ws_connect_honors_requested_session_outside_recent_window() {
        let state = test_state();
        let store = state.store.clone();

        let requested_session = store::store_run(&store, |s| {
            let requested = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &requested.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;
            Session::update_title(s, &requested.id, "Requested Older Session")?;
            Ok(requested)
        })
        .await
        .unwrap();

        for index in 0..55 {
            store::store_run(&store, move |s| {
                let session = Session::create(
                    s,
                    NewSession {
                        model: "test-model".to_string(),
                        provider: "test".to_string(),
                    },
                )?;
                Session::update_settings(
                    s,
                    &session.id,
                    r#"{"model":"test-model","provider":"test"}"#,
                )?;
                Session::update_title(s, &session.id, &format!("Recent Session {index}"))?;
                Ok(())
            })
            .await
            .unwrap();
        }

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", requested_session.id),
        )
        .await;

        let frame = recv_json(&mut socket).await;
        assert_eq!(frame["type"], "state.snapshot");
        assert_eq!(frame["session_id"], requested_session.id);
        assert_eq!(
            frame["selected_session"]["title"],
            "Requested Older Session"
        );
        assert!(
            frame["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["session_id"] == requested_session.id)
        );
    }

    #[tokio::test]
    async fn session_load_replays_history() {
        let state = test_state();
        let store = state.store.clone();

        let (first_session, second_session) = store::store_run(&store, |s| {
            let first = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &first.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &first.id, "First Session")?;
            Turn::append(
                s,
                NewTurn {
                    session_id: first.id.clone(),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: "First session transcript".to_string(),
                    model: Some("test-model".to_string()),
                    tokens_in: None,
                    tokens_out: None,
                },
            )?;

            let second = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &second.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &second.id, "Second Session")?;
            Turn::append(
                s,
                NewTurn {
                    session_id: second.id.clone(),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: "Second session transcript".to_string(),
                    model: Some("test-model".to_string()),
                    tokens_in: None,
                    tokens_out: None,
                },
            )?;

            Ok((first, second))
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", second_session.id),
        )
        .await;

        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["session_id"], second_session.id);

        socket
            .send(Message::Text(format!(
                r#"{{"type":"session.load","request_id":"req-load-1","session_id":"{}"}}"#,
                first_session.id
            )))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-load-1");
        assert_eq!(ack["data"]["session_id"], first_session.id);

        let snapshot = recv_json(&mut socket).await;
        assert_eq!(snapshot["type"], "state.snapshot");
        assert_eq!(snapshot["session_id"], first_session.id);
        assert_eq!(snapshot["selected_session"]["session_id"], first_session.id);
        assert_eq!(snapshot["selected_session"]["title"], "First Session");

        let transcript = snapshot["transcript"].as_array().unwrap();
        assert!(
            transcript
                .iter()
                .any(|entry| entry["content"] == "First session transcript")
        );
        assert!(
            !transcript
                .iter()
                .any(|entry| entry["content"] == "Second session transcript")
        );
    }

    #[tokio::test]
    async fn session_load_rejects_switch_while_turn_is_streaming() {
        let state = test_state();
        let store = state.store.clone();

        let first_session = store::store_run(&store, |s| {
            let first = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &first.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &first.id, "Streaming Session")?;
            Ok(first)
        })
        .await
        .unwrap();

        let second_session = store::store_run(&store, |s| {
            let second = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &second.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &second.id, "Other Session")?;
            Ok(second)
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket =
            connect_ws_path(server.addr, &format!("/ws?session_id={}", first_session.id)).await;

        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["session_id"], first_session.id);

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-load-busy-prompt","session_id":"{}","prompt":"abortable response please stop"}}"#,
                    first_session.id
                )
                ,
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        let turn_id = ack["data"]["turn_id"].as_str().unwrap().to_string();

        assert_eq!(recv_json(&mut socket).await["type"], "turn.started");
        assert_eq!(recv_json(&mut socket).await["type"], "message.started");
        assert_eq!(recv_json(&mut socket).await["type"], "message.delta");

        socket
            .send(Message::Text(format!(
                r#"{{"type":"session.load","request_id":"req-load-busy","session_id":"{}"}}"#,
                second_session.id
            )))
            .await
            .unwrap();

        let load_error = recv_until_type(&mut socket, "response.error").await;
        assert_eq!(load_error["type"], "response.error");
        assert_eq!(load_error["request_id"], "req-load-busy");
        assert_eq!(load_error["error"]["code"], "turn_in_progress");
        assert_eq!(
            load_error["error"]["details"]["session_id"],
            first_session.id
        );
        assert_eq!(load_error["error"]["details"]["turn_id"], turn_id);

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"turn.abort","request_id":"req-load-busy-abort","session_id":"{}","turn_id":"{}"}}"#,
                    first_session.id, turn_id
                )
                ,
            ))
            .await
            .unwrap();

        let abort_ack = recv_json(&mut socket).await;
        assert_eq!(abort_ack["type"], "response.ok");
        assert_eq!(abort_ack["request_id"], "req-load-busy-abort");

        let failed = recv_until_type(&mut socket, "turn.failed").await;
        assert_eq!(failed["session_id"], first_session.id);
        assert_eq!(failed["turn_id"], turn_id);
    }

    #[tokio::test]
    async fn invalid_command_returns_error_without_disconnect() {
        let server = spawn_server().await;
        let mut socket = connect_ws(server.addr).await;

        let _bootstrap = recv_json(&mut socket).await;

        socket
            .send(Message::Text(
                r#"{"type":"mock.invalid","request_id":"req-bad-1"}"#.to_string(),
            ))
            .await
            .unwrap();

        let error = recv_json(&mut socket).await;
        assert_eq!(error["type"], "response.error");
        assert_eq!(error["request_id"], "req-bad-1");
        assert_eq!(error["error"]["code"], "invalid_command");

        socket
            .send(Message::Text(
                r#"{"type":"state.get","request_id":"req-ok-1"}"#.to_string(),
            ))
            .await
            .unwrap();

        let ok = recv_json(&mut socket).await;
        assert_eq!(ok["type"], "response.ok");
        assert_eq!(ok["request_id"], "req-ok-1");
        assert_eq!(ok["data"]["protocol_version"], 1);
    }

    #[tokio::test]
    async fn prompt_stream_emits_ordered_events() {
        let server = spawn_server().await;
        let mut socket = connect_ws(server.addr).await;

        let bootstrap = recv_json(&mut socket).await;
        let session_id = bootstrap["session_id"].as_str().unwrap().to_string();

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-prompt-1","session_id":"{session_id}","prompt":"hello"}}"#
                )
                ,
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-prompt-1");

        let mut event_types = Vec::new();
        loop {
            let frame = recv_json(&mut socket).await;
            let event_type = frame["type"].as_str().unwrap().to_string();
            event_types.push(event_type.clone());
            if event_type == "turn.completed" {
                break;
            }
        }

        assert_eq!(
            event_types,
            vec![
                "turn.started",
                "message.started",
                "message.delta",
                "message.completed",
                "turn.completed",
            ]
        );
    }

    #[tokio::test]
    async fn mock_prompt_stream_emits_ordered_events() {
        let server = spawn_server().await;
        let mut socket = connect_mock_ws(server.addr).await;

        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");
        assert_eq!(bootstrap["session_id"], "sess-mock-001");

        socket
            .send(Message::Text(
                r#"{"type":"prompt.submit","request_id":"req-mock-prompt-1","session_id":"sess-mock-001","prompt":"mock prompt"}"#.to_string(),
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-mock-prompt-1");
        assert_eq!(ack["data"]["session_id"], "sess-mock-001");
        assert!(ack["data"]["accepted"].as_bool().unwrap_or(false));
        assert_eq!(ack["data"]["turn_id"], "turn-mock-0001");
        assert_eq!(ack["data"]["message_id"], "msg-mock-0001");

        let mut event_types = Vec::new();
        loop {
            let frame = recv_json(&mut socket).await;
            let event_type = frame["type"].as_str().unwrap().to_string();
            event_types.push(event_type.clone());
            if event_type == "turn.completed" {
                break;
            }
        }

        assert_eq!(
            event_types,
            vec![
                "turn.started",
                "message.started",
                "message.delta",
                "message.delta",
                "message.completed",
                "turn.completed",
            ],
        );
    }

    #[tokio::test]
    async fn mock_tool_completed_can_emit_edit_observation() {
        let server = spawn_server().await;
        let mut socket = connect_mock_ws(server.addr).await;

        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");

        socket
            .send(Message::Text(
                r#"{"type":"prompt.submit","request_id":"req-mock-edit-observation","session_id":"sess-mock-001","prompt":"please run tool with edit observation"}"#.to_string(),
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-mock-edit-observation");

        let completed = recv_until_type(&mut socket, "tool.completed").await;
        assert_eq!(completed["tool_name"], "read");
        assert_eq!(
            completed["edit_observation"]["engine"],
            serde_json::json!("mock-engine")
        );
        assert_eq!(
            completed["edit_observation"]["path"],
            serde_json::json!("templates/index.html")
        );
        assert_eq!(
            completed["edit_observation"]["applied_count"],
            serde_json::json!(2)
        );
        assert_eq!(
            completed["edit_observation"]["stale_reference_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            completed["edit_observation"]["artifact_path"],
            serde_json::json!("/tmp/mock-edit-artifact.json")
        );
    }

    #[tokio::test]
    async fn tool_events_round_trip() {
        let server = spawn_server().await;
        let mut socket = connect_ws(server.addr).await;

        let bootstrap = recv_json(&mut socket).await;
        let session_id = bootstrap["session_id"].as_str().unwrap().to_string();

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-tool-1","session_id":"{session_id}","prompt":"please use a tool"}}"#
                )
                ,
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");

        let mut started_id = None;
        let mut completed_id = None;
        loop {
            let frame = recv_json(&mut socket).await;
            let event_type = frame["type"].as_str().unwrap();

            if event_type == "tool.started" {
                started_id = frame["tool_call_id"].as_str().map(|s| s.to_string());
                assert!(
                    frame["tool_name"]
                        .as_str()
                        .unwrap()
                        .contains("unknown_tool")
                );
            }

            if event_type == "tool.completed" {
                completed_id = frame["tool_call_id"].as_str().map(|s| s.to_string());
                assert!(
                    frame["tool_name"]
                        .as_str()
                        .unwrap()
                        .contains("unknown_tool")
                );
            }

            if event_type == "turn.completed" {
                break;
            }
        }

        let started_id = started_id.expect("tool.started should be emitted");
        let completed_id = completed_id.expect("tool.completed should be emitted");
        assert_eq!(started_id, completed_id);
    }

    #[tokio::test]
    async fn prompt_submit_updates_transcript_and_tool_panel() {
        let server = spawn_server().await;
        let mut socket = connect_ws(server.addr).await;

        let bootstrap = recv_json(&mut socket).await;
        let session_id = bootstrap["session_id"].as_str().unwrap().to_string();

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-ui-prompt-1","session_id":"{session_id}","prompt":"please use a tool"}}"#
                )
                ,
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-ui-prompt-1");

        let mut saw_message_started = false;
        let mut saw_message_delta = false;
        let mut saw_message_completed = false;
        let mut tool_started: Option<String> = None;
        let mut tool_completed: Option<String> = None;

        loop {
            let frame = recv_json(&mut socket).await;
            let event_type = frame["type"].as_str().unwrap();
            match event_type {
                "message.started" => saw_message_started = true,
                "message.delta" => saw_message_delta = true,
                "message.completed" => saw_message_completed = true,
                "tool.started" => {
                    tool_started = frame["tool_call_id"].as_str().map(|s| s.to_string());
                }
                "tool.completed" => {
                    tool_completed = frame["tool_call_id"].as_str().map(|s| s.to_string());
                }
                "turn.completed" => break,
                _ => {}
            }
        }

        assert!(saw_message_started);
        assert!(saw_message_delta);
        assert!(saw_message_completed);
        assert_eq!(
            tool_started.expect("tool.started expected"),
            tool_completed.expect("tool.completed expected")
        );

        socket
            .send(Message::Text(
                r#"{"type":"state.get","request_id":"req-ui-state-1"}"#.to_string(),
            ))
            .await
            .unwrap();

        let state_frame = recv_json(&mut socket).await;
        assert_eq!(state_frame["type"], "response.ok");
        assert_eq!(state_frame["request_id"], "req-ui-state-1");

        let transcript = state_frame["data"]["transcript"].as_array().unwrap();
        assert!(
            transcript
                .iter()
                .any(|entry| entry["role"] == "user" && entry["content"] == "please use a tool")
        );
        assert!(transcript.iter().any(|entry| {
            entry["role"] == "assistant"
                && entry["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("tool")
        }));
    }

    #[tokio::test]
    async fn session_load_switches_visible_history() {
        let state = test_state();
        let store = state.store.clone();

        let (first_session, second_session) = store::store_run(&store, |s| {
            let first = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &first.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &first.id, "First Session")?;
            Turn::append(
                s,
                NewTurn {
                    session_id: first.id.clone(),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: "First session transcript".to_string(),
                    model: Some("test-model".to_string()),
                    tokens_in: None,
                    tokens_out: None,
                },
            )?;

            let second = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(s, &second.id, r#"{"model":"test-model","provider":"test"}"#)?;
            Session::update_title(s, &second.id, "Second Session")?;
            Turn::append(
                s,
                NewTurn {
                    session_id: second.id.clone(),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: "Second session transcript".to_string(),
                    model: Some("test-model".to_string()),
                    tokens_in: None,
                    tokens_out: None,
                },
            )?;

            Ok((first, second))
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", second_session.id),
        )
        .await;

        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");
        assert_eq!(bootstrap["session_id"], second_session.id);

        socket
            .send(Message::Text(format!(
                r#"{{"type":"session.load","request_id":"req-ui-load-1","session_id":"{}"}}"#,
                first_session.id
            )))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-ui-load-1");

        let snapshot = recv_json(&mut socket).await;
        assert_eq!(snapshot["type"], "state.snapshot");
        assert_eq!(snapshot["session_id"], first_session.id);
        assert_eq!(snapshot["selected_session"]["title"], "First Session");

        let transcript = snapshot["transcript"].as_array().unwrap();
        assert!(
            transcript
                .iter()
                .any(|entry| entry["content"] == "First session transcript")
        );
        assert!(
            !transcript
                .iter()
                .any(|entry| entry["content"] == "Second session transcript")
        );

        let sessions = snapshot["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["session_id"], first_session.id);
    }

    #[tokio::test]
    async fn abort_keeps_session_reusable() {
        let server = spawn_server().await;
        let mut socket = connect_ws(server.addr).await;

        let bootstrap = recv_json(&mut socket).await;
        let session_id = bootstrap["session_id"].as_str().unwrap().to_string();

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-ui-abort-prompt","session_id":"{session_id}","prompt":"abortable response please stop"}}"#
                )
                ,
            ))
            .await
            .unwrap();

        let first_ack = recv_json(&mut socket).await;
        assert_eq!(first_ack["type"], "response.ok");
        let first_turn_id = first_ack["data"]["turn_id"].as_str().unwrap().to_string();

        let started = recv_until_type(&mut socket, "turn.started").await;
        assert_eq!(started["turn_id"], first_turn_id);
        let message_started = recv_until_type(&mut socket, "message.started").await;
        assert_eq!(message_started["turn_id"], first_turn_id);
        let delta = recv_until_type(&mut socket, "message.delta").await;
        assert_eq!(delta["turn_id"], first_turn_id);

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"turn.abort","request_id":"req-ui-abort-turn","session_id":"{session_id}","turn_id":"{first_turn_id}"}}"#
                )
                ,
            ))
            .await
            .unwrap();

        let abort_ack = recv_until_type(&mut socket, "response.ok").await;
        assert_eq!(abort_ack["request_id"], "req-ui-abort-turn");

        let failed = recv_until_type(&mut socket, "turn.failed").await;
        assert_eq!(failed["request_id"], "req-ui-abort-prompt");
        assert_eq!(failed["turn_id"], first_turn_id);
        assert_eq!(failed["error"], "aborted");

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-ui-after-abort","session_id":"{session_id}","prompt":"hello after abort"}}"#
                )
                ,
            ))
            .await
            .unwrap();

        let second_ack = recv_json(&mut socket).await;
        assert_eq!(second_ack["type"], "response.ok");
        assert_eq!(second_ack["request_id"], "req-ui-after-abort");

        let completed = recv_until_type(&mut socket, "turn.completed").await;
        assert_eq!(completed["request_id"], "req-ui-after-abort");

        socket
            .send(Message::Text(
                r#"{"type":"state.get","request_id":"req-ui-post-abort-state"}"#.to_string(),
            ))
            .await
            .unwrap();

        let state_after = recv_json(&mut socket).await;
        assert_eq!(state_after["type"], "response.ok");
        assert!(state_after["data"]["active_turn"].is_null());

        let transcript = state_after["data"]["transcript"].as_array().unwrap();
        assert!(
            transcript
                .iter()
                .any(|entry| entry["role"] == "user" && entry["content"] == "hello after abort")
        );
    }

    #[tokio::test]
    async fn attach_second_controller_returns_session_busy() {
        let state = test_state();
        let server = spawn_server_with_state(state).await.unwrap();

        let mut socket1 = connect_ws(server.addr).await;
        let bootstrap1 = recv_json(&mut socket1).await;
        assert_eq!(bootstrap1["type"], "state.snapshot");

        let mut socket2 = connect_ws(server.addr).await;
        let rejection = recv_json(&mut socket2).await;
        assert_eq!(rejection["type"], "response.error");
        assert_eq!(rejection["error"]["code"], "session_busy");
        assert!(rejection["error"]["details"]["session_id"].is_string());
        assert!(rejection["error"]["details"]["active_controller_id"].is_string());

        socket1
            .send(Message::Text(
                r#"{"type":"state.get","request_id":"req-still-active"}"#.to_string(),
            ))
            .await
            .unwrap();
        let ok = recv_json(&mut socket1).await;
        assert_eq!(ok["type"], "response.ok");
        assert_eq!(ok["request_id"], "req-still-active");
    }

    #[tokio::test]
    async fn reconnect_bootstrap_skips_persisted_history_replay() {
        let state = test_state();
        let store = state.store.clone();

        let seeded_session = store::store_run(&store, |s| {
            let session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &session.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;
            Turn::append(
                s,
                NewTurn {
                    session_id: session.id.clone(),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: "Persisted history message".to_string(),
                    model: Some("test-model".to_string()),
                    tokens_in: None,
                    tokens_out: None,
                },
            )?;
            Ok(session)
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state.clone()).await.unwrap();
        let mut socket1 = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", seeded_session.id),
        )
        .await;
        let bootstrap1 = recv_json(&mut socket1).await;
        assert_eq!(bootstrap1["session_id"], seeded_session.id);

        socket1
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-reconnect-1","session_id":"{}","prompt":"abortable response please stop"}}"#,
                    seeded_session.id
                )
                ,
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket1).await;
        assert_eq!(ack["type"], "response.ok");
        assert!(ack["data"]["turn_id"].is_string());

        let started = recv_json(&mut socket1).await;
        let msg_started = recv_json(&mut socket1).await;
        let delta = recv_json(&mut socket1).await;
        assert_eq!(started["type"], "turn.started");
        assert_eq!(msg_started["type"], "message.started");
        assert_eq!(delta["type"], "message.delta");

        drop(socket1);

        let mut socket2 = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={}", seeded_session.id),
        )
        .await;
        let bootstrap2 = recv_json(&mut socket2).await;
        assert_eq!(bootstrap2["type"], "state.snapshot");
        assert_eq!(bootstrap2["session_id"], seeded_session.id);

        let transcript = bootstrap2["transcript"].as_array().unwrap();
        assert!(transcript.is_empty());

        let completed = recv_until_type(&mut socket2, "turn.completed").await;
        assert_eq!(completed["request_id"], "req-reconnect-1");
    }

    #[tokio::test]
    async fn disconnect_does_not_complete_session() {
        let state = test_state();
        let store = state.store.clone();
        let session = store::store_run(&store, |s| {
            Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state.clone()).await.unwrap();

        let mut socket = connect_ws(server.addr).await;
        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["session_id"], session.id);
        drop(socket);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let status = store::store_run(&store, move |s| Ok(Session::get(s, &session.id)?.status))
            .await
            .unwrap();
        assert_eq!(status, SessionStatus::Active);
    }

    #[tokio::test]
    async fn abort_command_emits_turn_failed_and_runtime_stops() {
        let server = spawn_server().await;
        let mut socket = connect_ws(server.addr).await;

        let bootstrap = recv_json(&mut socket).await;
        let session_id = bootstrap["session_id"].as_str().unwrap().to_string();

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-abort-prompt","session_id":"{session_id}","prompt":"abortable response please stop"}}"#
                )
                ,
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        let turn_id = ack["data"]["turn_id"].as_str().unwrap().to_string();

        let started = recv_until_type(&mut socket, "turn.started").await;
        assert_eq!(started["turn_id"], turn_id);
        let message_started = recv_until_type(&mut socket, "message.started").await;
        assert_eq!(message_started["turn_id"], turn_id);
        let delta = recv_until_type(&mut socket, "message.delta").await;
        assert_eq!(delta["turn_id"], turn_id);

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"turn.abort","request_id":"req-abort-turn","session_id":"{session_id}","turn_id":"{turn_id}"}}"#
                )
                ,
            ))
            .await
            .unwrap();

        let abort_ack = recv_until_type(&mut socket, "response.ok").await;
        assert_eq!(abort_ack["type"], "response.ok");
        assert_eq!(abort_ack["request_id"], "req-abort-turn");
        assert_eq!(abort_ack["data"]["turn_id"], turn_id);

        let failed = recv_until_type(&mut socket, "turn.failed").await;
        assert_eq!(failed["request_id"], "req-abort-prompt");
        assert_eq!(failed["turn_id"], turn_id);
        assert_eq!(failed["error"], "aborted");

        socket
            .send(Message::Text(
                r#"{"type":"state.get","request_id":"req-post-abort-state"}"#.to_string(),
            ))
            .await
            .unwrap();

        let state_after_abort = recv_json(&mut socket).await;
        assert_eq!(state_after_abort["type"], "response.ok");
        assert!(state_after_abort["data"]["active_turn"].is_null());

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-after-abort","session_id":"{session_id}","prompt":"hello after abort"}}"#
                )
                ,
            ))
            .await
            .unwrap();

        let second_ack = recv_json(&mut socket).await;
        assert_eq!(second_ack["type"], "response.ok");
        assert_eq!(second_ack["request_id"], "req-after-abort");
        let completed = recv_until_type(&mut socket, "turn.completed").await;
        assert_eq!(completed["request_id"], "req-after-abort");
    }

    pub(super) async fn task_event_cursor_replays_from_last_seen_sequence() {
        let state = test_state();
        let store = state.store.clone();

        let (last_seen_sequence, replayed_sequence) = store::store_run(&store, |s| {
            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-replay".to_string(),
                    parent_task_id: None,
                    title: Some("Replay task".to_string()),
                    priority: 5,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel_descendants".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: None,
                },
            )?;
            let replay_attempt = TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-replay".to_string(),
                    task_id: "task-replay".to_string(),
                    session_id: None,
                    status: "running".to_string(),
                    recovery_checkpoint: None,
                },
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-noise".to_string(),
                    parent_task_id: None,
                    title: Some("Noise task".to_string()),
                    priority: 1,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel_descendants".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: None,
                },
            )?;
            let noise_attempt = TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-noise".to_string(),
                    task_id: "task-noise".to_string(),
                    session_id: None,
                    status: "running".to_string(),
                    recovery_checkpoint: None,
                },
            )?;

            let first_seen = TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-replay".to_string(),
                    attempt_id: replay_attempt.attempt_id.clone(),
                    session_id: None,
                    event_type: "attempt.started".to_string(),
                    payload: r#"{"step":1}"#.to_string(),
                },
            )?;
            let noise_event = TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-noise".to_string(),
                    attempt_id: noise_attempt.attempt_id.clone(),
                    session_id: None,
                    event_type: "attempt.started".to_string(),
                    payload: r#"{"step":"noise"}"#.to_string(),
                },
            )?;
            let replayed = TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-replay".to_string(),
                    attempt_id: replay_attempt.attempt_id.clone(),
                    session_id: None,
                    event_type: "attempt.completed".to_string(),
                    payload: r#"{"step":2}"#.to_string(),
                },
            )?;

            assert!(first_seen.sequence < noise_event.sequence);
            assert!(noise_event.sequence < replayed.sequence);

            Ok((first_seen.sequence, replayed.sequence))
        })
        .await
        .unwrap();

        let replay = store::store_run(&store, move |s| {
            TaskEventRecord::list_for_task(s, "task-replay", last_seen_sequence)
        })
        .await
        .unwrap();

        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].task_id, "task-replay");
        assert_eq!(replay[0].sequence, replayed_sequence);
        assert_eq!(replay[0].event_type, "attempt.completed");
        assert_eq!(replay[0].payload, r#"{"step":2}"#);
    }

    pub(super) async fn task_event_cursor_rejects_gaps_for_unknown_task() {
        let state = test_state();
        let store = state.store.clone();

        let known_sequence = store::store_run(&store, |s| {
            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-known".to_string(),
                    parent_task_id: None,
                    title: Some("Known task".to_string()),
                    priority: 3,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel_descendants".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: None,
                },
            )?;
            let attempt = TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-known".to_string(),
                    task_id: "task-known".to_string(),
                    session_id: None,
                    status: "running".to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            let event = TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-known".to_string(),
                    attempt_id: attempt.attempt_id,
                    session_id: None,
                    event_type: "attempt.started".to_string(),
                    payload: "{}".to_string(),
                },
            )?;

            Ok(event.sequence)
        })
        .await
        .unwrap();

        let err = store::store_run(&store, move |s| {
            TaskEventRecord::list_for_task(s, "task-missing", known_sequence)
        })
        .await
        .unwrap_err();

        let error_text = format!("{err:#}");
        assert!(error_text.contains("task event stream not found"));
        assert!(error_text.contains("task-missing"));
    }
    pub(super) async fn task_snapshot_includes_graph_and_attempt_state() {
        let state = test_state();
        let store = state.store.clone();

        let (parent_session_id, child_session_id) = store::store_run(&store, |s| {
            let parent_session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &parent_session.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;

            let child_session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            let unrelated_session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            let child_session_id = child_session.id.clone();
            let unrelated_session_id = unrelated_session.id.clone();

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-root".to_string(),
                    parent_task_id: None,
                    title: Some("Root task".to_string()),
                    priority: 90,
                    policy_snapshot: serde_json::json!({
                        "max_depth": 3,
                        "max_concurrency": 2,
                        "budget": 5,
                    })
                    .to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: Some(
                        serde_json::json!({
                            "scheduler": {"owner_id": "sched-main"}
                        })
                        .to_string(),
                    ),
                    owner_session_id: Some(parent_session.id.clone()),
                },
            )?;
            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-dependency".to_string(),
                    parent_task_id: None,
                    title: Some("Dependency task".to_string()),
                    priority: 20,
                    policy_snapshot: serde_json::json!({"budget": 1}).to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(parent_session.id.clone()),
                },
            )?;
            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-child".to_string(),
                    parent_task_id: Some("task-root".to_string()),
                    title: Some("Child task".to_string()),
                    priority: 50,
                    policy_snapshot: serde_json::json!({"budget": 2}).to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(parent_session.id.clone()),
                },
            )?;
            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-unrelated".to_string(),
                    parent_task_id: None,
                    title: Some("Unrelated task".to_string()),
                    priority: 5,
                    policy_snapshot: serde_json::json!({"budget": 99}).to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(unrelated_session_id.clone()),
                },
            )?;

            TaskEdgeRecord::create(
                s,
                NewTaskEdgeRecord {
                    task_id: "task-root".to_string(),
                    depends_on_task_id: "task-dependency".to_string(),
                },
            )?;
            TaskEdgeRecord::create(
                s,
                NewTaskEdgeRecord {
                    task_id: "task-child".to_string(),
                    depends_on_task_id: "task-root".to_string(),
                },
            )?;

            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-root-1".to_string(),
                    task_id: "task-root".to_string(),
                    session_id: Some(child_session_id.clone()),
                    status: "running".to_string(),
                    recovery_checkpoint: Some(
                        serde_json::json!({
                            "child_bootstrap": {
                                "status": "linked",
                                "child_session_id": child_session_id,
                                "retryable": false,
                            }
                        })
                        .to_string(),
                    ),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-dependency-1".to_string(),
                    task_id: "task-dependency".to_string(),
                    session_id: None,
                    status: "completed".to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-child-1".to_string(),
                    task_id: "task-child".to_string(),
                    session_id: None,
                    status: "queued".to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-unrelated-1".to_string(),
                    task_id: "task-unrelated".to_string(),
                    session_id: Some(unrelated_session_id.clone()),
                    status: "running".to_string(),
                    recovery_checkpoint: None,
                },
            )?;

            TaskRecord::transition_state(
                s,
                "task-root",
                "attempt-root-1",
                TaskLifecycleState::Running,
            )?;
            TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-root".to_string(),
                    attempt_id: "attempt-root-1".to_string(),
                    session_id: Some(child_session_id.clone()),
                    event_type: "attempt.child_session.linked".to_string(),
                    payload: serde_json::json!({
                        "session_id": child_session_id,
                    })
                    .to_string(),
                },
            )?;

            TaskRecord::transition_state(
                s,
                "task-dependency",
                "attempt-dependency-1",
                TaskLifecycleState::Ready,
            )?;
            TaskRecord::transition_state(
                s,
                "task-dependency",
                "attempt-dependency-1",
                TaskLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "task-dependency",
                "attempt-dependency-1",
                TaskLifecycleState::Completed,
            )?;
            TaskRecord::transition_state(
                s,
                "task-unrelated",
                "attempt-unrelated-1",
                TaskLifecycleState::Running,
            )?;

            Ok((parent_session.id, child_session_id))
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket =
            connect_ws_path(server.addr, &format!("/ws?session_id={parent_session_id}")).await;
        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");
        assert_eq!(bootstrap["session_id"], parent_session_id);

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.watch","request_id":"req-task-snapshot","session_id":"{}","task_id":"task-root"}}"#,
                    parent_session_id
                ),
            ))
            .await
            .unwrap();

        let ack = recv_until_type(&mut socket, "response.ok").await;
        assert_eq!(ack["request_id"], "req-task-snapshot");
        assert_eq!(ack["data"]["watching"], true);
        assert_eq!(ack["data"]["task_id"], "task-root");
        assert_eq!(ack["data"]["cursor"]["after_sequence"], 0);

        let list_snapshot = recv_until_type(&mut socket, "task.list.snapshot").await;
        let detail_snapshot = recv_until_type(&mut socket, "task.detail.snapshot").await;

        assert_eq!(list_snapshot["request_id"], "req-task-snapshot");
        assert_eq!(list_snapshot["session_id"], parent_session_id);
        assert_eq!(list_snapshot["task_id"], "task-root");
        assert_eq!(
            list_snapshot["cursor"]["latest_sequence"],
            ack["data"]["cursor"]["latest_sequence"]
        );

        let nodes = list_snapshot["graph"]["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3);
        assert!(nodes.iter().all(|node| node["task_id"] != "task-unrelated"));

        let root_node = nodes
            .iter()
            .find(|node| node["task_id"] == "task-root")
            .unwrap();
        assert_eq!(root_node["state"], "running");
        assert_eq!(root_node["latest_attempt_id"], "attempt-root-1");
        assert_eq!(root_node["latest_attempt_state"], "running");
        assert_eq!(root_node["child_session_id"], child_session_id);

        let dependency_node = nodes
            .iter()
            .find(|node| node["task_id"] == "task-dependency")
            .unwrap();
        assert_eq!(dependency_node["state"], "completed");

        let child_node = nodes
            .iter()
            .find(|node| node["task_id"] == "task-child")
            .unwrap();
        assert_eq!(child_node["parent_task_id"], "task-root");
        assert_eq!(child_node["latest_attempt_state"], "queued");

        let edges = list_snapshot["graph"]["edges"].as_array().unwrap();
        assert!(edges.iter().any(|edge| {
            edge["task_id"] == "task-root" && edge["depends_on_task_id"] == "task-dependency"
        }));
        assert!(edges.iter().any(|edge| {
            edge["task_id"] == "task-child" && edge["depends_on_task_id"] == "task-root"
        }));

        assert_eq!(detail_snapshot["request_id"], "req-task-snapshot");
        assert_eq!(detail_snapshot["task"]["task_id"], "task-root");
        assert_eq!(detail_snapshot["task"]["priority"], 90);
        assert_eq!(detail_snapshot["task"]["state"], "running");
        assert_eq!(
            detail_snapshot["task"]["parent_close_policy"],
            "request_cancel"
        );
        assert_eq!(
            detail_snapshot["task"]["child_session_id"],
            child_session_id
        );
        assert_eq!(detail_snapshot["task"]["policy_snapshot"]["max_depth"], 3);
        assert_eq!(
            detail_snapshot["task"]["recovery_checkpoint"]["scheduler"]["owner_id"],
            "sched-main"
        );

        let attempts = detail_snapshot["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["attempt_id"], "attempt-root-1");
        assert_eq!(attempts[0]["state"], "running");
        assert_eq!(attempts[0]["child_session_id"], child_session_id);
        assert_eq!(
            attempts[0]["recovery_checkpoint"]["child_bootstrap"]["status"],
            "linked"
        );
    }

    pub(super) async fn task_watch_reconnect_from_cursor_recovers_missed_events() {
        let state = test_state();
        let store = state.store.clone();

        let (parent_session_id, first_cursor, child_session_id) = store::store_run(&store, |s| {
            let parent_session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &parent_session.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;

            let child_session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            let unrelated_session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            let child_session_id = child_session.id.clone();
            let unrelated_session_id = unrelated_session.id.clone();

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-watch".to_string(),
                    parent_task_id: None,
                    title: Some("Watch target".to_string()),
                    priority: 40,
                    policy_snapshot: serde_json::json!({"budget": 4}).to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(parent_session.id.clone()),
                },
            )?;
            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-watch-unrelated".to_string(),
                    parent_task_id: None,
                    title: Some("Unrelated watch task".to_string()),
                    priority: 1,
                    policy_snapshot: serde_json::json!({"budget": 1}).to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(unrelated_session_id.clone()),
                },
            )?;

            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-watch-1".to_string(),
                    task_id: "task-watch".to_string(),
                    session_id: Some(child_session_id.clone()),
                    status: "running".to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-watch-unrelated-1".to_string(),
                    task_id: "task-watch-unrelated".to_string(),
                    session_id: Some(unrelated_session_id.clone()),
                    status: "running".to_string(),
                    recovery_checkpoint: None,
                },
            )?;

            let first = TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-watch".to_string(),
                    attempt_id: "attempt-watch-1".to_string(),
                    session_id: None,
                    event_type: "task.state.transition".to_string(),
                    payload: serde_json::json!({
                        "from": "queued",
                        "to": "running",
                    })
                    .to_string(),
                },
            )?;
            let second = TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-watch".to_string(),
                    attempt_id: "attempt-watch-1".to_string(),
                    session_id: Some(child_session_id.clone()),
                    event_type: "attempt.child_session.linked".to_string(),
                    payload: serde_json::json!({
                        "session_id": child_session_id,
                    })
                    .to_string(),
                },
            )?;
            TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-watch-unrelated".to_string(),
                    attempt_id: "attempt-watch-unrelated-1".to_string(),
                    session_id: Some(unrelated_session_id),
                    event_type: "attempt.child_session.linked".to_string(),
                    payload: serde_json::json!({
                        "session_id": "sess-unrelated",
                    })
                    .to_string(),
                },
            )?;

            Ok((
                parent_session.id,
                (first.sequence, second.sequence),
                child_session_id,
            ))
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state.clone()).await.unwrap();
        let mut socket1 =
            connect_ws_path(server.addr, &format!("/ws?session_id={parent_session_id}")).await;
        let bootstrap1 = recv_json(&mut socket1).await;
        assert_eq!(bootstrap1["session_id"], parent_session_id);

        socket1
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.watch","request_id":"req-task-watch-1","session_id":"{}","task_id":"task-watch"}}"#,
                    parent_session_id
                ),
            ))
            .await
            .unwrap();

        let ack1 = recv_until_type(&mut socket1, "response.ok").await;
        assert_eq!(ack1["data"]["cursor"]["latest_sequence"], first_cursor.1);
        let _ = recv_until_type(&mut socket1, "task.list.snapshot").await;
        let _ = recv_until_type(&mut socket1, "task.detail.snapshot").await;

        let replayed_first = recv_until_type(&mut socket1, "task.event").await;
        let replayed_second = recv_until_type(&mut socket1, "task.event").await;
        assert_eq!(replayed_first["task_id"], "task-watch");
        assert_eq!(replayed_second["task_id"], "task-watch");
        assert_eq!(replayed_first["sequence"], first_cursor.0);
        assert_eq!(replayed_second["sequence"], first_cursor.1);

        drop(socket1);

        let later_sequences = store::store_run(&store, move |s| {
            let third = TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-watch".to_string(),
                    attempt_id: "attempt-watch-1".to_string(),
                    session_id: None,
                    event_type: "attempt.state.transition".to_string(),
                    payload: serde_json::json!({
                        "from": "running",
                        "to": "completed",
                    })
                    .to_string(),
                },
            )?;
            TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-watch-unrelated".to_string(),
                    attempt_id: "attempt-watch-unrelated-1".to_string(),
                    session_id: None,
                    event_type: "task.state.transition".to_string(),
                    payload: serde_json::json!({
                        "from": "running",
                        "to": "failed",
                    })
                    .to_string(),
                },
            )?;
            let fourth = TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-watch".to_string(),
                    attempt_id: "attempt-watch-1".to_string(),
                    session_id: Some(child_session_id.clone()),
                    event_type: "attempt.child_session.linked".to_string(),
                    payload: serde_json::json!({
                        "session_id": child_session_id,
                    })
                    .to_string(),
                },
            )?;

            Ok((third.sequence, fourth.sequence))
        })
        .await
        .unwrap();

        let mut socket2 =
            connect_ws_path(server.addr, &format!("/ws?session_id={parent_session_id}")).await;
        let bootstrap2 = recv_json(&mut socket2).await;
        assert_eq!(bootstrap2["session_id"], parent_session_id);

        socket2
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.watch","request_id":"req-task-watch-2","session_id":"{}","task_id":"task-watch","after_sequence":{}}}"#,
                    parent_session_id,
                    first_cursor.1
                ),
            ))
            .await
            .unwrap();

        let ack2 = recv_until_type(&mut socket2, "response.ok").await;
        assert_eq!(ack2["data"]["cursor"]["after_sequence"], first_cursor.1);
        assert_eq!(ack2["data"]["cursor"]["latest_sequence"], later_sequences.1);
        let _ = recv_until_type(&mut socket2, "task.list.snapshot").await;
        let _ = recv_until_type(&mut socket2, "task.detail.snapshot").await;

        let recovered_first = recv_until_type(&mut socket2, "task.event").await;
        let recovered_second = recv_until_type(&mut socket2, "task.event").await;
        assert_eq!(recovered_first["task_id"], "task-watch");
        assert_eq!(recovered_second["task_id"], "task-watch");
        assert_eq!(recovered_first["sequence"], later_sequences.0);
        assert_eq!(recovered_second["sequence"], later_sequences.1);
        assert_ne!(recovered_first["sequence"], first_cursor.0);
        assert_ne!(recovered_second["sequence"], first_cursor.1);

        let no_duplicate = tokio::time::timeout(
            std::time::Duration::from_millis(150),
            recv_until_type(&mut socket2, "task.event"),
        )
        .await;
        assert!(no_duplicate.is_err());

        drop(socket2);

        let latest_sequence = later_sequences.1;
        let final_sequence = store::store_run(&store, move |s| {
            let final_event = TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-watch".to_string(),
                    attempt_id: "attempt-watch-1".to_string(),
                    session_id: None,
                    event_type: "task.state.transition".to_string(),
                    payload: serde_json::json!({
                        "from": "running",
                        "to": "completed",
                    })
                    .to_string(),
                },
            )?;
            TaskEventRecord::append(
                s,
                NewTaskEventRecord {
                    task_id: "task-watch-unrelated".to_string(),
                    attempt_id: "attempt-watch-unrelated-1".to_string(),
                    session_id: None,
                    event_type: "task.state.transition".to_string(),
                    payload: serde_json::json!({
                        "from": "failed",
                        "to": "completed",
                    })
                    .to_string(),
                },
            )?;
            Ok(final_event.sequence)
        })
        .await
        .unwrap();

        let mut socket3 =
            connect_ws_path(server.addr, &format!("/ws?session_id={parent_session_id}")).await;
        let bootstrap3 = recv_json(&mut socket3).await;
        assert_eq!(bootstrap3["session_id"], parent_session_id);

        socket3
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.watch","request_id":"req-task-watch-3","session_id":"{}","task_id":"task-watch","after_sequence":{}}}"#,
                    parent_session_id,
                    latest_sequence
                ),
            ))
            .await
            .unwrap();

        let ack3 = recv_until_type(&mut socket3, "response.ok").await;
        assert_eq!(ack3["data"]["cursor"]["after_sequence"], latest_sequence);
        assert_eq!(ack3["data"]["cursor"]["latest_sequence"], final_sequence);
        let _ = recv_until_type(&mut socket3, "task.list.snapshot").await;
        let _ = recv_until_type(&mut socket3, "task.detail.snapshot").await;

        let recovered_third = recv_until_type(&mut socket3, "task.event").await;
        assert_eq!(recovered_third["task_id"], "task-watch");
        assert_eq!(recovered_third["sequence"], final_sequence);

        let no_unrelated_replay = tokio::time::timeout(
            std::time::Duration::from_millis(150),
            recv_until_type(&mut socket3, "task.event"),
        )
        .await;
        assert!(
            no_unrelated_replay.is_err(),
            "watch replay should remain scoped to the selected task"
        );
    }

    pub(super) async fn task_watch_keeps_runtime_events_flowing() {
        let state = test_state();
        let store = state.store.clone();

        let session_id = store::store_run(&store, |s| {
            let session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &session.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;
            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "task-watch-live".to_string(),
                    parent_task_id: None,
                    title: Some("task-watch-live".to_string()),
                    priority: 5,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            Ok(session.id)
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket =
            connect_ws_path(server.addr, &format!("/ws?session_id={session_id}")).await;
        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");
        assert_eq!(bootstrap["session_id"], session_id);

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.watch","request_id":"req-watch-live","session_id":"{}","task_id":"task-watch-live"}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();

        let watch_ack = recv_until_type(&mut socket, "response.ok").await;
        assert_eq!(watch_ack["request_id"], "req-watch-live");
        let _ = recv_until_type(&mut socket, "task.list.snapshot").await;
        let _ = recv_until_type(&mut socket, "task.detail.snapshot").await;

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"prompt.submit","request_id":"req-watch-prompt","session_id":"{}","prompt":"hello while watching"}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();

        let prompt_ack = recv_until_type(&mut socket, "response.ok").await;
        assert_eq!(prompt_ack["request_id"], "req-watch-prompt");

        let started = recv_until_type(&mut socket, "turn.started").await;
        assert_eq!(started["request_id"], "req-watch-prompt");
        let delta = recv_until_type(&mut socket, "message.delta").await;
        assert_eq!(delta["request_id"], "req-watch-prompt");
        let completed = recv_until_type(&mut socket, "turn.completed").await;
        assert_eq!(completed["request_id"], "req-watch-prompt");
    }

    pub(super) async fn task_commands_reject_cross_session_access() {
        let state = test_state();
        let store = state.store.clone();

        let attached_session_id = store::store_run(&store, |s| {
            let attached_session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &attached_session.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;

            let foreign_session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            let foreign_session_id = foreign_session.id.clone();

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "watch-foreign".to_string(),
                    parent_task_id: None,
                    title: Some("watch-foreign".to_string()),
                    priority: 10,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(foreign_session_id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-watch-foreign-1".to_string(),
                    task_id: "watch-foreign".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Running.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskRecord::transition_state(
                s,
                "watch-foreign",
                "attempt-watch-foreign-1",
                TaskLifecycleState::Running,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "cancel-foreign".to_string(),
                    parent_task_id: None,
                    title: Some("cancel-foreign".to_string()),
                    priority: 20,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel_descendants".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(foreign_session_id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-cancel-foreign-1".to_string(),
                    task_id: "cancel-foreign".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-cancel-foreign-1",
                AttemptLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "cancel-foreign",
                "attempt-cancel-foreign-1",
                TaskLifecycleState::Running,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "retry-foreign".to_string(),
                    parent_task_id: None,
                    title: Some("retry-foreign".to_string()),
                    priority: 30,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(foreign_session_id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-retry-foreign-1".to_string(),
                    task_id: "retry-foreign".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-retry-foreign-1",
                AttemptLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "retry-foreign",
                "attempt-retry-foreign-1",
                TaskLifecycleState::Running,
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-retry-foreign-1",
                AttemptLifecycleState::Failed,
            )?;
            TaskRecord::transition_state(
                s,
                "retry-foreign",
                "attempt-retry-foreign-1",
                TaskLifecycleState::Failed,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "resume-foreign".to_string(),
                    parent_task_id: None,
                    title: Some("resume-foreign".to_string()),
                    priority: 40,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(foreign_session_id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-resume-foreign-1".to_string(),
                    task_id: "resume-foreign".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: Some(r#"{"resume":"checkpoint"}"#.to_string()),
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-resume-foreign-1",
                AttemptLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "resume-foreign",
                "attempt-resume-foreign-1",
                TaskLifecycleState::Running,
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-resume-foreign-1",
                AttemptLifecycleState::Interrupted,
            )?;
            TaskRecord::transition_state(
                s,
                "resume-foreign",
                "attempt-resume-foreign-1",
                TaskLifecycleState::Interrupted,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "reprio-foreign".to_string(),
                    parent_task_id: None,
                    title: Some("reprio-foreign".to_string()),
                    priority: 50,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(foreign_session_id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-reprio-foreign-1".to_string(),
                    task_id: "reprio-foreign".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-reprio-foreign-1",
                AttemptLifecycleState::Ready,
            )?;
            TaskRecord::transition_state(
                s,
                "reprio-foreign",
                "attempt-reprio-foreign-1",
                TaskLifecycleState::Ready,
            )?;

            Ok(attached_session.id)
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket = connect_ws_path(
            server.addr,
            &format!("/ws?session_id={attached_session_id}"),
        )
        .await;
        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.watch","request_id":"req-watch-foreign","session_id":"{}","task_id":"watch-foreign"}}"#,
                    attached_session_id
                ),
            ))
            .await
            .unwrap();
        let watch_error = recv_until_type(&mut socket, "response.error").await;
        assert_eq!(watch_error["request_id"], "req-watch-foreign");
        assert_eq!(watch_error["error"]["code"], "task_not_found");

        for (request_id, command, task_id, action) in [
            (
                "req-cancel-foreign",
                format!(
                    r#"{{"type":"task.cancel","request_id":"req-cancel-foreign","session_id":"{}","task_id":"cancel-foreign"}}"#,
                    attached_session_id
                ),
                "cancel-foreign",
                "cancel",
            ),
            (
                "req-retry-foreign",
                format!(
                    r#"{{"type":"task.retry","request_id":"req-retry-foreign","session_id":"{}","task_id":"retry-foreign"}}"#,
                    attached_session_id
                ),
                "retry-foreign",
                "retry",
            ),
            (
                "req-resume-foreign",
                format!(
                    r#"{{"type":"task.resume","request_id":"req-resume-foreign","session_id":"{}","task_id":"resume-foreign"}}"#,
                    attached_session_id
                ),
                "resume-foreign",
                "resume",
            ),
            (
                "req-reprio-foreign",
                format!(
                    r#"{{"type":"task.reprioritize","request_id":"req-reprio-foreign","session_id":"{}","task_id":"reprio-foreign","priority":99}}"#,
                    attached_session_id
                ),
                "reprio-foreign",
                "reprioritize",
            ),
        ] {
            socket.send(Message::Text(command)).await.unwrap();
            let error = recv_until_type(&mut socket, "response.error").await;
            assert_eq!(error["request_id"], request_id);
            assert_eq!(error["error"]["code"], "task_not_found");
            assert_eq!(error["error"]["details"]["action"], action);
            assert_eq!(error["error"]["details"]["task_id"], task_id);
        }
    }

    pub(super) async fn task_control_commands_apply_runtime_lifecycle_helpers() {
        let state = test_state();
        let store = state.store.clone();

        let session_id = store::store_run(&store, |s| {
            let session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &session.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "cancel-root".to_string(),
                    parent_task_id: None,
                    title: Some("cancel-root".to_string()),
                    priority: 50,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel_descendants".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "cancel-child".to_string(),
                    parent_task_id: None,
                    title: Some("cancel-child".to_string()),
                    priority: 40,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel_descendants".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            TaskEdgeRecord::create(
                s,
                NewTaskEdgeRecord {
                    task_id: "cancel-child".to_string(),
                    depends_on_task_id: "cancel-root".to_string(),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-cancel-root-1".to_string(),
                    task_id: "cancel-root".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-cancel-root-1",
                AttemptLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "cancel-root",
                "attempt-cancel-root-1",
                TaskLifecycleState::Running,
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-cancel-child-1".to_string(),
                    task_id: "cancel-child".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "retry-task".to_string(),
                    parent_task_id: None,
                    title: Some("retry-task".to_string()),
                    priority: 10,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-retry-1".to_string(),
                    task_id: "retry-task".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-retry-1",
                AttemptLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "retry-task",
                "attempt-retry-1",
                TaskLifecycleState::Running,
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-retry-1",
                AttemptLifecycleState::Failed,
            )?;
            TaskRecord::transition_state(
                s,
                "retry-task",
                "attempt-retry-1",
                TaskLifecycleState::Failed,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "resume-task".to_string(),
                    parent_task_id: None,
                    title: Some("resume-task".to_string()),
                    priority: 20,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-resume-1".to_string(),
                    task_id: "resume-task".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: Some(r#"{"resume":"checkpoint"}"#.to_string()),
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-resume-1",
                AttemptLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "resume-task",
                "attempt-resume-1",
                TaskLifecycleState::Running,
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-resume-1",
                AttemptLifecycleState::Interrupted,
            )?;
            TaskRecord::transition_state(
                s,
                "resume-task",
                "attempt-resume-1",
                TaskLifecycleState::Interrupted,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "reprio-task".to_string(),
                    parent_task_id: None,
                    title: Some("reprio-task".to_string()),
                    priority: 5,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-reprio-1".to_string(),
                    task_id: "reprio-task".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-reprio-1",
                AttemptLifecycleState::Ready,
            )?;
            TaskRecord::transition_state(
                s,
                "reprio-task",
                "attempt-reprio-1",
                TaskLifecycleState::Ready,
            )?;

            Ok(session.id)
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket =
            connect_ws_path(server.addr, &format!("/ws?session_id={session_id}")).await;
        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");
        assert_eq!(bootstrap["session_id"], session_id);

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.cancel","request_id":"req-task-cancel","session_id":"{}","task_id":"cancel-root"}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();

        let cancel_ack = recv_until_type(&mut socket, "response.ok").await;
        assert_eq!(cancel_ack["request_id"], "req-task-cancel");
        assert_eq!(cancel_ack["data"]["action"], "cancel");
        assert_eq!(cancel_ack["data"]["session_id"], session_id);
        assert_eq!(cancel_ack["data"]["task_id"], "cancel-root");
        assert_eq!(cancel_ack["data"]["task_state"], "cancel_requested");
        let affected_task_ids = cancel_ack["data"]["affected_task_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(affected_task_ids.len(), 2);
        assert!(affected_task_ids.contains(&"cancel-root"));
        assert!(affected_task_ids.contains(&"cancel-child"));

        let (cancel_root_state, cancel_child_state) = store::store_run(&store, |s| {
            Ok((
                TaskRecord::current_state(s, "cancel-root")?,
                TaskRecord::current_state(s, "cancel-child")?,
            ))
        })
        .await
        .unwrap();
        assert_eq!(cancel_root_state, TaskLifecycleState::CancelRequested);
        assert_eq!(cancel_child_state, TaskLifecycleState::Cancelled);

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.retry","request_id":"req-task-retry","session_id":"{}","task_id":"retry-task"}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();

        let retry_ack = recv_until_type(&mut socket, "response.ok").await;
        assert_eq!(retry_ack["request_id"], "req-task-retry");
        assert_eq!(retry_ack["data"]["action"], "retry");
        assert_eq!(retry_ack["data"]["task_id"], "retry-task");
        assert_eq!(retry_ack["data"]["task_state"], "queued");
        let retry_attempt_id = retry_ack["data"]["new_attempt_id"]
            .as_str()
            .unwrap()
            .to_string();

        let (retry_state, retry_attempts) = store::store_run(&store, move |s| {
            Ok((
                TaskRecord::current_state(s, "retry-task")?,
                TaskAttemptRecord::list_for_task(s, "retry-task")?,
            ))
        })
        .await
        .unwrap();
        assert_eq!(retry_state, TaskLifecycleState::Queued);
        assert_eq!(retry_attempts.len(), 2);
        let retry_attempt = retry_attempts
            .iter()
            .find(|attempt| attempt.attempt_id == retry_attempt_id)
            .unwrap();
        assert_eq!(
            retry_attempt.status,
            AttemptLifecycleState::Queued.to_string()
        );

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.resume","request_id":"req-task-resume","session_id":"{}","task_id":"resume-task"}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();

        let resume_ack = recv_until_type(&mut socket, "response.ok").await;
        assert_eq!(resume_ack["request_id"], "req-task-resume");
        assert_eq!(resume_ack["data"]["action"], "resume");
        assert_eq!(resume_ack["data"]["task_id"], "resume-task");
        assert_eq!(resume_ack["data"]["task_state"], "queued");
        let resume_attempt_id = resume_ack["data"]["new_attempt_id"]
            .as_str()
            .unwrap()
            .to_string();

        let (resume_state, resume_attempts) = store::store_run(&store, move |s| {
            Ok((
                TaskRecord::current_state(s, "resume-task")?,
                TaskAttemptRecord::list_for_task(s, "resume-task")?,
            ))
        })
        .await
        .unwrap();
        assert_eq!(resume_state, TaskLifecycleState::Queued);
        assert_eq!(resume_attempts.len(), 2);
        let resume_attempt = resume_attempts
            .iter()
            .find(|attempt| attempt.attempt_id == resume_attempt_id)
            .unwrap();
        assert_eq!(
            resume_attempt.status,
            AttemptLifecycleState::Queued.to_string()
        );
        assert_eq!(
            resume_attempt.recovery_checkpoint,
            Some(r#"{"resume":"checkpoint"}"#.to_string())
        );

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.reprioritize","request_id":"req-task-reprioritize","session_id":"{}","task_id":"reprio-task","priority":33}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();

        let reprio_ack = recv_until_type(&mut socket, "response.ok").await;
        assert_eq!(reprio_ack["request_id"], "req-task-reprioritize");
        assert_eq!(reprio_ack["data"]["action"], "reprioritize");
        assert_eq!(reprio_ack["data"]["task_id"], "reprio-task");
        assert_eq!(reprio_ack["data"]["task_state"], "ready");
        assert_eq!(reprio_ack["data"]["priority"], 33);

        let reprio_task = store::store_run(&store, |s| TaskRecord::get(s, "reprio-task"))
            .await
            .unwrap();
        assert_eq!(reprio_task.priority, 33);
    }

    pub(super) async fn task_control_commands_reject_invalid_lifecycle_state() {
        let state = test_state();
        let store = state.store.clone();

        let session_id = store::store_run(&store, |s| {
            let session = Session::create(
                s,
                NewSession {
                    model: "test-model".to_string(),
                    provider: "test".to_string(),
                },
            )?;
            Session::update_settings(
                s,
                &session.id,
                r#"{"model":"test-model","provider":"test"}"#,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "cancel-completed".to_string(),
                    parent_task_id: None,
                    title: Some("cancel-completed".to_string()),
                    priority: 1,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-cancel-completed-1".to_string(),
                    task_id: "cancel-completed".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-cancel-completed-1",
                AttemptLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "cancel-completed",
                "attempt-cancel-completed-1",
                TaskLifecycleState::Running,
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-cancel-completed-1",
                AttemptLifecycleState::Completed,
            )?;
            TaskRecord::transition_state(
                s,
                "cancel-completed",
                "attempt-cancel-completed-1",
                TaskLifecycleState::Completed,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "retry-ready".to_string(),
                    parent_task_id: None,
                    title: Some("retry-ready".to_string()),
                    priority: 2,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-retry-ready-1".to_string(),
                    task_id: "retry-ready".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-retry-ready-1",
                AttemptLifecycleState::Ready,
            )?;
            TaskRecord::transition_state(
                s,
                "retry-ready",
                "attempt-retry-ready-1",
                TaskLifecycleState::Ready,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "resume-completed".to_string(),
                    parent_task_id: None,
                    title: Some("resume-completed".to_string()),
                    priority: 3,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-resume-completed-1".to_string(),
                    task_id: "resume-completed".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: Some(r#"{"resume":"checkpoint"}"#.to_string()),
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-resume-completed-1",
                AttemptLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "resume-completed",
                "attempt-resume-completed-1",
                TaskLifecycleState::Running,
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-resume-completed-1",
                AttemptLifecycleState::Completed,
            )?;
            TaskRecord::transition_state(
                s,
                "resume-completed",
                "attempt-resume-completed-1",
                TaskLifecycleState::Completed,
            )?;

            TaskRecord::create(
                s,
                NewTaskRecord {
                    task_id: "reprio-running".to_string(),
                    parent_task_id: None,
                    title: Some("reprio-running".to_string()),
                    priority: 9,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: Some(session.id.clone()),
                },
            )?;
            TaskAttemptRecord::create(
                s,
                NewTaskAttemptRecord {
                    attempt_id: "attempt-reprio-running-1".to_string(),
                    task_id: "reprio-running".to_string(),
                    session_id: None,
                    status: AttemptLifecycleState::Queued.to_string(),
                    recovery_checkpoint: None,
                },
            )?;
            TaskAttemptRecord::transition_state(
                s,
                "attempt-reprio-running-1",
                AttemptLifecycleState::Running,
            )?;
            TaskRecord::transition_state(
                s,
                "reprio-running",
                "attempt-reprio-running-1",
                TaskLifecycleState::Running,
            )?;

            Ok(session.id)
        })
        .await
        .unwrap();

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket =
            connect_ws_path(server.addr, &format!("/ws?session_id={session_id}")).await;
        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.cancel","request_id":"req-invalid-cancel","session_id":"{}","task_id":"cancel-completed"}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();
        let cancel_error = recv_until_type(&mut socket, "response.error").await;
        assert_eq!(cancel_error["request_id"], "req-invalid-cancel");
        assert_eq!(cancel_error["error"]["code"], "invalid_task_state");
        assert_eq!(cancel_error["error"]["details"]["action"], "cancel");
        assert_eq!(
            cancel_error["error"]["details"]["task_id"],
            "cancel-completed"
        );
        assert!(
            cancel_error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("nonterminal tasks")
        );

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.retry","request_id":"req-invalid-retry","session_id":"{}","task_id":"retry-ready"}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();
        let retry_error = recv_until_type(&mut socket, "response.error").await;
        assert_eq!(retry_error["request_id"], "req-invalid-retry");
        assert_eq!(retry_error["error"]["code"], "invalid_task_state");
        assert_eq!(retry_error["error"]["details"]["action"], "retry");
        assert_eq!(retry_error["error"]["details"]["task_id"], "retry-ready");
        assert!(
            retry_error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("retry is only allowed")
        );

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.resume","request_id":"req-invalid-resume","session_id":"{}","task_id":"resume-completed"}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();
        let resume_error = recv_until_type(&mut socket, "response.error").await;
        assert_eq!(resume_error["request_id"], "req-invalid-resume");
        assert_eq!(resume_error["error"]["code"], "invalid_task_state");
        assert_eq!(resume_error["error"]["details"]["action"], "resume");
        assert_eq!(
            resume_error["error"]["details"]["task_id"],
            "resume-completed"
        );
        assert!(
            resume_error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("resume is only allowed")
        );

        socket
            .send(Message::Text(
                format!(
                    r#"{{"type":"task.reprioritize","request_id":"req-invalid-reprioritize","session_id":"{}","task_id":"reprio-running","priority":55}}"#,
                    session_id
                ),
            ))
            .await
            .unwrap();
        let reprio_error = recv_until_type(&mut socket, "response.error").await;
        assert_eq!(reprio_error["request_id"], "req-invalid-reprioritize");
        assert_eq!(reprio_error["error"]["code"], "invalid_task_state");
        assert_eq!(reprio_error["error"]["details"]["action"], "reprioritize");
        assert_eq!(
            reprio_error["error"]["details"]["task_id"],
            "reprio-running"
        );
        assert!(
            reprio_error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("queued/ready")
        );
    }

    pub(super) async fn lsp_status_is_exposed_in_web_snapshot_and_events() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(
            fixture.path().join("Cargo.toml"),
            "[package]\nname = \"web-lsp\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let src_dir = fixture.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let file_path = src_dir.join("lib.rs");
        std::fs::write(&file_path, "fn demo() {}\n").unwrap();

        let state = test_state();
        state
            .runtime_manager
            .set_lsp_test_factory(Arc::new(ReadyDiagnosticsFactory));
        let session = seed_test_session(&state, "LSP Snapshot Session").await;

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket =
            connect_ws_path(server.addr, &format!("/ws?session_id={}", session.id)).await;

        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");
        assert!(bootstrap["lsp"]["supported"].as_array().is_some());
        assert_eq!(bootstrap["lsp"]["active"], serde_json::json!([]));
        assert!(
            bootstrap["lsp"]["supported"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| {
                    entry["server_id"] == "rust-analyzer"
                        && entry["command"] == serde_json::json!(["rust-analyzer"])
                        && entry["extensions"] == serde_json::json!(["rs"])
                })
        );

        let prompt = controlled_tool_prompt(
            "lsp_diagnostics",
            serde_json::json!({
                "file_path": file_path.display().to_string(),
                "severity": "all",
                "extension": null,
            }),
        );
        socket
            .send(Message::Text(
                serde_json::json!({
                    "type": "prompt.submit",
                    "request_id": "req-lsp-ready",
                    "session_id": session.id.clone(),
                    "prompt": prompt,
                })
                .to_string(),
            ))
            .await
            .unwrap();

        let ack = recv_json(&mut socket).await;
        assert_eq!(ack["type"], "response.ok");
        assert_eq!(ack["request_id"], "req-lsp-ready");

        let mut statuses = Vec::new();
        loop {
            let frame = recv_json(&mut socket).await;
            if frame["type"] == "status.report" {
                statuses.push(frame["status"].as_str().unwrap().to_string());
            }
            if frame["type"] == "turn.completed" {
                break;
            }
        }

        assert_eq!(
            statuses,
            vec![
                "lsp.detected".to_string(),
                "lsp.starting".to_string(),
                "lsp.ready".to_string(),
            ]
        );

        socket
            .send(Message::Text(
                serde_json::json!({
                    "type": "state.get",
                    "request_id": "req-lsp-snapshot",
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let snapshot = recv_json(&mut socket).await;
        assert_eq!(snapshot["type"], "response.ok");
        assert_eq!(snapshot["request_id"], "req-lsp-snapshot");
        assert_eq!(
            snapshot["data"]["lsp"]["supported"],
            bootstrap["lsp"]["supported"]
        );
        assert_eq!(
            snapshot["data"]["lsp"]["active"],
            serde_json::json!([{
                "server_id": "rust-analyzer",
                "status": "lsp.ready",
                "command": ["rust-analyzer"],
                "workspace_root": fixture.path().display().to_string(),
                "last_file": file_path.display().to_string(),
                "last_error": Value::Null,
            }])
        );
    }

    pub(super) async fn lsp_failed_server_emits_failed_status_once() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(
            fixture.path().join("Cargo.toml"),
            "[package]\nname = \"web-lsp-fail\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let src_dir = fixture.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let file_path = src_dir.join("lib.rs");
        std::fs::write(&file_path, "fn demo() {}\n").unwrap();

        let state = test_state();
        state
            .runtime_manager
            .set_lsp_test_factory(Arc::new(FailingStartupFactory::new(
                "missing binary: rust-analyzer",
            )));
        let session = seed_test_session(&state, "LSP Failed Session").await;

        let server = spawn_server_with_state(state).await.unwrap();
        let mut socket =
            connect_ws_path(server.addr, &format!("/ws?session_id={}", session.id)).await;
        let bootstrap = recv_json(&mut socket).await;
        assert_eq!(bootstrap["type"], "state.snapshot");

        let prompt = controlled_tool_prompt(
            "lsp_diagnostics",
            serde_json::json!({
                "file_path": file_path.display().to_string(),
                "severity": "all",
                "extension": null,
            }),
        );

        let mut statuses = Vec::new();

        for request_id in ["req-lsp-fail-1", "req-lsp-fail-2"] {
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "type": "prompt.submit",
                        "request_id": request_id,
                        "session_id": session.id.clone(),
                        "prompt": prompt.clone(),
                    })
                    .to_string(),
                ))
                .await
                .unwrap();

            let ack = recv_json(&mut socket).await;
            assert_eq!(ack["type"], "response.ok");
            assert_eq!(ack["request_id"], request_id);

            loop {
                let frame = recv_json(&mut socket).await;
                if frame["type"] == "status.report" {
                    statuses.push(frame["status"].as_str().unwrap().to_string());
                }
                if frame["type"] == "turn.completed" {
                    break;
                }
            }
        }

        assert_eq!(
            statuses,
            vec![
                "lsp.detected".to_string(),
                "lsp.starting".to_string(),
                "lsp.failed".to_string(),
            ]
        );

        socket
            .send(Message::Text(
                serde_json::json!({
                    "type": "state.get",
                    "request_id": "req-lsp-fail-snapshot",
                })
                .to_string(),
            ))
            .await
            .unwrap();

        let snapshot = recv_json(&mut socket).await;
        assert_eq!(snapshot["type"], "response.ok");
        assert_eq!(snapshot["request_id"], "req-lsp-fail-snapshot");
        assert_eq!(
            snapshot["data"]["lsp"]["active"],
            serde_json::json!([{
                "server_id": "rust-analyzer",
                "status": "lsp.failed",
                "command": ["rust-analyzer"],
                "workspace_root": fixture.path().display().to_string(),
                "last_file": file_path.display().to_string(),
                "last_error": "missing binary: rust-analyzer",
            }])
        );
    }
}

#[tokio::test]
async fn task_snapshot_includes_graph_and_attempt_state() {
    web::task_snapshot_includes_graph_and_attempt_state().await;
}

#[tokio::test]
async fn task_watch_reconnect_from_cursor_recovers_missed_events() {
    web::task_watch_reconnect_from_cursor_recovers_missed_events().await;
}

#[tokio::test]
async fn task_watch_keeps_runtime_events_flowing() {
    web::task_watch_keeps_runtime_events_flowing().await;
}

#[tokio::test]
async fn task_commands_reject_cross_session_access() {
    web::task_commands_reject_cross_session_access().await;
}

#[tokio::test]
async fn lsp_status_is_exposed_in_web_snapshot_and_events() {
    web::lsp_status_is_exposed_in_web_snapshot_and_events().await;
}

#[tokio::test]
async fn lsp_failed_server_emits_failed_status_once() {
    web::lsp_failed_server_emits_failed_status_once().await;
}

#[tokio::test]
async fn task_control_commands_apply_runtime_lifecycle_helpers() {
    web::task_control_commands_apply_runtime_lifecycle_helpers().await;
}

#[tokio::test]
async fn task_control_commands_reject_invalid_lifecycle_state() {
    web::task_control_commands_reject_invalid_lifecycle_state().await;
}

#[tokio::test]
async fn task_event_cursor_replays_from_last_seen_sequence() {
    web::task_event_cursor_replays_from_last_seen_sequence().await;
}

#[tokio::test]
async fn task_event_cursor_rejects_gaps_for_unknown_task() {
    web::task_event_cursor_rejects_gaps_for_unknown_task().await;
}

#[test]
fn task_events_include_attempt_and_child_session_metadata() {
    let claim_time = Utc::now();
    let expiry_time = claim_time + Duration::seconds(60);
    let interrupted_time = claim_time + Duration::seconds(61);

    let records = [
        TaskEventRecord {
            sequence: 11,
            task_id: "task-graph".to_string(),
            attempt_id: "attempt-graph-1".to_string(),
            session_id: None,
            event_type: "task.graph.created".to_string(),
            payload: serde_json::json!({
                "nodes": ["task-graph", "task-child"],
                "edges": [{
                    "task_id": "task-child",
                    "depends_on_task_id": "task-graph"
                }]
            })
            .to_string(),
            recorded_at: claim_time,
        },
        TaskEventRecord {
            sequence: 12,
            task_id: "task-ready".to_string(),
            attempt_id: "attempt-ready-1".to_string(),
            session_id: None,
            event_type: "task.state.transition".to_string(),
            payload: serde_json::json!({
                "from": "queued",
                "to": "ready"
            })
            .to_string(),
            recorded_at: claim_time,
        },
        TaskEventRecord {
            sequence: 13,
            task_id: "task-claim".to_string(),
            attempt_id: "attempt-claim-1".to_string(),
            session_id: None,
            event_type: "attempt.lease.claimed".to_string(),
            payload: serde_json::json!({
                "owner_id": "sched-1",
                "leased_at": claim_time.to_rfc3339(),
                "lease_expires_at": expiry_time.to_rfc3339()
            })
            .to_string(),
            recorded_at: claim_time,
        },
        TaskEventRecord {
            sequence: 14,
            task_id: "task-child-link".to_string(),
            attempt_id: "attempt-child-link-1".to_string(),
            session_id: Some("sess-child-1".to_string()),
            event_type: "attempt.child_session.linked".to_string(),
            payload: serde_json::json!({
                "session_id": "sess-child-1"
            })
            .to_string(),
            recorded_at: claim_time,
        },
        TaskEventRecord {
            sequence: 15,
            task_id: "task-control".to_string(),
            attempt_id: "attempt-control-1".to_string(),
            session_id: None,
            event_type: "task.state.transition".to_string(),
            payload: serde_json::json!({
                "from": "running",
                "to": "cancel_requested"
            })
            .to_string(),
            recorded_at: claim_time,
        },
        TaskEventRecord {
            sequence: 16,
            task_id: "task-recovery".to_string(),
            attempt_id: "attempt-recovery-1".to_string(),
            session_id: Some("sess-child-2".to_string()),
            event_type: "attempt.lease.expired".to_string(),
            payload: serde_json::json!({
                "owner_id": "sched-2",
                "lease_expires_at": expiry_time.to_rfc3339(),
                "interrupted_at": interrupted_time.to_rfc3339(),
                "recoverable": true
            })
            .to_string(),
            recorded_at: interrupted_time,
        },
    ];

    let frames = records
        .iter()
        .map(|record| {
            serde_json::to_value(
                runtime_event_to_ui_event(
                    &AgentEvent::from_task_event_record(record),
                    "req-task-surface",
                    "sess-parent-1",
                )
                .expect("task event should map to UI event"),
            )
            .expect("task UI event should serialize")
        })
        .collect::<Vec<_>>();

    assert!(frames.iter().all(|frame| frame["type"] == "task.event"));
    assert!(
        frames
            .iter()
            .all(|frame| frame["request_id"] == "req-task-surface")
    );

    assert_eq!(frames[0]["task_id"], "task-graph");
    assert_eq!(frames[0]["attempt_id"], "attempt-graph-1");
    assert_eq!(frames[0]["event_type"], "task.graph.created");
    assert_eq!(
        frames[0]["payload"]["edges"][0]["depends_on_task_id"],
        "task-graph"
    );

    assert_eq!(frames[1]["event_type"], "task.state.transition");
    assert_eq!(frames[1]["payload"]["to"], "ready");

    assert_eq!(frames[2]["event_type"], "attempt.lease.claimed");
    assert_eq!(frames[2]["payload"]["owner_id"], "sched-1");
    assert_eq!(
        frames[2]["payload"]["lease_expires_at"],
        expiry_time.to_rfc3339()
    );

    assert_eq!(frames[3]["event_type"], "attempt.child_session.linked");
    assert_eq!(frames[3]["child_session_id"], "sess-child-1");
    assert_eq!(frames[3]["payload"]["session_id"], "sess-child-1");

    assert_eq!(frames[4]["event_type"], "task.state.transition");
    assert_eq!(frames[4]["payload"]["to"], "cancel_requested");

    assert_eq!(frames[5]["event_type"], "attempt.lease.expired");
    assert_eq!(frames[5]["child_session_id"], "sess-child-2");
    assert_eq!(frames[5]["payload"]["recoverable"], true);
    assert_eq!(frames[5]["recorded_at"], interrupted_time.to_rfc3339());
}

#[test]
fn existing_turn_events_remain_backward_compatible() {
    let request_id = "req-turn-compat";
    let session_id = "sess-turn-1";
    let turn_id = "turn-1";
    let message_id = "msg-1";
    let tool_call_id = "tool-1";

    let events = [
        AgentEvent::TurnStarted {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            model: "test-model".to_string(),
            turn_number: 1,
            context_used_chars: 128,
            context_max_chars: 512,
        },
        AgentEvent::MessageStarted {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
        },
        AgentEvent::MessageDelta {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
            delta: "hello".to_string(),
        },
        AgentEvent::MessageCompleted {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
            content: "hello world".to_string(),
        },
        AgentEvent::ToolCallStarted {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool_name: "read".to_string(),
            arguments: "{\"file\":\"src/lib.rs\"}".to_string(),
        },
        AgentEvent::ToolCallCompleted {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool_name: "read".to_string(),
            output_preview: "ok".to_string(),
            edit_observation: None,
            diagnostics: vec![],
            success: true,
            context_used_chars: 196,
            context_max_chars: 512,
        },
        AgentEvent::TurnCompleted {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            model: "test-model".to_string(),
            turn_number: 1,
            message_id: message_id.to_string(),
            context_used_chars: 256,
            context_max_chars: 512,
            input_tokens: Some(21),
            output_tokens: Some(13),
            total_tokens: Some(34),
        },
        AgentEvent::TurnFailed {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            model: "test-model".to_string(),
            turn_number: 1,
            error: "aborted".to_string(),
        },
    ];

    let frames = events
        .iter()
        .map(|event| {
            serde_json::to_value(
                runtime_event_to_ui_event(event, request_id, session_id)
                    .expect("existing turn event should still map"),
            )
            .expect("turn UI event should serialize")
        })
        .collect::<Vec<_>>();

    let event_types = frames
        .iter()
        .map(|frame| frame["type"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        event_types,
        vec![
            "turn.started",
            "message.started",
            "message.delta",
            "message.completed",
            "tool.started",
            "tool.completed",
            "turn.completed",
            "turn.failed",
        ]
    );

    assert_eq!(frames[0]["request_id"], request_id);
    assert_eq!(frames[0]["session_id"], session_id);
    assert_eq!(frames[0]["turn_id"], turn_id);
    assert_eq!(frames[0]["context_usage"]["used_chars"], 128);
    assert_eq!(frames[0]["context_usage"]["max_chars"], 512);

    assert_eq!(frames[2]["message_id"], message_id);
    assert_eq!(frames[2]["delta"], "hello");

    assert_eq!(frames[4]["tool_call_id"], tool_call_id);
    assert_eq!(frames[4]["tool_name"], "read");

    assert_eq!(frames[5]["tool_call_id"], tool_call_id);
    assert_eq!(frames[5]["success"], true);
    assert_eq!(frames[5]["context_usage"]["used_chars"], 196);

    assert_eq!(frames[6]["context_usage"]["input_tokens"], 21);
    assert_eq!(frames[6]["context_usage"]["output_tokens"], 13);
    assert_eq!(frames[6]["context_usage"]["total_tokens"], 34);

    assert_eq!(frames[7]["request_id"], request_id);
    assert_eq!(frames[7]["error"], "aborted");
}
