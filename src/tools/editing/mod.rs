//! Shared write-path contract (P0 semantics).

pub mod artifacts;
pub mod hashline;
pub mod hashline_anchor;
pub mod io;
pub mod observability;
pub mod telemetry;

use crate::text::truncate_with_ascii_ellipsis;
use serde::{Deserialize, Serialize};

/// Contract is strictly single-file per invocation.
pub const EDIT_SINGLE_FILE_ONLY: bool = true;

/// Contract is all-or-nothing apply for one invocation.
pub const EDIT_APPLY_IS_ATOMIC: bool = true;

/// Contract forbids automatic fallback to the legacy `patch` tool.
pub const EDIT_ALLOW_PATCH_FALLBACK: bool = false;

/// Maximum chars for compact model-visible summaries.
pub const EDIT_TOOL_SUMMARY_MAX_CHARS: usize = 160;

/// Recoverable edit failures are represented via `EditOutcome::Failed`.
pub trait EditEngine: Send + Sync {
    fn name(&self) -> &str;
    fn apply(&self, request: &EditRequest) -> EditOutcome;
}

/// Single-file edit request with operation list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditRequest {
    pub path: String,
    pub operations: Vec<EditOperation>,
}

impl EditRequest {
    pub fn validate_contract(&self) -> Result<(), EditFailureKind> {
        if self.path.trim().is_empty() {
            return Err(EditFailureKind::InvalidRequest);
        }

        if self.operations.is_empty() {
            return Err(EditFailureKind::InvalidRequest);
        }

        Ok(())
    }
}

/// Shared operation envelope; engine-specific semantics are layered later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditOperation {
    pub kind: String,
    pub anchor: String,
    #[serde(default)]
    pub end_anchor: Option<String>,
    #[serde(default)]
    pub lines: Vec<String>,
}

/// Edit result: fully applied or failed (no partial state).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EditOutcome {
    Applied {
        summary: String,
        #[serde(default)]
        observations: Vec<EditObservation>,
    },
    Failed {
        kind: EditFailureKind,
        summary: String,
        #[serde(default)]
        observations: Vec<EditObservation>,
    },
}

impl EditOutcome {
    pub fn tool_summary(&self) -> String {
        let summary = match self {
            Self::Applied { summary, .. } => summary,
            Self::Failed { summary, .. } => summary,
        };

        let first_line = summary.lines().next().unwrap_or("(empty)");
        truncate_with_ascii_ellipsis(first_line, EDIT_TOOL_SUMMARY_MAX_CHARS)
    }

    pub fn failure_kind(&self) -> Option<EditFailureKind> {
        match self {
            Self::Applied { .. } => None,
            Self::Failed { kind, .. } => Some(*kind),
        }
    }

    pub fn into_summary_and_observations(self) -> (String, Vec<EditObservation>) {
        match self {
            Self::Applied {
                summary,
                observations,
            }
            | Self::Failed {
                summary,
                observations,
                ..
            } => (summary, observations),
        }
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditFailureKind {
    StaleReference,
    AmbiguousAnchor,
    NoOp,
    InvalidRequest,
    IoError,
    NonTextFile,
    TelemetryUnavailable,
}

impl EditFailureKind {
    pub const ALL: [Self; 7] = [
        Self::StaleReference,
        Self::AmbiguousAnchor,
        Self::NoOp,
        Self::InvalidRequest,
        Self::IoError,
        Self::NonTextFile,
        Self::TelemetryUnavailable,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaleReference => "stale_reference",
            Self::AmbiguousAnchor => "ambiguous_anchor",
            Self::NoOp => "no_op",
            Self::InvalidRequest => "invalid_request",
            Self::IoError => "io_error",
            Self::NonTextFile => "non_text_file",
            Self::TelemetryUnavailable => "telemetry_unavailable",
        }
    }
}

/// Rich diagnostics channel kept separate from compact tool summaries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditObservation {
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub edit_count: usize,
    #[serde(default)]
    pub applied_count: usize,
    #[serde(default)]
    pub stale_reference_count: usize,
    #[serde(default)]
    pub noop_count: usize,
    #[serde(default)]
    pub failure_kind: Option<String>,
    #[serde(default)]
    pub duration_ms: u128,
    #[serde(default)]
    pub artifact_path: Option<String>,
    #[serde(default)]
    pub artifact_id: Option<String>,
    #[serde(default)]
    pub model_output_bounded: bool,
}

impl EditObservation {
    pub fn new(
        engine: &str,
        tool_name: &str,
        path: &str,
        edit_count: usize,
        duration_ms: u128,
    ) -> Self {
        Self {
            engine: engine.to_string(),
            tool_name: tool_name.to_string(),
            path: path.to_string(),
            edit_count,
            applied_count: 0,
            stale_reference_count: 0,
            noop_count: 0,
            failure_kind: None,
            duration_ms,
            artifact_path: None,
            artifact_id: None,
            model_output_bounded: true,
        }
    }
}
