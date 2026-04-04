use serde_json::json;

use crate::diagnostics::web_error_code;
use crate::runtime::{AbortTurnError, AttachControllerError, SubmitPromptError};
use crate::web::protocol::ResponseError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TaskWatchApiError {
    InvalidCursor { message: String },
    TaskNotFound { message: String },
    Internal { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TaskControlApiError {
    TaskNotFound {
        action: String,
        task_id: String,
        message: String,
    },
    InvalidState {
        action: String,
        task_id: String,
        message: String,
    },
    Internal {
        action: String,
        task_id: String,
        message: String,
    },
}

fn response_error(
    code: impl Into<String>,
    message: impl Into<String>,
    details: Option<serde_json::Value>,
) -> ResponseError {
    ResponseError {
        code: code.into(),
        message: message.into(),
        details,
    }
}

pub(super) fn invalid_command_error(message: &str) -> ResponseError {
    response_error(web_error_code::INVALID_COMMAND, message, None)
}

pub(super) fn internal_error(message: &str) -> ResponseError {
    response_error(web_error_code::INTERNAL_ERROR, message, None)
}

pub(super) fn invalid_task_watch_session_error() -> ResponseError {
    response_error(
        web_error_code::INVALID_SESSION,
        "task watch session is not currently attached",
        None,
    )
}

pub(super) fn invalid_task_control_session_error() -> ResponseError {
    response_error(
        web_error_code::INVALID_SESSION,
        "task control session is not currently attached",
        None,
    )
}

pub(super) fn prompt_submit_error(error: SubmitPromptError) -> ResponseError {
    match error {
        SubmitPromptError::NoRuntime { .. } => response_error(
            web_error_code::RUNTIME_UNAVAILABLE,
            "runtime is not attached for this session",
            None,
        ),
        SubmitPromptError::NoActiveTurn { .. } => response_error(
            web_error_code::TURN_STATE_ERROR,
            "active turn state is not initialized",
            None,
        ),
        SubmitPromptError::TurnInProgress { turn_id, .. } => response_error(
            web_error_code::TURN_IN_PROGRESS,
            "session already has an active turn",
            Some(json!({ "turn_id": turn_id })),
        ),
        SubmitPromptError::RuntimeFailed { error, .. } => {
            response_error(web_error_code::RUNTIME_FAILED, error, None)
        }
    }
}

pub(super) fn abort_turn_error(error: AbortTurnError) -> ResponseError {
    match error {
        AbortTurnError::NoRuntime { .. } => response_error(
            web_error_code::RUNTIME_UNAVAILABLE,
            "runtime is not attached for this session",
            None,
        ),
        AbortTurnError::NoActiveTurn { .. } => response_error(
            web_error_code::TURN_NOT_FOUND,
            "session has no active turn",
            None,
        ),
        AbortTurnError::TurnMismatch {
            expected_turn_id,
            requested_turn_id,
            ..
        } => response_error(
            web_error_code::TURN_NOT_FOUND,
            "requested turn does not match the active turn",
            Some(json!({
                "expected_turn_id": expected_turn_id,
                "requested_turn_id": requested_turn_id,
            })),
        ),
    }
}

pub(super) fn session_busy_error(err: AttachControllerError) -> ResponseError {
    match err {
        AttachControllerError::SessionBusy {
            session_id,
            active_controller_id,
        } => response_error(
            web_error_code::SESSION_BUSY,
            "session already has an active controller",
            Some(json!({
                "session_id": session_id,
                "active_controller_id": active_controller_id,
            })),
        ),
    }
}

pub(super) fn task_watch_error(error: TaskWatchApiError) -> ResponseError {
    match error {
        TaskWatchApiError::InvalidCursor { message } => {
            response_error(web_error_code::INVALID_TASK_CURSOR, message, None)
        }
        TaskWatchApiError::TaskNotFound { message } => {
            response_error(web_error_code::TASK_NOT_FOUND, message, None)
        }
        TaskWatchApiError::Internal { message } => {
            response_error(web_error_code::TASK_WATCH_FAILED, message, None)
        }
    }
}

pub(super) fn task_control_error(error: TaskControlApiError) -> ResponseError {
    match error {
        TaskControlApiError::TaskNotFound {
            action,
            task_id,
            message,
        } => response_error(
            web_error_code::TASK_NOT_FOUND,
            message,
            Some(json!({ "action": action, "task_id": task_id })),
        ),
        TaskControlApiError::InvalidState {
            action,
            task_id,
            message,
        } => response_error(
            web_error_code::INVALID_TASK_STATE,
            message,
            Some(json!({ "action": action, "task_id": task_id })),
        ),
        TaskControlApiError::Internal {
            action,
            task_id,
            message,
        } => response_error(
            web_error_code::TASK_CONTROL_FAILED,
            message,
            Some(json!({ "action": action, "task_id": task_id })),
        ),
    }
}
