//! Shell tool — execute commands and return structured output.
//!
//! Follows the codex-rs pattern: always return exit code + duration + output
//! so the model knows whether the command succeeded.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::Value;

use super::Tool;

/// Maximum output size in bytes before truncation.
const MAX_OUTPUT_BYTES: usize = 100 * 1024; // 100 KB

/// Default timeout for command execution.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const TIMEOUT_TERMINATION_GRACE_PERIOD: Duration = Duration::from_millis(100);

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

        let (stdout_path, stdout_capture) = match create_capture_file("stdout") {
            Ok(capture) => capture,
            Err(err) => {
                return Ok(format!("Error: failed to create stdout capture: {err}"));
            }
        };
        let (stderr_path, stderr_capture) = match create_capture_file("stderr") {
            Ok(capture) => capture,
            Err(err) => {
                let _ = fs::remove_file(&stdout_path);
                return Ok(format!("Error: failed to create stderr capture: {err}"));
            }
        };

        let mut child = match spawn_shell_child(command, &stdout_capture, &stderr_capture) {
            Ok(child) => child,
            Err(err) => {
                let _ = fs::remove_file(&stdout_path);
                let _ = fs::remove_file(&stderr_path);
                return Ok(format!("Error: failed to execute command: {err}"));
            }
        };

        let mut timed_out = false;

        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() >= self.timeout => {
                    timed_out = true;
                    terminate_shell_process(&mut child);
                    break;
                }
                Ok(None) => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => {
                    terminate_shell_process(&mut child);
                    return Ok(format!("Error: failed while waiting for command: {err}"));
                }
            }
        }

        let status = match child.wait() {
            Ok(status) => status,
            Err(err) => {
                let _ = fs::remove_file(&stdout_path);
                let _ = fs::remove_file(&stderr_path);
                return Ok(format!("Error: command did not complete: {err}"));
            }
        };

        drop(stdout_capture);
        drop(stderr_capture);

        let stdout = match read_and_remove_capture_file(&stdout_path) {
            Ok(bytes) => bytes,
            Err(err) => {
                let _ = fs::remove_file(&stderr_path);
                return Ok(format!("Error: failed to collect command stdout: {err}"));
            }
        };
        let stderr = match read_and_remove_capture_file(&stderr_path) {
            Ok(bytes) => bytes,
            Err(err) => return Ok(format!("Error: failed to collect command stderr: {err}")),
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

fn terminate_shell_process(child: &mut std::process::Child) {
    let pid = child.id();
    let process_group = format!("-{pid}");

    let term_status = Command::new("kill")
        .arg("-TERM")
        .arg("--")
        .arg(&process_group)
        .status();

    let wait_deadline = Instant::now() + TIMEOUT_TERMINATION_GRACE_PERIOD;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < wait_deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => break,
        }
    }

    let kill_status = Command::new("kill")
        .arg("-KILL")
        .arg("--")
        .arg(&process_group)
        .status();

    if term_status.map(|status| !status.success()).unwrap_or(true)
        && kill_status.map(|status| !status.success()).unwrap_or(true)
    {
        let _ = child.kill();
    }
}

fn spawn_shell_child(
    command: &str,
    stdout_capture: &File,
    stderr_capture: &File,
) -> std::io::Result<Child> {
    spawn_shell_child_with_setsid_launcher("setsid", command, stdout_capture, stderr_capture)
}

fn spawn_shell_child_with_setsid_launcher(
    setsid_launcher: &str,
    command: &str,
    stdout_capture: &File,
    stderr_capture: &File,
) -> std::io::Result<Child> {
    match build_setsid_spawn_command(setsid_launcher, command, stdout_capture, stderr_capture)?
        .spawn()
    {
        Ok(child) => Ok(child),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            build_direct_spawn_command(command, stdout_capture, stderr_capture)?.spawn()
        }
        Err(err) => Err(err),
    }
}

fn build_setsid_spawn_command(
    setsid_launcher: &str,
    command: &str,
    stdout_capture: &File,
    stderr_capture: &File,
) -> std::io::Result<Command> {
    let mut cmd = Command::new(setsid_launcher);
    cmd.arg("sh").arg("-c").arg(command);

    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(stdout_capture.try_clone()?))
        .stderr(Stdio::from(stderr_capture.try_clone()?));

    Ok(cmd)
}

fn build_direct_spawn_command(
    command: &str,
    stdout_capture: &File,
    stderr_capture: &File,
) -> std::io::Result<Command> {
    let mut cmd = Command::new("sh");
    configure_process_group(&mut cmd);
    cmd.arg("-c").arg(command);

    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(stdout_capture.try_clone()?))
        .stderr(Stdio::from(stderr_capture.try_clone()?));

    Ok(cmd)
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

