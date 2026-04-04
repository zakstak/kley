//! Tool infrastructure for kley's agent loop.
//!
//! Each tool implements the `Tool` trait and is registered with a `ToolRegistry`.
//! The registry generates the OpenAI Responses API `tools` array and dispatches
//! calls by name.

pub mod editing;
pub mod hashline_edit;
pub mod lsp;
pub mod patch;
pub mod read_file;
pub mod read_skill;
pub mod report_status;
pub mod shell;

use anyhow::Result;
use serde_json::Value;

use crate::diagnostics::{Diagnostic, diagnostics_from_edit_observations, has_error_diagnostics};
use crate::tools::editing::EditObservation;

pub struct DelegateTaskTool;

impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "Create a delegated child task with a bounded handoff brief and return stable task-id tracking details."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "parent_task_id": {
                    "type": "string",
                    "description": "Stable task_id for the parent task that is delegating work."
                },
                "child_task_id": {
                    "type": "string",
                    "description": "Optional stable task_id for the child; if omitted, one is generated."
                },
                "title": {
                    "type": "string",
                    "description": "Optional child task title shown to the delegated worker."
                },
                "priority": {
                    "type": "integer",
                    "description": "Child task priority. Higher numbers run first."
                },
                "handoff_brief": {
                    "type": "string",
                    "description": "Bounded delegation brief for child bootstrap; do not pass raw transcript replay."
                },
                "artifact_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional artifact references included in child handoff bootstrap."
                },
                "requested_policy_json": {
                    "type": "string",
                    "description": "Optional JSON object string requesting a narrowed child policy."
                },
                "after_sequence": {
                    "type": "integer",
                    "description": "Optional task event replay cursor for initial status subscription."
                }
            },
            "required": [
                "parent_task_id",
                "child_task_id",
                "title",
                "priority",
                "handoff_brief",
                "artifact_ids",
                "requested_policy_json",
                "after_sequence"
            ],
            "additionalProperties": false,
        })
    }

    fn execute(&self, _args: Value) -> Result<String> {
        Ok("delegate_task is handled by the runtime delegation entrypoint".to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    pub output: String,
    pub edit_observations: Vec<EditObservation>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ToolExecutionResult {
    pub fn from_output(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            edit_observations: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn with_edit_observations(
        output: impl Into<String>,
        edit_observations: Vec<EditObservation>,
    ) -> Self {
        let diagnostics = diagnostics_from_edit_observations(&edit_observations);
        Self {
            output: output.into(),
            edit_observations,
            diagnostics,
        }
    }

    pub fn with_diagnostics(output: impl Into<String>, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            output: output.into(),
            edit_observations: Vec::new(),
            diagnostics,
        }
    }

    pub fn is_success(&self) -> bool {
        !has_error_diagnostics(&self.diagnostics)
    }
}

impl From<String> for ToolExecutionResult {
    fn from(value: String) -> Self {
        Self::from_output(value)
    }
}

/// A tool that the agent can invoke during a conversation.
pub trait Tool: Send + Sync {
    /// Unique name used in API tool declarations and dispatch.
    fn name(&self) -> &str;

    /// Human-readable description shown to the model.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's parameters.
    fn parameters_schema(&self) -> Value;

    /// Execute the tool with the given JSON arguments.
    /// Returns a string result to feed back to the model.
    ///
    /// Errors from this method are **tool-level** failures (bug in the tool
    /// implementation). Recoverable domain errors (file not found, command
    /// exited non-zero) should be returned as `Ok(error_message)` so the
    /// model can see them and adapt.
    fn execute(&self, args: Value) -> Result<String>;

    fn execute_with_result(&self, args: Value) -> Result<ToolExecutionResult> {
        self.execute(args).map(ToolExecutionResult::from_output)
    }

    fn bind_session_context(&mut self, _session_id: &str) {}
}

/// Registry of available tools. Handles schema generation and dispatch.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Register a tool. Panics if a tool with the same name already exists.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        debug_assert!(
            !self.tools.iter().any(|t| t.name() == tool.name()),
            "duplicate tool name: {}",
            tool.name()
        );
        self.tools.push(tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|t| &**t)
    }

    pub fn bind_session_context(&mut self, session_id: &str) {
        for tool in &mut self.tools {
            tool.bind_session_context(session_id);
        }
    }

    /// Generate the `tools` array for the OpenAI Responses API request.
    pub fn to_api_tools(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "name": t.name(),
                    "description": t.description(),
                    "parameters": t.parameters_schema(),
                    "strict": true,
                })
            })
            .collect()
    }
}

