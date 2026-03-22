use std::future::pending;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::compact::{CompactConfig, estimate_effective_history_chars};
use crate::events::AgentEvent;
use crate::runtime::{
    AbortTurnError, ActiveTurnReplay, AttachControllerError, RuntimeEventEnvelope,
    SubmitPromptError, SubmitPromptOutcome,
};
use crate::store::{self, NewSession, Session, Turn};

use super::protocol::{
    ActiveTurnSnapshot, ContextUsage, PROTOCOL_VERSION, ResponseError, SelectedSession,
    SessionSummary, StateSnapshotData, TranscriptEntry, UiEvent, WebCommand, WebResponse,
};
use super::state::WebAppState;

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
                    }
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
) -> Result<(), ()> {
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

async fn snapshot_data(state: &WebAppState, session_id: &str) -> Result<StateSnapshotData> {
    let selected_session = load_selected_session(state, session_id).await?;
    let sessions = list_sessions(state, Some(session_id)).await?;
    let turns = load_turns(state, session_id).await?;
    let transcript = turns_to_transcript(&turns);
    let active_turn = state
        .runtime_manager
        .active_turn(session_id)
        .map(active_turn_snapshot);
    let context_usage = if let Some((used_chars, max_chars)) =
        state.runtime_manager.context_usage_chars(session_id)
    {
        let (input_tokens, output_tokens, total_tokens) = state
            .runtime_manager
            .token_usage(session_id)
            .unwrap_or((None, None, None));
        context_usage_from_chars(
            used_chars,
            max_chars,
            input_tokens,
            output_tokens,
            total_tokens,
        )
    } else {
        let compact_threshold = state
            .runtime_manager
            .compact_threshold_chars(session_id)
            .unwrap_or_else(|| CompactConfig::default().threshold_chars);
        estimate_persisted_context_usage(&turns, compact_threshold)
    };

    Ok(StateSnapshotData {
        protocol_version: PROTOCOL_VERSION,
        session_id: session_id.to_string(),
        selected_session,
        sessions,
        transcript,
        active_turn,
        context_usage,
    })
}

fn estimate_persisted_context_usage(turns: &[Turn], compact_threshold: usize) -> ContextUsage {
    let history_items = crate::runtime::history_items_from_turns(turns);
    let compact_config = CompactConfig {
        threshold_chars: compact_threshold.max(1),
        ..CompactConfig::default()
    };
    let used_chars = estimate_effective_history_chars(&history_items, &compact_config);
    let max_chars = compact_config.threshold_chars;
    let trailing_assistant = turns
        .last()
        .filter(|turn| turn.role == "assistant" && turn.kind == "message");
    let input_tokens = trailing_assistant
        .and_then(|turn| turn.tokens_in)
        .and_then(|value| usize::try_from(value).ok());
    let output_tokens = trailing_assistant
        .and_then(|turn| turn.tokens_out)
        .and_then(|value| usize::try_from(value).ok());
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(input), Some(output)) => Some(input.saturating_add(output)),
        _ => None,
    };
    context_usage_from_chars(
        used_chars,
        max_chars,
        input_tokens,
        output_tokens,
        total_tokens,
    )
}

fn context_usage_from_chars(
    used_chars: usize,
    max_chars: usize,
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
    total_tokens: Option<usize>,
) -> ContextUsage {
    let clamped_max = max_chars.max(1);
    let percent_used = ((used_chars.saturating_mul(100)) / clamped_max).min(100) as u8;

    ContextUsage {
        used_chars,
        max_chars: clamped_max,
        percent_used,
        input_tokens,
        output_tokens,
        total_tokens,
    }
}

fn active_turn_snapshot(active: ActiveTurnReplay) -> ActiveTurnSnapshot {
    ActiveTurnSnapshot {
        request_id: active.request_id,
        turn_id: active.turn_id,
        message_id: active.message_id,
        content: active.content,
    }
}

async fn load_selected_session(state: &WebAppState, session_id: &str) -> Result<SelectedSession> {
    let session_id = session_id.to_string();
    let store_ref = state.store.clone();
    let session = store::store_run(&store_ref, move |store| {
        Session::find(store, &session_id)?.ok_or_else(|| anyhow::anyhow!("session not found"))
    })
    .await?;

    Ok(SelectedSession {
        session_id: session.id,
        title: session
            .title
            .unwrap_or_else(|| "Untitled session".to_string()),
        status: session.status.to_string(),
        created_at: session.created_at.to_rfc3339(),
        updated_at: session.updated_at.to_rfc3339(),
    })
}

enum SelectSessionError {
    Busy(AttachControllerError),
    Store,
}

