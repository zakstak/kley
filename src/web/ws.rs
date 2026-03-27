use std::future::pending;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::runtime::{RuntimeEventEnvelope, SubmitPromptOutcome};

mod context_usage;
mod errors;
mod event_map;
mod io;
mod session;
mod snapshot;

use super::protocol::{ResponseError, UiEvent, WebCommand, WebResponse};
use super::state::WebAppState;
use errors::{
    abort_turn_error, internal_error, invalid_command_error, prompt_submit_error,
    session_busy_error,
};
use event_map::{runtime_event_to_ui_event, ts_now};
use io::{send_event, send_response};
use session::{
    LoadSessionError, SelectSessionError, attach_or_select_session, load_session_for_controller,
};
use snapshot::{list_sessions, snapshot_data};

const DEFAULT_WEB_MODEL: &str = "test-model";
const DEFAULT_WEB_PROVIDER: &str = "test";

#[derive(Debug, Default, Deserialize)]
pub struct WsConnectQuery {
    session_id: Option<String>,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(query): Query<WsConnectQuery>,
    State(state): State<WebAppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state, query.session_id))
}

async fn handle_socket(
    mut socket: WebSocket,
    state: WebAppState,
    requested_session_id: Option<String>,
) {
    let controller_id = format!("controller-{}", Uuid::new_v4());

    let mut active_session =
        match attach_or_select_session(&state, &controller_id, requested_session_id.as_deref())
            .await
        {
            Ok(session) => session,
            Err(SelectSessionError::Busy(attach_err)) => {
                let _ = send_response(
                    &mut socket,
                    WebResponse::Error {
                        request_id: "attach".to_string(),
                        error: session_busy_error(attach_err),
                    },
                )
                .await;
                return;
            }
            Err(SelectSessionError::Store) => return,
        };

    let mut runtime_events = state.runtime_manager.subscribe(&active_session.id);

    if send_bootstrap_event(&mut socket, &state, &active_session.id)
        .await
        .is_err()
    {
        state
            .runtime_manager
            .release_controller(&active_session.id, &controller_id);
        return;
    }

    loop {
        tokio::select! {
            biased;
            maybe_message = socket.recv() => {
                let Some(Ok(message)) = maybe_message else {
                    break;
                };

                let Message::Text(text) = message else {
                    if matches!(message, Message::Close(_)) {
                        break;
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
                                error: invalid_command_error("invalid command payload"),
                            },
                        )
                        .await
                        .is_err()
                        {
                            break;
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
                                error: invalid_command_error("unsupported command"),
                            },
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                };

                match command {
                    WebCommand::StateGet { request_id } => {
                        match snapshot_data(&state, &active_session.id).await {
                            Ok(snapshot) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: serde_json::to_value(snapshot).unwrap_or_else(|_| json!({})),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(_) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: internal_error("failed to build state snapshot"),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    WebCommand::SessionsList { request_id } => {
                        match list_sessions(&state, Some(&active_session.id)).await {
                            Ok(sessions) => {
                                let data = json!({
                                    "sessions": sessions,
                                    "session_id": active_session.id,
                                });
                                if send_response(&mut socket, WebResponse::Ok { request_id, data })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(_) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: internal_error("failed to list sessions"),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    WebCommand::SessionLoad {
                        request_id,
                        session_id,
                    } => match load_session_for_controller(
                        &state,
                        &controller_id,
                        &active_session,
                        &session_id,
                    )
                    .await
                    {
                        Ok(session) => {
                            active_session = session;
                            runtime_events = state.runtime_manager.subscribe(&active_session.id);
                            if send_response(
                                &mut socket,
                                WebResponse::Ok {
                                    request_id,
                                    data: json!({
                                        "session_id": active_session.id,
                                        "loaded": true,
                                    }),
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            if send_bootstrap_event(&mut socket, &state, &active_session.id)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(LoadSessionError::Busy(err)) => {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: session_busy_error(err),
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                        Err(LoadSessionError::TurnInProgress { turn_id }) => {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "turn_in_progress".to_string(),
                                        message: "cannot switch sessions while a turn is still streaming"
                                            .to_string(),
                                        details: Some(json!({
                                            "session_id": active_session.id,
                                            "turn_id": turn_id,
                                        })),
                                    },
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                        Err(LoadSessionError::NotFound) => {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "session_not_found".to_string(),
                                        message: "session does not exist".to_string(),
                                        details: None,
                                    },
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                        Err(LoadSessionError::Store) => {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: internal_error("failed to load session"),
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                    },
                    WebCommand::PromptSubmit {
                        request_id,
                        session_id,
                        prompt,
                    } => {
                        if session_id != active_session.id {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "invalid_session".to_string(),
                                        message: "prompt session is not currently attached".to_string(),
                                        details: None,
                                    },
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            continue;
                        }

                        let turn_id = format!("turn-{}", Uuid::new_v4());
                        let message_id = format!("msg-{}", Uuid::new_v4());

                        match state.runtime_manager.start_active_turn(
                            &active_session.id,
                            request_id.clone(),
                            turn_id.clone(),
                            message_id.clone(),
                        ) {
                            Ok(()) => {}
                            Err(err) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: prompt_submit_error(err),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                        }

                        match state.runtime_manager.submit_prompt(
                            state.store.clone(),
                            &active_session.id,
                            prompt,
                        ) {
                            Ok(SubmitPromptOutcome::Accepted {
                                turn_id,
                                message_id,
                            }) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: json!({
                                            "accepted": true,
                                            "session_id": active_session.id,
                                            "turn_id": turn_id,
                                            "message_id": message_id,
                                        }),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(err) => {
                                state.runtime_manager.clear_active_turn(&active_session.id, &turn_id);
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: prompt_submit_error(err),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    WebCommand::TurnAbort {
                        request_id,
                        session_id,
                        turn_id,
                    } => {
                        if session_id != active_session.id {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "invalid_session".to_string(),
                                        message: "abort session is not currently attached".to_string(),
                                        details: None,
                                    },
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            continue;
                        }

                        match state.runtime_manager.request_abort(&active_session.id, &turn_id) {
                            Ok(_) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: json!({
                                            "abort_requested": true,
                                            "session_id": active_session.id,
                                            "turn_id": turn_id,
                                        }),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(err) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: abort_turn_error(err),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    },
                }
            }
            runtime_event = next_runtime_event(&mut runtime_events) => {
                if let Some(envelope) = runtime_event {
                    let Some(ui_event) = runtime_event_to_ui_event(
                        &envelope.event,
                        &envelope.request_id,
                        &active_session.id,
                    ) else {
                        continue;
                    };

                    if send_event(&mut socket, ui_event).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    state
        .runtime_manager
        .release_controller(&active_session.id, &controller_id);
}

async fn next_runtime_event(
    receiver: &mut Option<broadcast::Receiver<RuntimeEventEnvelope>>,
) -> Option<RuntimeEventEnvelope> {
    match receiver.as_mut() {
        Some(rx) => loop {
            match rx.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        },
        None => pending().await,
    }
}

async fn send_bootstrap_event(
    socket: &mut WebSocket,
    state: &WebAppState,
    session_id: &str,
) -> std::result::Result<(), ()> {
    let data = snapshot_data(state, session_id).await.map_err(|_| ())?;

    send_event(
        socket,
        UiEvent::StateSnapshot {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            data,
        },
    )
    .await
}
