use std::collections::BTreeSet;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::Value;
use sha2::{Digest, Sha256};

const EDIT_ARTIFACT_DIR_ENV: &str = "KLEY_EDIT_ARTIFACT_DIR";

struct TestServer {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct MockWritePathState {
    request_count: Arc<AtomicUsize>,
    tool_arguments: String,
}

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn hashline_harness_fixture_dir() -> PathBuf {
    manifest_dir().join("tests/fixtures/hashline/harness")
}

fn hash_line(line: &str) -> String {
    let digest = Sha256::digest(line.as_bytes());
    hex::encode(&digest[..4])
}

fn anchor_for_line(snapshot: &str, line_number: usize) -> String {
    let line = snapshot
        .lines()
        .nth(line_number - 1)
        .unwrap_or_else(|| panic!("line {line_number} missing in snapshot"));
    format!("{line_number}#{}", hash_line(line))
}

fn output_text(output: &[u8]) -> String {
    String::from_utf8_lossy(output).into_owned()
}

fn debug_command_output(output: &Output) -> String {
    format!(
        "status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        output_text(&output.stdout),
        output_text(&output.stderr)
    )
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect()
}

fn json_string<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key}: {value}"))
}

fn run_hashline_harness(output_dir: &Path, scenario_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hashline-harness"))
        .arg("--provider")
        .arg("test")
        .arg("--model")
        .arg("test-model")
        .arg("--engine")
        .arg("hashline_edit")
        .arg("--scenario-dir")
        .arg(scenario_dir)
        .arg("--output-dir")
        .arg(output_dir)
        .output()
        .expect("failed to launch hashline-harness")
}

async fn spawn_app(app: Router) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer { addr, task }
}