/// Create a registry with all built-in tools.
pub fn default_registry(project_dir: std::path::PathBuf) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    let lsp_service: std::sync::Arc<dyn crate::lsp::LspService> =
        std::sync::Arc::new(crate::lsp::LspManager::new());
    reg.register(Box::new(shell::ShellTool::new()));
    reg.register(Box::new(read_file::ReadFileTool));
    reg.register(Box::new(patch::PatchTool));
    reg.register(Box::new(hashline_edit::HashlineEditTool));
    reg.register(Box::new(lsp::LspDiagnosticsTool::new(
        project_dir.clone(),
        "tool-registry-lsp",
        lsp_service.clone(),
    )));
    reg.register(Box::new(lsp::LspSymbolsTool::new(
        project_dir.clone(),
        "tool-registry-lsp",
        lsp_service.clone(),
    )));
    reg.register(Box::new(lsp::LspGotoDefinitionTool::new(
        project_dir.clone(),
        "tool-registry-lsp",
        lsp_service.clone(),
    )));
    reg.register(Box::new(lsp::LspFindReferencesTool::new(
        project_dir.clone(),
        "tool-registry-lsp",
        lsp_service.clone(),
    )));
    reg.register(Box::new(lsp::LspPrepareRenameTool::new(
        project_dir.clone(),
        "tool-registry-lsp",
        lsp_service.clone(),
    )));
    reg.register(Box::new(lsp::LspRenameTool::new(
        project_dir.clone(),
        "tool-registry-lsp",
        lsp_service,
    )));
    reg.register(Box::new(read_skill::ReadSkillTool::new(project_dir)));
    reg.register(Box::new(DelegateTaskTool));
    reg.register(Box::new(report_status::ReportStatusTool));
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTool;
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy"
        }
        fn description(&self) -> &str {
            "A dummy tool for testing"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false,
            })
        }
        fn execute(&self, _args: Value) -> Result<String> {
            Ok("dummy result".into())
        }
    }

    #[test]
    fn registry_register_and_lookup() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(DummyTool));
        assert!(reg.get("dummy").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn registry_to_api_tools() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(DummyTool));
        let tools = reg.to_api_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "dummy");
        assert_eq!(tools[0]["strict"], true);
    }

    #[test]
    fn default_registry_tool_schemas_match_strict_mode_requirements() {
        let reg = default_registry(std::env::temp_dir());

        for tool in reg.to_api_tools() {
            assert_eq!(tool["strict"], true);

            let parameters = tool["parameters"].as_object().unwrap();
            assert_eq!(
                parameters.get("additionalProperties"),
                Some(&serde_json::json!(false))
            );

            let properties = parameters
                .get("properties")
                .and_then(|value| value.as_object())
                .unwrap();
            let required = parameters
                .get("required")
                .and_then(|value| value.as_array())
                .unwrap();

            for property_name in properties.keys() {
                assert!(
                    required.iter().any(|value| value == property_name),
                    "tool '{}' is missing '{}' from required",
                    tool["name"],
                    property_name
                );
            }
        }
    }

    #[test]
    fn default_registry_has_builtins() {
        let reg = default_registry(std::env::temp_dir());
        assert!(reg.get("shell").is_some());
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("patch").is_some());
        assert!(reg.get("hashline_edit").is_some());
        assert!(reg.get("lsp_diagnostics").is_some());
        assert!(reg.get("lsp_symbols").is_some());
        assert!(reg.get("lsp_goto_definition").is_some());
        assert!(reg.get("lsp_find_references").is_some());
        assert!(reg.get("lsp_prepare_rename").is_some());
        assert!(reg.get("lsp_rename").is_some());
        assert!(reg.get("read_skill").is_some());
        assert!(reg.get("delegate_task").is_some());
        assert!(reg.get("report_status").is_some());
    }

    #[test]
    fn tool_execution_result_from_output_defaults_to_no_observations() {
        let result = ToolExecutionResult::from_output("ok");
        assert_eq!(result.output, "ok");
        assert!(result.edit_observations.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn tool_execution_result_with_edit_observations_derives_diagnostics() {
        let result = ToolExecutionResult::with_edit_observations(
            "Error: stale_reference",
            vec![EditObservation {
                engine: "hashline".to_string(),
                tool_name: "hashline_edit".to_string(),
                path: "src/lib.rs".to_string(),
                edit_count: 1,
                applied_count: 0,
                stale_reference_count: 1,
                noop_count: 0,
                failure_kind: Some("stale_reference".to_string()),
                duration_ms: 3,
                artifact_path: None,
                artifact_id: None,
                model_output_bounded: true,
            }],
        );

        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, "tool.edit.stale_reference");
        assert!(!result.is_success());
    }
}
