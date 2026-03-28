//! Patch tool — search/replace editing.
//!
//! This is a simple placeholder implementation. It will be replaced by
//! hashline-edit in a future PR for better accuracy on cheap models.

use anyhow::Result;
use serde_json::Value;
use std::time::Instant;

use super::{Tool, ToolExecutionResult};
use crate::tools::editing::io::atomic_replace;
use crate::tools::editing::observability::finalize_outcome;
use crate::tools::editing::{
    EditEngine, EditFailureKind, EditObservation, EditOperation, EditOutcome, EditRequest,
};

pub struct PatchTool;

pub struct PatchEditEngine;

impl PatchEditEngine {
    fn replacement_from_operation(operation: &EditOperation) -> String {
        if operation.lines.len() == 1 {
            return operation.lines[0].clone();
        }

        operation.lines.join("\n")
    }

    fn invalid_request(
        summary: String,
        path: &str,
        edit_count: usize,
        duration_ms: u128,
    ) -> EditOutcome {
        EditOutcome::Failed {
            kind: EditFailureKind::InvalidRequest,
            summary,
            observations: vec![failure_observation(
                EditFailureKind::InvalidRequest,
                path,
                edit_count,
                duration_ms,
            )],
        }
    }

    fn io_error(summary: String, path: &str, edit_count: usize, duration_ms: u128) -> EditOutcome {
        EditOutcome::Failed {
            kind: EditFailureKind::IoError,
            summary,
            observations: vec![failure_observation(
                EditFailureKind::IoError,
                path,
                edit_count,
                duration_ms,
            )],
        }
    }
}

impl EditEngine for PatchEditEngine {
    fn name(&self) -> &str {
        "patch"
    }

    fn apply(&self, request: &EditRequest) -> EditOutcome {
        let started_at = Instant::now();
        let path = request.path.as_str();
        let edit_count = request.operations.len();

        if request.path.trim().is_empty() {
            return Self::invalid_request(
                "Error: path is required".to_string(),
                path,
                edit_count,
                started_at.elapsed().as_millis(),
            );
        }

        if edit_count != 1 {
            let summary = if edit_count == 0 {
                "Error: target is required".to_string()
            } else {
                "Error: patch only supports a single operation".to_string()
            };

            return Self::invalid_request(
                summary,
                path,
                edit_count,
                started_at.elapsed().as_millis(),
            );
        }

        let operation = &request.operations[0];
        let target = operation.anchor.as_str();
        if target.is_empty() {
            return Self::invalid_request(
                "Error: target is required".to_string(),
                path,
                edit_count,
                started_at.elapsed().as_millis(),
            );
        }

        let replacement = Self::replacement_from_operation(operation);

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                return Self::io_error(
                    format!("Error: {e}"),
                    path,
                    edit_count,
                    started_at.elapsed().as_millis(),
                );
            }
        };

        let matches: Vec<usize> = content.match_indices(target).map(|(idx, _)| idx).collect();

        match matches.len() {
            0 => {
                let target_first = target.lines().next().unwrap_or(target).trim();
                let mut best_match = None;
                let mut best_score = 0usize;

                for (line_num, line) in content.lines().enumerate() {
                    let score = common_prefix_len(line.trim(), target_first);
                    if score > best_score {
                        best_score = score;
                        best_match = Some((line_num + 1, line));
                    }
                }

                let hint = if let Some((ln, _line)) = best_match {
                    format!(" Closest match at line {ln}.")
                } else {
                    String::new()
                };

                EditOutcome::Failed {
                    kind: EditFailureKind::StaleReference,
                    summary: format!(
                        "Error: target not found in {path}. No exact match for the search text.{hint}"
                    ),
                    observations: vec![failure_observation(
                        EditFailureKind::StaleReference,
                        path,
                        edit_count,
                        started_at.elapsed().as_millis(),
                    )],
                }
            }
            1 => {
                let new_content = content.replacen(target, &replacement, 1);
                if let Err(e) = atomic_replace(std::path::Path::new(path), new_content.as_bytes()) {
                    return Self::io_error(
                        format!("Error: failed to write {path}: {e}"),
                        path,
                        edit_count,
                        started_at.elapsed().as_millis(),
                    );
                }

                let target_lines = target.lines().count();
                let replacement_lines = replacement.lines().count();
                EditOutcome::Applied {
                    summary: format!(
                        "Applied: replaced {target_lines} line(s) with {replacement_lines} line(s) in {path}"
                    ),
                    observations: vec![success_observation(
                        path,
                        edit_count,
                        started_at.elapsed().as_millis(),
                    )],
                }
            }
            n => {
                let line_numbers: Vec<usize> = matches
                    .iter()
                    .map(|&byte_offset| content[..byte_offset].lines().count() + 1)
                    .collect();
                let lines_str: Vec<String> = line_numbers.iter().map(|n| n.to_string()).collect();

                EditOutcome::Failed {
                    kind: EditFailureKind::AmbiguousAnchor,
                    summary: format!(
                        "Error: found {n} matches in {path} (at lines {}). Target must be unique — include more context to disambiguate.",
                        lines_str.join(", ")
                    ),
                    observations: vec![failure_observation(
                        EditFailureKind::AmbiguousAnchor,
                        path,
                        edit_count,
                        started_at.elapsed().as_millis(),
                    )],
                }
            }
        }
    }
}

