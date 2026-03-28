use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::editing::hashline::HashlineEditEngine;
use super::editing::hashline_anchor::{parse_hashline_anchor, HashlineSnapshot};
use super::editing::observability::finalize_outcome;
use super::editing::{
    EditEngine, EditFailureKind, EditObservation, EditOperation, EditOutcome, EditRequest,
};
use super::{Tool, ToolExecutionResult};

pub struct HashlineEditTool;

impl Tool for HashlineEditTool {
    fn name(&self) -> &str {
        "hashline_edit"
    }

    fn description(&self) -> &str {
        "Validate anchored edits for one original file snapshot. Stale or ambiguous anchors fail explicitly with no fallback."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to one file"
                },
                "edits": {
                    "type": "array",
                    "description": "Edits against one original snapshot",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["replace", "insert_before", "insert_after", "delete"],
                            "description": "Edit kind"
                        },
                        "start": {
                            "type": "string",
                            "description": "Start anchor as LINE#HASH"
                        },
                        "end": {
                            "type": ["string", "null"],
                            "description": "Optional end anchor as LINE#HASH"
                        },
                        "replacement": {
                            "type": ["string", "null"],
                            "description": "Replacement text"
                        }
                    },
                    "required": ["kind", "start", "end", "replacement"],
                    "additionalProperties": false
                }
            }
            },
            "required": ["path", "edits"],
            "additionalProperties": false
        })
    }

    fn execute(&self, args: Value) -> Result<String> {
        self.execute_with_result(args).map(|result| result.output)
    }

    fn execute_with_result(&self, args: Value) -> Result<ToolExecutionResult> {
        let request = match serde_json::from_value::<HashlineEditRequest>(args) {
            Ok(request) => request,
            Err(err) => {
                let mut observation = EditObservation::new("hashline", self.name(), "", 0, 0);
                observation.failure_kind =
                    Some(EditFailureKind::InvalidRequest.as_str().to_string());
                let outcome = EditOutcome::Failed {
                    kind: EditFailureKind::InvalidRequest,
                    summary: format!("Error: invalid_request ({err})"),
                    observations: vec![observation],
                };
                let (output, edit_observations) = finalize_outcome(self.name(), outcome);
                return Ok(ToolExecutionResult {
                    output,
                    edit_observations,
                });
            }
        };

        if let Err(kind) = request.validate_contract() {
            return Ok(ToolExecutionResult::from_output(format!(
                "Error: {}",
                kind.as_str()
            )));
        }

        let outcome = HashlineEditEngine.apply(&request.as_edit_request());
        let (output, edit_observations) = finalize_outcome(self.name(), outcome);
        Ok(ToolExecutionResult {
            output,
            edit_observations,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HashlineEditRequest {
    pub path: String,
    pub edits: Vec<HashlineEdit>,
}

impl HashlineEditRequest {
    pub fn validate_contract(&self) -> Result<(), EditFailureKind> {
        if self.path.trim().is_empty() || self.edits.is_empty() {
            return Err(EditFailureKind::InvalidRequest);
        }

        for edit in &self.edits {
            edit.validate_contract()?;
        }

        Ok(())
    }

    pub fn validate_against_snapshot(
        &self,
        original_snapshot: &str,
    ) -> Result<Vec<ResolvedHashlineEdit>, EditFailureKind> {
        self.validate_contract()?;

        let snapshot = HashlineSnapshot::from_text(original_snapshot);
        let mut previous_end_line = 0usize;
        let mut resolved = Vec::with_capacity(self.edits.len());

        for edit in &self.edits {
            let start_anchor = parse_hashline_anchor(&edit.start)?;
            let start_line = snapshot.resolve_anchor(&start_anchor)?;
            let end_line = match edit.end.as_deref() {
                Some(end) => snapshot.resolve_anchor(&parse_hashline_anchor(end)?)?,
                None => start_line,
            };

            if end_line < start_line || start_line <= previous_end_line {
                return Err(EditFailureKind::InvalidRequest);
            }

            previous_end_line = end_line;
            resolved.push(ResolvedHashlineEdit {
                kind: edit.kind,
                start_line,
                end_line,
                replacement: edit.replacement.clone(),
            });
        }

        Ok(resolved)
    }

    pub fn as_edit_request(&self) -> EditRequest {
        EditRequest {
            path: self.path.clone(),
            operations: self
                .edits
                .iter()
                .map(HashlineEdit::as_edit_operation)
                .collect(),
        }
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HashlineEditKind {
    Replace,
    InsertBefore,
    InsertAfter,
    Delete,
}

impl HashlineEditKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::InsertBefore => "insert_before",
            Self::InsertAfter => "insert_after",
            Self::Delete => "delete",
        }
    }

    const fn supports_end(self) -> bool {
        matches!(self, Self::Replace | Self::Delete)
    }

    const fn requires_replacement(self) -> bool {
        matches!(self, Self::Replace | Self::InsertBefore | Self::InsertAfter)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HashlineEdit {
    pub kind: HashlineEditKind,
    pub start: String,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub replacement: Option<String>,
}

impl HashlineEdit {
    fn validate_contract(&self) -> Result<(), EditFailureKind> {
        parse_hashline_anchor(&self.start)?;

        if !self.kind.supports_end() && self.end.is_some() {
            return Err(EditFailureKind::InvalidRequest);
        }

        if let Some(end) = &self.end {
            parse_hashline_anchor(end)?;
        }

        if self.kind.requires_replacement() != self.replacement.is_some() {
            return Err(EditFailureKind::InvalidRequest);
        }

        Ok(())
    }

    fn as_edit_operation(&self) -> EditOperation {
        let mut lines = Vec::new();
        if let Some(replacement) = &self.replacement {
            lines.push(replacement.clone());
        }

        EditOperation {
            kind: self.kind.as_str().to_string(),
            anchor: self.start.clone(),
            end_anchor: self.end.clone(),
            lines,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHashlineEdit {
    pub kind: HashlineEditKind,
    pub start_line: usize,
    pub end_line: usize,
    pub replacement: Option<String>,
}
