use axum::Router;
use axum::body::Bytes;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::post;
use std::fs;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

use kley::compact::CompactConfig;
use kley::events::{AgentEvent, event_channel};
use kley::runtime::{SessionRuntime, SubmitResult};
use kley::store::{Store, Turn};
use kley::test_openai::{self, ControlledResponse, TEST_MODEL};
use kley::tools::editing::{EDIT_TOOL_SUMMARY_MAX_CHARS, EditObservation};
use kley::tools::hashline_edit::HashlineEditTool;
use kley::tools::patch::PatchTool;
use kley::tools::{Tool, ToolExecutionResult, ToolRegistry};
use sha2::{Digest, Sha256};

const EDIT_ARTIFACT_DIR_ENV: &str = "KLEY_EDIT_ARTIFACT_DIR";
const EDIT_METRICS_DIR_ENV: &str = "KLEY_EDIT_METRICS_DIR";

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => {
                unsafe { std::env::set_var(self.key, previous) };
            }
            None => {
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }
}

fn hash_line(line: &str) -> String {
    let digest = Sha256::digest(line.as_bytes());
    hex::encode(&digest[..4])
}

fn anchor_for_line(snapshot: &str, line_number: usize) -> String {
    let line = snapshot
        .lines()
        .nth(line_number - 1)
        .unwrap_or_else(|| panic!("line {} missing in snapshot", line_number));
    format!("{}#{}", line_number, hash_line(line))
}

fn assert_required_fields(observation: &EditObservation) {
    assert!(!observation.engine.is_empty());
    assert!(!observation.tool_name.is_empty());
    assert!(!observation.path.is_empty());
    assert!(observation.model_output_bounded);
    assert!(observation.artifact_id.is_some());
    assert!(observation.artifact_path.is_some());
}

fn read_json_artifact(result: &ToolExecutionResult) -> serde_json::Value {
    let observation = result.edit_observations.first().unwrap();
    let artifact_path = observation.artifact_path.as_ref().unwrap();
    let raw = fs::read_to_string(artifact_path).unwrap();
    serde_json::from_str::<serde_json::Value>(&raw).unwrap()
}

struct TestServer {
    addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_app(app: Router) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer { addr, task }
}

async fn controlled_openai_sse_handler(body: Bytes) -> impl IntoResponse {
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let prompt = payload
        .get("input")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.last())
        .and_then(|item| item.get("content"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let body = match test_openai::parse_controlled_prompt(prompt) {
        Some(ControlledResponse::ToolCall { name, arguments }) => {
            test_openai::tool_call_sse(&name, &arguments)
        }
        Some(ControlledResponse::Text { content }) => test_openai::text_sse(&content),
        None if prompt.to_lowercase().contains("tool") => {
            test_openai::tool_call_sse("unknown_tool", &serde_json::json!({}))
        }
        None => test_openai::text_sse(&format!("Mock assistant reply: {prompt}")),
    };
    ([(header::CONTENT_TYPE, "text/event-stream")], body)
}

#[derive(Clone)]
struct ObservedUnknownTool {
    output: String,
    observation: EditObservation,
}

impl Tool for ObservedUnknownTool {
    fn name(&self) -> &str {
        "unknown_tool"
    }

    fn description(&self) -> &str {
        "tool that emits edit observations"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        })
    }

    fn execute(&self, _args: serde_json::Value) -> anyhow::Result<String> {
        Ok(self.output.clone())
    }

    fn execute_with_result(&self, _args: serde_json::Value) -> anyhow::Result<ToolExecutionResult> {
        Ok(ToolExecutionResult::with_edit_observations(
            self.output.clone(),
            vec![self.observation.clone()],
        ))
    }
}

