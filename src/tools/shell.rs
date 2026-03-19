//! Shell tool — execute commands and return structured output.
//!
//! Follows the codex-rs pattern: always return exit code + duration + output
//! so the model knows whether the command succeeded.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;

use super::Tool;

/// Maximum output size in bytes before truncation.
const MAX_OUTPUT_BYTES: usize = 100 * 1024; // 100 KB

/// Default timeout for command execution.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

pub struct ShellTool {
    timeout: Duration,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellTool {
    pub fn new() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
        }
    }

    #[allow(dead_code)]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its output. Returns exit code, duration, and stdout/stderr."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute (passed to sh -c)"
                }
            },
            "required": ["command"],
            "additionalProperties": false,
        })
    }

    fn execute(&self, args: Value) -> Result<String> {
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");

        if command.is_empty() {
            return Ok("Error: empty command".into());
        }

        let start = Instant::now();

        // Run synchronously — the tool trait is sync. The agent loop can
        // call this from spawn_blocking if needed.
        let result = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output();

        let duration = start.elapsed();

        match result {
            Ok(output) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                // Merge stdout and stderr (stderr first if non-empty, like a terminal)
                let mut combined = String::new();
                if !stderr.is_empty() {
                    combined.push_str(&stderr);
                    if !stderr.ends_with('\n') {
                        combined.push('\n');
                    }
                }
                combined.push_str(&stdout);

                // Count total lines before truncation
                let total_lines = combined.lines().count();

                // Truncate if too large
                let output_text = if combined.len() > MAX_OUTPUT_BYTES {
                    let half = MAX_OUTPUT_BYTES / 2;
                    let head_end = prev_char_boundary(&combined, half);
                    let tail_start = next_char_boundary(&combined, combined.len() - half);
                    let head = &combined[..head_end];
                    let tail = &combined[tail_start..];
                    format!(
                        "{head}\n\n... ({} bytes truncated) ...\n\n{tail}",
                        combined.len() - MAX_OUTPUT_BYTES
                    )
                } else {
                    combined
                };

                let duration_secs = duration.as_secs_f64();
                Ok(format!(
                    "Exit code: {exit_code}\nDuration: {duration_secs:.1}s\nTotal output lines: {total_lines}\nOutput:\n{output_text}"
                ))
            }
            Err(e) => Ok(format!("Error: failed to execute command: {e}")),
        }
    }
}

fn prev_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }

    if s.is_char_boundary(max_bytes) {
        return max_bytes;
    }

    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn next_char_boundary(s: &str, min_bytes: usize) -> usize {
    if min_bytes == 0 {
        return 0;
    }

    if min_bytes >= s.len() {
        return s.len();
    }

    if s.is_char_boundary(min_bytes) {
        return min_bytes;
    }

    let mut idx = min_bytes;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx.min(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_echo() {
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}))
            .unwrap();
        assert!(result.contains("Exit code: 0"));
        assert!(result.contains("hello"));
    }

    #[test]
    fn shell_exit_code() {
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "exit 42"}))
            .unwrap();
        assert!(result.contains("Exit code: 42"));
    }

    #[test]
    fn shell_stderr() {
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "echo err >&2 && echo out"}))
            .unwrap();
        assert!(result.contains("err"));
        assert!(result.contains("out"));
    }

    #[test]
    fn shell_empty_command() {
        let tool = ShellTool::new();
        let result = tool.execute(serde_json::json!({"command": ""})).unwrap();
        assert!(result.contains("Error: empty command"));
    }

    #[test]
    fn shell_duration_present() {
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "true"}))
            .unwrap();
        assert!(result.contains("Duration:"));
    }

    #[test]
    fn shell_large_multibyte_output_truncates_without_panic() {
        let tool = ShellTool::new();
        let cmd = "awk 'BEGIN { for (i = 0; i < 40000; i++ ) printf \"你\" }'";

        let result = tool.execute(serde_json::json!({"command": cmd}));
        assert!(
            result.is_ok(),
            "tool should not panic on multibyte truncation"
        );

        let result = result.unwrap();
        assert!(result.contains("Output:"));
        assert!(result.contains("... ("));
        assert!(result.contains("bytes truncated"));
    }
}
