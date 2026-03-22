use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    #[serde(rename = "prompt.submit")]
    PromptSubmit {
        request_id: String,
        session_id: String,
        prompt: String,
    },
    #[serde(rename = "turn.abort")]
    TurnAbort {
        request_id: String,
        session_id: String,
        turn_id: String,
    },
    #[serde(rename = "self_improve.get")]
    SelfImproveGet { request_id: String },
    #[serde(rename = "self_improve.start")]
    SelfImproveStart {
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_cycles: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        turns_per_cycle: Option<u32>,
    },
    #[serde(rename = "self_improve.stop")]
    SelfImproveStop { request_id: String },
    #[serde(rename = "self_improve.restart")]
    SelfImproveRestart {
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_cycles: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        turns_per_cycle: Option<u32>,
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
    pub sessions: Vec<SessionSummary>,
    pub transcript: Vec<TranscriptEntry>,
    pub active_turn: Option<ActiveTurnSnapshot>,
    pub context_usage: ContextUsage,
    pub self_improve: SelfImproveSnapshotData,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelfImproveSnapshotData {
    pub available: bool,
    pub active_run: Option<SelfImproveActiveRun>,
    pub history: Vec<SelfImproveRunRecord>,
    pub recent_logs: Vec<String>,
    pub retrospectives: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfImproveActiveRun {
    pub run_id: String,
    pub pid: u32,
    pub started_at: String,
    pub max_cycles: u32,
    pub turns_per_cycle: u32,
    pub stop_requested: bool,
    pub latest_status: String,
    pub latest_detail: String,
    pub log_tail: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfImproveRunRecord {
    pub run_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub max_cycles: u32,
    pub turns_per_cycle: u32,
    pub stop_requested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextUsage {
    pub used_chars: usize,
    pub max_chars: usize,
    pub percent_used: u8,
    pub input_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub total_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectedSession {
    pub session_id: String,
    pub title: String,
    pub status: String,
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
    #[serde(rename = "self_improve.snapshot")]
    SelfImproveSnapshot {
        event_id: String,
        ts: String,
        data: SelfImproveSnapshotData,
    },
    #[serde(rename = "self_improve.log")]
    SelfImproveLog {
        event_id: String,
        ts: String,
        run_id: String,
        line: String,
    },
    #[serde(rename = "self_improve.status")]
    SelfImproveStatus {
        event_id: String,
        ts: String,
        run_id: String,
        status: String,
        detail: String,
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
            | WebCommand::PromptSubmit { request_id, .. }
            | WebCommand::TurnAbort { request_id, .. }
            | WebCommand::SelfImproveGet { request_id }
            | WebCommand::SelfImproveStart { request_id, .. }
            | WebCommand::SelfImproveStop { request_id }
            | WebCommand::SelfImproveRestart { request_id, .. } => request_id,
        }
    }
}