async fn attach_or_select_session(
    state: &WebAppState,
    controller_id: &str,
    preferred_session_id: Option<&str>,
) -> std::result::Result<Session, SelectSessionError> {
    let store_ref = state.store.clone();
    let mut sessions = store::store_run(&store_ref, |store| Session::list(store, 50))
        .await
        .map_err(|_| SelectSessionError::Store)?;

    if sessions.is_empty() {
        let session = create_default_session(state)
            .await
            .map_err(|_| SelectSessionError::Store)?;
        state
            .runtime_manager
            .attach_controller(&session, controller_id)
            .map_err(SelectSessionError::Busy)?;
        return Ok(session);
    }

    if let Some(session_id) = preferred_session_id {
        if let Some(index) = sessions.iter().position(|session| session.id == session_id) {
            let preferred = sessions.remove(index);
            sessions.insert(0, preferred);
        } else {
            let preferred_session_id = session_id.to_string();
            if let Some(preferred) = store::store_run(&store_ref, move |store| {
                Session::find(store, &preferred_session_id)
            })
            .await
            .map_err(|_| SelectSessionError::Store)?
            {
                sessions.insert(0, preferred);
            }
        }
    }

    let mut busy_error = None;
    for session in sessions {
        let session = ensure_session_settings(state, session)
            .await
            .map_err(|_| SelectSessionError::Store)?;
        let is_requested_session = preferred_session_id == Some(session.id.as_str());
        match state
            .runtime_manager
            .attach_controller(&session, controller_id)
        {
            Ok(()) => return Ok(session),
            Err(err) => {
                if is_requested_session {
                    return Err(SelectSessionError::Busy(err));
                }
                busy_error = Some(err);
                continue;
            }
        }
    }

    Err(SelectSessionError::Busy(busy_error.expect(
        "busy error should exist when no attachable session remains",
    )))
}

async fn create_default_session(state: &WebAppState) -> Result<Session> {
    let store_ref = state.store.clone();
    let session = store::store_run(&store_ref, |store| {
        Session::create(
            store,
            NewSession {
                model: DEFAULT_WEB_MODEL.to_string(),
                provider: DEFAULT_WEB_PROVIDER.to_string(),
            },
        )
    })
    .await?;

    ensure_session_settings(state, session).await
}

async fn ensure_session_settings(state: &WebAppState, mut session: Session) -> Result<Session> {
    if session.settings.is_none() {
        let settings_json = default_settings_json(&session.model, &session.provider);
        let id = session.id.clone();
        let store_ref = state.store.clone();
        let update_value = settings_json.clone();
        store::store_run(&store_ref, move |store| {
            Session::update_settings(store, &id, &update_value)?;
            Ok(())
        })
        .await?;
        session.settings = Some(settings_json);
    }

    Ok(session)
}

fn default_settings_json(model: &str, provider: &str) -> String {
    json!({
        "model": model,
        "provider": provider,
    })
    .to_string()
}

async fn list_sessions(
    state: &WebAppState,
    selected_session_id: Option<&str>,
) -> Result<Vec<SessionSummary>> {
    let store_ref = state.store.clone();
    let mut sessions = store::store_run(&store_ref, |store| Session::list(store, 50)).await?;

    if let Some(selected_session_id) = selected_session_id {
        let contains_selected = sessions
            .iter()
            .any(|session| session.id == selected_session_id);
        if !contains_selected {
            let selected_session_id = selected_session_id.to_string();
            if let Some(selected_session) = store::store_run(&store_ref, move |store| {
                Session::find(store, &selected_session_id)
            })
            .await?
            {
                sessions.insert(0, selected_session);
            }
        }
    }

    Ok(sessions
        .into_iter()
        .map(|session| SessionSummary {
            session_id: session.id,
            title: session
                .title
                .unwrap_or_else(|| "Untitled session".to_string()),
            updated_at: session.updated_at.to_rfc3339(),
        })
        .collect())
}

async fn load_turns(state: &WebAppState, session_id: &str) -> Result<Vec<Turn>> {
    let session_id = session_id.to_string();
    let store_ref = state.store.clone();
    store::store_run(&store_ref, move |store| {
        Turn::list_for_session(store, &session_id)
    })
    .await
}

fn turns_to_transcript(turns: &[Turn]) -> Vec<TranscriptEntry> {
    turns
        .iter()
        .map(|turn| TranscriptEntry {
            turn_number: turn.turn_number,
            kind: turn.kind.clone(),
            role: turn.role.clone(),
            content: turn.content.clone(),
        })
        .collect()
}

enum LoadSessionError {
    Busy(AttachControllerError),
    TurnInProgress { turn_id: String },
    NotFound,
    Store,
}

