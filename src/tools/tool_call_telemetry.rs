use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;

pub const TOOL_CALL_METRICS_JSONL_FILE: &str = "tool-call-events.jsonl";
const TOOL_CALL_METRICS_DIR_ENV: &str = "KLEY_TOOL_CALL_METRICS_DIR";

thread_local! {
    static TOOL_CALL_METRICS_ROOT_OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallExecutionKind {
    ApprovalDenied,
    InvalidArguments,
    RuntimeEntrypoint,
    RegistryTool,
    UnknownTool,
}

impl ToolCallExecutionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalDenied => "approval_denied",
            Self::InvalidArguments => "invalid_arguments",
            Self::RuntimeEntrypoint => "runtime_entrypoint",
            Self::RegistryTool => "registry_tool",
            Self::UnknownTool => "unknown_tool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallFailureKind {
    ApprovalDenied,
    InvalidArguments,
    RuntimeHandlerFailed,
    ExecutionFailed,
    ToolReportedError,
    UnknownTool,
}

impl ToolCallFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalDenied => "approval_denied",
            Self::InvalidArguments => "invalid_arguments",
            Self::RuntimeHandlerFailed => "runtime_handler_failed",
            Self::ExecutionFailed => "execution_failed",
            Self::ToolReportedError => "tool_reported_error",
            Self::UnknownTool => "unknown_tool",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallMetricRecord {
    pub session_id: String,
    pub turn_id: String,
    pub message_id: String,
    pub tool_call_id: String,
    pub provider: String,
    pub model: String,
    pub tool_name: String,
    pub arguments_preview: String,
    pub output_preview: String,
    pub execution_kind: ToolCallExecutionKind,
    pub success: bool,
    pub duration_ms: u128,
    pub failure_kind: Option<ToolCallFailureKind>,
    pub diagnostic_codes: Vec<String>,
    pub edit_failure_kind: Option<String>,
    pub edit_artifact_id: Option<String>,
    pub edit_artifact_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct PersistedToolCallMetricRecord<'a> {
    event: &'static str,
    created_at: String,
    session_id: &'a str,
    turn_id: &'a str,
    message_id: &'a str,
    tool_call_id: &'a str,
    provider: &'a str,
    model: &'a str,
    tool_name: &'a str,
    arguments_preview: &'a str,
    output_preview: &'a str,
    execution_kind: &'static str,
    success: bool,
    duration_ms: u128,
    failure_kind: Option<&'static str>,
    diagnostic_codes: &'a [String],
    edit_failure_kind: Option<&'a str>,
    edit_artifact_id: Option<&'a str>,
    edit_artifact_path: Option<&'a str>,
}

pub fn persist_metric(record: &ToolCallMetricRecord) -> std::io::Result<()> {
    let root = metrics_root_dir();
    fs::create_dir_all(&root)?;

    let persisted = PersistedToolCallMetricRecord {
        event: "tool.call.completed",
        created_at: Utc::now().to_rfc3339(),
        session_id: &record.session_id,
        turn_id: &record.turn_id,
        message_id: &record.message_id,
        tool_call_id: &record.tool_call_id,
        provider: &record.provider,
        model: &record.model,
        tool_name: &record.tool_name,
        arguments_preview: &record.arguments_preview,
        output_preview: &record.output_preview,
        execution_kind: record.execution_kind.as_str(),
        success: record.success,
        duration_ms: record.duration_ms,
        failure_kind: record.failure_kind.map(|kind| kind.as_str()),
        diagnostic_codes: &record.diagnostic_codes,
        edit_failure_kind: record.edit_failure_kind.as_deref(),
        edit_artifact_id: record.edit_artifact_id.as_deref(),
        edit_artifact_path: record.edit_artifact_path.as_deref(),
    };

    let mut line = serde_json::to_string(&persisted).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(TOOL_CALL_METRICS_JSONL_FILE))?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn metrics_root_dir() -> PathBuf {
    if let Some(override_path) = TOOL_CALL_METRICS_ROOT_OVERRIDE.with(|slot| slot.borrow().clone())
    {
        return override_path;
    }

    if let Ok(override_dir) = std::env::var(TOOL_CALL_METRICS_DIR_ENV) {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    if let Some(local_data) = dirs::data_local_dir() {
        return local_data.join("kley").join("tool-call-metrics");
    }

    std::env::temp_dir().join("kley").join("tool-call-metrics")
}

pub fn with_metrics_root_override<T>(path: &Path, action: impl FnOnce() -> T) -> T {
    let _guard = install_metrics_root_override(path);
    action()
}

pub async fn with_metrics_root_override_async<T>(
    path: &Path,
    future: impl Future<Output = T>,
) -> T {
    let _guard = install_metrics_root_override(path);
    future.await
}

struct ResetGuard {
    previous: Option<PathBuf>,
}

impl Drop for ResetGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        TOOL_CALL_METRICS_ROOT_OVERRIDE.with(|slot| {
            *slot.borrow_mut() = previous;
        });
    }
}

fn install_metrics_root_override(path: &Path) -> ResetGuard {
    let previous = TOOL_CALL_METRICS_ROOT_OVERRIDE.with(|slot| {
        let mut current = slot.borrow_mut();
        (*current).replace(path.to_path_buf())
    });
    ResetGuard { previous }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn sample_record() -> ToolCallMetricRecord {
        ToolCallMetricRecord {
            session_id: "session-1".to_string(),
            turn_id: "turn-1".to_string(),
            message_id: "message-1".to_string(),
            tool_call_id: "call-1".to_string(),
            provider: "openai".to_string(),
            model: "gpt-5.4".to_string(),
            tool_name: "shell".to_string(),
            arguments_preview: "{\"command\":\"pwd\"}".to_string(),
            output_preview: "/tmp".to_string(),
            execution_kind: ToolCallExecutionKind::RegistryTool,
            success: true,
            duration_ms: 12,
            failure_kind: None,
            diagnostic_codes: vec![],
            edit_failure_kind: None,
            edit_artifact_id: None,
            edit_artifact_path: None,
        }
    }

    #[test]
    fn persist_metric_writes_complete_jsonl_line() {
        let metrics_root = tempdir().unwrap();
        with_metrics_root_override(metrics_root.path(), || {
            let record = sample_record();
            persist_metric(&record).unwrap();
            let mut failed_record = sample_record();
            failed_record.tool_call_id = "call-2".to_string();
            failed_record.failure_kind = Some(ToolCallFailureKind::ExecutionFailed);
            persist_metric(&failed_record).unwrap();
        });

        let metrics_path = metrics_root.path().join(TOOL_CALL_METRICS_JSONL_FILE);
        let contents = fs::read_to_string(metrics_path).unwrap();
        let newline_count = contents.chars().filter(|c| *c == '\n').count();
        assert_eq!(newline_count, 2);
        assert!(contents.ends_with('\n'));
    }
}
