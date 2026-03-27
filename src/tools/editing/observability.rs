use crate::text::truncate_with_ascii_ellipsis;

use super::artifacts::persist_observation;
use super::telemetry::persist_metric;
use super::{EditFailureKind, EditObservation, EditOutcome, EDIT_TOOL_SUMMARY_MAX_CHARS};

pub fn finalize_outcome(tool_name: &str, outcome: EditOutcome) -> (String, Vec<EditObservation>) {
    let first_line = outcome.tool_summary();
    let outcome_failure_kind = outcome.failure_kind().map(|kind| kind.as_str().to_string());
    let (_raw_summary, mut observations) = outcome.into_summary_and_observations();

    if observations.is_empty() {
        observations.push(EditObservation::new(tool_name, tool_name, "", 0, 0));
    }

    let observation = &mut observations[0];
    if observation.tool_name.is_empty() {
        observation.tool_name = tool_name.to_string();
    }
    observation.model_output_bounded = true;
    if observation.failure_kind.is_none() {
        observation.failure_kind = outcome_failure_kind.clone();
    }

    let mut telemetry_failure_kind = None::<String>;

    let output = match persist_observation(observation, &first_line) {
        Ok(artifact) => {
            observation.artifact_id = Some(artifact.artifact_id.clone());
            observation.artifact_path = Some(artifact.artifact_path.clone());
            compact_summary_with_artifact(
                &first_line,
                &artifact.artifact_id,
                &artifact.artifact_path,
            )
        }
        Err(_err) => {
            telemetry_failure_kind =
                Some(EditFailureKind::TelemetryUnavailable.as_str().to_string());
            observation.artifact_id = None;
            observation.artifact_path = None;
            truncate_with_ascii_ellipsis(&first_line, EDIT_TOOL_SUMMARY_MAX_CHARS)
        }
    };

    if persist_metric(
        observation,
        &first_line,
        outcome_failure_kind.as_deref(),
        telemetry_failure_kind.as_deref(),
    )
    .is_err()
    {
        telemetry_failure_kind = Some(EditFailureKind::TelemetryUnavailable.as_str().to_string());
    }

    if telemetry_failure_kind.is_some() {
        observation.failure_kind = telemetry_failure_kind;
    }

    (output, observations)
}

fn compact_summary_with_artifact(
    first_line: &str,
    artifact_id: &str,
    artifact_path: &str,
) -> String {
    let bounded_first_line = truncate_with_ascii_ellipsis(first_line, EDIT_TOOL_SUMMARY_MAX_CHARS);
    let bounded_path = truncate_with_ascii_ellipsis(artifact_path, EDIT_TOOL_SUMMARY_MAX_CHARS);
    format!(
        "{}\nartifact_id={} artifact_path={}",
        bounded_first_line, artifact_id, bounded_path
    )
}
