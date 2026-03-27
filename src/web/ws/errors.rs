use serde_json::json;

use crate::runtime::{AbortTurnError, AttachControllerError, SubmitPromptError};
use crate::web::protocol::ResponseError;

pub(super) fn invalid_command_error(message: &str) -> ResponseError {
    ResponseError {
        code: "invalid_command".to_string(),
        message: message.to_string(),
        details: None,
    }
}

pub(super) fn internal_error(message: &str) -> ResponseError {
    ResponseError {
        code: "internal_error".to_string(),
        message: message.to_string(),
        details: None,
    }
}

pub(super) fn prompt_submit_error(error: SubmitPromptError) -> ResponseError {
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

pub(super) fn abort_turn_error(error: AbortTurnError) -> ResponseError {
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

pub(super) fn session_busy_error(err: AttachControllerError) -> ResponseError {
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
