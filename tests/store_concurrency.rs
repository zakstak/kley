//! Integration tests for SharedStore + store_run under async concurrency.

mod harness;

use chrono::{Duration, Utc};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::sync::{Barrier, OnceLock};

use kley::runtime::manager::{RuntimeManager, spawn_autonomous_child_task_with_policy};
use kley::store::{
    self, AttemptLifecycleState, NewSession, NewTaskAttemptRecord, NewTaskEdgeRecord,
    NewTaskEventRecord, NewTaskRecord, Session, SharedStore, Store, TaskAttemptRecord,
    TaskEdgeRecord, TaskEventRecord, TaskLifecycleState, TaskRecord,
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

// ── Basic store_run ─────────────────────────────────────────────────────────

#[tokio::test]
async fn store_run_basic_query() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    let sessions = store::store_run(&shared, |s| Session::list(s, 100))
        .await
        .unwrap();
    assert_eq!(sessions.len(), 0);
}

// ── Concurrent store_run calls don't corrupt data ───────────────────────────

#[tokio::test]
async fn concurrent_store_run_sessions() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    let mut handles = Vec::new();

    for i in 0..10 {
        let store = Arc::clone(&shared);
        let handle = tokio::spawn(async move {
            store::store_run(&store, move |s| {
                Session::create(
                    s,
                    NewSession {
                        model: format!("model-{i}"),
                        provider: "openai".into(),
                    },
                )?;
                Ok(())
            })
            .await
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    // All 10 sessions should exist
    let sessions = store::store_run(&shared, |s| Session::list(s, 100))
        .await
        .unwrap();
    assert_eq!(sessions.len(), 10);
}

// ── Sequential store_run operations compose correctly ───────────────────────

#[tokio::test]
async fn sequential_store_run_create_then_read() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    // Create a session
    let session_id = store::store_run(&shared, |s| {
        let session = Session::create(
            s,
            NewSession {
                model: "gpt-4.1".into(),
                provider: "openai".into(),
            },
        )?;
        Ok(session.id)
    })
    .await
    .unwrap();

    // Read it back in a separate store_run
    let fetched = store::store_run(&shared, move |s| Session::get(s, &session_id))
        .await
        .unwrap();

    assert_eq!(fetched.model, "gpt-4.1");
    assert_eq!(fetched.provider, "openai");
}

// ── Multiple concurrent reads are safe ──────────────────────────────────────

#[tokio::test]
async fn concurrent_reads_after_writes() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    // Insert some data first
    store::store_run(&shared, |s| {
        for _ in 0..5 {
            Session::create(
                s,
                NewSession {
                    model: "m".into(),
                    provider: "p".into(),
                },
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();

    // Fire off 20 concurrent reads
    let mut handles = Vec::new();
    for _ in 0..20 {
        let store = Arc::clone(&shared);
        handles.push(tokio::spawn(async move {
            store::store_run(&store, |s| Session::list(s, 100)).await
        }));
    }

    for handle in handles {
        let sessions = handle.await.unwrap().unwrap();
        assert_eq!(sessions.len(), 5, "each read should see all 5 sessions");
    }
}

#[tokio::test]
async fn task_rows_persist_policy_and_recovery_metadata() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    let child_session_id = store::store_run(&shared, |s| {
        let session = Session::create(
            s,
            NewSession {
                model: "gpt-5.3-codex-spark".into(),
                provider: "openai".into(),
            },
        )?;
        Ok(session.id)
    })
    .await
    .unwrap();

    let child_session_id_for_write = child_session_id.clone();
    store::store_run(&shared, move |s| {
        TaskRecord::create(
            s,
            NewTaskRecord {
                task_id: "task-metadata".to_string(),
                parent_task_id: None,
                title: Some("metadata task".to_string()),
                priority: 42,
                policy_snapshot: r#"{"max_depth":3,"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: Some(r#"{"checkpoint":"task-level"}"#.to_string()),
                owner_session_id: None,
            },
        )?;

        TaskAttemptRecord::create(
            s,
            NewTaskAttemptRecord {
                attempt_id: "attempt-metadata-1".to_string(),
                task_id: "task-metadata".to_string(),
                session_id: Some(child_session_id_for_write.clone()),
                status: "running".to_string(),
                recovery_checkpoint: Some(r#"{"checkpoint":"attempt-level"}"#.to_string()),
            },
        )?;

        TaskEventRecord::append(
            s,
            NewTaskEventRecord {
                task_id: "task-metadata".to_string(),
                attempt_id: "attempt-metadata-1".to_string(),
                session_id: Some(child_session_id_for_write.clone()),
                event_type: "attempt.started".to_string(),
                payload: r#"{"seq":1}"#.to_string(),
            },
        )?;
        TaskEventRecord::append(
            s,
            NewTaskEventRecord {
                task_id: "task-metadata".to_string(),
                attempt_id: "attempt-metadata-1".to_string(),
                session_id: Some(child_session_id_for_write.clone()),
                event_type: "attempt.progress".to_string(),
                payload: r#"{"seq":2}"#.to_string(),
            },
        )?;

        Ok(())
    })
    .await
    .unwrap();

    let task = store::store_run(&shared, |s| TaskRecord::get(s, "task-metadata"))
        .await
        .unwrap();
    assert_eq!(task.priority, 42);
    assert_eq!(
        task.policy_snapshot,
        r#"{"max_depth":3,"mode":"auto"}"#.to_string()
    );
    assert_eq!(
        task.parent_close_policy,
        "request_cancel_descendants".to_string()
    );
    assert_eq!(
        task.recovery_checkpoint,
        Some(r#"{"checkpoint":"task-level"}"#.to_string())
    );

    let attempts = store::store_run(&shared, |s| {
        TaskAttemptRecord::list_for_task(s, "task-metadata")
    })
    .await
    .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].attempt_id, "attempt-metadata-1");
    assert_eq!(
        attempts[0].session_id.as_deref(),
        Some(child_session_id.as_str())
    );
    assert_eq!(
        attempts[0].recovery_checkpoint,
        Some(r#"{"checkpoint":"attempt-level"}"#.to_string())
    );

    let events = store::store_run(&shared, |s| {
        TaskEventRecord::list_for_task(s, "task-metadata", 0)
    })
    .await
    .unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0].sequence < events[1].sequence);
    assert_eq!(events[0].attempt_id, "attempt-metadata-1");
    assert_eq!(events[1].attempt_id, "attempt-metadata-1");
    assert_eq!(
        events[0].session_id.as_deref(),
        Some(child_session_id.as_str())
    );
}

#[tokio::test]
async fn task_claim_is_single_winner_under_race() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    store::store_run(&shared, |s| {
        TaskRecord::create(
            s,
            NewTaskRecord {
                task_id: "task-claim-race".to_string(),
                parent_task_id: None,
                title: Some("task-claim-race".to_string()),
                priority: 1,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )?;

        TaskAttemptRecord::create(
            s,
            NewTaskAttemptRecord {
                attempt_id: "attempt-claim-race".to_string(),
                task_id: "task-claim-race".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Ready.to_string(),
                recovery_checkpoint: Some(r#"{"checkpoint":"seed"}"#.to_string()),
            },
        )?;

        Ok(())
    })
    .await
    .unwrap();

    let claim_now = Utc::now();
    let mut handles = Vec::new();
    for index in 0..16 {
        let store = Arc::clone(&shared);
        let owner = format!("worker-{index}");
        let claim_time = claim_now;
        handles.push(tokio::spawn(async move {
            store::store_run(&store, move |s| {
                TaskAttemptRecord::claim_runnable_with_lease(
                    s,
                    "attempt-claim-race",
                    &owner,
                    60,
                    claim_time,
                )
            })
            .await
            .unwrap()
        }));
    }

    let mut winners = 0;
    for handle in handles {
        if handle.await.unwrap() {
            winners += 1;
        }
    }
    assert_eq!(winners, 1, "exactly one worker should claim the attempt");

    let attempt = store::store_run(&shared, |s| TaskAttemptRecord::get(s, "attempt-claim-race"))
        .await
        .unwrap();
    assert_eq!(attempt.status, AttemptLifecycleState::Running.to_string());

    let checkpoint_json: serde_json::Value =
        serde_json::from_str(attempt.recovery_checkpoint.as_deref().unwrap()).unwrap();
    let lease = checkpoint_json.get("scheduler_lease").unwrap();
    assert!(
        lease
            .get("owner_id")
            .and_then(serde_json::Value::as_str)
            .is_some()
    );
    assert_eq!(
        lease
            .get("recoverable")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );

    let events = store::store_run(&shared, |s| {
        TaskEventRecord::list_for_task(s, "task-claim-race", 0)
    })
    .await
    .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "attempt.lease.claimed")
            .count(),
        1
    );
}

#[tokio::test]
async fn expired_task_lease_is_recoverable_without_double_run() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    store::store_run(&shared, |s| {
        TaskRecord::create(
            s,
            NewTaskRecord {
                task_id: "task-lease-expiry".to_string(),
                parent_task_id: None,
                title: Some("task-lease-expiry".to_string()),
                priority: 1,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )?;

        TaskAttemptRecord::create(
            s,
            NewTaskAttemptRecord {
                attempt_id: "attempt-lease-expiry".to_string(),
                task_id: "task-lease-expiry".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Ready.to_string(),
                recovery_checkpoint: None,
            },
        )?;
        Ok(())
    })
    .await
    .unwrap();

    let base_now = Utc::now();
    let first_claim_time = base_now;
    let first_claim = store::store_run(&shared, move |s| {
        TaskAttemptRecord::claim_runnable_with_lease(
            s,
            "attempt-lease-expiry",
            "worker-initial",
            30,
            first_claim_time,
        )
    })
    .await
    .unwrap();
    assert!(first_claim);

    let still_running_time = base_now + Duration::seconds(10);
    let still_running = store::store_run(&shared, move |s| {
        TaskAttemptRecord::claim_runnable_with_lease(
            s,
            "attempt-lease-expiry",
            "worker-contender",
            30,
            still_running_time,
        )
    })
    .await
    .unwrap();
    assert!(!still_running);

    let expired_mark_time = base_now + Duration::seconds(31);
    let expired_marked = store::store_run(&shared, move |s| {
        TaskAttemptRecord::mark_expired_lease_interrupted_recoverable(
            s,
            "attempt-lease-expiry",
            expired_mark_time,
        )
    })
    .await
    .unwrap();
    assert!(expired_marked);

    let marked_again_time = base_now + Duration::seconds(32);
    let marked_again = store::store_run(&shared, move |s| {
        TaskAttemptRecord::mark_expired_lease_interrupted_recoverable(
            s,
            "attempt-lease-expiry",
            marked_again_time,
        )
    })
    .await
    .unwrap();
    assert!(!marked_again);

    let attempt_after_expiry = store::store_run(&shared, |s| {
        TaskAttemptRecord::get(s, "attempt-lease-expiry")
    })
    .await
    .unwrap();
    assert_eq!(
        attempt_after_expiry.status,
        AttemptLifecycleState::Interrupted.to_string()
    );
    let checkpoint_json: serde_json::Value =
        serde_json::from_str(attempt_after_expiry.recovery_checkpoint.as_deref().unwrap()).unwrap();
    let lease = checkpoint_json.get("scheduler_lease").unwrap();
    assert_eq!(
        lease
            .get("recoverable")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(
        lease
            .get("interrupted_at")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "expired leases should be marked with interruption metadata"
    );

    let mut blocked_claims = Vec::new();
    for index in 0..8 {
        let store = Arc::clone(&shared);
        let owner = format!("worker-blocked-{index}");
        let claim_time = base_now + Duration::seconds(40);
        blocked_claims.push(tokio::spawn(async move {
            store::store_run(&store, move |s| {
                TaskAttemptRecord::claim_runnable_with_lease(
                    s,
                    "attempt-lease-expiry",
                    &owner,
                    30,
                    claim_time,
                )
            })
            .await
            .unwrap()
        }));
    }
    let mut blocked_winners = 0;
    for handle in blocked_claims {
        if handle.await.unwrap() {
            blocked_winners += 1;
        }
    }
    assert_eq!(blocked_winners, 0, "interrupted attempts are not runnable");

    store::store_run(&shared, |s| {
        TaskAttemptRecord::transition_state(
            s,
            "attempt-lease-expiry",
            AttemptLifecycleState::Retryable,
        )?;
        TaskAttemptRecord::transition_state(
            s,
            "attempt-lease-expiry",
            AttemptLifecycleState::Ready,
        )?;
        Ok(())
    })
    .await
    .unwrap();

    let mut recovery_claims = Vec::new();
    for index in 0..12 {
        let store = Arc::clone(&shared);
        let owner = format!("worker-recovery-{index}");
        let claim_time = base_now + Duration::seconds(41);
        recovery_claims.push(tokio::spawn(async move {
            store::store_run(&store, move |s| {
                TaskAttemptRecord::claim_runnable_with_lease(
                    s,
                    "attempt-lease-expiry",
                    &owner,
                    30,
                    claim_time,
                )
            })
            .await
            .unwrap()
        }));
    }
    let mut recovery_winners = 0;
    for handle in recovery_claims {
        if handle.await.unwrap() {
            recovery_winners += 1;
        }
    }
    assert_eq!(
        recovery_winners, 1,
        "recovered runnable attempt must still have one claim winner"
    );

    let events = store::store_run(&shared, |s| {
        TaskEventRecord::list_for_task(s, "task-lease-expiry", 0)
    })
    .await
    .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "attempt.lease.expired")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "attempt.lease.claimed")
            .count(),
        2,
        "initial claim + post-recovery claim should both be durable"
    );
}

