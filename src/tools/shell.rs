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
const MAX_CAPTURE_BYTES: u64 = (MAX_OUTPUT_BYTES as u64) * 2;
const PRE_TIMEOUT_OBSERVATION_WINDOW: Duration = Duration::from_millis(20);

/// Default timeout for command execution.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const TIMEOUT_TERMINATION_GRACE_PERIOD: Duration = Duration::from_millis(100);

pub struct ShellTool {
    timeout: Duration,
    max_capture_bytes: u64,
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
            max_capture_bytes: MAX_CAPTURE_BYTES,
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            max_capture_bytes: MAX_CAPTURE_BYTES,
        }
    }

    #[cfg(test)]
    fn with_limits(timeout: Duration, max_capture_bytes: u64) -> Self {
        Self {
            timeout,
            max_capture_bytes,
        }
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
        let mut terminated_for_output_limit = false;
        let mut remaining_capture_holders_after_termination = 0usize;
        let mut observed_descendants = Vec::new();
        track_observed_descendants(child.id(), &mut observed_descendants);

        let observation_deadline = Instant::now() + PRE_TIMEOUT_OBSERVATION_WINDOW;
        while Instant::now() < observation_deadline {
            track_observed_descendants(child.id(), &mut observed_descendants);
            if !observed_descendants.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        loop {
            track_observed_descendants(child.id(), &mut observed_descendants);

            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if capture_limit_exceeded(&stdout_path, &stderr_path, self.max_capture_bytes) {
                        terminated_for_output_limit = true;
                        let termination_outcome = terminate_shell_process(
                            &mut child,
                            &[&stdout_path, &stderr_path],
                            &observed_descendants,
                        );
                        remaining_capture_holders_after_termination =
                            remaining_capture_holders_after_termination
                                .max(termination_outcome.remaining_capture_holders);
                        break;
                    }

                    if start.elapsed() >= self.timeout {
                        timed_out = true;
                        let termination_outcome = terminate_shell_process(
                            &mut child,
                            &[&stdout_path, &stderr_path],
                            &observed_descendants,
                        );
                        remaining_capture_holders_after_termination =
                            remaining_capture_holders_after_termination
                                .max(termination_outcome.remaining_capture_holders);
                        break;
                    }

                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => {
                    let _ = terminate_shell_process(
                        &mut child,
                        &[&stdout_path, &stderr_path],
                        &observed_descendants,
                    );
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

        reap_known_children_nonblocking(&observed_descendants);

        drop(stdout_capture);
        drop(stderr_capture);

        let (stdout, stdout_truncated) =
            match read_and_remove_capture_file(&stdout_path, self.max_capture_bytes) {
                Ok(bytes) => bytes,
                Err(err) => {
                    let _ = fs::remove_file(&stderr_path);
                    return Ok(format!("Error: failed to collect command stdout: {err}"));
                }
            };
        let (stderr, stderr_truncated) =
            match read_and_remove_capture_file(&stderr_path, self.max_capture_bytes) {
                Ok(bytes) => bytes,
                Err(err) => return Ok(format!("Error: failed to collect command stderr: {err}")),
            };

        let capture_truncated = stdout_truncated || stderr_truncated;

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
        } else if terminated_for_output_limit {
            output_text = format!(
                "Command exceeded the output capture limit of {} bytes and was terminated.\n\n{output_text}",
                self.max_capture_bytes
            );
        } else if capture_truncated {
            output_text = format!(
                "Command output exceeded the capture readback limit of {} bytes, so only a truncated snapshot is shown.\n\n{output_text}",
                self.max_capture_bytes
            );
        }

        output_text = maybe_prepend_capture_holder_warning(
            output_text,
            remaining_capture_holders_after_termination,
        );

        let duration_secs = start.elapsed().as_secs_f64();
        Ok(format!(
            "Exit code: {exit_code}\nDuration: {duration_secs:.1}s\nTotal output lines: {total_lines}\nOutput:\n{output_text}"
        ))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct TerminationOutcome {
    remaining_capture_holders: usize,
}

fn terminate_shell_process(
    child: &mut std::process::Child,
    capture_paths: &[&PathBuf],
    known_processes: &[u32],
) -> TerminationOutcome {
    let pid = child.id();
    let process_group = format!("-{pid}");
    let mut tracked_descendants =
        collect_termination_candidates(pid, known_processes, capture_paths);
    let mut tracked_process_groups = collect_termination_process_groups(&tracked_descendants);

    let _ = signal_processes("-TERM", std::iter::once(process_group.as_str()));
    let _ = signal_descendants("-TERM", &tracked_descendants);
    let _ = signal_process_groups("-TERM", &tracked_process_groups);

    let wait_deadline = Instant::now() + TIMEOUT_TERMINATION_GRACE_PERIOD;
    loop {
        tracked_descendants =
            collect_termination_candidates(pid, &tracked_descendants, capture_paths);
        if child_exited(child) && tracked_descendants.is_empty() {
            return termination_outcome(capture_paths);
        }

        if Instant::now() < wait_deadline {
            thread::sleep(Duration::from_millis(10));
        } else {
            break;
        }
    }

    let remaining_descendants =
        collect_termination_candidates(pid, &tracked_descendants, capture_paths);
    let remaining_process_groups = collect_termination_process_groups(&remaining_descendants);
    let _ = signal_processes("-KILL", std::iter::once(process_group.as_str()));
    let _ = signal_descendants("-KILL", &remaining_descendants);
    let _ = signal_process_groups("-KILL", &remaining_process_groups);

    let kill_deadline = Instant::now() + TIMEOUT_TERMINATION_GRACE_PERIOD;
    while Instant::now() < kill_deadline {
        tracked_descendants =
            collect_termination_candidates(pid, &tracked_descendants, capture_paths);
        tracked_process_groups = collect_termination_process_groups(&tracked_descendants);

        if child_exited(child) && tracked_descendants.is_empty() {
            return termination_outcome(capture_paths);
        }

        if !tracked_descendants.is_empty() {
            let _ = signal_descendants("-KILL", &tracked_descendants);
        }

        if !tracked_process_groups.is_empty() {
            let _ = signal_process_groups("-KILL", &tracked_process_groups);
        }

        thread::sleep(Duration::from_millis(10));
    }

    if !child_exited(child) {
        let _ = child.kill();
    }

    termination_outcome(capture_paths)
}

fn termination_outcome(capture_paths: &[&PathBuf]) -> TerminationOutcome {
    TerminationOutcome {
        remaining_capture_holders: remaining_capture_holders_excluding_self(capture_paths),
    }
}

fn remaining_capture_holders_excluding_self(capture_paths: &[&PathBuf]) -> usize {
    let self_pid = std::process::id();
    processes_holding_capture_files(capture_paths)
        .into_iter()
        .filter(|pid| *pid != self_pid)
        .count()
}

fn maybe_prepend_capture_holder_warning(
    output_text: String,
    remaining_capture_holders: usize,
) -> String {
    if remaining_capture_holders == 0 {
        return output_text;
    }

    format!(
        "Warning: best-effort termination left {remaining_capture_holders} process(es) still holding shell capture file descriptors; detached processes may still be running.\n\n{output_text}"
    )
}

fn collect_termination_candidates(
    root_pid: u32,
    known_processes: &[u32],
    capture_paths: &[&PathBuf],
) -> Vec<u32> {
    let mut candidates: Vec<u32> = known_processes
        .iter()
        .copied()
        .filter(|pid| process_exists(*pid))
        .collect();
    candidates.extend(descendant_processes(root_pid));
    candidates.extend(processes_holding_capture_files(capture_paths));
    candidates.retain(|pid| *pid != std::process::id());
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

fn collect_termination_process_groups(known_processes: &[u32]) -> Vec<u32> {
    let mut process_groups: Vec<u32> = known_processes
        .iter()
        .filter_map(|pid| process_group_id(*pid))
        .collect();
    process_groups.retain(|process_group| *process_group > 0);
    process_groups.sort_unstable();
    process_groups.dedup();
    process_groups
}

fn track_observed_descendants(root_pid: u32, observed_descendants: &mut Vec<u32>) {
    observed_descendants.retain(|pid| process_exists(*pid));
    observed_descendants.extend(descendant_processes(root_pid));
    observed_descendants.sort_unstable();
    observed_descendants.dedup();
}

fn capture_limit_exceeded(
    stdout_path: &PathBuf,
    stderr_path: &PathBuf,
    max_capture_bytes: u64,
) -> bool {
    capture_file_len(stdout_path).saturating_add(capture_file_len(stderr_path)) >= max_capture_bytes
}

fn capture_file_len(path: &PathBuf) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn signal_descendants(
    signal: &str,
    descendant_pids: &[u32],
) -> std::io::Result<std::process::ExitStatus> {
    signal_processes(signal, descendant_pids.iter().map(|pid| pid.to_string()))
}

fn signal_process_groups(
    signal: &str,
    process_groups: &[u32],
) -> std::io::Result<std::process::ExitStatus> {
    signal_processes(
        signal,
        process_groups
            .iter()
            .map(|process_group| format!("-{process_group}")),
    )
}

fn signal_processes<I, S>(signal: &str, processes: I) -> std::io::Result<std::process::ExitStatus>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let processes: Vec<String> = processes
        .into_iter()
        .map(|process| process.as_ref().to_owned())
        .collect();

    if processes.is_empty() {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;

            return Ok(std::process::ExitStatus::from_raw(0));
        }

        #[cfg(not(unix))]
        {
            return Command::new("true").status();
        }
    }

    let mut command = Command::new("kill");
    command.arg(signal).arg("--");
    for process in processes {
        command.arg(process);
    }
    command.stdout(Stdio::null()).stderr(Stdio::null());
    command.status()
}

fn child_exited(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(Some(_)) | Err(_))
}

#[cfg(target_os = "linux")]
fn reap_known_children_nonblocking(known_processes: &[u32]) {
    const WNOHANG: i32 = 1;

    unsafe extern "C" {
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }

    let mut known_processes: Vec<u32> = known_processes.to_vec();
    known_processes.sort_unstable();
    known_processes.dedup();

    for known_pid in known_processes {
        let mut status = 0;
        let _ = unsafe { waitpid(known_pid as i32, &mut status, WNOHANG) };
    }
}

#[cfg(not(target_os = "linux"))]
fn reap_known_children_nonblocking(_known_processes: &[u32]) {}

#[cfg(target_os = "linux")]
fn descendant_processes(root_pid: u32) -> Vec<u32> {
    use std::collections::HashMap;

    let mut children_by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
    let entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = file_name.parse::<u32>() else {
            continue;
        };
        let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let Some(parent_pid) = parse_parent_pid(&status) else {
            continue;
        };
        children_by_parent.entry(parent_pid).or_default().push(pid);
    }

    let mut descendants = Vec::new();
    let mut stack = children_by_parent.remove(&root_pid).unwrap_or_default();
    while let Some(pid) = stack.pop() {
        descendants.push(pid);
        if let Some(children) = children_by_parent.remove(&pid) {
            stack.extend(children);
        }
    }
    descendants
}

