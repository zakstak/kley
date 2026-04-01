use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Mutex, OnceLock};

use chrono::{Duration, Utc};
use kley::auth::ResolvedAuth;
use kley::compact::CompactConfig;
use kley::events::event_channel;
use kley::runtime::{RuntimeHooks, SessionRuntime};
use kley::store::{
    NewSession, NewTaskAttemptRecord, NewTaskEdgeRecord, NewTaskEventRecord, NewTaskRecord,
    Session, SessionStatus, Store, TaskAttemptRecord, TaskLifecycleState, TaskRecord, Turn,
};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => unsafe { std::env::set_var(self.key, previous) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

struct SeededGraph {
    child_session_id: String,
}

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
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

fn run_cli(home_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kley"))
        .current_dir(manifest_dir())
        .env("HOME", home_dir)
        .env("XDG_CONFIG_HOME", home_dir.join("xdg-config"))
        .env("XDG_DATA_HOME", home_dir.join("xdg-data"))
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("failed to launch kley: {error}"))
}

fn with_store_home<T>(home_dir: &Path, f: impl FnOnce(&Store) -> T) -> T {
    let _guard = env_lock().lock().unwrap();
    std::fs::create_dir_all(home_dir.join("xdg-config")).unwrap();
    std::fs::create_dir_all(home_dir.join("xdg-data")).unwrap();
    let _home = EnvVarGuard::set_path("HOME", home_dir);
    let _config = EnvVarGuard::set_path("XDG_CONFIG_HOME", &home_dir.join("xdg-config"));
    let _data = EnvVarGuard::set_path("XDG_DATA_HOME", &home_dir.join("xdg-data"));
    let store = Store::open().unwrap();
    f(&store)
}

