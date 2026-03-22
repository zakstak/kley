use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, broadcast, mpsc};

use super::protocol::{SelfImproveActiveRun, SelfImproveRunRecord, SelfImproveSnapshotData};

const DEFAULT_MAX_CYCLES: u32 = 5;
const DEFAULT_TURNS_PER_CYCLE: u32 = 30;
const MAX_CYCLES: u32 = 100;
const MAX_TURNS_PER_CYCLE: u32 = 200;
const MAX_LOG_TAIL: usize = 600;
const MAX_HISTORY: usize = 30;
const MAX_RECENT_LOGS: usize = 30;
const MAX_RETROSPECTIVES: usize = 30;
const GENERIC_STATUS_DETAIL: &str = "status parsed from script output";

#[derive(Debug, Clone)]
pub enum SelfImproveEvent {
    Snapshot(SelfImproveSnapshotData),
    LogLine {
        run_id: String,
        line: String,
    },
    Status {
        run_id: String,
        status: String,
        detail: String,
    },
}

#[derive(Debug, Clone)]
pub struct SelfImproveError {
    pub code: &'static str,
    pub message: String,
}

impl SelfImproveError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone)]
pub struct SelfImproveManager {
    inner: Arc<Mutex<InnerState>>,
    events: broadcast::Sender<SelfImproveEvent>,
    repo_root: PathBuf,
}

#[derive(Debug)]
struct InnerState {
    active: Option<ActiveRunState>,
    history: Vec<SelfImproveRunRecord>,
    next_run_seq: u64,
}

#[derive(Debug)]
struct ActiveRunState {
    run_id: String,
    pid: u32,
    started_at: String,
    max_cycles: u32,
    turns_per_cycle: u32,
    stop_requested: bool,
    latest_status: String,
    latest_detail: String,
    log_tail: VecDeque<String>,
}

