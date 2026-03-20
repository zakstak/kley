use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde_json::{json, Value};

use super::protocol::{
    ResponseError, SelectedSession, SessionSummary, StateSnapshotData, TranscriptEntry, UiEvent,
    WebCommand, WebResponse, PROTOCOL_VERSION,
};

pub async fn ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    let mut prompt_counter: usize = 0;

    if send_event(&mut socket, bootstrap_event()).await.is_err() {
        return;
    }

    while let Some(Ok(message)) = socket.recv().await {
        let Message::Text(text) = message else {
            if matches!(message, Message::Close(_)) {
                return;
            }
            continue;
        };

        let raw: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) => {
                if send_response(
                    &mut socket,
                    WebResponse::Error {
                        request_id: "unknown".to_string(),
                        error: ResponseError {
                            code: "invalid_command".to_string(),
                            message: "invalid command payload".to_string(),
                            details: None,
                        },
                    },
                )
                .await
                .is_err()
                {
                    return;
                }
                continue;
            }
        };

        let request_id = raw
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        let command: WebCommand = match serde_json::from_value(raw) {
            Ok(command) => command,
            Err(_) => {
                if send_response(
                    &mut socket,
                    WebResponse::Error {
                        request_id,
                        error: ResponseError {
                            code: "invalid_command".to_string(),
                            message: "unsupported command".to_string(),
                            details: None,
                        },
                    },
                )
                .await
                .is_err()
                {
                    return;
                }
                continue;
            }
        };

        match command {
            WebCommand::StateGet { request_id } => {
                let data = serde_json::to_value(snapshot_data()).unwrap();
                if send_response(&mut socket, WebResponse::Ok { request_id, data })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            WebCommand::SessionsList { request_id } => {
                let data = json!({
                    "sessions": sessions(),
                });
                if send_response(&mut socket, WebResponse::Ok { request_id, data })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            WebCommand::SessionLoad {
                request_id,
                session_id,
            } => {
                let data = json!({
                    "session_id": session_id,
                    "protocol_version": PROTOCOL_VERSION,
                });
                if send_response(&mut socket, WebResponse::Ok { request_id, data })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            WebCommand::PromptSubmit {
                request_id,
                session_id,
                prompt,
            } => {
                prompt_counter += 1;
                let response_data = json!({
                    "accepted": true,
                    "session_id": session_id,
                });
                if send_response(
                    &mut socket,
                    WebResponse::Ok {
                        request_id: request_id.clone(),
                        data: response_data,
                    },
                )
                .await
                .is_err()
                {
                    return;
                }

                let sequence = prompt_sequence(prompt_counter, &request_id, &session_id, &prompt);
                for event in sequence {
                    if send_event(&mut socket, event).await.is_err() {
                        return;
                    }
                }
            }
            WebCommand::TurnAbort {
                request_id,
                session_id,
                turn_id,
            } => {
                if send_response(
                    &mut socket,
                    WebResponse::Ok {
                        request_id: request_id.clone(),
                        data: json!({ "aborted": true }),
                    },
                )
                .await
                .is_err()
                {
                    return;
                }
                let event = UiEvent::TurnFailed {
                    event_id: "evt-turn-failed-0001".to_string(),
                    ts: ts(30),
                    request_id,
                    session_id,
                    turn_id,
                    error: "aborted".to_string(),
                };
                if send_event(&mut socket, event).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn bootstrap_event() -> UiEvent {
    UiEvent::StateSnapshot {
        event_id: "evt-bootstrap-0001".to_string(),
        ts: ts(1),
        data: snapshot_data(),
    }
}

fn snapshot_data() -> StateSnapshotData {
    StateSnapshotData {
        protocol_version: PROTOCOL_VERSION,
        session_id: "sess-mock-001".to_string(),
        selected_session: SelectedSession {
            session_id: "sess-mock-001".to_string(),
            title: "Mock Session".to_string(),
            status: "active".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        },
        sessions: sessions(),
        transcript: Vec::<TranscriptEntry>::new(),
        active_turn: None,
    }
}

fn sessions() -> Vec<SessionSummary> {
    vec![SessionSummary {
        session_id: "sess-mock-001".to_string(),
        title: "Mock Session".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    }]
}

fn prompt_sequence(index: usize, request_id: &str, session_id: &str, prompt: &str) -> Vec<UiEvent> {
    let turn_id = format!("turn-mock-{index:04}");
    let message_id = format!("msg-mock-{index:04}");
    let tool_call_id = format!("tool-mock-{index:04}");
    let has_tool_activity = prompt.to_lowercase().contains("tool");
    let mut events = vec![
        UiEvent::TurnStarted {
            event_id: format!("evt-turn-started-{index:04}"),
            ts: ts(10),
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.clone(),
        },
        UiEvent::MessageStarted {
            event_id: format!("evt-message-started-{index:04}"),
            ts: ts(11),
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
        },
        UiEvent::MessageDelta {
            event_id: format!("evt-message-delta-{}-01", index),
            ts: ts(12),
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
            delta: "Working through your request. ".to_string(),
        },
        UiEvent::MessageDelta {
            event_id: format!("evt-message-delta-{}-02", index),
            ts: ts(13),
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
            delta: "Preparing a deterministic response.".to_string(),
        },
        UiEvent::MessageCompleted {
            event_id: format!("evt-message-completed-{index:04}"),
            ts: ts(14),
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.clone(),
            message_id,
            content: format!("Mock response for: {prompt}"),
        },
    ];

    if has_tool_activity {
        events.push(UiEvent::ToolStarted {
            event_id: format!("evt-tool-started-{index:04}"),
            ts: ts(15),
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
            tool_name: "read".to_string(),
        });
        events.push(UiEvent::ToolCompleted {
            event_id: format!("evt-tool-completed-{index:04}"),
            ts: ts(16),
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.clone(),
            tool_call_id,
            tool_name: "read".to_string(),
            success: true,
        });
    }

    events.push(UiEvent::TurnCompleted {
        event_id: format!("evt-turn-completed-{index:04}"),
        ts: if has_tool_activity {
            ts(17)
        } else {
            ts(15)
        },
        request_id: request_id.to_string(),
        session_id: session_id.to_string(),
        turn_id,
    });

    events
}

fn ts(second: u8) -> String {
    format!("2026-01-01T00:00:{second:02}Z")
}

async fn send_response(socket: &mut WebSocket, response: WebResponse) -> Result<(), ()> {
    let text = serde_json::to_string(&response).map_err(|_| ())?;
    socket.send(Message::Text(text.into())).await.map_err(|_| ())
}

async fn send_event(socket: &mut WebSocket, event: UiEvent) -> Result<(), ()> {
    let text = serde_json::to_string(&event).map_err(|_| ())?;
    socket.send(Message::Text(text.into())).await.map_err(|_| ())
}