fn seed_task_graph(home_dir: &Path) -> SeededGraph {
    with_store_home(home_dir, |store| {
        let root_session = Session::create(
            store,
            NewSession {
                model: "test-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap();
        let child_session = Session::create(
            store,
            NewSession {
                model: "child-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap();
        let unrelated_session = Session::create(
            store,
            NewSession {
                model: "other-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap();

        let lease_expires_at = (Utc::now() + Duration::minutes(10)).to_rfc3339();
        let leased_at = Utc::now().to_rfc3339();

        TaskRecord::create(
            store,
            NewTaskRecord {
                task_id: "task-root".to_string(),
                parent_task_id: None,
                title: Some("Root task".to_string()),
                priority: 90,
                policy_snapshot: serde_json::json!({
                    "max_depth": 3,
                    "max_concurrency": 2,
                    "budget": 5,
                })
                .to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: Some(
                    serde_json::json!({
                        "scheduler": {"owner_id": "scheduler-main"}
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();
        TaskRecord::create(
            store,
            NewTaskRecord {
                task_id: "task-dependency".to_string(),
                parent_task_id: None,
                title: Some("Dependency task".to_string()),
                priority: 20,
                policy_snapshot: serde_json::json!({"budget": 1}).to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::create(
            store,
            NewTaskRecord {
                task_id: "task-child".to_string(),
                parent_task_id: Some("task-root".to_string()),
                title: Some("Child task".to_string()),
                priority: 50,
                policy_snapshot: serde_json::json!({"budget": 2}).to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::create(
            store,
            NewTaskRecord {
                task_id: "task-unrelated".to_string(),
                parent_task_id: None,
                title: Some("Unrelated task".to_string()),
                priority: 5,
                policy_snapshot: serde_json::json!({"budget": 99}).to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();

        kley::store::TaskEdgeRecord::create(
            store,
            NewTaskEdgeRecord {
                task_id: "task-root".to_string(),
                depends_on_task_id: "task-dependency".to_string(),
            },
        )
        .unwrap();
        kley::store::TaskEdgeRecord::create(
            store,
            NewTaskEdgeRecord {
                task_id: "task-child".to_string(),
                depends_on_task_id: "task-root".to_string(),
            },
        )
        .unwrap();

        TaskAttemptRecord::create(
            store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-root-1".to_string(),
                task_id: "task-root".to_string(),
                session_id: Some(child_session.id.clone()),
                status: "running".to_string(),
                recovery_checkpoint: Some(
                    serde_json::json!({
                        "child_bootstrap": {
                            "status": "linked",
                            "child_session_id": child_session.id,
                        },
                        "scheduler_lease": {
                            "owner_id": "scheduler-main",
                            "leased_at": leased_at,
                            "lease_expires_at": lease_expires_at,
                            "recoverable": true,
                        }
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();
        TaskAttemptRecord::create(
            store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-dependency-1".to_string(),
                task_id: "task-dependency".to_string(),
                session_id: None,
                status: "completed".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskAttemptRecord::create(
            store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-child-1".to_string(),
                task_id: "task-child".to_string(),
                session_id: None,
                status: "queued".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskAttemptRecord::create(
            store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-unrelated-1".to_string(),
                task_id: "task-unrelated".to_string(),
                session_id: Some(unrelated_session.id.clone()),
                status: "running".to_string(),
                recovery_checkpoint: Some(
                    serde_json::json!({
                        "scheduler_lease": {
                            "owner_id": "scheduler-other",
                            "leased_at": Utc::now().to_rfc3339(),
                            "lease_expires_at": (Utc::now() + Duration::minutes(5)).to_rfc3339(),
                            "recoverable": false,
                        }
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();

        TaskRecord::transition_state(
            store,
            "task-root",
            "attempt-root-1",
            TaskLifecycleState::Running,
        )
        .unwrap();
        kley::store::TaskEventRecord::append(
            store,
            NewTaskEventRecord {
                task_id: "task-root".to_string(),
                attempt_id: "attempt-root-1".to_string(),
                session_id: Some(child_session.id.clone()),
                event_type: "attempt.child_session.linked".to_string(),
                payload: serde_json::json!({
                    "session_id": child_session.id.clone(),
                })
                .to_string(),
            },
        )
        .unwrap();
        kley::store::TaskEventRecord::append(
            store,
            NewTaskEventRecord {
                task_id: "task-root".to_string(),
                attempt_id: "attempt-root-1".to_string(),
                session_id: Some(child_session.id.clone()),
                event_type: "attempt.lease.claimed".to_string(),
                payload: serde_json::json!({
                    "owner_id": "scheduler-main",
                    "lease_expires_at": (Utc::now() + Duration::minutes(10)).to_rfc3339(),
                })
                .to_string(),
            },
        )
        .unwrap();

        TaskRecord::transition_state(
            store,
            "task-dependency",
            "attempt-dependency-1",
            TaskLifecycleState::Ready,
        )
        .unwrap();
        TaskRecord::transition_state(
            store,
            "task-dependency",
            "attempt-dependency-1",
            TaskLifecycleState::Running,
        )
        .unwrap();
        TaskRecord::transition_state(
            store,
            "task-dependency",
            "attempt-dependency-1",
            TaskLifecycleState::Completed,
        )
        .unwrap();
        TaskRecord::transition_state(
            store,
            "task-unrelated",
            "attempt-unrelated-1",
            TaskLifecycleState::Running,
        )
        .unwrap();

        let _ = root_session;
        SeededGraph {
            child_session_id: child_session.id.clone(),
        }
    })
}

fn seed_control_tasks(home_dir: &Path) {
    with_store_home(home_dir, |store| {
        TaskRecord::create(
            store,
            NewTaskRecord {
                task_id: "task-running".to_string(),
                parent_task_id: None,
                title: Some("task-running".to_string()),
                priority: 10,
                policy_snapshot: serde_json::json!({"mode": "auto"}).to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskAttemptRecord::create(
            store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-running-1".to_string(),
                task_id: "task-running".to_string(),
                session_id: None,
                status: "running".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            store,
            "task-running",
            "attempt-running-1",
            TaskLifecycleState::Running,
        )
        .unwrap();

        TaskRecord::create(
            store,
            NewTaskRecord {
                task_id: "task-completed".to_string(),
                parent_task_id: None,
                title: Some("task-completed".to_string()),
                priority: 20,
                policy_snapshot: serde_json::json!({"mode": "auto"}).to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskAttemptRecord::create(
            store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-completed-1".to_string(),
                task_id: "task-completed".to_string(),
                session_id: None,
                status: "completed".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            store,
            "task-completed",
            "attempt-completed-1",
            TaskLifecycleState::Running,
        )
        .unwrap();
        TaskRecord::transition_state(
            store,
            "task-completed",
            "attempt-completed-1",
            TaskLifecycleState::Completed,
        )
        .unwrap();

        TaskRecord::create(
            store,
            NewTaskRecord {
                task_id: "task-ready".to_string(),
                parent_task_id: None,
                title: Some("task-ready".to_string()),
                priority: 1,
                policy_snapshot: serde_json::json!({"mode": "auto"}).to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskAttemptRecord::create(
            store,
            NewTaskAttemptRecord {
                attempt_id: "attempt-ready-1".to_string(),
                task_id: "task-ready".to_string(),
                session_id: None,
                status: "ready".to_string(),
                recovery_checkpoint: None,
            },
        )
        .unwrap();
        TaskRecord::transition_state(
            store,
            "task-ready",
            "attempt-ready-1",
            TaskLifecycleState::Ready,
        )
        .unwrap();
    });
}

fn test_auth() -> ResolvedAuth {
    ResolvedAuth {
        provider: "test".to_string(),
        api_key: "test-key".to_string(),
        base_url: "http://unused".to_string(),
        account_id: None,
    }
}

#[tokio::test]
async fn existing_interactive_flow_still_persists_turns() {
    let store = Store::open_memory().unwrap();
    let (emitter, _receiver) = event_channel();

    let mut runtime = SessionRuntime::new(
        &store,
        test_auth(),
        Some("test-model"),
        None,
        emitter,
        CompactConfig::default(),
        kley::tools::default_registry(std::env::current_dir().unwrap()),
        "system".to_string(),
        RuntimeHooks::default(),
    )
    .unwrap();

    let prompts = vec!["one".to_string(), "two".to_string()];
    kley::agent::run_cli_adapter_with_runtime_for_test(&mut runtime, &prompts)
        .await
        .unwrap();

    let session = Session::get(&store, runtime.session_id()).unwrap();
    assert_eq!(session.status, SessionStatus::Completed);

    let turns = Turn::list_for_session(&store, &session.id).unwrap();
    assert_eq!(turns.len(), 4);
    assert_eq!(turns[0].role, "user");
    assert_eq!(turns[1].role, "assistant");
    assert_eq!(turns[2].role, "user");
    assert_eq!(turns[3].role, "assistant");
}

#[test]
fn cli_lists_and_inspects_task_graph_state() {
    let temp = tempfile::tempdir().unwrap();
    let home_dir = temp.path().join("home");
    std::fs::create_dir_all(&home_dir).unwrap();
    let seeded = seed_task_graph(&home_dir);

    let list_output = run_cli(&home_dir, &["task", "list"]);
    assert!(
        list_output.status.success(),
        "{}",
        debug_command_output(&list_output)
    );
    let list_stdout = output_text(&list_output.stdout);
    assert!(list_stdout.contains("task_id=task-root"), "{list_stdout}");
    assert!(
        list_stdout.contains("latest_attempt_id=attempt-root-1"),
        "{list_stdout}"
    );
    assert!(
        list_stdout.contains(&format!("child_session_id={}", seeded.child_session_id)),
        "{list_stdout}"
    );
    assert!(
        list_stdout.contains("lease_owner=scheduler-main"),
        "{list_stdout}"
    );
    assert!(
        list_stdout.contains("task_id=task-unrelated"),
        "{list_stdout}"
    );

    let inspect_output = run_cli(&home_dir, &["task", "inspect", "task-root"]);
    assert!(
        inspect_output.status.success(),
        "{}",
        debug_command_output(&inspect_output)
    );
    let inspect_stdout = output_text(&inspect_output.stdout);
    assert!(
        inspect_stdout.contains("task_id=task-root"),
        "{inspect_stdout}"
    );
    assert!(
        inspect_stdout.contains("cursor_latest_sequence="),
        "{inspect_stdout}"
    );
    assert!(inspect_stdout.contains("graph_nodes:"), "{inspect_stdout}");
    assert!(
        inspect_stdout.contains("task_id=task-root depends_on_task_id=task-dependency"),
        "{inspect_stdout}"
    );
    assert!(
        inspect_stdout.contains("task_id=task-child depends_on_task_id=task-root"),
        "{inspect_stdout}"
    );
    assert!(inspect_stdout.contains("attempts:"), "{inspect_stdout}");
    assert!(
        inspect_stdout.contains("attempt_id=attempt-root-1"),
        "{inspect_stdout}"
    );
    assert!(
        inspect_stdout.contains(&format!("child_session_id={}", seeded.child_session_id)),
        "{inspect_stdout}"
    );
    assert!(inspect_stdout.contains("events:"), "{inspect_stdout}");
    assert!(
        inspect_stdout.contains("event_type=attempt.lease.claimed"),
        "{inspect_stdout}"
    );
    assert!(
        inspect_stdout.contains("lease_owner=scheduler-main"),
        "{inspect_stdout}"
    );
}

#[test]
fn cli_control_commands_require_valid_task_state() {
    let temp = tempfile::tempdir().unwrap();
    let home_dir = temp.path().join("home");
    std::fs::create_dir_all(&home_dir).unwrap();
    seed_control_tasks(&home_dir);

    let retry_output = run_cli(&home_dir, &["task", "control", "retry", "task-running"]);
    assert!(
        !retry_output.status.success(),
        "{}",
        debug_command_output(&retry_output)
    );
    let retry_stderr = output_text(&retry_output.stderr);
    assert!(retry_stderr.contains("error:"), "{retry_stderr}");
    assert!(
        retry_stderr.contains("task task-running is running"),
        "{retry_stderr}"
    );

    let cancel_output = run_cli(&home_dir, &["task", "control", "cancel", "task-completed"]);
    assert!(
        !cancel_output.status.success(),
        "{}",
        debug_command_output(&cancel_output)
    );
    let cancel_stderr = output_text(&cancel_output.stderr);
    assert!(cancel_stderr.contains("error:"), "{cancel_stderr}");
    assert!(
        cancel_stderr.contains(
            "cancel is only allowed for nonterminal tasks: task task-completed is completed"
        ),
        "{cancel_stderr}"
    );

    let reprioritize_output = run_cli(
        &home_dir,
        &["task", "control", "reprioritize", "task-ready", "33"],
    );
    assert!(
        reprioritize_output.status.success(),
        "{}",
        debug_command_output(&reprioritize_output)
    );
    let reprioritize_stdout = output_text(&reprioritize_output.stdout);
    assert!(
        reprioritize_stdout
            .contains("action=reprioritize task_id=task-ready task_state=ready priority=33"),
        "{reprioritize_stdout}"
    );
}