async fn openai_hashline_write_handler(
    State(state): State<MockWritePathState>,
    _body: Bytes,
) -> Response {
    let index = state.request_count.fetch_add(1, Ordering::Relaxed);
    let body = if index == 0 {
        tool_call_sse("hashline_edit", &state.tool_arguments)
    } else {
        text_sse("write path complete")
    };

    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

fn tool_call_sse(name: &str, arguments: &str) -> String {
    format!(
        concat!(
            "event: response.output_item.added\n",
            "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"{name}\"}}}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {{\"type\":\"response.function_call_arguments.delta\",\"delta\":{arguments}}}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {{\"type\":\"response.function_call_arguments.done\",\"call_id\":\"call-1\",\"name\":\"{name}\"}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"usage\":{{\"input_tokens\":11,\"output_tokens\":7,\"total_tokens\":18}}}}\n\n"
        ),
        name = name,
        arguments = serde_json::to_string(arguments).unwrap(),
    )
}

fn text_sse(text: &str) -> String {
    format!(
        concat!(
            "event: response.output_text.delta\n",
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":{text}}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"usage\":{{\"input_tokens\":13,\"output_tokens\":5,\"total_tokens\":18}}}}\n\n"
        ),
        text = serde_json::to_string(text).unwrap(),
    )
}

#[test]
fn harness_subprocess_writes_runs_jsonl_and_status_coverage() {
    let output_root = tempfile::tempdir().unwrap();
    let output = run_hashline_harness(output_root.path(), &hashline_harness_fixture_dir());
    assert!(output.status.success(), "{}", debug_command_output(&output));

    let stdout = output_text(&output.stdout);
    let stderr = output_text(&output.stderr);
    assert!(
        stdout.contains("hashline harness complete: runs=5"),
        "{stdout}"
    );
    assert!(stdout.contains("success=1"), "{stdout}");
    assert!(stdout.contains("correctness_failure=1"), "{stdout}");
    assert!(stdout.contains("edit_failure=1"), "{stdout}");
    assert!(stdout.contains("provider_failure=1"), "{stdout}");
    assert!(stdout.contains("telemetry_failure=1"), "{stdout}");
    assert!(stderr.trim().is_empty(), "{stderr}");

    let runs_jsonl = output_root.path().join("runs.jsonl");
    assert!(runs_jsonl.is_file(), "missing {}", runs_jsonl.display());

    let records = read_jsonl(&runs_jsonl);
    assert_eq!(records.len(), 5);

    let statuses = records
        .iter()
        .map(|record| json_string(record, "status").to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        statuses,
        BTreeSet::from([
            "correctness_failure".to_string(),
            "edit_failure".to_string(),
            "provider_failure".to_string(),
            "success".to_string(),
            "telemetry_failure".to_string(),
        ])
    );

    for record in &records {
        assert_eq!(json_string(record, "engine"), "hashline_edit");
        assert_eq!(json_string(record, "provider"), "test");
        assert_eq!(json_string(record, "model"), "test-model");
        assert!(record.get("duration_ms").and_then(Value::as_u64).is_some());

        for key in [
            "prompt_path",
            "provider_result_path",
            "tool_result_path",
            "run_artifact_path",
            "workspace_snapshot_dir",
        ] {
            let path = PathBuf::from(json_string(record, key));
            assert!(path.exists(), "missing {key} at {}", path.display());
        }

        let tool_result_path = PathBuf::from(json_string(record, "tool_result_path"));
        let tool_result: Value =
            serde_json::from_str(&fs::read_to_string(&tool_result_path).unwrap()).unwrap();
        assert!(tool_result.get("observations").is_some());
        assert!(tool_result.get("error").is_some());

        let run_artifact_path = PathBuf::from(json_string(record, "run_artifact_path"));
        let run_artifact: Value =
            serde_json::from_str(&fs::read_to_string(&run_artifact_path).unwrap()).unwrap();
        assert_eq!(run_artifact["status"], record["status"]);
        assert_eq!(run_artifact["scenario"], record["scenario"]);

        let metrics_jsonl =
            PathBuf::from(json_string(record, "artifact_root")).join("metrics.jsonl");
        match json_string(record, "status") {
            "success" | "correctness_failure" | "edit_failure" => {
                assert!(
                    metrics_jsonl.is_file(),
                    "missing metrics at {}",
                    metrics_jsonl.display()
                );
            }
            "provider_failure" | "telemetry_failure" => {
                assert!(
                    !metrics_jsonl.exists(),
                    "unexpected metrics at {}",
                    metrics_jsonl.display()
                );
            }
            other => panic!("unexpected status: {other}"),
        }

        match json_string(record, "status") {
            "provider_failure" => {
                assert!(
                    record
                        .get("provider_error")
                        .and_then(Value::as_str)
                        .is_some()
                );
                let selected_tool_call = record.get("selected_tool_call");
                assert!(match selected_tool_call {
                    None => true,
                    Some(value) => value.is_null(),
                });
            }
            _ => {
                assert_eq!(
                    record["selected_tool_call"]["name"],
                    serde_json::json!("hashline_edit")
                );
            }
        }

        if json_string(record, "status") == "telemetry_failure" {
            let observation = record
                .get("edit_observation")
                .and_then(Value::as_object)
                .expect("telemetry failure should still include an edit observation");
            assert_eq!(
                observation
                    .get("failure_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                "telemetry_unavailable"
            );
            let artifact_path = observation
                .get("artifact_path")
                .and_then(Value::as_str)
                .expect("telemetry failure should keep local artifact path");
            assert!(
                PathBuf::from(artifact_path).is_file(),
                "missing {artifact_path}"
            );
        }
    }
}

#[test]
fn harness_subprocess_reports_non_zero_exit_for_missing_scenarios() {
    let temp = tempfile::tempdir().unwrap();
    let missing_dir = temp.path().join("missing-scenarios");
    let output_dir = temp.path().join("output");
    let output = run_hashline_harness(&output_dir, &missing_dir);

    assert!(
        !output.status.success(),
        "{}",
        debug_command_output(&output)
    );

    let stderr = output_text(&output.stderr);
    assert!(stderr.contains("error:"), "{stderr}");
    assert!(
        stderr.contains("scenario directory does not exist"),
        "{stderr}"
    );
}

#[tokio::test]
async fn bounded_process_output_is_artifact_backed() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let home_dir = temp.path().join("home");
    let artifact_dir = temp.path().join("edit-artifacts");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::create_dir_all(&home_dir).unwrap();

    let file_path = workspace.join("src/sample.txt");
    let original = "sensitive_line_alpha\nsensitive_line_beta\n";
    fs::write(&file_path, original).unwrap();

    let anchor = anchor_for_line(original, 2);
    let tool_arguments = serde_json::json!({
        "path": "src/sample.txt",
        "edits": [
            {
                "kind": "replace",
                "start": anchor,
                "end": anchor_for_line(original, 2),
                "replacement": "REDACTED\n"
            }
        ]
    });

    let request_count = Arc::new(AtomicUsize::new(0));
    let server = spawn_app(
        Router::new()
            .route("/responses", post(openai_hashline_write_handler))
            .with_state(MockWritePathState {
                request_count: request_count.clone(),
                tool_arguments: tool_arguments.to_string(),
            }),
    )
    .await;

    let openai_base_url = format!("http://{}", server.addr);
    let output = tokio::task::spawn_blocking({
        let workspace = workspace.clone();
        let home_dir = home_dir.clone();
        let artifact_dir = artifact_dir.clone();
        move || {
            Command::new(env!("CARGO_BIN_EXE_kley"))
                .current_dir(&workspace)
                .env("HOME", &home_dir)
                .env("XDG_CONFIG_HOME", home_dir.join("xdg-config"))
                .env("XDG_DATA_HOME", home_dir.join("xdg-data"))
                .env("KLEY_PASSPHRASE", "test-passphrase")
                .env("OPENAI_API_KEY", "test-key")
                .env("OPENAI_BASE_URL", openai_base_url)
                .env(EDIT_ARTIFACT_DIR_ENV, &artifact_dir)
                .arg("chat")
                .arg("--autonomous")
                .arg("--max-turns")
                .arg("1")
                .arg("--tool-approval")
                .arg("auto")
                .arg("--model")
                .arg("test-model")
                .arg("--prompt")
                .arg("Use hashline_edit exactly once to update src/sample.txt.")
                .output()
                .expect("failed to launch kley chat subprocess")
        }
    })
    .await
    .unwrap();

    server.task.abort();

    assert!(output.status.success(), "{}", debug_command_output(&output));
    assert_eq!(request_count.load(Ordering::Relaxed), 2);
    assert_eq!(
        fs::read_to_string(&file_path).unwrap(),
        "sensitive_line_alpha\nREDACTED\n"
    );

    let stdout = output_text(&output.stdout);
    let stderr = output_text(&output.stderr);
    assert!(stdout.contains("write path complete"), "{stdout}");
    assert!(!stdout.contains("artifact_path="), "{stdout}");
    assert!(!stdout.contains("duration_ms"), "{stdout}");
    assert!(!stdout.contains("sensitive_line_alpha"), "{stdout}");
    assert!(!stderr.contains("artifact_path="), "{stderr}");
    assert!(!stderr.contains("duration_ms"), "{stderr}");
    assert!(!stderr.contains("sensitive_line_alpha"), "{stderr}");
    assert!(!stderr.contains("sensitive_line_beta"), "{stderr}");

    let tool_preview = stderr
        .lines()
        .find(|line| line.contains("[tool] -> artifact_id="))
        .unwrap_or_else(|| panic!("missing compact tool preview in stderr:\n{stderr}"));
    assert!(tool_preview.chars().count() <= 100, "{tool_preview}");

    let runs_jsonl = artifact_dir.join("runs.jsonl");
    assert!(runs_jsonl.is_file(), "missing {}", runs_jsonl.display());
    let run_records = read_jsonl(&runs_jsonl);
    assert_eq!(run_records.len(), 1);

    let metrics_jsonl = artifact_dir.join("metrics.jsonl");
    assert!(
        metrics_jsonl.is_file(),
        "missing {}",
        metrics_jsonl.display()
    );
    let metric_records = read_jsonl(&metrics_jsonl);
    assert_eq!(metric_records.len(), 1);
    assert_eq!(
        metric_records[0]["event"],
        serde_json::json!("edit.write_path.completed")
    );
    assert_eq!(
        metric_records[0]["tool_name"],
        serde_json::json!("hashline_edit")
    );
    assert!(metric_records[0]["telemetry_failure_kind"].is_null());

    let artifact_entry = &run_records[0];
    assert_eq!(
        artifact_entry["summary_first_line"],
        serde_json::json!("Applied 1 hashline edit(s) to src/sample.txt")
    );
    assert_eq!(
        artifact_entry["observation"]["tool_name"],
        serde_json::json!("hashline_edit")
    );
    assert_eq!(
        artifact_entry["observation"]["path"],
        serde_json::json!("src/sample.txt")
    );
    assert_eq!(
        artifact_entry["observation"]["applied_count"],
        serde_json::json!(1)
    );
    assert_eq!(
        artifact_entry["observation"]["model_output_bounded"],
        serde_json::json!(true)
    );
    assert!(artifact_entry["observation"]["duration_ms"].is_number());

    let artifact_path = PathBuf::from(
        artifact_entry["observation"]["artifact_path"]
            .as_str()
            .expect("artifact path missing"),
    );
    assert!(
        artifact_path.is_file(),
        "missing {}",
        artifact_path.display()
    );

    let artifact_json: Value =
        serde_json::from_str(&fs::read_to_string(&artifact_path).unwrap()).unwrap();
    assert_eq!(artifact_json["artifact_id"], artifact_entry["artifact_id"]);
    assert_eq!(
        artifact_json["observation"]["artifact_id"],
        artifact_entry["artifact_id"]
    );
    assert_eq!(
        artifact_json["observation"]["artifact_path"],
        serde_json::json!(artifact_path.to_string_lossy().to_string())
    );
}
