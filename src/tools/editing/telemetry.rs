use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use serde::Serialize;

use super::EditObservation;
use super::artifacts::artifact_root_dir;

const METRICS_JSONL_FILE: &str = "metrics.jsonl";
const EDIT_METRICS_DIR_ENV: &str = "KLEY_EDIT_METRICS_DIR";

thread_local! {
    static METRICS_ROOT_OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[derive(Debug, Serialize)]
struct EditMetricRecord<'a> {
    event: &'static str,
    created_at: String,
    summary_first_line: &'a str,
    tool_name: &'a str,
    engine: &'a str,
    path: &'a str,
    edit_count: usize,
    applied_count: usize,
    stale_reference_count: usize,
    noop_count: usize,
    duration_ms: u128,
    model_output_bounded: bool,
    artifact_id: Option<&'a str>,
    artifact_path: Option<&'a str>,
    outcome_failure_kind: Option<&'a str>,
    telemetry_failure_kind: Option<&'a str>,
}

pub fn persist_metric(
    observation: &EditObservation,
    summary_first_line: &str,
    outcome_failure_kind: Option<&str>,
    telemetry_failure_kind: Option<&str>,
) -> std::io::Result<()> {
    let root = metrics_root_dir();
    fs::create_dir_all(&root)?;

    let record = EditMetricRecord {
        event: "edit.write_path.completed",
        created_at: Utc::now().to_rfc3339(),
        summary_first_line,
        tool_name: &observation.tool_name,
        engine: &observation.engine,
        path: &observation.path,
        edit_count: observation.edit_count,
        applied_count: observation.applied_count,
        stale_reference_count: observation.stale_reference_count,
        noop_count: observation.noop_count,
        duration_ms: observation.duration_ms,
        model_output_bounded: observation.model_output_bounded,
        artifact_id: observation.artifact_id.as_deref(),
        artifact_path: observation.artifact_path.as_deref(),
        outcome_failure_kind,
        telemetry_failure_kind,
    };

    let line = serde_json::to_string(&record).map_err(std::io::Error::other)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(METRICS_JSONL_FILE))?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn metrics_root_dir() -> PathBuf {
    if let Some(override_path) = METRICS_ROOT_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return override_path;
    }

    if let Ok(override_dir) = std::env::var(EDIT_METRICS_DIR_ENV) {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    artifact_root_dir()
}

pub fn with_metrics_root_override<T>(path: &Path, action: impl FnOnce() -> T) -> T {
    struct ResetGuard {
        previous: Option<PathBuf>,
    }

    impl Drop for ResetGuard {
        fn drop(&mut self) {
            let previous = self.previous.take();
            METRICS_ROOT_OVERRIDE.with(|slot| {
                *slot.borrow_mut() = previous;
            });
        }
    }

    let previous = METRICS_ROOT_OVERRIDE.with(|slot| {
        let mut current = slot.borrow_mut();
        (*current).replace(path.to_path_buf())
    });
    let _guard = ResetGuard { previous };
    action()
}