#[tokio::test]
async fn autonomous_spawn_max_concurrency_is_atomic_across_store_connections() {
    let _lock = env_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let home_dir = temp.path().join("home");
    std::fs::create_dir_all(home_dir.join("xdg-config")).unwrap();
    std::fs::create_dir_all(home_dir.join("xdg-data")).unwrap();
    let _home = EnvVarGuard::set_path("HOME", &home_dir);
    let _config = EnvVarGuard::set_path("XDG_CONFIG_HOME", &home_dir.join("xdg-config"));
    let _data = EnvVarGuard::set_path("XDG_DATA_HOME", &home_dir.join("xdg-data"));

    {
        let store = Store::open().unwrap();
        TaskRecord::create(
            &store,
            NewTaskRecord {
                task_id: "atomic-parent".to_string(),
                parent_task_id: None,
                title: Some("atomic-parent".to_string()),
                priority: 1,
                policy_snapshot: serde_json::json!({
                    "allow_autonomous_spawn": true,
                    "current_depth": 0,
                    "max_depth": 3,
                    "max_concurrency": 1,
                    "budget": 10,
                    "allowed_providers": ["openai"],
                    "allowed_models": ["gpt-5.3-codex-spark"],
                    "approved_tools": ["read_file"],
                    "tool_approval_mode": "ask",
                    "parent_close_policy": "request_cancel_descendants"
                })
                .to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )
        .unwrap();
    }

    let workers = 8usize;
    let barrier = Arc::new(Barrier::new(workers));
    let mut handles = Vec::new();
    for index in 0..workers {
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let store = Store::open().unwrap();
            spawn_autonomous_child_task_with_policy(
                &store,
                "atomic-parent",
                &format!("atomic-child-{index}"),
                Some(format!("atomic-child-{index}")),
                10 + i64::try_from(index).unwrap(),
                None,
            )
            .is_ok()
        }));
    }

    let mut success = 0usize;
    for handle in handles {
        if handle.join().unwrap() {
            success += 1;
        }
    }
    assert_eq!(
        success, 1,
        "atomic max_concurrency admission should allow exactly one child insert"
    );

    let store = Store::open().unwrap();
    let children = TaskRecord::list(&store)
        .unwrap()
        .into_iter()
        .filter(|task| task.parent_task_id.as_deref() == Some("atomic-parent"))
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1);
}