#[test]
fn successful_edits_write_structured_artifacts_with_required_fields() {
    let _lock = env_lock().lock().unwrap();

    let artifacts_dir = tempfile::tempdir().unwrap();
    let _env = EnvVarGuard::set(
        EDIT_ARTIFACT_DIR_ENV,
        &artifacts_dir.path().to_string_lossy(),
    );

    let mut patch_target = tempfile::NamedTempFile::new().unwrap();
    patch_target.write_all(b"alpha\nbeta\n").unwrap();

    let patch_result = PatchTool
        .execute_with_result(serde_json::json!({
            "path": patch_target.path().to_string_lossy(),
            "target": "beta",
            "replacement": "BETA",
        }))
        .unwrap();
    let patch_observation = patch_result.edit_observations.first().unwrap();
    assert_required_fields(patch_observation);
    assert_eq!(patch_observation.tool_name, "patch");
    assert_eq!(patch_observation.applied_count, 1);
    assert_eq!(patch_observation.stale_reference_count, 0);
    assert_eq!(patch_observation.noop_count, 0);
    assert!(
        patch_result
            .output
            .contains(patch_observation.artifact_id.as_ref().unwrap())
    );
    assert!(
        patch_result
            .output
            .contains(patch_observation.artifact_path.as_ref().unwrap())
    );

    let patch_artifact = read_json_artifact(&patch_result);
    let patch_artifact_obs = patch_artifact.get("observation").unwrap();
    for required in [
        "engine",
        "tool_name",
        "path",
        "edit_count",
        "applied_count",
        "stale_reference_count",
        "noop_count",
        "failure_kind",
        "duration_ms",
        "artifact_path",
        "artifact_id",
        "model_output_bounded",
    ] {
        assert!(
            patch_artifact_obs.get(required).is_some(),
            "missing required observation field: {required}"
        );
    }
    assert_eq!(patch_artifact_obs["model_output_bounded"], true);

    let mut hashline_target = tempfile::NamedTempFile::new().unwrap();
    hashline_target.write_all(b"one\ntwo\n").unwrap();
    let snapshot = fs::read_to_string(hashline_target.path()).unwrap();
    let anchor = anchor_for_line(&snapshot, 2);

    let hashline_result = HashlineEditTool
        .execute_with_result(serde_json::json!({
            "path": hashline_target.path().to_string_lossy(),
            "edits": [
                {
                    "kind": "replace",
                    "start": anchor,
                    "end": anchor_for_line(&snapshot, 2),
                    "replacement": "TWO\n"
                }
            ]
        }))
        .unwrap();

    let hashline_observation = hashline_result.edit_observations.first().unwrap();
    assert_required_fields(hashline_observation);
    assert_eq!(hashline_observation.tool_name, "hashline_edit");
    assert_eq!(hashline_observation.applied_count, 1);
    assert_eq!(hashline_observation.stale_reference_count, 0);
    assert_eq!(hashline_observation.noop_count, 0);

    let runs_jsonl = artifacts_dir.path().join("runs.jsonl");
    assert!(runs_jsonl.exists());
    let runs_contents = fs::read_to_string(runs_jsonl).unwrap();
    assert!(runs_contents.contains(patch_observation.artifact_id.as_ref().unwrap()));
    assert!(runs_contents.contains(hashline_observation.artifact_id.as_ref().unwrap()));

    let metrics_jsonl = artifacts_dir.path().join("metrics.jsonl");
    assert!(metrics_jsonl.exists());
    let metrics_contents = fs::read_to_string(metrics_jsonl).unwrap();
    assert!(metrics_contents.contains("\"event\":\"edit.write_path.completed\""));
    assert!(metrics_contents.contains(patch_observation.artifact_id.as_ref().unwrap()));
    assert!(metrics_contents.contains(hashline_observation.artifact_id.as_ref().unwrap()));
}