fn has_local_self_improve_script(repo_root: &Path) -> bool {
    repo_root.join("self-improve.sh").is_file()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedStatusLine {
    status: String,
    detail: Option<String>,
}

impl SelfImproveManager {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            inner: Arc::new(Mutex::new(InnerState {
                active: None,
                history: Vec::new(),
                next_run_seq: 1,
            })),
            events,
            repo_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SelfImproveEvent> {
        self.events.subscribe()
    }

    pub async fn snapshot(&self) -> SelfImproveSnapshotData {
        let (active_run, history) = {
            let inner = self.inner.lock().await;
            (
                inner.active.as_ref().map(|active| SelfImproveActiveRun {
                    run_id: active.run_id.clone(),
                    pid: active.pid,
                    started_at: active.started_at.clone(),
                    max_cycles: active.max_cycles,
                    turns_per_cycle: active.turns_per_cycle,
                    stop_requested: active.stop_requested,
                    latest_status: active.latest_status.clone(),
                    latest_detail: active.latest_detail.clone(),
                    log_tail: active.log_tail.iter().cloned().collect(),
                }),
                inner.history.clone(),
            )
        };

        let (recent_logs, retrospectives) = self.load_artifacts();
        SelfImproveSnapshotData {
            available: self.launch_supported(),
            active_run,
            history,
            recent_logs,
            retrospectives,
        }
    }

    pub async fn start(
        &self,
        max_cycles: Option<u32>,
        turns_per_cycle: Option<u32>,
    ) -> Result<SelfImproveSnapshotData, SelfImproveError> {
        let cycles = max_cycles
            .unwrap_or(DEFAULT_MAX_CYCLES)
            .clamp(1, MAX_CYCLES);
        let turns = turns_per_cycle
            .unwrap_or(DEFAULT_TURNS_PER_CYCLE)
            .clamp(1, MAX_TURNS_PER_CYCLE);

        let (run_id, child, start_detail) = {
            let mut inner = self.inner.lock().await;
            if inner.active.is_some() {
                return Err(SelfImproveError::new(
                    "already_running",
                    "self-improve run is already active",
                ));
            }

            let run_id = format!("self-improve-{}", inner.next_run_seq);
            inner.next_run_seq += 1;

            let mut child = self.spawn_command(cycles, turns)?;

            let child = child.spawn().map_err(|err| {
                SelfImproveError::new("process_spawn_failed", format!("failed to start: {err}"))
            })?;

            let pid = child.id().ok_or_else(|| {
                SelfImproveError::new("process_spawn_failed", "started process without pid")
            })?;
            let start_detail = format!("started self-improve pid={pid}");

            inner.active = Some(ActiveRunState {
                run_id: run_id.clone(),
                pid,
                started_at: Utc::now().to_rfc3339(),
                max_cycles: cycles,
                turns_per_cycle: turns,
                stop_requested: false,
                latest_status: "starting".to_string(),
                latest_detail: start_detail.clone(),
                log_tail: VecDeque::new(),
            });

            (run_id, child, start_detail)
        };

        let _ = self.events.send(SelfImproveEvent::Status {
            run_id: run_id.clone(),
            status: "starting".to_string(),
            detail: start_detail,
        });
        self.broadcast_snapshot().await;

        let manager = self.clone();
        tokio::spawn(async move {
            manager.monitor_run(run_id, child).await;
        });

        Ok(self.snapshot().await)
    }

    pub async fn stop(&self) -> Result<SelfImproveSnapshotData, SelfImproveError> {
        let (pid, run_id) = {
            let mut inner = self.inner.lock().await;
            let Some(active) = inner.active.as_mut() else {
                return Err(SelfImproveError::new(
                    "not_running",
                    "no active self-improve run",
                ));
            };
            active.stop_requested = true;
            active.latest_status = "stopping".to_string();
            (active.pid, active.run_id.clone())
        };
        let stop_detail = format!("sent SIGTERM to process group -{pid}");

        let process_group = OsString::from(format!("-{pid}"));
        let status = Command::new("kill")
            .arg("-TERM")
            .arg("--")
            .arg(&process_group)
            .status()
            .await
            .map_err(|err| {
                SelfImproveError::new(
                    "stop_failed",
                    format!("failed to send SIGTERM to process group -{pid}: {err}"),
                )
            })?;

        if !status.success() {
            let check = Command::new("kill")
                .arg("-0")
                .arg("--")
                .arg(&process_group)
                .status()
                .await
                .map_err(|err| {
                    SelfImproveError::new(
                        "stop_failed",
                        format!("failed to verify process group -{pid}: {err}"),
                    )
                })?;

            if check.success() {
                return Err(SelfImproveError::new(
                    "stop_failed",
                    format!("SIGTERM rejected for process group -{pid}"),
                ));
            }
        }

        {
            let mut inner = self.inner.lock().await;
            if let Some(active) = inner.active.as_mut()
                && active.run_id == run_id
            {
                active.latest_detail = stop_detail.clone();
            }
        }

        let _ = self.events.send(SelfImproveEvent::Status {
            run_id,
            status: "stopping".to_string(),
            detail: stop_detail,
        });
        self.broadcast_snapshot().await;
        Ok(self.snapshot().await)
    }

    pub async fn restart(
        &self,
        max_cycles: Option<u32>,
        turns_per_cycle: Option<u32>,
    ) -> Result<SelfImproveSnapshotData, SelfImproveError> {
        let active = { self.inner.lock().await.active.is_some() };
        if active {
            self.stop().await?;
            self.wait_until_stopped().await?;
        }
        self.start(max_cycles, turns_per_cycle).await
    }

    async fn wait_until_stopped(&self) -> Result<(), SelfImproveError> {
        for _ in 0..50 {
            if self.inner.lock().await.active.is_none() {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        Err(SelfImproveError::new(
            "stop_timeout",
            "self-improve run did not stop within timeout",
        ))
    }

    async fn monitor_run(&self, run_id: String, mut child: tokio::process::Child) {
        let mut line_rx = self.spawn_line_receivers(&mut child);
        let exit_code = loop {
            tokio::select! {
                status = child.wait() => {
                    let code = status.ok().and_then(|s| s.code());
                    break code;
                }
                maybe_line = line_rx.recv() => {
                    let Some(line) = maybe_line else { continue; };
                    self.append_log_line(&run_id, line).await;
                }
            }
        };

        while let Ok(line) = line_rx.try_recv() {
            self.append_log_line(&run_id, line).await;
        }

        while let Some(line) = line_rx.recv().await {
            self.append_log_line(&run_id, line).await;
        }

        self.finalize_run(&run_id, exit_code).await;
    }

    fn spawn_line_receivers(&self, child: &mut tokio::process::Child) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel::<String>(256);

        if let Some(stdout) = child.stdout.take() {
            let tx_out = tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if tx_out.send(line).await.is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let tx_err = tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if tx_err.send(format!("[stderr] {line}")).await.is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
            });
        }

        rx
    }

    async fn append_log_line(&self, run_id: &str, line: String) {
        let mut status_update = None;
        {
            let mut inner = self.inner.lock().await;
            let Some(active) = inner.active.as_mut() else {
                return;
            };
            if active.run_id != run_id {
                return;
            }

            active.log_tail.push_back(line.clone());
            while active.log_tail.len() > MAX_LOG_TAIL {
                active.log_tail.pop_front();
            }

            if let Some(parsed) = parse_status_line(&line) {
                let detail = parsed
                    .detail
                    .clone()
                    .or_else(|| {
                        if parsed.status == "blocked" {
                            recent_status_detail(&active.log_tail)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| GENERIC_STATUS_DETAIL.to_string());
                active.latest_status = parsed.status.clone();
                active.latest_detail = detail.clone();
                status_update = Some((parsed.status, detail));
            } else if active.latest_status == "blocked"
                && let Some(detail) = recent_status_detail(&active.log_tail)
                && should_refresh_blocked_detail(&active.latest_detail, &detail)
            {
                active.latest_detail = detail.clone();
                status_update = Some(("blocked".to_string(), detail));
            }
        }

        let _ = self.events.send(SelfImproveEvent::LogLine {
            run_id: run_id.to_string(),
            line,
        });

        if let Some((status, detail)) = status_update {
            let _ = self.events.send(SelfImproveEvent::Status {
                run_id: run_id.to_string(),
                status,
                detail,
            });
        }
    }

    async fn finalize_run(&self, run_id: &str, exit_code: Option<i32>) {
        let (record, detail) = {
            let mut inner = self.inner.lock().await;
            let Some(active) = inner.active.take() else {
                return;
            };
            if active.run_id != run_id {
                return;
            }

            let outcome = match (active.stop_requested, exit_code) {
                (true, _) => "stopped".to_string(),
                (_, _) if active.latest_status == "blocked" => "failed".to_string(),
                (_, Some(0)) => "success".to_string(),
                _ => "failed".to_string(),
            };

            let record = SelfImproveRunRecord {
                run_id: active.run_id.clone(),
                started_at: active.started_at,
                ended_at: Some(Utc::now().to_rfc3339()),
                outcome: outcome.clone(),
                exit_code,
                max_cycles: active.max_cycles,
                turns_per_cycle: active.turns_per_cycle,
                stop_requested: active.stop_requested,
            };

            inner.history.insert(0, record.clone());
            if inner.history.len() > MAX_HISTORY {
                inner.history.truncate(MAX_HISTORY);
            }

            (
                record,
                if active.latest_status == "blocked" {
                    active.latest_detail
                } else {
                    format!("run finished outcome={} exit_code={:?}", outcome, exit_code)
                },
            )
        };

        let _ = self.events.send(SelfImproveEvent::Status {
            run_id: record.run_id.clone(),
            status: record.outcome.clone(),
            detail,
        });
        self.broadcast_snapshot().await;
    }

    async fn broadcast_snapshot(&self) {
        let _ = self
            .events
            .send(SelfImproveEvent::Snapshot(self.snapshot().await));
    }

    fn load_artifacts(&self) -> (Vec<String>, Vec<Value>) {
        let log_dir = self.repo_root.join(".self-improve-logs");
        let logs = list_recent_log_files(&log_dir, MAX_RECENT_LOGS);
        let retros = read_retrospectives(&log_dir.join("retrospectives.jsonl"), MAX_RETROSPECTIVES);
        (logs, retros)
    }

    fn launch_supported(&self) -> bool {
        self.launch_supported_with(docker_host_ready)
    }

    fn launch_supported_with<F>(&self, docker_ready: F) -> bool
    where
        F: FnOnce(&Path) -> bool,
    {
        if has_local_self_improve_script(&self.repo_root) {
            return true;
        }

        let launcher = self.repo_root.join("docker-session.sh");
        launcher.is_file() && docker_ready(&self.repo_root)
    }

    fn spawn_command(
        &self,
        cycles: u32,
        turns_per_cycle: u32,
    ) -> Result<Command, SelfImproveError> {
        self.spawn_command_with(cycles, turns_per_cycle, docker_host_ready)
    }

    fn spawn_command_with<F>(
        &self,
        cycles: u32,
        turns_per_cycle: u32,
        docker_ready: F,
    ) -> Result<Command, SelfImproveError>
    where
        F: FnOnce(&Path) -> bool,
    {
        let mut command = Command::new("setsid");
        command.arg("bash");

        if has_local_self_improve_script(&self.repo_root) {
            command.arg("./self-improve.sh");
        } else {
            let launcher = self.repo_root.join("docker-session.sh");
            if !launcher.is_file() {
                return Err(SelfImproveError::new(
                    "unsupported_environment",
                    "self-improve unavailable: missing docker-session.sh launcher",
                ));
            }
            if !docker_ready(&self.repo_root) {
                return Err(SelfImproveError::new(
                    "unsupported_environment",
                    "self-improve unavailable: Docker CLI or daemon is not runnable",
                ));
            }
            command.arg("docker-session.sh").arg("self-improve.sh");
        }

        command
            .arg(cycles.to_string())
            .current_dir(&self.repo_root)
            .env("MAX_TURN_PER_CYCLE", turns_per_cycle.to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());

        Ok(command)
    }
}

impl Default for SelfImproveManager {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_status_line(line: &str) -> Option<ParsedStatusLine> {
    let value = line.strip_prefix("STATUS: ")?.trim();
    if value.is_empty() {
        return None;
    }

    let status_end = value.find(char::is_whitespace).unwrap_or(value.len());
    let status = value[..status_end].trim();
    if status.is_empty() {
        return None;
    }

    let detail = value[status_end..].trim();
    Some(ParsedStatusLine {
        status: status.to_string(),
        detail: (!detail.is_empty()).then(|| detail.to_string()),
    })
}

fn recent_status_detail(log_tail: &VecDeque<String>) -> Option<String> {
    let candidates: Vec<String> = log_tail
        .iter()
        .filter_map(|line| normalize_status_detail_line(line))
        .collect();

    let candidate = candidates
        .iter()
        .rev()
        .find(|line| is_error_like(line))
        .cloned()
        .or_else(|| {
            candidates
                .iter()
                .rev()
                .find(|line| !line.starts_with("STATUS: "))
                .cloned()
        })?;

    Some(truncate_status_detail(&candidate))
}

fn normalize_status_detail_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(
        trimmed
            .strip_prefix("[stderr] ")
            .unwrap_or(trimmed)
            .trim()
            .to_string(),
    )
}

fn should_refresh_blocked_detail(current: &str, candidate: &str) -> bool {
    if candidate == current {
        return false;
    }

    current == GENERIC_STATUS_DETAIL
        || (!is_error_like(current) && is_error_like(candidate))
        || current.is_empty()
}

fn docker_host_ready(repo_root: &Path) -> bool {
    command_succeeds(repo_root, "docker", &["compose", "version"])
        && command_succeeds(repo_root, "docker", &["info"])
}

fn command_succeeds(repo_root: &Path, program: &str, args: &[&str]) -> bool {
    StdCommand::new(program)
        .args(args)
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn is_error_like(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("error:")
        || lower.starts_with("error ")
        || lower.starts_with("fatal:")
        || lower.starts_with("panic:")
        || lower.contains(" failed")
        || lower.ends_with(" failed")
        || lower.contains("failure")
        || lower.contains("panicked at")
        || lower.contains("denied")
}

fn truncate_status_detail(line: &str) -> String {
    const MAX_CHARS: usize = 160;

    let trimmed = line.trim();
    let mut chars = trimmed.chars();
    let truncated: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn list_recent_log_files(log_dir: &Path, max_items: usize) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(log_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with("cycle-") && file_name.ends_with(".log") {
                Some(file_name)
            } else {
                None
            }
        })
        .collect();
    names.sort_unstable();
    names.reverse();
    names.truncate(max_items);
    names
}

fn read_retrospectives(path: &Path, max_items: usize) -> Vec<Value> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut values: Vec<Value> = content
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    if values.len() > max_items {
        values.drain(..values.len().saturating_sub(max_items));
    }
    values.reverse();
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_line_extracts_status_and_detail() {
        assert_eq!(
            parse_status_line("STATUS: blocked auth bootstrap failed"),
            Some(ParsedStatusLine {
                status: "blocked".to_string(),
                detail: Some("auth bootstrap failed".to_string()),
            })
        );
    }

    #[test]
    fn parse_status_line_supports_status_without_detail() {
        assert_eq!(
            parse_status_line("STATUS: success"),
            Some(ParsedStatusLine {
                status: "success".to_string(),
                detail: None,
            })
        );
    }

    #[test]
    fn recent_status_detail_prefers_error_like_line() {
        let log_tail = VecDeque::from(vec![
            "progress update".to_string(),
            "fatal: authentication failed for https://example.com/repo.git".to_string(),
            "Try refreshing your credentials.".to_string(),
            "STATUS: blocked".to_string(),
        ]);

        assert_eq!(
            recent_status_detail(&log_tail),
            Some("fatal: authentication failed for https://example.com/repo.git".to_string())
        );
    }

    #[test]
    fn recent_status_detail_falls_back_to_last_non_status_line() {
        let log_tail = VecDeque::from(vec![
            "working".to_string(),
            "[stderr] waiting for retry budget".to_string(),
            "STATUS: blocked".to_string(),
        ]);

        assert_eq!(
            recent_status_detail(&log_tail),
            Some("waiting for retry budget".to_string())
        );
    }

    #[test]
    fn blocked_detail_refreshes_when_late_stderr_arrives() {
        assert!(should_refresh_blocked_detail(
            GENERIC_STATUS_DETAIL,
            "error: decryption failed"
        ));
        assert!(should_refresh_blocked_detail(
            "waiting for retry budget",
            "error: decryption failed"
        ));
        assert!(!should_refresh_blocked_detail(
            "error: decryption failed",
            "error: decryption failed"
        ));
    }

    #[tokio::test]
    async fn append_log_line_updates_blocked_detail_when_stderr_arrives_late() {
        let manager = SelfImproveManager::new();
        let mut events = manager.subscribe();

        {
            let mut inner = manager.inner.lock().await;
            inner.active = Some(ActiveRunState {
                run_id: "self-improve-1".to_string(),
                pid: 42,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                max_cycles: 5,
                turns_per_cycle: 30,
                stop_requested: false,
                latest_status: "starting".to_string(),
                latest_detail: "started self-improve pid=42".to_string(),
                log_tail: VecDeque::new(),
            });
        }

        manager
            .append_log_line("self-improve-1", "STATUS: blocked".to_string())
            .await;
        manager
            .append_log_line(
                "self-improve-1",
                "[stderr] error: decryption failed".to_string(),
            )
            .await;

        let active = manager.snapshot().await.active_run.expect("active run");
        assert_eq!(active.latest_status, "blocked");
        assert_eq!(active.latest_detail, "error: decryption failed");

        let mut status_events = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let SelfImproveEvent::Status { status, detail, .. } = event {
                status_events.push((status, detail));
            }
        }

        assert!(
            status_events.contains(&("blocked".to_string(), GENERIC_STATUS_DETAIL.to_string()))
        );
        assert!(status_events.contains(&(
            "blocked".to_string(),
            "error: decryption failed".to_string()
        )));
    }

    #[test]
    fn launch_supported_prefers_local_script_without_docker() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            temp_dir.path().join("self-improve.sh"),
            "#!/usr/bin/env bash\n",
        )
        .expect("local self-improve script should be writable");

        let manager = SelfImproveManager {
            inner: Arc::new(Mutex::new(InnerState {
                active: None,
                history: Vec::new(),
                next_run_seq: 1,
            })),
            events: broadcast::channel(8).0,
            repo_root: temp_dir.path().to_path_buf(),
        };

        let command = manager
            .spawn_command_with(3, 15, |_| false)
            .expect("should run local self-improve script directly");

        assert!(manager.launch_supported_with(|_| false));
        let debug = format!("{command:?}");
        assert!(debug.contains("./self-improve.sh"));
        assert!(!debug.contains("docker-session.sh"));
    }

    #[test]
    fn spawn_command_rejects_host_without_runnable_docker() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            temp_dir.path().join("docker-session.sh"),
            "#!/usr/bin/env bash\n",
        )
        .expect("launcher should be written");

        let manager = SelfImproveManager {
            inner: Arc::new(Mutex::new(InnerState {
                active: None,
                history: Vec::new(),
                next_run_seq: 1,
            })),
            events: broadcast::channel(8).0,
            repo_root: temp_dir.path().to_path_buf(),
        };

        let err = manager
            .spawn_command_with(5, 30, |_| false)
            .expect_err("docker should be required on host");
        assert_eq!(err.code, "unsupported_environment");
        assert!(err.message.contains("Docker CLI or daemon is not runnable"));
    }

    #[tokio::test]
    async fn snapshot_includes_latest_detail_for_active_run() {
        let manager = SelfImproveManager::new();

        {
            let mut inner = manager.inner.lock().await;
            inner.active = Some(ActiveRunState {
                run_id: "self-improve-1".to_string(),
                pid: 42,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                max_cycles: 5,
                turns_per_cycle: 30,
                stop_requested: false,
                latest_status: "blocked".to_string(),
                latest_detail: "error: decryption failed".to_string(),
                log_tail: VecDeque::from(vec!["error: decryption failed".to_string()]),
            });
        }

        let snapshot = manager.snapshot().await;
        let active = snapshot.active_run.expect("active run should be present");

        assert_eq!(active.latest_status, "blocked");
        assert_eq!(active.latest_detail, "error: decryption failed");
        assert_eq!(
            active.log_tail,
            vec!["error: decryption failed".to_string()]
        );
    }

    #[tokio::test]
    async fn finalize_run_preserves_blocked_failure_detail_on_clean_exit() {
        let manager = SelfImproveManager::new();
        let mut events = manager.subscribe();

        {
            let mut inner = manager.inner.lock().await;
            inner.active = Some(ActiveRunState {
                run_id: "self-improve-1".to_string(),
                pid: 42,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                max_cycles: 5,
                turns_per_cycle: 30,
                stop_requested: false,
                latest_status: "blocked".to_string(),
                latest_detail: "error: decryption failed".to_string(),
                log_tail: VecDeque::from(vec![
                    "STATUS: blocked".to_string(),
                    "[stderr] error: decryption failed".to_string(),
                ]),
            });
        }

        manager.finalize_run("self-improve-1", Some(0)).await;

        let snapshot = manager.snapshot().await;
        assert!(snapshot.active_run.is_none());
        assert_eq!(snapshot.history[0].outcome, "failed");

        let mut terminal_status = None;
        while let Ok(event) = events.try_recv() {
            if let SelfImproveEvent::Status { status, detail, .. } = event {
                terminal_status = Some((status, detail));
            }
        }

        assert_eq!(
            terminal_status,
            Some(("failed".to_string(), "error: decryption failed".to_string(),))
        );
    }
}
