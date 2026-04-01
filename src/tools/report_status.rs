//! Tool: `report_status` — heartbeat for autonomous agent loops.
//!
//! The model calls this periodically during long-running tasks to report
//! progress. The return value ("Continue to the next task.") keeps the
//! tool-call chain alive, preventing the model from terminating the loop.

use anyhow::Result;
use serde_json::Value;

use super::Tool;

pub struct ReportStatusTool;

impl Tool for ReportStatusTool {
    fn name(&self) -> &str {
        "report_status"
    }

    fn description(&self) -> &str {
        "Report progress on a long-running task. Call this periodically to keep \
         the user informed of what you have accomplished and what you plan to do \
         next. You can also pass a delegated task_id to fetch durable task-event \
         updates by stable id. This does NOT end your turn — you should continue \
         working after calling this."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "A brief status update describing what you just accomplished and what you plan to do next."
                },
                "task_id": {
                    "type": "string",
                    "description": "Optional stable delegated task_id to fetch durable outcome updates."
                },
                "after_sequence": {
                    "type": "integer",
                    "description": "Optional task event replay cursor. Returns events with sequence > after_sequence."
                }
            },
            "required": ["summary"],
            "additionalProperties": false,
        })
    }

    fn execute(&self, args: Value) -> Result<String> {
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no summary provided)");

        // Print to stderr so the user sees it in real-time
        eprintln!("\n  📋 Status: {summary}\n");

        // The return value is what the model sees — it's the "keep going" signal
        Ok("Status recorded. Continue to the next task.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_status_returns_continue_signal() {
        let tool = ReportStatusTool;
        let args = serde_json::json!({ "summary": "Finished refactoring module A" });
        let result = tool.execute(args).unwrap();
        assert!(result.contains("Continue"));
    }

    #[test]
    fn report_status_schema_is_valid() {
        let tool = ReportStatusTool;
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["summary"].is_object());
        assert_eq!(schema["required"][0], "summary");
    }

    #[test]
    fn report_status_handles_missing_summary() {
        let tool = ReportStatusTool;
        let args = serde_json::json!({});
        let result = tool.execute(args).unwrap();
        assert!(result.contains("Continue"));
    }
}
