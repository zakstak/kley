use std::future::pending;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

use crate::compact::CompactConfig;
use crate::runtime::{RuntimeEventEnvelope, SubmitPromptOutcome};
use crate::runtime::{SessionSettingsOverrides, canonical_settings_json};
use crate::store::{self, Session, TaskLifecycleState, TaskRecord};

mod context_usage;
mod errors;
mod event_map;
mod io;
mod session;
mod snapshot;

use super::origin::is_websocket_origin_allowed;
use super::protocol::{ResponseError, TaskControlResponseData, UiEvent, WebCommand, WebResponse};
use super::state::WebAppState;
use errors::{
    abort_turn_error, internal_error, invalid_command_error, prompt_submit_error,
    session_busy_error,
};
pub use event_map::runtime_event_to_ui_event;
use event_map::ts_now;
use io::{send_event, send_response};
use session::{
    LoadSessionError, SelectSessionError, attach_or_select_session, load_session_for_controller,
};
use snapshot::{
    bootstrap_snapshot_data, snapshot_data, task_event_records, task_watch_bootstrap_data,
};

const DEFAULT_WEB_MODEL: &str = "test-model";
const DEFAULT_WEB_PROVIDER: &str = "test";
const OPENAI_COMPLETION_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Default, Deserialize)]
pub struct WsConnectQuery {
    session_id: Option<String>,
}

