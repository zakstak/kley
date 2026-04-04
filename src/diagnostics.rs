use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::editing::EditObservation;

pub mod lsp_status {
    pub const DETECTED: &str = "lsp.detected";
    pub const STARTING: &str = "lsp.starting";
    pub const READY: &str = "lsp.ready";
    pub const FAILED: &str = "lsp.failed";
}

pub mod web_error_code {
    pub const AUTH_COMPLETION_FAILED: &str = "auth_completion_failed";
    pub const AUTH_FLOW_MISMATCH: &str = "auth_flow_mismatch";
    pub const AUTH_LOGIN_FAILED: &str = "auth_login_failed";
    pub const AUTH_START_FAILED: &str = "auth_start_failed";
    pub const INTERNAL_ERROR: &str = "internal_error";
    pub const INVALID_API_KEY: &str = "invalid_api_key";
    pub const INVALID_AUTH_CALLBACK: &str = "invalid_auth_callback";
    pub const INVALID_COMMAND: &str = "invalid_command";
    pub const INVALID_MODEL: &str = "invalid_model";
    pub const INVALID_PROVIDER: &str = "invalid_provider";
    pub const INVALID_SESSION: &str = "invalid_session";
    pub const INVALID_TASK_CURSOR: &str = "invalid_task_cursor";
    pub const INVALID_TASK_STATE: &str = "invalid_task_state";
    pub const RUNTIME_FAILED: &str = "runtime_failed";
    pub const RUNTIME_UNAVAILABLE: &str = "runtime_unavailable";
    pub const SESSION_NOT_FOUND: &str = "session_not_found";
    pub const SESSION_BUSY: &str = "session_busy";
    pub const SETTINGS_UPDATE_FAILED: &str = "settings_update_failed";
    pub const TASK_CONTROL_FAILED: &str = "task_control_failed";
    pub const TASK_NOT_FOUND: &str = "task_not_found";
    pub const TASK_WATCH_FAILED: &str = "task_watch_failed";
    pub const TURN_IN_PROGRESS: &str = "turn_in_progress";
    pub const TURN_NOT_FOUND: &str = "turn_not_found";
    pub const TURN_STATE_ERROR: &str = "turn_state_error";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl Diagnostic {
    pub fn new(
        code: impl Into<String>,
        severity: DiagnosticSeverity,
        message: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
            source: source.into(),
            operation: None,
            details: None,
        }
    }

    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self::new(code, DiagnosticSeverity::Error, message, source)
    }

    pub fn warning(
        code: impl Into<String>,
        message: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self::new(code, DiagnosticSeverity::Warning, message, source)
    }

    pub fn info(
        code: impl Into<String>,
        message: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self::new(code, DiagnosticSeverity::Info, message, source)
    }

    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

pub fn has_error_diagnostics(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
}

pub fn diagnostics_from_edit_observations(observations: &[EditObservation]) -> Vec<Diagnostic> {
    observations
        .iter()
        .map(|observation| {
            let details = json!({
                "engine": observation.engine,
                "tool_name": observation.tool_name,
                "path": observation.path,
                "edit_count": observation.edit_count,
                "applied_count": observation.applied_count,
                "stale_reference_count": observation.stale_reference_count,
                "noop_count": observation.noop_count,
                "failure_kind": observation.failure_kind,
                "duration_ms": observation.duration_ms,
                "artifact_id": observation.artifact_id,
                "artifact_path": observation.artifact_path,
                "model_output_bounded": observation.model_output_bounded,
            });

            if let Some(kind) = observation.failure_kind.as_deref() {
                Diagnostic::error(
                    format!("tool.edit.{kind}"),
                    format!("{} reported {kind}", observation.tool_name),
                    "tools.editing",
                )
                .with_operation(observation.tool_name.clone())
                .with_details(details)
            } else {
                Diagnostic::info(
                    "tool.edit.observation",
                    format!("{} produced an edit observation", observation.tool_name),
                    "tools.editing",
                )
                .with_operation(observation.tool_name.clone())
                .with_details(details)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_observation_failure_maps_to_error_diagnostic() {
        let observations = vec![EditObservation {
            engine: "hashline".to_string(),
            tool_name: "hashline_edit".to_string(),
            path: "src/lib.rs".to_string(),
            edit_count: 1,
            applied_count: 0,
            stale_reference_count: 1,
            noop_count: 0,
            failure_kind: Some("stale_reference".to_string()),
            duration_ms: 12,
            artifact_path: None,
            artifact_id: None,
            model_output_bounded: true,
        }];

        let diagnostics = diagnostics_from_edit_observations(&observations);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "tool.edit.stale_reference");
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
        assert!(has_error_diagnostics(&diagnostics));
    }
}