#[test]
fn telemetry_failure_does_not_fail_successful_edit() {
    let _lock = env_lock().lock().unwrap();

    let root = tempfile::tempdir().unwrap();
    let artifacts_root = root.path().join("artifacts");
    fs::create_dir_all(&artifacts_root).unwrap();
    let blocked_metrics_root = root.path().join("metrics-root-blocked");
    fs::write(&blocked_metrics_root, "blocked").unwrap();
    let _artifact_env = EnvVarGuard::set(EDIT_ARTIFACT_DIR_ENV, &artifacts_root.to_string_lossy());
    let _metrics_env = EnvVarGuard::set(
        EDIT_METRICS_DIR_ENV,
        &blocked_metrics_root.to_string_lossy(),
    );

    let mut target = tempfile::NamedTempFile::new().unwrap();
    target.write_all(b"alpha\nbeta\n").unwrap();

    let result = PatchTool
        .execute_with_result(serde_json::json!({
            "path": target.path().to_string_lossy(),
            "target": "beta",
            "replacement": "BETA",
        }))
        .unwrap();

    assert_eq!(fs::read_to_string(target.path()).unwrap(), "alpha\nBETA\n");
    assert!(result.output.starts_with("Applied:"), "{}", result.output);
    assert!(!result.output.starts_with("Error:"), "{}", result.output);

    let observation = result.edit_observations.first().unwrap();
    assert_eq!(observation.applied_count, 1);
    assert_eq!(
        observation.failure_kind.as_deref(),
        Some("telemetry_unavailable")
    );
    assert!(observation.artifact_id.is_some());
    assert!(observation.artifact_path.is_some());
    assert!(
        fs::metadata(observation.artifact_path.as_ref().unwrap()).is_ok(),
        "artifact should still be persisted when metrics fail"
    );

    assert!(!blocked_metrics_root.join("metrics.jsonl").exists());
}

#[test]
fn tool_output_is_bounded_and_artifact_backed() {
    let _lock = env_lock().lock().unwrap();

    let artifacts_dir = tempfile::tempdir().unwrap();
    let _env = EnvVarGuard::set(
        EDIT_ARTIFACT_DIR_ENV,
        &artifacts_dir.path().to_string_lossy(),
    );

    let workdir = tempfile::tempdir().unwrap();
    let long_name = format!("{}_{}.txt", "very_long_file_name_segment".repeat(8), "tail");
    let file_path = workdir.path().join(long_name);
    fs::write(&file_path, "sensitive_line_alpha\nsensitive_line_beta\n").unwrap();

    let result = PatchTool
        .execute_with_result(serde_json::json!({
            "path": file_path,
            "target": "sensitive_line_beta",
            "replacement": "REDACTED",
        }))
        .unwrap();

    let output_lines = result.output.lines().collect::<Vec<_>>();
    assert_eq!(output_lines.len(), 2);
    assert!(output_lines[0].chars().count() <= EDIT_TOOL_SUMMARY_MAX_CHARS + 3);
    assert!(output_lines[1].contains("artifact_id="));
    assert!(output_lines[1].contains("artifact_path="));
    assert!(!result.output.contains("sensitive_line_alpha"));
    assert!(!result.output.contains("sensitive_line_beta"));

    let observation = result.edit_observations.first().unwrap();
    let artifact_path = observation.artifact_path.as_ref().unwrap();
    assert!(fs::metadata(artifact_path).is_ok());

    let artifact = read_json_artifact(&result);
    assert_eq!(artifact["observation"]["model_output_bounded"], true);
    assert!(artifact["observation"]["duration_ms"].is_number());
}

#[test]
fn malformed_hashline_request_still_records_observation() {
    let _lock = env_lock().lock().unwrap();

    let artifacts_dir = tempfile::tempdir().unwrap();
    let _env = EnvVarGuard::set(
        EDIT_ARTIFACT_DIR_ENV,
        &artifacts_dir.path().to_string_lossy(),
    );

    let result = HashlineEditTool
        .execute_with_result(serde_json::json!({
            "path": "ignored",
        }))
        .unwrap();

    assert!(result.output.contains("Error: invalid_request"));
    assert!(result.output.contains("artifact_id="));
    assert!(result.output.contains("artifact_path="));

    let observation = result.edit_observations.first().unwrap();
    assert_eq!(observation.failure_kind.as_deref(), Some("invalid_request"));
    assert!(observation.artifact_id.is_some());
    assert!(observation.artifact_path.is_some());
    assert!(fs::metadata(observation.artifact_path.as_ref().unwrap()).is_ok());
}