fn success_observation(path: &str, edit_count: usize, duration_ms: u128) -> EditObservation {
    let mut observation = EditObservation::new("patch", "patch", path, edit_count, duration_ms);
    observation.applied_count = edit_count;
    observation
}

fn failure_observation(
    kind: EditFailureKind,
    path: &str,
    edit_count: usize,
    duration_ms: u128,
) -> EditObservation {
    let mut observation = EditObservation::new("patch", "patch", path, edit_count, duration_ms);
    observation.failure_kind = Some(kind.as_str().to_string());
    observation.stale_reference_count = usize::from(kind == EditFailureKind::StaleReference);
    observation.noop_count = usize::from(kind == EditFailureKind::NoOp);
    observation
}

impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "Apply a search/replace edit to a file. Finds exactly one occurrence of the target text and replaces it. Returns an error if zero or multiple matches are found."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "target": {
                    "type": "string",
                    "description": "Exact text to find (must match exactly one occurrence)"
                },
                "replacement": {
                    "type": "string",
                    "description": "Text to replace the target with"
                }
            },
            "required": ["path", "target", "replacement"],
            "additionalProperties": false,
        })
    }

    fn execute(&self, args: Value) -> Result<String> {
        self.execute_with_result(args).map(|result| result.output)
    }

    fn execute_with_result(&self, args: Value) -> Result<ToolExecutionResult> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let target = args.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let replacement = args
            .get("replacement")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let request = EditRequest {
            path: path.to_string(),
            operations: vec![EditOperation {
                kind: "replace_exact".to_string(),
                anchor: target.to_string(),
                end_anchor: None,
                lines: vec![replacement.to_string()],
            }],
        };

        let outcome = PatchEditEngine.apply(&request);
        let (output, edit_observations) = finalize_outcome(self.name(), outcome);
        Ok(ToolExecutionResult {
            output,
            edit_observations,
        })
    }
}

/// Length of the common prefix between two strings (character-wise).
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn patch_exact_match() {
        let f = temp_file("fn main() {\n    println!(\"hello\");\n}\n");
        let path = f.path().to_str().unwrap();
        let tool = PatchTool;
        let result = tool
            .execute(serde_json::json!({
                "path": path,
                "target": "    println!(\"hello\");",
                "replacement": "    println!(\"world\");",
            }))
            .unwrap();
        assert!(result.contains("Applied"));

        let updated = std::fs::read_to_string(path).unwrap();
        assert!(updated.contains("world"));
        assert!(!updated.contains("hello"));
    }

    #[test]
    fn patch_no_match() {
        let f = temp_file("fn main() {}\n");
        let tool = PatchTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap(),
                "target": "nonexistent text",
                "replacement": "whatever",
            }))
            .unwrap();
        assert!(result.contains("Error: target not found"));
    }

    #[test]
    fn patch_multiple_matches() {
        let f = temp_file("aaa\nbbb\naaa\n");
        let tool = PatchTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap(),
                "target": "aaa",
                "replacement": "ccc",
            }))
            .unwrap();
        assert!(result.contains("found 2 matches"));
        assert!(result.contains("disambiguate"));
    }

    #[test]
    fn patch_rejects_multiple_operations() {
        let f = temp_file("abc\n");
        let path = f.path().to_str().unwrap().to_string();
        let request = EditRequest {
            path: path.clone(),
            operations: vec![
                EditOperation {
                    kind: "replace_exact".to_string(),
                    anchor: "abc".to_string(),
                    end_anchor: None,
                    lines: vec!["first".to_string()],
                },
                EditOperation {
                    kind: "replace_exact".to_string(),
                    anchor: "abc".to_string(),
                    end_anchor: None,
                    lines: vec!["second".to_string()],
                },
            ],
        };

        let outcome = PatchEditEngine.apply(&request);
        match outcome {
            EditOutcome::Failed { kind, summary, .. } => {
                assert_eq!(kind, EditFailureKind::InvalidRequest);
                assert_eq!(summary, "Error: patch only supports a single operation");
            }
            other => panic!("Expected invalid request failure, got {other:?}"),
        }

        let final_contents = std::fs::read_to_string(path).unwrap();
        assert_eq!(final_contents, "abc\n");
    }
}
