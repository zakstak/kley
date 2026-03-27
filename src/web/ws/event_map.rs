use chrono::Utc;
use uuid::Uuid;

use super::context_usage::context_usage_from_event;
use crate::events::AgentEvent;
use crate::web::protocol::UiEvent;

pub(super) fn runtime_event_to_ui_event(
    event: &AgentEvent,
    request_id: &str,
    default_session_id: &str,
) -> Option<UiEvent> {
    match event {
        AgentEvent::TurnStarted {
            session_id,
            turn_id,
            context_used_chars,
            context_max_chars,
            ..
        } => Some(UiEvent::TurnStarted {
            event_id: format!("evt-{}", Uuid::new_v4()),
            ts: ts_now(),
            request_id: request_id.to_string(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            context_usage: context_usage_from_event(
                *context_used_chars,
                *context_max_chars,
                None,
                None,
                None,
            ),
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
            context_used_chars,
            context_max_chars,
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
            context_usage: context_usage_from_event(
                *context_used_chars,
                *context_max_chars,
                None,
                None,
                None,
            ),
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
            context_usage: context_usage_from_event(
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

pub(super) fn ts_now() -> String {
    Utc::now().to_rfc3339()
}