#[tokio::test]
async fn runtime_persists_edit_observation_in_function_call_output() {
    let store = Store::open_memory().unwrap();
    let (emitter, _receiver) = event_channel();
    let server =
        spawn_app(Router::new().route("/responses", post(controlled_openai_sse_handler))).await;

    let mut observation = EditObservation::new("hashline", "unknown_tool", "src/lib.rs", 2, 9);
    observation.applied_count = 1;
    observation.stale_reference_count = 1;
    observation.failure_kind = Some("stale_reference".to_string());
    observation.artifact_id = Some("artifact-rt-1".to_string());
    observation.artifact_path = Some("/tmp/artifact-rt-1.json".to_string());

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ObservedUnknownTool {
        output: "summary line\nartifact_id=artifact-rt-1 artifact_path=/tmp/artifact-rt-1.json"
            .to_string(),
        observation: observation.clone(),
    }));

    let mut runtime = SessionRuntime::new(
        &store,
        test_openai::auth(format!("http://{}", server.addr)),
        Some(TEST_MODEL),
        None,
        emitter,
        CompactConfig::default(),
        registry,
        "system".to_string(),
        kley::runtime::RuntimeHooks::default(),
    )
    .unwrap();

    let result = runtime
        .submit_prompt("please use a tool".to_string())
        .await
        .unwrap();
    assert!(matches!(result, SubmitResult::Completed { .. }));

    let turns = Turn::list_for_session(&store, runtime.session_id()).unwrap();
    let function_output = turns
        .iter()
        .find(|turn| turn.kind == "function_call_output")
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&function_output.content).unwrap();
    assert_eq!(
        payload["output"],
        serde_json::json!(
            "summary line\nartifact_id=artifact-rt-1 artifact_path=/tmp/artifact-rt-1.json"
        )
    );
    assert_eq!(
        payload["edit_observation"]["engine"],
        serde_json::json!("hashline")
    );
    assert_eq!(
        payload["edit_observation"]["artifact_id"],
        serde_json::json!("artifact-rt-1")
    );
    assert_eq!(
        payload["edit_observation"]["failure_kind"],
        serde_json::json!("stale_reference")
    );
}

#[tokio::test]
async fn runtime_events_include_engine_and_failure_metadata() {
    let store = Store::open_memory().unwrap();
    let (emitter, receiver) = event_channel();
    let server =
        spawn_app(Router::new().route("/responses", post(controlled_openai_sse_handler))).await;

    let mut observation = EditObservation::new("hashline", "unknown_tool", "src/lib.rs", 3, 11);
    observation.applied_count = 1;
    observation.stale_reference_count = 1;
    observation.noop_count = 0;
    observation.failure_kind = Some("stale_reference".to_string());
    observation.artifact_id = Some("artifact-rt-2".to_string());
    observation.artifact_path = Some("/tmp/artifact-rt-2.json".to_string());

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ObservedUnknownTool {
        output: "summary line\nartifact_id=artifact-rt-2 artifact_path=/tmp/artifact-rt-2.json"
            .to_string(),
        observation,
    }));

    let mut runtime = SessionRuntime::new(
        &store,
        test_openai::auth(format!("http://{}", server.addr)),
        Some(TEST_MODEL),
        None,
        emitter,
        CompactConfig::default(),
        registry,
        "system".to_string(),
        kley::runtime::RuntimeHooks::default(),
    )
    .unwrap();

    let result = runtime
        .submit_prompt("please use a tool".to_string())
        .await
        .unwrap();
    assert!(matches!(result, SubmitResult::Completed { .. }));

    let events = receiver.drain();
    let tool_completed = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallCompleted {
                output_preview,
                edit_observation,
                ..
            } => Some((output_preview, edit_observation)),
            _ => None,
        })
        .expect("expected tool completion event");

    assert!(tool_completed.0.starts_with("artifact_id=artifact-rt-2"));

    let observation = tool_completed
        .1
        .as_deref()
        .expect("expected edit observation on tool completion");
    assert_eq!(observation.engine, "hashline");
    assert_eq!(observation.edit_count, 3);
    assert_eq!(observation.applied_count, 1);
    assert_eq!(observation.stale_reference_count, 1);
    assert_eq!(observation.failure_kind.as_deref(), Some("stale_reference"));
    assert_eq!(observation.artifact_id.as_deref(), Some("artifact-rt-2"));
}
