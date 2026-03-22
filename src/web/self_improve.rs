use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
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
    log_tail: VecDeque<String>,
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
                    log_tail: active.log_tail.iter().cloned().collect(),
                }),
                inner.history.clone(),
            )
        };

        let (recent_logs, retrospectives) = self.load_artifacts();
        SelfImproveSnapshotData {
            available: Path::new("/.dockerenv").exists(),
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
        if !Path::new("/.dockerenv").exists() {
            return Err(SelfImproveError::new(
                "unsupported_environment",
                "self-improve requires Docker; run web server in-container",
            ));
        }

        let cycles = max_cycles
            .unwrap_or(DEFAULT_MAX_CYCLES)
            .clamp(1, MAX_CYCLES);
        let turns = turns_per_cycle
            .unwrap_or(DEFAULT_TURNS_PER_CYCLE)
            .clamp(1, MAX_TURNS_PER_CYCLE);

        let (run_id, child, pid) = {
            let mut inner = self.inner.lock().await;
            if inner.active.is_some() {
                return Err(SelfImproveError::new(
                    "already_running",
                    "self-improve run is already active",
                ));
            }

            let run_id = format!("self-improve-{}", inner.next_run_seq);
            inner.next_run_seq += 1;

            let mut child = Command::new("setsid");
            child
                .arg("bash")
                .arg("self-improve.sh")
                .arg(cycles.to_string())
                .current_dir(&self.repo_root)
                .env("MAX_TURN_PER_CYCLE", turns.to_string())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .stdin(std::process::Stdio::null());

            let child = child.spawn().map_err(|err| {
                SelfImproveError::new("process_spawn_failed", format!("failed to start: {err}"))
            })?;

            let pid = child.id().ok_or_else(|| {
                SelfImproveError::new("process_spawn_failed", "started process without pid")
            })?;

            inner.active = Some(ActiveRunState {
                run_id: run_id.clone(),
                pid,
                started_at: Utc::now().to_rfc3339(),
                max_cycles: cycles,
                turns_per_cycle: turns,
                stop_requested: false,
                latest_status: "starting".to_string(),
                log_tail: VecDeque::new(),
            });

            (run_id, child, pid)
        };

        let _ = self.events.send(SelfImproveEvent::Status {
            run_id: run_id.clone(),
            status: "starting".to_string(),
            detail: format!("started self-improve pid={pid}"),
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

        let _ = self.events.send(SelfImproveEvent::Status {
            run_id,
            status: "stopping".to_string(),
            detail: format!("sent SIGTERM to process group -{pid}"),
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
                active.latest_status = parsed.clone();
                status_update = Some(parsed);
            }
        }

        let _ = self.events.send(SelfImproveEvent::LogLine {
            run_id: run_id.to_string(),
            line,
        });

        if let Some(status) = status_update {
            let _ = self.events.send(SelfImproveEvent::Status {
                run_id: run_id.to_string(),
                status,
                detail: "status parsed from script output".to_string(),
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
                format!("run finished outcome={} exit_code={:?}", outcome, exit_code),
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
}

fn parse_status_line(line: &str) -> Option<String> {
    line.strip_prefix("STATUS: ")
        .map(|value| value.trim().to_string())
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
