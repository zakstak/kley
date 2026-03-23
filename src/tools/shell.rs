//! Shell tool — execute commands and return structured output.
//!
//! Follows the codex-rs pattern: always return exit code + duration + output
//! so the model knows whether the command succeeded.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
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

        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                return Ok(format!("Error: failed to execute command: {err}"));
            }
        };

        let mut timed_out = false;

        let mut stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                return Ok("Error: failed to capture stdout handle".into());
            }
        };
        let mut stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                return Ok("Error: failed to capture stderr handle".into());
            }
        };

        let stdout_handle = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            bytes
        });
        let stderr_handle = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });

        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() >= self.timeout => {
                    timed_out = true;
                    let _ = child.kill();
                    break;
                }
                Ok(None) => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => {
                    let _ = child.kill();
                    return Ok(format!("Error: failed while waiting for command: {err}"));
                }
            }
        }

        let status = match child.wait() {
            Ok(status) => status,
            Err(err) => {
                return Ok(format!("Error: command did not complete: {err}"));
            }
        };

        let stdout = match stdout_handle.join() {
            Ok(bytes) => bytes,
            Err(_) => return Ok("Error: failed to collect command stdout".into()),
        };
        let stderr = match stderr_handle.join() {
            Ok(bytes) => bytes,
            Err(_) => return Ok("Error: failed to collect command stderr".into()),
        };

        let exit_code = status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&stdout);
        let stderr = String::from_utf8_lossy(&stderr);

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

        // Truncate if too large.
        let mut output_text = if combined.len() > MAX_OUTPUT_BYTES {
            let half = MAX_OUTPUT_BYTES / 2;
            let head_end = char_boundary_before_or_at(&combined, half);
            let tail_start = char_boundary_at_or_after(&combined, combined.len() - half);
            let output_bytes = head_end + (combined.len() - tail_start);
            let truncated_bytes = combined.len().saturating_sub(output_bytes);

            let head = &combined[..head_end];
            let tail = &combined[tail_start..];
            format!("{head}\n\n... ({truncated_bytes} bytes truncated) ...\n\n{tail}")
        } else {
            combined
        };

        if timed_out {
            output_text = format!(
                "Command timed out after {:.1}s and was terminated.\n\n{output_text}",
                self.timeout.as_secs_f64()
            );
        }

        let duration_secs = start.elapsed().as_secs_f64();
        Ok(format!(
            "Exit code: {exit_code}\nDuration: {duration_secs:.1}s\nTotal output lines: {total_lines}\nOutput:\n{output_text}"
        ))
    }
}

/// Return the greatest byte index <= `limit` that is a char boundary.
fn char_boundary_before_or_at(s: &str, limit: usize) -> usize {
    s.char_indices()
        .map(|(idx, _)| idx)
        .chain(std::iter::once(s.len()))
        .filter(|&idx| idx <= limit)
        .max()
        .unwrap_or_default()
}

/// Return the smallest byte index >= `start` that is a char boundary.
fn char_boundary_at_or_after(s: &str, start: usize) -> usize {
    let start = start.min(s.len());
    s.char_indices()
        .map(|(idx, _)| idx)
        .chain(std::iter::once(s.len()))
        .find(|&idx| idx >= start)
        .unwrap_or(s.len())
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
    fn shell_truncates_unicode_output_without_panic() {
        let tool = ShellTool::new();
        let unicode_count = 35_001;
        let command = format!(
            "i=0; while [ \"$i\" -lt {unicode_count} ]; do printf '界'; i=$((i + 1)); done"
        );
        let result = tool
            .execute(serde_json::json!({
                "command": command,
            }))
            .unwrap();

        assert!(result.contains("... ("));
        assert!(result.contains("Exit code: 0"));
        assert!(result.contains("界"));
    }

    #[test]
    fn shell_times_out_long_running_command() {
        let tool = ShellTool::with_timeout(Duration::from_millis(150));
        let start = Instant::now();
        let result = tool
            .execute(serde_json::json!({"command": "sleep 1"}))
            .unwrap();

        assert!(result.contains("Command timed out after"));
        assert!(start.elapsed() < Duration::from_millis(900));
        assert!(start.elapsed() >= Duration::from_millis(120));
    }
}