fn create_capture_file(stream_name: &str) -> std::io::Result<(PathBuf, File)> {
    let temp_dir = std::env::temp_dir();
    let pid = std::process::id();

    for attempt in 0..100u32 {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = temp_dir.join(format!(
            "kley-shell-{stream_name}-{pid}-{timestamp}-{attempt}.log"
        ));

        let mut open_options = OpenOptions::new();
        open_options.create_new(true).write(true).read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            open_options.mode(0o600);
        }

        match open_options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "failed to allocate unique capture file",
    ))
}

fn read_and_remove_capture_file(path: &PathBuf) -> std::io::Result<Vec<u8>> {
    let snapshot_len = fs::metadata(path)?.len();
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(snapshot_len).read_to_end(&mut bytes)?;
    let _ = fs::remove_file(path);
    Ok(bytes)
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
    fn shell_closes_stdin_for_noninteractive_commands() {
        let tool = ShellTool::with_timeout(Duration::from_millis(300));
        let start = Instant::now();
        let result = tool.execute(serde_json::json!({"command": "cat"})).unwrap();

        assert!(result.contains("Exit code: 0"));
        assert!(!result.contains("Command timed out after"));
        assert!(start.elapsed() < Duration::from_millis(250));
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

    #[test]
    fn shell_timeout_terminates_descendant_processes() {
        let tool = ShellTool::with_timeout(Duration::from_millis(150));
        let start = Instant::now();
        let result = tool
            .execute(serde_json::json!({"command": "sleep 1 & wait"}))
            .unwrap();

        assert!(result.contains("Command timed out after"));
        assert!(start.elapsed() < Duration::from_millis(900));
        assert!(start.elapsed() >= Duration::from_millis(120));
    }

    #[test]
    fn shell_timeout_escalates_when_term_is_ignored() {
        let tool = ShellTool::with_timeout(Duration::from_millis(150));
        let start = Instant::now();
        let result = tool
            .execute(serde_json::json!({
                "command": "trap '' TERM; while :; do sleep 1; done"
            }))
            .unwrap();

        assert!(result.contains("Command timed out after"));
        assert!(start.elapsed() < Duration::from_millis(900));
        assert!(start.elapsed() >= Duration::from_millis(120));
    }

    #[test]
    fn shell_timeout_returns_when_detached_descendant_holds_output_fds() {
        let tool = ShellTool::with_timeout(Duration::from_millis(150));
        let start = Instant::now();
        let result = tool
            .execute(serde_json::json!({
                "command": "setsid sh -c 'sleep 3; echo done' & wait"
            }))
            .unwrap();

        assert!(result.contains("Command timed out after"));
        assert!(start.elapsed() < Duration::from_millis(900));
        assert!(start.elapsed() >= Duration::from_millis(120));
    }

    #[test]
    fn shell_falls_back_to_sh_when_setsid_launcher_missing() {
        let (stdout_path, stdout_capture) = create_capture_file("stdout").unwrap();
        let (stderr_path, stderr_capture) = create_capture_file("stderr").unwrap();

        let mut child = spawn_shell_child_with_setsid_launcher(
            "definitely-missing-kley-setsid-launcher",
            "printf fallback-ok",
            &stdout_capture,
            &stderr_capture,
        )
        .unwrap();

        let status = child.wait().unwrap();
        drop(stdout_capture);
        drop(stderr_capture);

        let stdout = read_and_remove_capture_file(&stdout_path).unwrap();
        let stderr = read_and_remove_capture_file(&stderr_path).unwrap();

        assert!(status.success());
        assert_eq!(String::from_utf8_lossy(&stdout), "fallback-ok");
        assert!(stderr.is_empty());
    }

    #[test]
    fn shell_fallback_process_group_still_times_out_descendants() {
        let (stdout_path, stdout_capture) = create_capture_file("stdout").unwrap();
        let (stderr_path, stderr_capture) = create_capture_file("stderr").unwrap();
        let mut child = spawn_shell_child_with_setsid_launcher(
            "definitely-missing-kley-setsid-launcher",
            "sleep 1 & wait",
            &stdout_capture,
            &stderr_capture,
        )
        .unwrap();

        let timeout = Duration::from_millis(150);
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() >= timeout => {
                    terminate_shell_process(&mut child);
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    terminate_shell_process(&mut child);
                    break;
                }
            }
        }

        let _ = child.wait();
        drop(stdout_capture);
        drop(stderr_capture);
        let _ = read_and_remove_capture_file(&stdout_path);
        let _ = read_and_remove_capture_file(&stderr_path);

        assert!(start.elapsed() < Duration::from_millis(900));
        assert!(start.elapsed() >= Duration::from_millis(120));
    }
}