async fn load_session_for_controller(
    state: &WebAppState,
    controller_id: &str,
    previous_session: &Session,
    next_session_id: &str,
) -> std::result::Result<Session, LoadSessionError> {
    let next_session_id = next_session_id.to_string();
    let store_ref = state.store.clone();
    let session = store::store_run(&store_ref, move |store| {
        Session::find(store, &next_session_id)
    })
    .await
    .map_err(|_| LoadSessionError::Store)?
    .ok_or(LoadSessionError::NotFound)?;

    if session.id != previous_session.id
        && let Some(active_turn) = state.runtime_manager.active_turn(&previous_session.id)
    {
        return Err(LoadSessionError::TurnInProgress {
            turn_id: active_turn.turn_id,
        });
    }

    state
        .runtime_manager
        .attach_controller(&session, controller_id)
        .map_err(LoadSessionError::Busy)?;

    if session.id != previous_session.id {
        state
            .runtime_manager
            .release_controller(&previous_session.id, controller_id);
    }
    Ok(session)
}

fn invalid_command_error(message: &str) -> ResponseError {
    ResponseError {
        code: "invalid_command".to_string(),
        message: message.to_string(),
        details: None,
    }
}

fn internal_error(message: &str) -> ResponseError {
    ResponseError {
        code: "internal_error".to_string(),
        message: message.to_string(),
        details: None,
    }
}

fn prompt_submit_error(error: SubmitPromptError) -> ResponseError {
    match error {
        SubmitPromptError::NoRuntime { .. } => ResponseError {
            code: "runtime_unavailable".to_string(),
            message: "runtime is not attached for this session".to_string(),
            details: None,
        },
        SubmitPromptError::NoActiveTurn { .. } => ResponseError {
            code: "turn_state_error".to_string(),
            message: "active turn state is not initialized".to_string(),
            details: None,
        },
        SubmitPromptError::TurnInProgress { turn_id, .. } => ResponseError {
            code: "turn_in_progress".to_string(),
            message: "session already has an active turn".to_string(),
            details: Some(json!({ "turn_id": turn_id })),
        },
        SubmitPromptError::RuntimeFailed { error, .. } => ResponseError {
            code: "runtime_failed".to_string(),
            message: error,
            details: None,
        },
    }
}

fn abort_turn_error(error: AbortTurnError) -> ResponseError {
    match error {
        AbortTurnError::NoRuntime { .. } => ResponseError {
            code: "runtime_unavailable".to_string(),
            message: "runtime is not attached for this session".to_string(),
            details: None,
        },
        AbortTurnError::NoActiveTurn { .. } => ResponseError {
            code: "turn_not_found".to_string(),
            message: "session has no active turn".to_string(),
            details: None,
        },
        AbortTurnError::TurnMismatch {
            expected_turn_id,
            requested_turn_id,
            ..
        } => ResponseError {
            code: "turn_not_found".to_string(),
            message: "requested turn does not match the active turn".to_string(),
            details: Some(json!({
                "expected_turn_id": expected_turn_id,
                "requested_turn_id": requested_turn_id,
            })),
        },
    }
}

