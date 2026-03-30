use std::sync::{Arc, Mutex};

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use futures_util::{SinkExt, StreamExt};
use kley::store::{self, NewSession, NewTurn, Session, SessionStatus, SharedStore, Store, Turn};
use kley::web::state::{MockWebAuthService, WebAppState, WebAuthService};
use serde_json::Value;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tower::util::ServiceExt;

mod web {
    use super::*;

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
        let mut socket = connect_ws(server.addr).await;

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
        assert!(frame["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["updated_at"].as_str().is_some()));
        let transcript = frame["transcript"].as_array().unwrap();
        assert!(transcript
            .iter()
            .any(|entry| entry["content"] == "Persisted bootstrap prompt"));
        assert!(transcript
            .iter()
            .any(|entry| entry["content"] == "Persisted bootstrap reply"));
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
        let mut socket = connect_ws(server.addr).await;
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
        let mut socket = connect_ws(server.addr).await;
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
        assert!(frame["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["session_id"] == requested_session.id));
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
        let mut socket = connect_ws(server.addr).await;

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
        assert!(transcript
            .iter()
            .any(|entry| entry["content"] == "First session transcript"));
        assert!(!transcript
            .iter()
            .any(|entry| entry["content"] == "Second session transcript"));
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
                assert!(frame["tool_name"]
                    .as_str()
                    .unwrap()
                    .contains("unknown_tool"));
            }

            if event_type == "tool.completed" {
                completed_id = frame["tool_call_id"].as_str().map(|s| s.to_string());
                assert!(frame["tool_name"]
                    .as_str()
                    .unwrap()
                    .contains("unknown_tool"));
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
        assert!(transcript
            .iter()
            .any(|entry| entry["role"] == "user" && entry["content"] == "please use a tool"));
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
        let mut socket = connect_ws(server.addr).await;

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
        assert!(transcript
            .iter()
            .any(|entry| entry["content"] == "First session transcript"));
        assert!(!transcript
            .iter()
            .any(|entry| entry["content"] == "Second session transcript"));

        let sessions = snapshot["sessions"].as_array().unwrap();
        assert!(sessions
            .iter()
            .any(|entry| entry["session_id"] == first_session.id));
        assert!(sessions
            .iter()
            .any(|entry| entry["session_id"] == second_session.id));
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
        assert!(transcript
            .iter()
            .any(|entry| entry["role"] == "user" && entry["content"] == "hello after abort"));
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
    async fn reconnect_replays_active_turn() {
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
        let mut socket1 = connect_ws(server.addr).await;
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
        let turn_id = ack["data"]["turn_id"].as_str().unwrap().to_string();

        let started = recv_json(&mut socket1).await;
        let msg_started = recv_json(&mut socket1).await;
        let delta = recv_json(&mut socket1).await;
        assert_eq!(started["type"], "turn.started");
        assert_eq!(msg_started["type"], "message.started");
        assert_eq!(delta["type"], "message.delta");

        drop(socket1);

        let mut socket2 = connect_ws(server.addr).await;
        let bootstrap2 = recv_json(&mut socket2).await;
        assert_eq!(bootstrap2["type"], "state.snapshot");
        assert_eq!(bootstrap2["session_id"], seeded_session.id);

        let transcript = bootstrap2["transcript"].as_array().unwrap();
        assert!(transcript
            .iter()
            .any(|turn| turn["content"] == "Persisted history message"));
        assert!(transcript
            .iter()
            .any(|turn| turn["role"] == "user"
                && turn["content"] == "abortable response please stop"));

        assert_eq!(bootstrap2["active_turn"]["request_id"], "req-reconnect-1");
        let replayed_content = bootstrap2["active_turn"]["content"].as_str().unwrap();
        assert!(!replayed_content.is_empty());

        let completed = recv_until_type(&mut socket2, "turn.completed").await;
        assert_eq!(completed["request_id"], "req-reconnect-1");

        socket2
            .send(Message::Text(
                r#"{"type":"state.get","request_id":"req-reconnect-state"}"#.to_string(),
            ))
            .await
            .unwrap();

        let state_after = recv_json(&mut socket2).await;
        assert_eq!(state_after["type"], "response.ok");
        assert!(state_after["data"]["active_turn"].is_null());

        socket2
            .send(Message::Text(
                format!(
                    r#"{{"type":"turn.abort","request_id":"req-reconnect-abort","session_id":"{}","turn_id":"{}"}}"#,
                    seeded_session.id, turn_id
                )
                ,
            ))
            .await
            .unwrap();

        let abort_after_completion = recv_json(&mut socket2).await;
        assert_eq!(abort_after_completion["type"], "response.error");
        assert_eq!(abort_after_completion["request_id"], "req-reconnect-abort");
        assert_eq!(abort_after_completion["error"]["code"], "turn_not_found");
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
}
