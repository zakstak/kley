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
                    "minimum": 1,
                    "description":
                        "First line to include (1-based, inclusive). Pass null to start from beginning."
                },
                "end_line": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Last line to include (1-based, inclusive). Pass null to read to end."
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

        let requested_start = match parse_line_bound(&args, "start_line", 1) {
            Ok(value) => value,
            Err(error) => return Ok(error),
        };
        let requested_end = match parse_line_bound(&args, "end_line", total) {
            Ok(value) => value,
            Err(error) => return Ok(error),
        };

        if requested_start > requested_end {
            return Ok(format!(
                "Error: invalid range start_line={requested_start}, end_line={requested_end}. end_line must be >= start_line"
            ));
        }

        let start = requested_start.min(total).max(1);
        let end = requested_end.min(total).max(1);

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

fn parse_line_bound(args: &Value, key: &str, default: usize) -> Result<usize, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => {
            let raw = value
                .as_i64()
                .ok_or_else(|| format!("Error: {key} must be an integer"))?;

            if raw < 1 {
                return Err(format!("Error: {key} must be >= 1"));
            }

            usize::try_from(raw).map_err(|_| format!("Error: {key} out of range"))
        }
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
    fn read_whole_file_omits_optional_bounds_when_absent() {
        let f = temp_file("line1\nline2\nline3\n");
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap()
            }))
            .unwrap();
        assert!(result.contains("1: line1"));
        assert!(result.contains("3: line3"));
        assert!(result.contains("3 lines total"));
        assert!(!result.contains("Showing lines"));
    }

    #[test]
    fn schema_requires_nullable_ranges_for_strict_mode() {
        let tool = ReadFileTool;
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(
            required,
            &vec![
                serde_json::json!("path"),
                serde_json::json!("start_line"),
                serde_json::json!("end_line")
            ]
        );
        let start_line = &schema["properties"]["start_line"];
        assert_eq!(start_line["minimum"], 1);
        let start_types = start_line["type"].as_array().unwrap();
        assert_eq!(start_types.len(), 2);
        assert!(start_types.iter().any(|v| v == "integer"));
        assert!(start_types.iter().any(|v| v == "null"));
        let end_line = &schema["properties"]["end_line"];
        assert_eq!(end_line["minimum"], 1);
        let end_types = end_line["type"].as_array().unwrap();
        assert_eq!(end_types.len(), 2);
        assert!(end_types.iter().any(|v| v == "integer"));
        assert!(end_types.iter().any(|v| v == "null"));
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
    fn read_out_of_bounds_range_is_clamped() {
        let f = temp_file("a\nb\n");
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap(),
                "start_line": 10,
                "end_line": 20,
            }))
            .unwrap();

        assert!(result.contains("2: b"));
        assert!(!result.contains("1: a"));
    }

    #[test]
    fn read_invalid_range_reports_error() {
        let f = temp_file("a\nb\nc\n");
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap(),
                "start_line": 3,
                "end_line": 2,
            }))
            .unwrap();

        assert!(result.starts_with("Error: invalid range"));
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

    #[test]
    fn invalid_start_line_is_domain_error() {
        let f = temp_file("line1\nline2\n");
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap(),
                "start_line": "first",
                "end_line": 2,
            }))
            .unwrap();

        assert_eq!(result, "Error: start_line must be an integer");
    }

    #[test]
    fn invalid_end_line_is_domain_error() {
        let f = temp_file("line1\nline2\n");
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({
                "path": f.path().to_str().unwrap(),
                "start_line": 1,
                "end_line": 0,
            }))
            .unwrap();

        assert_eq!(result, "Error: end_line must be >= 1");
    }
}