fn runtime_event_to_ui_event(
    event: &AgentEvent,
    request_id: &str,
    default_session_id: &str,
) -> Option<UiEvent> {
    match event {
        AgentEvent::TurnStarted {
            session_id,
            turn_id,
            ..
        } => Some(UiEvent::TurnStarted {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            request_id: request_id.to_string(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
        }),
        AgentEvent::MessageStarted {
            session_id,
            turn_id,
            message_id,
        } => Some(UiEvent::MessageStarted {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            request_id: request_id.to_string(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
        }),
        AgentEvent::MessageDelta {
            session_id,
            turn_id,
            message_id,
            delta,
        } => Some(UiEvent::MessageDelta {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            request_id: request_id.to_string(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
            delta: delta.clone(),
        }),
        AgentEvent::MessageCompleted {
            session_id,
            turn_id,
            message_id,
            content,
        } => Some(UiEvent::MessageCompleted {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            request_id: request_id.to_string(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
            content: content.clone(),
        }),
        AgentEvent::ToolCallStarted {
            session_id,
            turn_id,
            tool_call_id,
            tool_name,
            ..
        } => Some(UiEvent::ToolStarted {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            request_id: request_id.to_string(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
        }),
        AgentEvent::ToolCallCompleted {
            session_id,
            turn_id,
            tool_call_id,
            tool_name,
            success,
            ..
        } => Some(UiEvent::ToolCompleted {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            request_id: request_id.to_string(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            success: *success,
        }),
        AgentEvent::TurnCompleted {
            session_id,
            turn_id,
            context_used_chars,
            context_max_chars,
            input_tokens,
            output_tokens,
            total_tokens,
            ..
        } => Some(UiEvent::TurnCompleted {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            request_id: request_id.to_string(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            context_usage: context_usage_from_chars(
                *context_used_chars,
                *context_max_chars,
                *input_tokens,
                *output_tokens,
                *total_tokens,
            ),
        }),
        AgentEvent::TurnFailed {
            session_id,
            turn_id,
            error,
            ..
        } => Some(UiEvent::TurnFailed {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            request_id: request_id.to_string(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            error: error.clone(),
        }),
        AgentEvent::TransportSelected {
            session_id,
            transport,
            ..
        } => Some(UiEvent::TransportSelected {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            session_id: session_id
                .clone()
                .unwrap_or_else(|| default_session_id.to_string()),
            transport: transport.to_string(),
        }),
        AgentEvent::TransportFallback {
            session_id,
            from,
            to,
            reason,
            ..
        } => Some(UiEvent::TransportFallback {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            session_id: session_id
                .clone()
                .unwrap_or_else(|| default_session_id.to_string()),
            from: from.to_string(),
            to: to.to_string(),
            reason: reason.clone(),
        }),
        AgentEvent::TokenRefreshed {
            session_id,
            provider,
        } => Some(UiEvent::AuthTokenRefreshed {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            session_id: session_id
                .clone()
                .unwrap_or_else(|| default_session_id.to_string()),
            provider: provider.clone(),
        }),
        AgentEvent::StatusReport {
            session_id,
            summary,
            ..
        } => Some(UiEvent::StatusReport {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            session_id: session_id
                .clone()
                .unwrap_or_else(|| default_session_id.to_string()),
            status: "runtime".to_string(),
            detail: summary.clone(),
        }),
        AgentEvent::HistoryCompacted {
            session_id,
            old_items,
            new_chars,
            ..
        } => Some(UiEvent::StatusReport {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            session_id: session_id
                .clone()
                .unwrap_or_else(|| default_session_id.to_string()),
            status: "history_compacted".to_string(),
            detail: format!("compacted {old_items} items into {new_chars} chars"),
        }),
    }
}

fn session_busy_error(err: AttachControllerError) -> ResponseError {
    match err {
        AttachControllerError::SessionBusy {
            session_id,
            active_controller_id,
        } => ResponseError {
            code: "session_busy".to_string(),
            message: "session already has an active controller".to_string(),
            details: Some(json!({
                "session_id": session_id,
                "active_controller_id": active_controller_id,
            })),
        },
    }
}

fn ts_now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_turn(kind: &str, role: &str, content: &str) -> Turn {
        Turn {
            id: 1,
            session_id: "sess-1".to_string(),
            kind: kind.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            model: None,
            tokens_in: None,
            tokens_out: None,
            turn_number: 1,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn persisted_context_usage_uses_compaction_aware_estimate() {
        let turns: Vec<Turn> = (0..30)
            .map(|index| Turn {
                turn_number: (index + 1) as i64,
                ..make_turn(
                    "message",
                    "user",
                    &format!("message-{index}-{}", "x".repeat(40_000)),
                )
            })
            .collect();

        let history_items = crate::runtime::history_items_from_turns(&turns);
        let raw_chars = crate::compact::estimate_history_chars(&history_items);
        let expected_chars = crate::compact::estimate_effective_history_chars(
            &history_items,
            &CompactConfig::default(),
        );
        let usage =
            estimate_persisted_context_usage(&turns, CompactConfig::default().threshold_chars);

        assert_eq!(usage.used_chars, expected_chars);
        assert!(usage.used_chars < raw_chars);
        assert_eq!(usage.max_chars, CompactConfig::default().threshold_chars);
    }

    #[test]
    fn persisted_context_usage_drops_tokens_when_tail_is_not_assistant() {
        let mut assistant = make_turn("message", "assistant", "done");
        assistant.tokens_in = Some(120);
        assistant.tokens_out = Some(30);

        let turns = vec![assistant, make_turn("message", "user", "follow-up")];
        let usage =
            estimate_persisted_context_usage(&turns, CompactConfig::default().threshold_chars);

        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.total_tokens, None);
    }

    #[test]
    fn persisted_context_usage_honors_custom_threshold() {
        let turns: Vec<Turn> = (0..25)
            .map(|index| Turn {
                turn_number: (index + 1) as i64,
                ..make_turn(
                    "message",
                    "user",
                    &format!("message-{index}-{}", "x".repeat(8_000)),
                )
            })
            .collect();

        let usage = estimate_persisted_context_usage(&turns, 120_000);

        assert_eq!(usage.max_chars, 120_000);
        assert!(usage.percent_used <= 100);
    }
}

async fn send_response(socket: &mut WebSocket, response: WebResponse) -> Result<(), ()> {
    let text = serde_json::to_string(&response).map_err(|_| ())?;
    socket.send(Message::Text(text)).await.map_err(|_| ())
}

async fn send_event(socket: &mut WebSocket, event: UiEvent) -> Result<(), ()> {
    let text = serde_json::to_string(&event).map_err(|_| ())?;
    socket.send(Message::Text(text)).await.map_err(|_| ())
}
