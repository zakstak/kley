//! Tool infrastructure for kley's agent loop.
//!
//! Each tool implements the `Tool` trait and is registered with a `ToolRegistry`.
//! The registry generates the OpenAI Responses API `tools` array and dispatches
//! calls by name.

pub mod patch;
pub mod read_file;
pub mod read_skill;
pub mod report_status;
pub mod shell;

use anyhow::Result;
use serde_json::Value;

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
    reg.register(Box::new(shell::ShellTool::new()));
    reg.register(Box::new(read_file::ReadFileTool));
    reg.register(Box::new(patch::PatchTool));
    reg.register(Box::new(read_skill::ReadSkillTool::new(project_dir)));
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
        assert!(reg.get("read_skill").is_some());
        assert!(reg.get("report_status").is_some());
    }
}
