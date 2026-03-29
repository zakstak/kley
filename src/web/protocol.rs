use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::editing::EditObservation;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum WebCommand {
    #[serde(rename = "state.get")]
    StateGet { request_id: String },
    #[serde(rename = "sessions.list")]
    SessionsList { request_id: String },
    #[serde(rename = "session.load")]
    SessionLoad {
        request_id: String,
        session_id: String,
    },
    #[serde(rename = "session.settings.update")]
    SessionSettingsUpdate {
        request_id: String,
        session_id: String,
        provider: String,
        model: String,
    },
    #[serde(rename = "prompt.submit")]
    PromptSubmit {
        request_id: String,
        session_id: String,
        prompt: String,
    },
    #[serde(rename = "auth.openai.start")]
    AuthOpenAiStart { request_id: String },
    #[serde(rename = "auth.openai.complete")]
    AuthOpenAiComplete {
        request_id: String,
        callback_input: String,
    },
    #[serde(rename = "auth.login")]
    AuthLogin {
        request_id: String,
        provider: String,
        api_key: String,
    },
    #[serde(rename = "turn.abort")]
    TurnAbort {
        request_id: String,
        session_id: String,
        turn_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum WebResponse {
    #[serde(rename = "response.ok")]
    Ok { request_id: String, data: Value },
    #[serde(rename = "response.error")]
    Error {
        request_id: String,
        error: ResponseError,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateSnapshotData {
    pub protocol_version: u32,
    pub session_id: String,
    pub selected_session: SelectedSession,
    pub auth: AuthStateSnapshot,
    pub sessions: Vec<SessionSummary>,
    pub transcript: Vec<TranscriptEntry>,
    pub active_turn: Option<ActiveTurnSnapshot>,
    pub context_usage: ContextUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthStateSnapshot {
    pub storage_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_provider: Option<String>,
    pub openai_logged_in: bool,
    pub zai_logged_in: bool,
    pub pending_openai_login: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextUsage {
    pub used_chars: usize,
    pub max_chars: usize,
    pub percent_used: u8,
    pub input_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub total_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<ContextUsageBreakdown>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextUsageBreakdown {
    pub system_prompt: ContextUsageBucket,
    pub user_input: ContextUsageBucket,
    pub assistant_output: ContextUsageBucket,
    pub skill_calls: ContextUsageBucket,
    pub mcp_calls: ContextUsageBucket,
    pub tool_calls: ContextUsageBucket,
    pub other: ContextUsageBucket,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextUsageBucket {
    pub chars_estimate: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_estimate: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectedSession {
    pub session_id: String,
    pub title: String,
    pub status: String,
    pub provider: String,
    pub model: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "type")]
pub enum UiEvent {
    #[serde(rename = "state.snapshot")]
    StateSnapshot {
        event_id: String,
        ts: String,
        #[serde(flatten)]
        data: StateSnapshotData,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {
        event_id: String,
        ts: String,
        request_id: String,
        session_id: String,
        turn_id: String,
        context_usage: ContextUsage,
    },
    #[serde(rename = "message.started")]
    MessageStarted {
        event_id: String,
        ts: String,
        request_id: String,
        session_id: String,
        turn_id: String,
        message_id: String,
    },
    #[serde(rename = "message.delta")]
    MessageDelta {
        event_id: String,
        ts: String,
        request_id: String,
        session_id: String,
        turn_id: String,
        message_id: String,
        delta: String,
    },
    #[serde(rename = "message.completed")]
    MessageCompleted {
        event_id: String,
        ts: String,
        request_id: String,
        session_id: String,
        turn_id: String,
        message_id: String,
        content: String,
    },
    #[serde(rename = "tool.started")]
    ToolStarted {
        event_id: String,
        ts: String,
        request_id: String,
        session_id: String,
        turn_id: String,
        tool_call_id: String,
        tool_name: String,
    },
    #[serde(rename = "tool.completed")]
    ToolCompleted {
        event_id: String,
        ts: String,
        request_id: String,
        session_id: String,
        turn_id: String,
        tool_call_id: String,
        tool_name: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        edit_observation: Option<EditObservation>,
        context_usage: ContextUsage,
    },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        event_id: String,
        ts: String,
        request_id: String,
        session_id: String,
        turn_id: String,
        context_usage: ContextUsage,
    },
    #[serde(rename = "turn.failed")]
    TurnFailed {
        event_id: String,
        ts: String,
        request_id: String,
        session_id: String,
        turn_id: String,
        error: String,
    },
    #[serde(rename = "status.report")]
    StatusReport {
        event_id: String,
        ts: String,
        session_id: String,
        status: String,
        detail: String,
    },
    #[serde(rename = "transport.selected")]
    TransportSelected {
        event_id: String,
        ts: String,
        session_id: String,
        transport: String,
    },
    #[serde(rename = "transport.fallback")]
    TransportFallback {
        event_id: String,
        ts: String,
        session_id: String,
        from: String,
        to: String,
        reason: String,
    },
    #[serde(rename = "auth.token_refreshed")]
    AuthTokenRefreshed {
        event_id: String,
        ts: String,
        session_id: String,
        provider: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub session_id: String,
    pub title: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub turn_number: i64,
    pub kind: String,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveTurnSnapshot {
    pub request_id: String,
    pub turn_id: String,
    pub message_id: String,
    pub content: String,
}

impl WebCommand {
    pub fn request_id(&self) -> &str {
        match self {
            WebCommand::StateGet { request_id }
            | WebCommand::SessionsList { request_id }
            | WebCommand::SessionLoad { request_id, .. }
            | WebCommand::SessionSettingsUpdate { request_id, .. }
            | WebCommand::PromptSubmit { request_id, .. }
            | WebCommand::AuthOpenAiStart { request_id }
            | WebCommand::AuthOpenAiComplete { request_id, .. }
            | WebCommand::AuthLogin { request_id, .. }
            | WebCommand::TurnAbort { request_id, .. } => request_id,
        }
    }
}
