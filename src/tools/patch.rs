//! Patch tool — search/replace editing.
//!
//! This is a simple placeholder implementation. It will be replaced by
//! hashline-edit in a future PR for better accuracy on cheap models.

use anyhow::Result;
use serde_json::Value;

use super::Tool;

pub struct PatchTool;

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
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let target = args.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let replacement = args
            .get("replacement")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if path.is_empty() {
            return Ok("Error: path is required".into());
        }
        if target.is_empty() {
            return Ok("Error: target is required".into());
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return Ok(format!("Error: {e}")),
        };

        // Count occurrences
        let matches: Vec<usize> = content.match_indices(target).map(|(idx, _)| idx).collect();

        match matches.len() {
            0 => {
                // Try to help: find the most similar line to the target's first line
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

                let hint = if let Some((ln, line)) = best_match {
                    format!("\nClosest match at line {ln}:\n  {line}")
                } else {
                    String::new()
                };

                Ok(format!(
                    "Error: target not found in {path}. No exact match for the search text.{hint}"
                ))
            }
            1 => {
                let new_content = content.replacen(target, replacement, 1);
                if let Err(e) = std::fs::write(path, &new_content) {
                    return Ok(format!("Error: failed to write {path}: {e}"));
                }

                let target_lines = target.lines().count();
                let replacement_lines = replacement.lines().count();
                Ok(format!(
                    "Applied: replaced {target_lines} line(s) with {replacement_lines} line(s) in {path}"
                ))
            }
            n => {
                // Report line numbers of each match
                let line_numbers: Vec<usize> = matches
                    .iter()
                    .map(|&byte_offset| content[..byte_offset].lines().count() + 1)
                    .collect();
                let lines_str: Vec<String> = line_numbers.iter().map(|n| n.to_string()).collect();

                Ok(format!(
                    "Error: found {n} matches in {path} (at lines {}). Target must be unique — include more context to disambiguate.",
                    lines_str.join(", ")
                ))
            }
        }
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
}