#[derive(Debug, Clone)]
struct TaskWatchState {
    request_id: String,
    session_id: String,
    task_id: String,
    last_sequence: i64,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(query): Query<WsConnectQuery>,
    State(state): State<WebAppState>,
) -> Response {
    if !is_websocket_origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }

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
    let mut task_watch: Option<TaskWatchState> = None;

    if send_connect_snapshot_event(&mut socket, &state, &active_session.id, &controller_id)
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
                        match snapshot_data(&state, &active_session.id, &controller_id).await {
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
                        let title = active_session
                            .title
                            .clone()
                            .unwrap_or_else(|| "Untitled session".to_string());
                        let data = json!({
                            "sessions": [{
                                "session_id": active_session.id,
                                "title": title,
                                "updated_at": active_session.updated_at.to_rfc3339(),
                            }],
                            "session_id": active_session.id,
                        });
                        if send_response(&mut socket, WebResponse::Ok { request_id, data })
                            .await
                            .is_err()
                        {
                            break;
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
                            task_watch = None;
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
                            if send_bootstrap_event(
                                &mut socket,
                                &state,
                                &active_session.id,
                                &controller_id,
                            )
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
                    WebCommand::SessionSettingsUpdate {
                        request_id,
                        session_id,
                        provider,
                        model,
                    } => {
                        if session_id != active_session.id {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "invalid_session".to_string(),
                                        message: "settings session is not currently attached"
                                            .to_string(),
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

                        if let Some(active_turn) = state.runtime_manager.active_turn(&active_session.id)
                        {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "turn_in_progress".to_string(),
                                        message: "cannot change model or provider while a turn is still streaming"
                                            .to_string(),
                                        details: Some(json!({
                                            "session_id": active_session.id,
                                            "turn_id": active_turn.turn_id,
                                        })),
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

                        let provider = provider.trim().to_string();
                        let model = model.trim().to_string();

                        if !is_supported_session_provider(&provider) {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "invalid_provider".to_string(),
                                        message: "provider must be one of openai, zai, or test"
                                            .to_string(),
                                        details: Some(json!({ "provider": provider })),
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

                        if model.is_empty() {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "invalid_model".to_string(),
                                        message: "model must not be empty".to_string(),
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

                        match update_session_runtime_selection(
                            &state,
                            &controller_id,
                            &active_session.id,
                            &provider,
                            &model,
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
                                            "provider": active_session.provider,
                                            "model": active_session.model,
                                            "updated": true,
                                        }),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }

                                if send_bootstrap_event(
                                    &mut socket,
                                    &state,
                                    &active_session.id,
                                    &controller_id,
                                )
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: ResponseError {
                                            code: "settings_update_failed".to_string(),
                                            message: error,
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
                        }
                    }
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
                    WebCommand::AuthOpenAiStart { request_id } => {
                        match state.start_openai_login(&controller_id) {
                            Ok(authorize_url) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: json!({
                                            "started": true,
                                            "provider": "openai",
                                            "authorize_url": authorize_url,
                                        }),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }

                                if send_bootstrap_event(
                                    &mut socket,
                                    &state,
                                    &active_session.id,
                                    &controller_id,
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: ResponseError {
                                            code: "auth_start_failed".to_string(),
                                            message: error.to_string(),
                                            details: Some(json!({ "provider": "openai" })),
                                        },
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
                    WebCommand::AuthOpenAiComplete {
                        request_id,
                        callback_input,
                        verifier,
                        state: expected_state,
                    } => {
                        let callback_input = callback_input.trim().to_string();
                        if callback_input.is_empty() {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "invalid_auth_callback".to_string(),
                                        message: "paste the final redirect URL or authorization code"
                                            .to_string(),
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

                        let completion = timeout(OPENAI_COMPLETION_TIMEOUT, async {
                            if let (Some(verifier), Some(expected_state)) =
                                (verifier.as_deref(), expected_state.as_deref())
                            {
                                state
                                    .complete_openai_login_with_verifier_state(
                                        &controller_id,
                                        &callback_input,
                                        verifier,
                                        expected_state,
                                    )
                                    .await
                            } else {
                                state.complete_openai_login(&controller_id, &callback_input).await
                            }
                        })
                        .await
                        .map_err(|_| anyhow::anyhow!("openai login completion timed out"))
                        .and_then(|result| result);

                        match completion {
                            Ok(()) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: json!({
                                            "logged_in": true,
                                            "provider": "openai",
                                        }),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }

                                if send_bootstrap_event(
                                    &mut socket,
                                    &state,
                                    &active_session.id,
                                    &controller_id,
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: ResponseError {
                                            code: "auth_completion_failed".to_string(),
                                            message: error.to_string(),
                                            details: Some(json!({ "provider": "openai" })),
                                        },
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
                    WebCommand::AuthLogin {
                        request_id,
                        provider,
                        api_key,
                    } => {
                        let provider = provider.trim().to_string();
                        let api_key = api_key.trim().to_string();

                        if !matches!(provider.as_str(), "openai" | "zai") {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "invalid_provider".to_string(),
                                        message: "login provider must be openai or zai".to_string(),
                                        details: Some(json!({ "provider": provider })),
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

                        if provider == "openai" {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "auth_flow_mismatch".to_string(),
                                        message: "openai login must be completed in the browser"
                                            .to_string(),
                                        details: Some(json!({ "provider": provider })),
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

                        if api_key.is_empty() {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "invalid_api_key".to_string(),
                                        message: "API key must not be empty".to_string(),
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

                        match state.login_zai(&api_key) {
                            Ok(()) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: json!({
                                            "logged_in": true,
                                            "provider": provider,
                                        }),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }

                                if send_bootstrap_event(
                                    &mut socket,
                                    &state,
                                    &active_session.id,
                                    &controller_id,
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: ResponseError {
                                            code: "auth_login_failed".to_string(),
                                            message: error.to_string(),
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
                    WebCommand::TaskCancel {
                        request_id,
                        session_id,
                        task_id,
                    } => {
                        if session_id != active_session.id {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: invalid_task_control_session_error(),
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            continue;
                        }

                        match cancel_task_via_api(&state, &active_session.id, &task_id).await {
                            Ok(data) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: serde_json::to_value(data)
                                            .unwrap_or_else(|_| json!({})),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: task_control_error("cancel", &task_id, error),
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
                    WebCommand::TaskRetry {
                        request_id,
                        session_id,
                        task_id,
                    } => {
                        if session_id != active_session.id {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: invalid_task_control_session_error(),
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            continue;
                        }

                        match retry_task_via_api(&state, &active_session.id, &task_id).await {
                            Ok(data) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: serde_json::to_value(data)
                                            .unwrap_or_else(|_| json!({})),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: task_control_error("retry", &task_id, error),
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
                    WebCommand::TaskResume {
                        request_id,
                        session_id,
                        task_id,
                    } => {
                        if session_id != active_session.id {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: invalid_task_control_session_error(),
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            continue;
                        }

                        match resume_task_via_api(&state, &active_session.id, &task_id).await {
                            Ok(data) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: serde_json::to_value(data)
                                            .unwrap_or_else(|_| json!({})),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: task_control_error("resume", &task_id, error),
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
                    WebCommand::TaskReprioritize {
                        request_id,
                        session_id,
                        task_id,
                        priority,
                    } => {
                        if session_id != active_session.id {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: invalid_task_control_session_error(),
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            continue;
                        }

                        match reprioritize_task_via_api(
                            &state,
                            &active_session.id,
                            &task_id,
                            priority,
                        )
                        .await
                        {
                            Ok(data) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id,
                                        data: serde_json::to_value(data)
                                            .unwrap_or_else(|_| json!({})),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: task_control_error(
                                            "reprioritize",
                                            &task_id,
                                            error,
                                        ),
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
                    WebCommand::TaskWatch {
                        request_id,
                        session_id,
                        task_id,
                        after_sequence,
                    } => {
                        if session_id != active_session.id {
                            if send_response(
                                &mut socket,
                                WebResponse::Error {
                                    request_id,
                                    error: ResponseError {
                                        code: "invalid_session".to_string(),
                                        message: "task watch session is not currently attached"
                                            .to_string(),
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

                        let after_sequence = after_sequence.unwrap_or(0);
                        match task_watch_bootstrap_data(
                            &state,
                            &active_session.id,
                            &request_id,
                            &task_id,
                            after_sequence,
                        )
                        .await
                        {
                            Ok(bootstrap) => {
                                let latest_sequence = bootstrap.detail_snapshot.cursor.latest_sequence;
                                let cursor = bootstrap.detail_snapshot.cursor.clone();
                                if send_response(
                                    &mut socket,
                                    WebResponse::Ok {
                                        request_id: request_id.clone(),
                                        data: json!({
                                            "watching": true,
                                            "session_id": active_session.id.clone(),
                                            "task_id": task_id.clone(),
                                            "cursor": cursor,
                                        }),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }

                                if send_event(
                                    &mut socket,
                                    UiEvent::TaskListSnapshot {
                                        event_id: format!("evt-{}", Uuid::new_v4()),
                                        ts: ts_now(),
                                        data: bootstrap.list_snapshot,
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }

                                if send_event(
                                    &mut socket,
                                    UiEvent::TaskDetailSnapshot {
                                        event_id: format!("evt-{}", Uuid::new_v4()),
                                        ts: ts_now(),
                                        data: bootstrap.detail_snapshot,
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }

                                if send_task_event_records(
                                    &mut socket,
                                    &request_id,
                                    &active_session.id,
                                    &bootstrap.replay_events,
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }

                                task_watch = Some(TaskWatchState {
                                    request_id,
                                    session_id: active_session.id.clone(),
                                    task_id,
                                    last_sequence: latest_sequence,
                                });
                            }
                            Err(error) => {
                                if send_response(
                                    &mut socket,
                                    WebResponse::Error {
                                        request_id,
                                        error: task_watch_error(error),
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
            _ = next_task_watch_tick(task_watch.as_ref()) => {
                let Some(watch) = task_watch.as_mut() else {
                    continue;
                };

                match task_event_records(&state, &watch.task_id, watch.last_sequence).await {
                    Ok(records) => {
                        if let Some(last_record) = records.last() {
                            watch.last_sequence = last_record.sequence;
                        }
                        if send_task_event_records(
                            &mut socket,
                            &watch.request_id,
                            &watch.session_id,
                            &records,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => {
                        task_watch = None;
                    }
                }
            }
        }
    }

    state.clear_openai_login(&controller_id);
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

async fn next_task_watch_tick(task_watch: Option<&TaskWatchState>) {
    match task_watch {
        Some(_) => tokio::time::sleep(Duration::from_millis(25)).await,
        None => pending().await,
    }
}

fn task_watch_error(error: anyhow::Error) -> ResponseError {
    let message = error.to_string();
    if message.contains("task event cursor") {
        return ResponseError {
            code: "invalid_task_cursor".to_string(),
            message,
            details: None,
        };
    }
    if message.contains("task event stream not found") || message.contains("task not found") {
        return ResponseError {
            code: "task_not_found".to_string(),
            message,
            details: None,
        };
    }

    ResponseError {
        code: "task_watch_failed".to_string(),
        message,
        details: None,
    }
}

fn invalid_task_control_session_error() -> ResponseError {
    ResponseError {
        code: "invalid_session".to_string(),
        message: "task control session is not currently attached".to_string(),
        details: None,
    }
}

fn task_control_error(action: &str, task_id: &str, error: anyhow::Error) -> ResponseError {
    let message = error.to_string();
    let code = if message.contains("task not found") {
        "task_not_found"
    } else if message.contains("only allowed")
        || message.contains("already requested")
        || message.contains("invalid task transition")
        || message.contains("invalid attempt transition")
    {
        "invalid_task_state"
    } else {
        "task_control_failed"
    };

    ResponseError {
        code: code.to_string(),
        message,
        details: Some(json!({
            "action": action,
            "task_id": task_id,
        })),
    }
}

async fn ensure_task_owned_by_session(
    state: &WebAppState,
    session_id: &str,
    task_id: &str,
) -> anyhow::Result<TaskRecord> {
    let store_ref = state.store.clone();
    let session_id_owned = session_id.to_string();
    let task_id_owned = task_id.to_string();
    store::store_run(&store_ref, move |store| {
        TaskRecord::get_owned_by_session(store, &task_id_owned, &session_id_owned)
    })
    .await
}

async fn cancel_task_via_api(
    state: &WebAppState,
    session_id: &str,
    task_id: &str,
) -> anyhow::Result<TaskControlResponseData> {
    ensure_task_owned_by_session(state, session_id, task_id).await?;
    validate_cancelable_task_for_api(state, task_id).await?;
    let affected_task_ids = state
        .runtime_manager
        .cancel_task_graph(&state.store, task_id)?;
    let (_, task_state) = task_snapshot_for_control(state, session_id, task_id).await?;
    Ok(TaskControlResponseData {
        action: "cancel".to_string(),
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
        task_state: task_state.to_string(),
        affected_task_ids: Some(affected_task_ids),
        new_attempt_id: None,
        priority: None,
    })
}

async fn retry_task_via_api(
    state: &WebAppState,
    session_id: &str,
    task_id: &str,
) -> anyhow::Result<TaskControlResponseData> {
    ensure_task_owned_by_session(state, session_id, task_id).await?;
    let new_attempt_id = state.runtime_manager.retry_task(&state.store, task_id)?;
    let (_, task_state) = task_snapshot_for_control(state, session_id, task_id).await?;
    Ok(TaskControlResponseData {
        action: "retry".to_string(),
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
        task_state: task_state.to_string(),
        affected_task_ids: None,
        new_attempt_id: Some(new_attempt_id),
        priority: None,
    })
}

async fn resume_task_via_api(
    state: &WebAppState,
    session_id: &str,
    task_id: &str,
) -> anyhow::Result<TaskControlResponseData> {
    ensure_task_owned_by_session(state, session_id, task_id).await?;
    let new_attempt_id = state.runtime_manager.resume_task(&state.store, task_id)?;
    let (_, task_state) = task_snapshot_for_control(state, session_id, task_id).await?;
    Ok(TaskControlResponseData {
        action: "resume".to_string(),
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
        task_state: task_state.to_string(),
        affected_task_ids: None,
        new_attempt_id: Some(new_attempt_id),
        priority: None,
    })
}

async fn reprioritize_task_via_api(
    state: &WebAppState,
    session_id: &str,
    task_id: &str,
    priority: i64,
) -> anyhow::Result<TaskControlResponseData> {
    ensure_task_owned_by_session(state, session_id, task_id).await?;
    state
        .runtime_manager
        .reprioritize_task(&state.store, task_id, priority)?;
    let (task, task_state) = task_snapshot_for_control(state, session_id, task_id).await?;
    Ok(TaskControlResponseData {
        action: "reprioritize".to_string(),
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
        task_state: task_state.to_string(),
        affected_task_ids: None,
        new_attempt_id: None,
        priority: Some(task.priority),
    })
}

async fn task_snapshot_for_control(
    state: &WebAppState,
    session_id: &str,
    task_id: &str,
) -> anyhow::Result<(TaskRecord, TaskLifecycleState)> {
    let store_ref = state.store.clone();
    let session_id_owned = session_id.to_string();
    let task_id_owned = task_id.to_string();
    store::store_run(&store_ref, move |store| {
        let task = TaskRecord::get_owned_by_session(store, &task_id_owned, &session_id_owned)?;
        let task_state = TaskRecord::current_state(store, &task_id_owned)?;
        Ok((task, task_state))
    })
    .await
}

async fn validate_cancelable_task_for_api(
    state: &WebAppState,
    task_id: &str,
) -> anyhow::Result<()> {
    let store_ref = state.store.clone();
    let task_id_owned = task_id.to_string();
    store::store_run(&store_ref, move |store| {
        let task_state = TaskRecord::current_state(store, &task_id_owned)?;
        if matches!(
            task_state,
            TaskLifecycleState::Completed | TaskLifecycleState::Cancelled
        ) {
            anyhow::bail!(
                "cancel is only allowed for nonterminal tasks: task {task_id_owned} is {task_state}"
            );
        }
        if task_state == TaskLifecycleState::CancelRequested {
            anyhow::bail!("cancel is already requested for task {task_id_owned}");
        }
        if task_state == TaskLifecycleState::Failed {
            anyhow::bail!(
                "cancel is only allowed before terminal failure: task {task_id_owned} is {task_state}"
            );
        }
        Ok(())
    })
    .await
}

async fn send_task_event_records(
    socket: &mut WebSocket,
    request_id: &str,
    session_id: &str,
    records: &[crate::store::TaskEventRecord],
) -> std::result::Result<(), ()> {
    for record in records {
        let Some(ui_event) = runtime_event_to_ui_event(
            &crate::events::AgentEvent::from_task_event_record(record),
            request_id,
            session_id,
        ) else {
            continue;
        };

        send_event(socket, ui_event).await?;
    }

    Ok(())
}

async fn send_bootstrap_event(
    socket: &mut WebSocket,
    state: &WebAppState,
    session_id: &str,
    controller_id: &str,
) -> std::result::Result<(), ()> {
    let data = snapshot_data(state, session_id, controller_id)
        .await
        .map_err(|_| ())?;

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

async fn send_connect_snapshot_event(
    socket: &mut WebSocket,
    state: &WebAppState,
    session_id: &str,
    controller_id: &str,
) -> std::result::Result<(), ()> {
    let data = bootstrap_snapshot_data(state, session_id, controller_id)
        .await
        .map_err(|_| ())?;

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

fn is_supported_session_provider(provider: &str) -> bool {
    matches!(provider, "openai" | "zai" | "test")
}

fn compact_threshold_for_session(session: &Session) -> usize {
    SessionSettingsOverrides::from_session(session)
        .and_then(|settings| settings.compact_threshold)
        .unwrap_or_else(|| CompactConfig::default().threshold_chars)
}

async fn update_session_runtime_selection(
    state: &WebAppState,
    controller_id: &str,
    session_id: &str,
    provider: &str,
    model: &str,
) -> std::result::Result<Session, String> {
    let session_id_owned = session_id.to_string();
    let model_owned = model.to_string();
    let provider_owned = provider.to_string();
    let store_ref = state.store.clone();

    let session = store::store_run(&store_ref, move |store| {
        let mut session = Session::find(store, &session_id_owned)?
            .ok_or_else(|| anyhow::anyhow!("session not found"))?;
        let compact_threshold = compact_threshold_for_session(&session);
        let settings_json =
            canonical_settings_json(&model_owned, &provider_owned, compact_threshold);

        Session::update_runtime_selection(
            store,
            &session.id,
            &model_owned,
            &provider_owned,
            &settings_json,
        )?;

        session.model = model_owned;
        session.provider = provider_owned;
        session.settings = Some(settings_json);
        Ok(session)
    })
    .await
    .map_err(|error| error.to_string())?;

    state
        .runtime_manager
        .attach_controller(&session, controller_id)
        .map_err(|error| match error {
            crate::runtime::AttachControllerError::SessionBusy { .. } => {
                "session is currently controlled elsewhere".to_string()
            }
        })?;

    Ok(session)
}
