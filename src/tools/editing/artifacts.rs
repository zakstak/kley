use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use super::EditObservation;

const EDIT_ARTIFACT_DIR_ENV: &str = "KLEY_EDIT_ARTIFACT_DIR";

thread_local! {
    static ARTIFACT_ROOT_OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedArtifact {
    pub artifact_id: String,
    pub artifact_path: String,
}

#[derive(Debug, Serialize)]
struct EditArtifactRecord<'a> {
    artifact_id: &'a str,
    created_at: String,
    summary_first_line: &'a str,
    observation: &'a EditObservation,
}

pub fn artifact_root_dir() -> PathBuf {
    if let Some(override_path) = ARTIFACT_ROOT_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return override_path;
    }

    if let Ok(override_dir) = std::env::var(EDIT_ARTIFACT_DIR_ENV) {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    if let Some(local_data) = dirs::data_local_dir() {
        return local_data.join("kley").join("edit-artifacts");
    }

    std::env::temp_dir().join("kley").join("edit-artifacts")
}

pub fn with_artifact_root_override<T>(path: &Path, action: impl FnOnce() -> T) -> T {
    struct ResetGuard {
        previous: Option<PathBuf>,
    }

    impl Drop for ResetGuard {
        fn drop(&mut self) {
            let previous = self.previous.take();
            ARTIFACT_ROOT_OVERRIDE.with(|slot| {
                *slot.borrow_mut() = previous;
            });
        }
    }

    let previous = ARTIFACT_ROOT_OVERRIDE.with(|slot| {
        let mut current = slot.borrow_mut();
        (*current).replace(path.to_path_buf())
    });
    let _guard = ResetGuard { previous };
    action()
}

pub fn persist_observation(
    observation: &EditObservation,
    summary_first_line: &str,
) -> std::io::Result<PersistedArtifact> {
    let root = artifact_root_dir();
    fs::create_dir_all(&root)?;

    let artifact_id = Uuid::new_v4().to_string();
    let artifact_path = root.join(format!("{artifact_id}.json"));
    let artifact_path_string = artifact_path.to_string_lossy().to_string();
    let runs_jsonl = root.join("runs.jsonl");

    let mut observation_with_artifact = observation.clone();
    observation_with_artifact.artifact_id = Some(artifact_id.clone());
    observation_with_artifact.artifact_path = Some(artifact_path_string.clone());

    let record = EditArtifactRecord {
        artifact_id: &artifact_id,
        created_at: Utc::now().to_rfc3339(),
        summary_first_line,
        observation: &observation_with_artifact,
    };

    let json_bytes = serde_json::to_vec_pretty(&record)?;
    fs::write(&artifact_path, json_bytes)?;

    let mut jsonl_line = serde_json::to_string(&record)?;
    jsonl_line.push('\n');
    let mut runs = OpenOptions::new()
        .create(true)
        .append(true)
        .open(runs_jsonl)?;
    runs.write_all(jsonl_line.as_bytes())?;

    Ok(PersistedArtifact {
        artifact_id,
        artifact_path: artifact_path_string,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn sample_observation() -> EditObservation {
        EditObservation::new("hashline", "hashline_edit", "src/lib.rs", 1, 10)
    }

    #[test]
    fn persist_observation_writes_complete_jsonl_line() {
        let artifact_root = tempdir().unwrap();
        with_artifact_root_override(artifact_root.path(), || {
            let obs = sample_observation();
            persist_observation(&obs, "summary-one").unwrap();
            let mut obs2 = sample_observation();
            obs2.tool_name = "hashline_edit_2".to_string();
            persist_observation(&obs2, "summary-two").unwrap();
        });

        let runs_path = artifact_root.path().join("runs.jsonl");
        let contents = fs::read_to_string(runs_path).unwrap();
        let newline_count = contents.chars().filter(|c| *c == '\n').count();
        assert_eq!(newline_count, 2, "each record should end with a newline");
        assert!(
            contents.ends_with('\n'),
            "runs file should terminate with newline"
        );
    }
}
