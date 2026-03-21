//! Read file tool — return file contents with line numbers.

use anyhow::Result;
use serde_json::Value;

use super::Tool;

pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a file and return its contents with line numbers. Supports optional start_line/end_line range."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "start_line": {
                    "type": ["integer", "null"],
                    "description": "First line to include (1-based, inclusive). Omit to start from beginning."
                },
                "end_line": {
                    "type": ["integer", "null"],
                    "description": "Last line to include (1-based, inclusive). Omit to read to end."
                }
            },
            "required": ["path", "start_line", "end_line"],
            "additionalProperties": false,
        })
    }

    fn execute(&self, args: Value) -> Result<String> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");

        if path.is_empty() {
            return Ok("Error: path is required".into());
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return Ok(format!("Error: {e}")),
        };

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        if total == 0 {
            return Ok(format!("File: {path} (0 lines total)\n"));
        }

        let start = args
            .get("start_line")
            .and_then(|v| v.as_u64())
            .map(|n| n.max(1) as usize)
            .unwrap_or(1);

        let end = args
            .get("end_line")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(total);

        // Clamp to valid range
        let start = start.min(total).max(1);
        let end = end.min(total).max(start);

        let mut output = format!("File: {path} ({total} lines total)\n");
        if start > 1 || end < total {
            output.push_str(&format!("Showing lines {start}-{end}\n"));
        }
        output.push('\n');

        for (i, line) in lines[start - 1..=end - 1].iter().enumerate() {
            let line_num = start + i;
            output.push_str(&format!("{line_num}: {line}\n"));
        }

        Ok(output)
    }
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
    fn read_whole_file() {
        let f = temp_file("line1\nline2\nline3\n");
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap(),
                "start_line": null,
                "end_line": null,
            }))
            .unwrap();
        assert!(result.contains("1: line1"));
        assert!(result.contains("2: line2"));
        assert!(result.contains("3: line3"));
        assert!(result.contains("3 lines total"));
    }

    #[test]
    fn read_range() {
        let f = temp_file("a\nb\nc\nd\ne\n");
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap(),
                "start_line": 2,
                "end_line": 4,
            }))
            .unwrap();
        assert!(result.contains("2: b"));
        assert!(result.contains("3: c"));
        assert!(result.contains("4: d"));
        assert!(!result.contains("1: a"));
        assert!(!result.contains("5: e"));
        assert!(result.contains("Showing lines 2-4"));
    }

    #[test]
    fn read_empty_file() {
        let f = temp_file("");
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap(),
                "start_line": 1,
                "end_line": 1,
            }))
            .unwrap();

        assert_eq!(
            result,
            format!("File: {} (0 lines total)\n", f.path().display())
        );
    }

    #[test]
    fn read_not_found() {
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": "/nonexistent/file.txt",
                "start_line": null,
                "end_line": null,
            }))
            .unwrap();
        assert!(result.contains("Error:"));
    }
}