#[cfg(not(target_os = "linux"))]
fn descendant_processes(_root_pid: u32) -> Vec<u32> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn parse_parent_pid(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        let value = line.strip_prefix("PPid:")?.trim();
        value.parse().ok()
    })
}

#[cfg(target_os = "linux")]
fn process_exists(pid: u32) -> bool {
    PathBuf::from(format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_exists(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn process_group_id(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let right_paren = stat.rfind(')')?;
    let fields = stat.get(right_paren + 1..)?.trim_start();
    let mut parts = fields.split_whitespace();
    let _state = parts.next()?;
    let _parent_pid = parts.next()?;
    let process_group = parts.next()?;
    process_group.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn process_group_id(_pid: u32) -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn processes_holding_capture_files(capture_paths: &[&PathBuf]) -> Vec<u32> {
    use std::collections::HashSet;
    use std::os::unix::fs::MetadataExt;

    let targets: HashSet<(u64, u64)> = capture_paths
        .iter()
        .filter_map(|path| fs::metadata(path).ok().map(|meta| (meta.dev(), meta.ino())))
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }

    let mut holders = Vec::new();
    let entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = file_name.parse::<u32>() else {
            continue;
        };

        let Ok(fd_entries) = fs::read_dir(entry.path().join("fd")) else {
            continue;
        };

        let mut holds_capture = false;
        for fd_entry in fd_entries.flatten() {
            let Ok(metadata) = fs::metadata(fd_entry.path()) else {
                continue;
            };

            if targets.contains(&(metadata.dev(), metadata.ino())) {
                holds_capture = true;
                break;
            }
        }

        if holds_capture {
            holders.push(pid);
        }
    }

    holders
}

#[cfg(not(target_os = "linux"))]
fn processes_holding_capture_files(_capture_paths: &[&PathBuf]) -> Vec<u32> {
    Vec::new()
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

fn read_and_remove_capture_file(
    path: &PathBuf,
    max_bytes: u64,
) -> std::io::Result<(Vec<u8>, bool)> {
    let snapshot_len = fs::metadata(path)?.len();
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(snapshot_len.min(max_bytes))
        .read_to_end(&mut bytes)?;
    let _ = fs::remove_file(path);
    Ok((bytes, snapshot_len > max_bytes))
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
    use tempfile::NamedTempFile;

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
        let tool = ShellTool::with_timeout(Duration::from_millis(500));
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
        let tool = ShellTool::with_timeout(Duration::from_millis(300));
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

        let (stdout, _) = read_and_remove_capture_file(&stdout_path, MAX_CAPTURE_BYTES).unwrap();
        let (stderr, _) = read_and_remove_capture_file(&stderr_path, MAX_CAPTURE_BYTES).unwrap();

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
                    terminate_shell_process(&mut child, &[&stdout_path, &stderr_path], &[]);
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    terminate_shell_process(&mut child, &[&stdout_path, &stderr_path], &[]);
                    break;
                }
            }
        }

        let _ = child.wait();
        drop(stdout_capture);
        drop(stderr_capture);
        let _ = read_and_remove_capture_file(&stdout_path, MAX_CAPTURE_BYTES);
        let _ = read_and_remove_capture_file(&stderr_path, MAX_CAPTURE_BYTES);

        assert!(start.elapsed() < Duration::from_millis(900));
        assert!(start.elapsed() >= Duration::from_millis(120));
    }

    #[test]
    fn terminate_shell_process_falls_back_to_child_kill_without_descendants() {
        let (stdout_path, stdout_capture) = create_capture_file("stdout").unwrap();
        let (stderr_path, stderr_capture) = create_capture_file("stderr").unwrap();
        let mut child = spawn_shell_child_with_setsid_launcher(
            "definitely-missing-kley-setsid-launcher",
            "exec sleep 60",
            &stdout_capture,
            &stderr_capture,
        )
        .unwrap();

        let start = Instant::now();
        terminate_shell_process(&mut child, &[], &[]);
        let _ = child.wait();

        drop(stdout_capture);
        drop(stderr_capture);
        let _ = read_and_remove_capture_file(&stdout_path, MAX_CAPTURE_BYTES);
        let _ = read_and_remove_capture_file(&stderr_path, MAX_CAPTURE_BYTES);

        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn shell_timeout_kills_detached_descendants() {
        let pid_file = NamedTempFile::new().unwrap();
        let pid_path = pid_file.path().display();
        let tool = ShellTool::with_timeout(Duration::from_millis(150));

        let result = tool
            .execute(serde_json::json!({
                "command": format!("setsid sh -c 'echo $$ > {pid_path}; sleep 60' & sleep 999")
            }))
            .unwrap();

        let detached_pid = fs::read_to_string(pid_file.path())
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();

        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline && process_exists(detached_pid) {
            thread::sleep(Duration::from_millis(10));
        }

        assert!(result.contains("Command timed out after"));
        assert!(!process_exists(detached_pid));
    }

    #[test]
    fn shell_timeout_kills_reparented_detached_descendants() {
        let pid_file = NamedTempFile::new().unwrap();
        let pid_path = pid_file.path().display();
        let tool = ShellTool::with_timeout(Duration::from_millis(150));

        let result = tool
            .execute(serde_json::json!({
                "command": format!("setsid sh -c 'sleep 60 & echo $! > {pid_path}; exit 0' & sleep 999")
            }))
            .unwrap();

        let detached_pid = fs::read_to_string(pid_file.path())
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();

        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline && process_exists(detached_pid) {
            thread::sleep(Duration::from_millis(10));
        }

        assert!(result.contains("Command timed out after"));
        assert!(!process_exists(detached_pid));
    }

    #[test]
    fn shell_timeout_kills_daemonized_descendants_without_capture_fds() {
        let pid_file = NamedTempFile::new().unwrap();
        let pid_path = pid_file.path().display();
        let tool = ShellTool::with_timeout(Duration::from_millis(150));

        let result = tool
            .execute(serde_json::json!({
                "command": format!("setsid sh -c 'sleep 60 >/dev/null 2>&1 < /dev/null & echo $! > {pid_path}; sleep 0.2; exit 0' & sleep 999")
            }))
            .unwrap();

        let detached_pid = fs::read_to_string(pid_file.path())
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();

        let deadline = Instant::now() + Duration::from_millis(700);
        while Instant::now() < deadline && process_exists(detached_pid) {
            thread::sleep(Duration::from_millis(10));
        }

        assert!(result.contains("Command timed out after"));
        assert!(!process_exists(detached_pid));
    }

    #[test]
    fn shell_terminates_when_capture_limit_is_exceeded() {
        let tool = ShellTool::with_limits(Duration::from_secs(5), 1024);
        let start = Instant::now();
        let result = tool
            .execute(serde_json::json!({
                "command": "while :; do printf '0123456789abcdef'; done"
            }))
            .unwrap();

        assert!(result.contains("Command exceeded the output capture limit"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn shell_reports_truncated_capture_without_false_termination_message() {
        let tool = ShellTool::with_limits(Duration::from_secs(5), 256);
        let result = tool
            .execute(serde_json::json!({
                "command": "printf '%4096s' ''"
            }))
            .unwrap();

        assert!(result.contains("Exit code: 0"));
        assert!(result.contains("Command output exceeded the capture readback limit"));
        assert!(!result.contains("and was terminated"));
    }

    #[test]
    fn read_and_remove_capture_file_caps_memory_usage() {
        use std::io::Write;

        let (capture_path, mut capture_file) = create_capture_file("stdout").unwrap();
        capture_file.write_all(&vec![b'x'; 4096]).unwrap();
        drop(capture_file);

        let (bytes, truncated) = read_and_remove_capture_file(&capture_path, 1024).unwrap();

        assert_eq!(bytes.len(), 1024);
        assert!(truncated);
        assert!(!capture_path.exists());
    }

    #[test]
    fn capture_holder_warning_is_prefixed_when_holders_remain() {
        let output = maybe_prepend_capture_holder_warning("tool output".to_string(), 2);

        assert!(output.contains("Warning: best-effort termination left 2 process(es)"));
        assert!(output.ends_with("tool output"));
    }

    #[test]
    fn capture_holder_warning_is_omitted_when_no_holders_remain() {
        let output = maybe_prepend_capture_holder_warning("tool output".to_string(), 0);

        assert_eq!(output, "tool output");
    }

    #[test]
    fn remaining_capture_holders_excluding_self_does_not_count_parent_fd() {
        let (capture_path, capture_file) = create_capture_file("stdout").unwrap();

        let holders = remaining_capture_holders_excluding_self(&[&capture_path]);

        drop(capture_file);
        let _ = fs::remove_file(capture_path);
        assert_eq!(holders, 0);
    }

    #[test]
    fn collect_termination_process_groups_uses_live_processes_only() {
        assert!(collect_termination_process_groups(&[]).is_empty());

        let groups = collect_termination_process_groups(&[std::process::id()]);
        assert!(!groups.is_empty());
    }
}