#[tokio::test]
async fn cancel_retry_resume_and_reprioritize_are_serialized() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let manager = Arc::new(RuntimeManager::new());

    store::store_run(&shared, |s| {
        TaskRecord::create(
            s,
            NewTaskRecord {
                task_id: "cancel-root".to_string(),
                parent_task_id: None,
                title: Some("cancel-root".to_string()),
                priority: 10,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )?;
        TaskRecord::create(
            s,
            NewTaskRecord {
                task_id: "cancel-child".to_string(),
                parent_task_id: None,
                title: Some("cancel-child".to_string()),
                priority: 9,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )?;
        TaskRecord::create(
            s,
            NewTaskRecord {
                task_id: "cancel-grandchild".to_string(),
                parent_task_id: None,
                title: Some("cancel-grandchild".to_string()),
                priority: 8,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )?;
        TaskEdgeRecord::create(
            s,
            NewTaskEdgeRecord {
                task_id: "cancel-child".to_string(),
                depends_on_task_id: "cancel-root".to_string(),
            },
        )?;
        TaskEdgeRecord::create(
            s,
            NewTaskEdgeRecord {
                task_id: "cancel-grandchild".to_string(),
                depends_on_task_id: "cancel-child".to_string(),
            },
        )?;

        let cancel_root_attempt = TaskAttemptRecord::create(
            s,
            NewTaskAttemptRecord {
                attempt_id: "attempt-cancel-root".to_string(),
                task_id: "cancel-root".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Running.to_string(),
                recovery_checkpoint: None,
            },
        )?;
        TaskRecord::transition_state(
            s,
            "cancel-root",
            &cancel_root_attempt.attempt_id,
            TaskLifecycleState::Running,
        )?;

        let cancel_child_attempt = TaskAttemptRecord::create(
            s,
            NewTaskAttemptRecord {
                attempt_id: "attempt-cancel-child".to_string(),
                task_id: "cancel-child".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Ready.to_string(),
                recovery_checkpoint: None,
            },
        )?;
        TaskRecord::transition_state(
            s,
            "cancel-child",
            &cancel_child_attempt.attempt_id,
            TaskLifecycleState::Ready,
        )?;

        let cancel_grandchild_attempt = TaskAttemptRecord::create(
            s,
            NewTaskAttemptRecord {
                attempt_id: "attempt-cancel-grandchild".to_string(),
                task_id: "cancel-grandchild".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Queued.to_string(),
                recovery_checkpoint: None,
            },
        )?;
        TaskRecord::transition_state(
            s,
            "cancel-grandchild",
            &cancel_grandchild_attempt.attempt_id,
            TaskLifecycleState::Ready,
        )?;

        TaskRecord::create(
            s,
            NewTaskRecord {
                task_id: "retry-task".to_string(),
                parent_task_id: None,
                title: Some("retry-task".to_string()),
                priority: 4,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )?;
        let retry_attempt = TaskAttemptRecord::create(
            s,
            NewTaskAttemptRecord {
                attempt_id: "attempt-retry-0".to_string(),
                task_id: "retry-task".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Failed.to_string(),
                recovery_checkpoint: Some(r#"{"retry":"seed"}"#.to_string()),
            },
        )?;
        TaskRecord::transition_state(
            s,
            "retry-task",
            &retry_attempt.attempt_id,
            TaskLifecycleState::Running,
        )?;
        TaskRecord::transition_state(
            s,
            "retry-task",
            &retry_attempt.attempt_id,
            TaskLifecycleState::Failed,
        )?;

        TaskRecord::create(
            s,
            NewTaskRecord {
                task_id: "resume-task".to_string(),
                parent_task_id: None,
                title: Some("resume-task".to_string()),
                priority: 3,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )?;
        let resume_attempt = TaskAttemptRecord::create(
            s,
            NewTaskAttemptRecord {
                attempt_id: "attempt-resume-0".to_string(),
                task_id: "resume-task".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Interrupted.to_string(),
                recovery_checkpoint: Some(r#"{"resume":"checkpoint"}"#.to_string()),
            },
        )?;
        TaskRecord::transition_state(
            s,
            "resume-task",
            &resume_attempt.attempt_id,
            TaskLifecycleState::Running,
        )?;
        TaskRecord::transition_state(
            s,
            "resume-task",
            &resume_attempt.attempt_id,
            TaskLifecycleState::Interrupted,
        )?;

        TaskRecord::create(
            s,
            NewTaskRecord {
                task_id: "reprio-task".to_string(),
                parent_task_id: None,
                title: Some("reprio-task".to_string()),
                priority: 1,
                policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                parent_close_policy: "request_cancel_descendants".to_string(),
                recovery_checkpoint: None,
                owner_session_id: None,
            },
        )?;
        TaskAttemptRecord::create(
            s,
            NewTaskAttemptRecord {
                attempt_id: "attempt-reprio-0".to_string(),
                task_id: "reprio-task".to_string(),
                session_id: None,
                status: AttemptLifecycleState::Queued.to_string(),
                recovery_checkpoint: None,
            },
        )?;

        Ok(())
    })
    .await
    .unwrap();

    let mut cancel_handles = Vec::new();
    for _ in 0..6 {
        let shared_clone = Arc::clone(&shared);
        let manager_clone = Arc::clone(&manager);
        cancel_handles.push(tokio::spawn(async move {
            manager_clone.cancel_task_graph(&shared_clone, "cancel-root")
        }));
    }
    for handle in cancel_handles {
        handle.await.unwrap().unwrap();
    }

    let mut retry_handles = Vec::new();
    for _ in 0..8 {
        let shared_clone = Arc::clone(&shared);
        let manager_clone = Arc::clone(&manager);
        retry_handles.push(tokio::spawn(async move {
            manager_clone.retry_task(&shared_clone, "retry-task")
        }));
    }
    let mut retry_success = 0usize;
    for handle in retry_handles {
        if handle.await.unwrap().is_ok() {
            retry_success += 1;
        }
    }
    assert_eq!(
        retry_success, 1,
        "retry should produce exactly one fresh attempt"
    );

    let mut resume_handles = Vec::new();
    for _ in 0..8 {
        let shared_clone = Arc::clone(&shared);
        let manager_clone = Arc::clone(&manager);
        resume_handles.push(tokio::spawn(async move {
            manager_clone.resume_task(&shared_clone, "resume-task")
        }));
    }
    let mut resume_success = 0usize;
    for handle in resume_handles {
        if handle.await.unwrap().is_ok() {
            resume_success += 1;
        }
    }
    assert_eq!(
        resume_success, 1,
        "resume should produce exactly one fresh attempt"
    );

    let requested_priorities = [11_i64, 13_i64, 17_i64, 19_i64, 23_i64];
    let mut reprio_handles = Vec::new();
    for priority in requested_priorities.iter().copied() {
        let shared_clone = Arc::clone(&shared);
        let manager_clone = Arc::clone(&manager);
        reprio_handles.push(tokio::spawn(async move {
            manager_clone.reprioritize_task(&shared_clone, "reprio-task", priority)
        }));
    }
    for handle in reprio_handles {
        handle.await.unwrap().unwrap();
    }

    let (cancel_root_state, cancel_child_state, cancel_grandchild_state) =
        store::store_run(&shared, |s| {
            Ok((
                TaskRecord::current_state(s, "cancel-root")?,
                TaskRecord::current_state(s, "cancel-child")?,
                TaskRecord::current_state(s, "cancel-grandchild")?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(cancel_root_state, TaskLifecycleState::CancelRequested);
    assert_eq!(cancel_child_state, TaskLifecycleState::Cancelled);
    assert_eq!(cancel_grandchild_state, TaskLifecycleState::Cancelled);

    let retry_attempts = store::store_run(&shared, |s| {
        TaskAttemptRecord::list_for_task(s, "retry-task")
    })
    .await
    .unwrap();
    assert_eq!(retry_attempts.len(), 2);
    assert_eq!(retry_attempts[0].attempt_id, "attempt-retry-0");
    assert_eq!(
        retry_attempts[0].status,
        AttemptLifecycleState::Retryable.to_string()
    );
    assert_eq!(retry_attempts[1].task_id, "retry-task");
    assert_eq!(
        retry_attempts[1].status,
        AttemptLifecycleState::Queued.to_string()
    );
    assert_ne!(
        retry_attempts[0].attempt_id, retry_attempts[1].attempt_id,
        "serialized retry should create exactly one distinct replacement attempt"
    );

    let resume_attempts = store::store_run(&shared, |s| {
        TaskAttemptRecord::list_for_task(s, "resume-task")
    })
    .await
    .unwrap();
    assert_eq!(resume_attempts.len(), 2);
    assert_eq!(resume_attempts[0].attempt_id, "attempt-resume-0");
    assert_eq!(
        resume_attempts[0].status,
        AttemptLifecycleState::Retryable.to_string()
    );
    assert_eq!(resume_attempts[1].task_id, "resume-task");
    assert_eq!(
        resume_attempts[1].status,
        AttemptLifecycleState::Queued.to_string()
    );
    assert_ne!(
        resume_attempts[0].attempt_id, resume_attempts[1].attempt_id,
        "serialized resume should create exactly one distinct replacement attempt"
    );
    assert_eq!(
        resume_attempts[1].recovery_checkpoint,
        Some(r#"{"resume":"checkpoint"}"#.to_string())
    );

    let (retry_state, resume_state) = store::store_run(&shared, |s| {
        Ok((
            TaskRecord::current_state(s, "retry-task")?,
            TaskRecord::current_state(s, "resume-task")?,
        ))
    })
    .await
    .unwrap();
    assert_eq!(retry_state, TaskLifecycleState::Queued);
    assert_eq!(resume_state, TaskLifecycleState::Queued);

    let reprio_task = store::store_run(&shared, |s| TaskRecord::get(s, "reprio-task"))
        .await
        .unwrap();
    assert!(requested_priorities.contains(&reprio_task.priority));

    manager.cancel_task_graph(&shared, "cancel-root").unwrap();
    let (root_state_after_repeat, child_state_after_repeat, grandchild_state_after_repeat) =
        store::store_run(&shared, |s| {
            Ok((
                TaskRecord::current_state(s, "cancel-root")?,
                TaskRecord::current_state(s, "cancel-child")?,
                TaskRecord::current_state(s, "cancel-grandchild")?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(root_state_after_repeat, TaskLifecycleState::CancelRequested);
    assert_eq!(child_state_after_repeat, TaskLifecycleState::Cancelled);
    assert_eq!(grandchild_state_after_repeat, TaskLifecycleState::Cancelled);
}
