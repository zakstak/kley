//! Session persistence.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, HashMap};
use std::fmt;

/// Lightweight error for row-mapping failures (implements `std::error::Error`).
#[derive(Debug)]
struct RowParseError(String);

impl fmt::Display for RowParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RowParseError {}

use super::Store;

/// Session status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    Paused,
    Completed,
    Failed,
    Aborted,
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SessionStatus::Active => "active",
            SessionStatus::Paused => "paused",
            SessionStatus::Completed => "completed",
            SessionStatus::Failed => "failed",
            SessionStatus::Aborted => "aborted",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for SessionStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "active" => Ok(SessionStatus::Active),
            "paused" => Ok(SessionStatus::Paused),
            "completed" => Ok(SessionStatus::Completed),
            "failed" => Ok(SessionStatus::Failed),
            "aborted" => Ok(SessionStatus::Aborted),
            other => anyhow::bail!("unknown session status: {other:?}"),
        }
    }
}

/// A persisted session.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub title: Option<String>,
    pub status: SessionStatus,
    pub model: String,
    pub provider: String,
    /// Approval / sandbox policy tag (e.g. "auto-approve", "ask").
    pub policy: Option<String>,
    /// Freeform JSON blob for model/provider settings needed to resume.
    /// Intentionally unstructured to avoid lock-in.
    pub settings: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Data needed to create a new session.
pub struct NewSession {
    pub model: String,
    pub provider: String,
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub title: Option<String>,
    pub priority: i64,
    pub policy_snapshot: String,
    pub parent_close_policy: String,
    pub recovery_checkpoint: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct NewTaskRecord {
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub title: Option<String>,
    pub priority: i64,
    pub policy_snapshot: String,
    pub parent_close_policy: String,
    pub recovery_checkpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskLifecycleState {
    Queued,
    Ready,
    Running,
    Blocked,
    CancelRequested,
    Cancelled,
    Failed,
    Completed,
    Interrupted,
    Retryable,
}

impl fmt::Display for TaskLifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self {
            TaskLifecycleState::Queued => "queued",
            TaskLifecycleState::Ready => "ready",
            TaskLifecycleState::Running => "running",
            TaskLifecycleState::Blocked => "blocked",
            TaskLifecycleState::CancelRequested => "cancel_requested",
            TaskLifecycleState::Cancelled => "cancelled",
            TaskLifecycleState::Failed => "failed",
            TaskLifecycleState::Completed => "completed",
            TaskLifecycleState::Interrupted => "interrupted",
            TaskLifecycleState::Retryable => "retryable",
        };
        write!(f, "{state}")
    }
}

impl std::str::FromStr for TaskLifecycleState {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "queued" => Ok(TaskLifecycleState::Queued),
            "ready" => Ok(TaskLifecycleState::Ready),
            "running" => Ok(TaskLifecycleState::Running),
            "blocked" => Ok(TaskLifecycleState::Blocked),
            "cancel_requested" => Ok(TaskLifecycleState::CancelRequested),
            "cancelled" => Ok(TaskLifecycleState::Cancelled),
            "failed" => Ok(TaskLifecycleState::Failed),
            "completed" => Ok(TaskLifecycleState::Completed),
            "interrupted" => Ok(TaskLifecycleState::Interrupted),
            "retryable" => Ok(TaskLifecycleState::Retryable),
            other => anyhow::bail!("unknown task lifecycle state: {other:?}"),
        }
    }
}

impl TaskLifecycleState {
    fn can_transition_to(self, next: TaskLifecycleState) -> bool {
        match self {
            TaskLifecycleState::Queued => matches!(
                next,
                TaskLifecycleState::Ready
                    | TaskLifecycleState::Running
                    | TaskLifecycleState::CancelRequested
                    | TaskLifecycleState::Cancelled
                    | TaskLifecycleState::Interrupted
            ),
            TaskLifecycleState::Ready => matches!(
                next,
                TaskLifecycleState::Running
                    | TaskLifecycleState::Blocked
                    | TaskLifecycleState::CancelRequested
                    | TaskLifecycleState::Cancelled
                    | TaskLifecycleState::Interrupted
            ),
            TaskLifecycleState::Running => matches!(
                next,
                TaskLifecycleState::Blocked
                    | TaskLifecycleState::CancelRequested
                    | TaskLifecycleState::Completed
                    | TaskLifecycleState::Failed
                    | TaskLifecycleState::Interrupted
                    | TaskLifecycleState::Retryable
            ),
            TaskLifecycleState::Blocked => matches!(
                next,
                TaskLifecycleState::Ready
                    | TaskLifecycleState::Running
                    | TaskLifecycleState::CancelRequested
                    | TaskLifecycleState::Cancelled
                    | TaskLifecycleState::Interrupted
            ),
            TaskLifecycleState::CancelRequested => {
                matches!(
                    next,
                    TaskLifecycleState::Cancelled | TaskLifecycleState::Interrupted
                )
            }
            TaskLifecycleState::Failed => matches!(next, TaskLifecycleState::Retryable),
            TaskLifecycleState::Interrupted => matches!(next, TaskLifecycleState::Retryable),
            TaskLifecycleState::Retryable => {
                matches!(next, TaskLifecycleState::Queued | TaskLifecycleState::Ready)
            }
            TaskLifecycleState::Cancelled | TaskLifecycleState::Completed => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptLifecycleState {
    Queued,
    Ready,
    Running,
    Blocked,
    CancelRequested,
    Cancelled,
    Failed,
    Completed,
    Interrupted,
    Retryable,
}

impl fmt::Display for AttemptLifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self {
            AttemptLifecycleState::Queued => "queued",
            AttemptLifecycleState::Ready => "ready",
            AttemptLifecycleState::Running => "running",
            AttemptLifecycleState::Blocked => "blocked",
            AttemptLifecycleState::CancelRequested => "cancel_requested",
            AttemptLifecycleState::Cancelled => "cancelled",
            AttemptLifecycleState::Failed => "failed",
            AttemptLifecycleState::Completed => "completed",
            AttemptLifecycleState::Interrupted => "interrupted",
            AttemptLifecycleState::Retryable => "retryable",
        };
        write!(f, "{state}")
    }
}

impl std::str::FromStr for AttemptLifecycleState {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "queued" => Ok(AttemptLifecycleState::Queued),
            "ready" => Ok(AttemptLifecycleState::Ready),
            "running" => Ok(AttemptLifecycleState::Running),
            "blocked" => Ok(AttemptLifecycleState::Blocked),
            "cancel_requested" => Ok(AttemptLifecycleState::CancelRequested),
            "cancelled" => Ok(AttemptLifecycleState::Cancelled),
            "failed" => Ok(AttemptLifecycleState::Failed),
            "completed" => Ok(AttemptLifecycleState::Completed),
            "interrupted" => Ok(AttemptLifecycleState::Interrupted),
            "retryable" => Ok(AttemptLifecycleState::Retryable),
            other => anyhow::bail!("unknown attempt lifecycle state: {other:?}"),
        }
    }
}

impl AttemptLifecycleState {
    fn can_transition_to(self, next: AttemptLifecycleState) -> bool {
        match self {
            AttemptLifecycleState::Queued => matches!(
                next,
                AttemptLifecycleState::Ready
                    | AttemptLifecycleState::Running
                    | AttemptLifecycleState::CancelRequested
                    | AttemptLifecycleState::Cancelled
                    | AttemptLifecycleState::Interrupted
            ),
            AttemptLifecycleState::Ready => matches!(
                next,
                AttemptLifecycleState::Running
                    | AttemptLifecycleState::Blocked
                    | AttemptLifecycleState::CancelRequested
                    | AttemptLifecycleState::Cancelled
                    | AttemptLifecycleState::Interrupted
            ),
            AttemptLifecycleState::Running => matches!(
                next,
                AttemptLifecycleState::Blocked
                    | AttemptLifecycleState::CancelRequested
                    | AttemptLifecycleState::Completed
                    | AttemptLifecycleState::Failed
                    | AttemptLifecycleState::Interrupted
                    | AttemptLifecycleState::Retryable
            ),
            AttemptLifecycleState::Blocked => matches!(
                next,
                AttemptLifecycleState::Ready
                    | AttemptLifecycleState::Running
                    | AttemptLifecycleState::CancelRequested
                    | AttemptLifecycleState::Cancelled
                    | AttemptLifecycleState::Interrupted
            ),
            AttemptLifecycleState::CancelRequested => {
                matches!(
                    next,
                    AttemptLifecycleState::Cancelled | AttemptLifecycleState::Interrupted
                )
            }
            AttemptLifecycleState::Failed => matches!(next, AttemptLifecycleState::Retryable),
            AttemptLifecycleState::Interrupted => {
                matches!(next, AttemptLifecycleState::Retryable)
            }
            AttemptLifecycleState::Retryable => matches!(
                next,
                AttemptLifecycleState::Queued
                    | AttemptLifecycleState::Ready
                    | AttemptLifecycleState::Running
            ),
            AttemptLifecycleState::Cancelled | AttemptLifecycleState::Completed => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskEdgeRecord {
    pub task_id: String,
    pub depends_on_task_id: String,
    pub created_at: DateTime<Utc>,
}

pub struct NewTaskEdgeRecord {
    pub task_id: String,
    pub depends_on_task_id: String,
}

#[derive(Debug, Clone)]
pub struct TaskAttemptRecord {
    pub attempt_id: String,
    pub task_id: String,
    pub session_id: Option<String>,
    pub status: String,
    pub recovery_checkpoint: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct NewTaskAttemptRecord {
    pub attempt_id: String,
    pub task_id: String,
    pub session_id: Option<String>,
    pub status: String,
    pub recovery_checkpoint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskEventRecord {
    pub sequence: i64,
    pub task_id: String,
    pub attempt_id: String,
    pub session_id: Option<String>,
    pub event_type: String,
    pub payload: String,
    pub recorded_at: DateTime<Utc>,
}

pub struct NewTaskEventRecord {
    pub task_id: String,
    pub attempt_id: String,
    pub session_id: Option<String>,
    pub event_type: String,
    pub payload: String,
}

impl Session {
    /// Create a new session and persist it.
    pub fn create(store: &Store, new: NewSession) -> Result<Session> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        store
            .conn()
            .execute(
                "INSERT INTO sessions (id, status, model, provider, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (&id, "active", &new.model, &new.provider, &now_str, &now_str),
            )
            .context("failed to insert session")?;

        Ok(Session {
            id,
            title: None,
            status: SessionStatus::Active,
            model: new.model,
            provider: new.provider,
            policy: None,
            settings: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Get a session by ID.
    pub fn get(store: &Store, id: &str) -> Result<Session> {
        store
            .conn()
            .query_row(
                "SELECT id, title, status, model, provider, policy, settings, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                [id],
                Self::from_row,
            )
            .context("session not found")
    }

    pub fn find(store: &Store, id: &str) -> Result<Option<Session>> {
        let mut stmt = store.conn().prepare(
            "SELECT id, title, status, model, provider, policy, settings, created_at, updated_at
             FROM sessions WHERE id = ?1",
        )?;

        let mut rows = stmt.query_map([id], Self::from_row)?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Get the most recently created session.
    pub fn get_latest(store: &Store) -> Result<Option<Session>> {
        let mut stmt = store.conn().prepare(
            "SELECT id, title, status, model, provider, policy, settings, created_at, updated_at
             FROM sessions ORDER BY created_at DESC LIMIT 1",
        )?;

        let mut rows = stmt.query_map([], Self::from_row)?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// List sessions, most recent first.
    pub fn list(store: &Store, limit: u32) -> Result<Vec<Session>> {
        let mut stmt = store.conn().prepare(
            "SELECT id, title, status, model, provider, policy, settings, created_at, updated_at
             FROM sessions ORDER BY created_at DESC LIMIT ?1",
        )?;

        let sessions = stmt
            .query_map([limit], Self::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(sessions)
    }

    /// Update the status of a session.
    pub fn update_status(store: &Store, id: &str, status: SessionStatus) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        store.conn().execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            (&status.to_string(), &now, id),
        )?;
        Ok(())
    }

    /// Set the title of a session.
    pub fn update_title(store: &Store, id: &str, title: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        store.conn().execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            (title, &now, id),
        )?;
        Ok(())
    }

    /// Save settings JSON (for session resume).
    pub fn update_settings(store: &Store, id: &str, settings: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        store.conn().execute(
            "UPDATE sessions SET settings = ?1, updated_at = ?2 WHERE id = ?3",
            (settings, &now, id),
        )?;
        Ok(())
    }

    pub fn update_runtime_selection(
        store: &Store,
        id: &str,
        model: &str,
        provider: &str,
        settings: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        store.conn().execute(
            "UPDATE sessions
             SET model = ?1, provider = ?2, settings = ?3, updated_at = ?4
             WHERE id = ?5",
            (model, provider, settings, &now, id),
        )?;
        Ok(())
    }

    /// Shared row mapper for all queries.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
        let status_str: String = row.get(2)?;
        let created_str: String = row.get(7)?;
        let updated_str: String = row.get(8)?;

        Ok(Session {
            id: row.get(0)?,
            title: row.get(1)?,
            status: status_str.parse::<SessionStatus>().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(RowParseError(e.to_string())),
                )
            })?,
            model: row.get(3)?,
            provider: row.get(4)?,
            policy: row.get(5)?,
            settings: row.get(6)?,
            created_at: DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            updated_at: DateTime::parse_from_rfc3339(&updated_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
        })
    }
}

impl TaskRecord {
    pub fn create(store: &Store, new: NewTaskRecord) -> Result<TaskRecord> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        store
            .conn()
            .execute(
                "INSERT INTO tasks (task_id, parent_task_id, title, priority, policy_snapshot, parent_close_policy, recovery_checkpoint, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                (
                    &new.task_id,
                    &new.parent_task_id,
                    &new.title,
                    new.priority,
                    &new.policy_snapshot,
                    &new.parent_close_policy,
                    &new.recovery_checkpoint,
                    &now_str,
                    &now_str,
                ),
            )
            .context("failed to insert task")?;

        Ok(TaskRecord {
            task_id: new.task_id,
            parent_task_id: new.parent_task_id,
            title: new.title,
            priority: new.priority,
            policy_snapshot: new.policy_snapshot,
            parent_close_policy: new.parent_close_policy,
            recovery_checkpoint: new.recovery_checkpoint,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn get(store: &Store, task_id: &str) -> Result<TaskRecord> {
        store
            .conn()
            .query_row(
                "SELECT task_id, parent_task_id, title, priority, policy_snapshot, parent_close_policy, recovery_checkpoint, created_at, updated_at
                 FROM tasks WHERE task_id = ?1",
                [task_id],
                Self::from_row,
            )
            .context("task not found")
    }

    pub fn list(store: &Store) -> Result<Vec<TaskRecord>> {
        let mut stmt = store.conn().prepare(
            "SELECT task_id, parent_task_id, title, priority, policy_snapshot, parent_close_policy, recovery_checkpoint, created_at, updated_at
             FROM tasks ORDER BY created_at, task_id",
        )?;

        let tasks = stmt
            .query_map([], Self::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(tasks)
    }

    pub fn create_or_update(store: &Store, new: NewTaskRecord) -> Result<TaskRecord> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        store
            .conn()
            .execute(
                "INSERT INTO tasks (task_id, parent_task_id, title, priority, policy_snapshot, parent_close_policy, recovery_checkpoint, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                 ON CONFLICT(task_id) DO UPDATE SET
                    parent_task_id = excluded.parent_task_id,
                    title = excluded.title,
                    priority = excluded.priority,
                    policy_snapshot = excluded.policy_snapshot,
                    parent_close_policy = excluded.parent_close_policy,
                    recovery_checkpoint = excluded.recovery_checkpoint,
                    updated_at = excluded.updated_at",
                (
                    &new.task_id,
                    &new.parent_task_id,
                    &new.title,
                    new.priority,
                    &new.policy_snapshot,
                    &new.parent_close_policy,
                    &new.recovery_checkpoint,
                    &now_str,
                ),
            )
            .context("failed to upsert task")?;

        Self::get(store, &new.task_id)
    }

    pub fn current_state(store: &Store, task_id: &str) -> Result<TaskLifecycleState> {
        let _ = Self::get(store, task_id)?;
        let latest = Self::latest_transition_state(store, task_id)?;
        Ok(latest.unwrap_or(TaskLifecycleState::Queued))
    }

    pub fn transition_state(
        store: &Store,
        task_id: &str,
        attempt_id: &str,
        next_state: TaskLifecycleState,
    ) -> Result<TaskEventRecord> {
        let current_state = Self::current_state(store, task_id)?;
        if !current_state.can_transition_to(next_state) {
            anyhow::bail!(
                "invalid task transition: {} -> {}",
                current_state,
                next_state
            );
        }

        let attempt = TaskAttemptRecord::get(store, attempt_id)?;
        if attempt.task_id != task_id {
            anyhow::bail!("attempt {attempt_id} does not belong to task {task_id}",);
        }

        let now = Utc::now().to_rfc3339();
        store
            .conn()
            .execute(
                "UPDATE tasks SET updated_at = ?1 WHERE task_id = ?2",
                (&now, task_id),
            )
            .context("failed to update task timestamp")?;

        TaskEventRecord::append(
            store,
            NewTaskEventRecord {
                task_id: task_id.to_string(),
                attempt_id: attempt_id.to_string(),
                session_id: attempt.session_id,
                event_type: "task.state.transition".to_string(),
                payload: serde_json::json!({
                    "from": current_state.to_string(),
                    "to": next_state.to_string(),
                })
                .to_string(),
            },
        )
    }

    fn latest_transition_state(store: &Store, task_id: &str) -> Result<Option<TaskLifecycleState>> {
        let mut stmt = store.conn().prepare(
            "SELECT payload
             FROM task_events
             WHERE task_id = ?1 AND event_type = 'task.state.transition'
             ORDER BY sequence DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query([task_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        let payload: String = row.get(0)?;
        let payload_json: serde_json::Value = serde_json::from_str(&payload)
            .context("failed to parse latest task state transition payload")?;
        let next_state = payload_json
            .get("to")
            .and_then(serde_json::Value::as_str)
            .context("task state transition payload missing 'to' field")?;
        Ok(Some(next_state.parse::<TaskLifecycleState>()?))
    }

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRecord> {
        let created_at: String = row.get(7)?;
        let updated_at: String = row.get(8)?;

        Ok(TaskRecord {
            task_id: row.get(0)?,
            parent_task_id: row.get(1)?,
            title: row.get(2)?,
            priority: row.get(3)?,
            policy_snapshot: row.get(4)?,
            parent_close_policy: row.get(5)?,
            recovery_checkpoint: row.get(6)?,
            created_at: DateTime::parse_from_rfc3339(&created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            updated_at: DateTime::parse_from_rfc3339(&updated_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
        })
    }
}

impl TaskEdgeRecord {
    pub fn create(store: &Store, new: NewTaskEdgeRecord) -> Result<TaskEdgeRecord> {
        if new.task_id == new.depends_on_task_id {
            anyhow::bail!(
                "task graph cycle detected: {} -> {}",
                new.task_id,
                new.task_id
            );
        }

        let mut edges = Self::load_all_pairs(store)?;
        edges.push((new.task_id.clone(), new.depends_on_task_id.clone()));
        validate_task_graph_acyclic(&edges)?;

        let now = Utc::now();
        let now_str = now.to_rfc3339();

        store
            .conn()
            .execute(
                "INSERT INTO task_edges (task_id, depends_on_task_id, created_at)
                 VALUES (?1, ?2, ?3)",
                (&new.task_id, &new.depends_on_task_id, &now_str),
            )
            .context("failed to insert task edge")?;

        Ok(TaskEdgeRecord {
            task_id: new.task_id,
            depends_on_task_id: new.depends_on_task_id,
            created_at: now,
        })
    }

    pub fn replace_for_task(
        store: &Store,
        task_id: &str,
        depends_on_task_ids: &[String],
    ) -> Result<Vec<TaskEdgeRecord>> {
        let mut deduped = depends_on_task_ids.to_vec();
        deduped.sort();
        deduped.dedup();

        if deduped.iter().any(|dep| dep == task_id) {
            anyhow::bail!("task graph cycle detected: {task_id} -> {task_id}");
        }

        let mut edges = Self::load_all_pairs(store)?;
        edges.retain(|(child, _)| child != task_id);
        edges.extend(
            deduped
                .iter()
                .cloned()
                .map(|dep| (task_id.to_string(), dep)),
        );
        validate_task_graph_acyclic(&edges)?;

        store
            .conn()
            .execute("DELETE FROM task_edges WHERE task_id = ?1", [task_id])
            .context("failed to clear task edges")?;

        let now = Utc::now();
        let now_str = now.to_rfc3339();
        for dep in &deduped {
            store
                .conn()
                .execute(
                    "INSERT INTO task_edges (task_id, depends_on_task_id, created_at)
                     VALUES (?1, ?2, ?3)",
                    (task_id, dep, &now_str),
                )
                .context("failed to upsert task edge")?;
        }

        Self::list_for_task(store, task_id)
    }

    pub fn list(store: &Store) -> Result<Vec<TaskEdgeRecord>> {
        let mut stmt = store.conn().prepare(
            "SELECT task_id, depends_on_task_id, created_at
             FROM task_edges ORDER BY task_id, depends_on_task_id",
        )?;

        let edges = stmt
            .query_map([], Self::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(edges)
    }

    pub fn list_for_task(store: &Store, task_id: &str) -> Result<Vec<TaskEdgeRecord>> {
        let mut stmt = store.conn().prepare(
            "SELECT task_id, depends_on_task_id, created_at
             FROM task_edges WHERE task_id = ?1 ORDER BY created_at",
        )?;

        let edges = stmt
            .query_map([task_id], Self::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(edges)
    }

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskEdgeRecord> {
        let created_at: String = row.get(2)?;

        Ok(TaskEdgeRecord {
            task_id: row.get(0)?,
            depends_on_task_id: row.get(1)?,
            created_at: DateTime::parse_from_rfc3339(&created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
        })
    }

    fn load_all_pairs(store: &Store) -> Result<Vec<(String, String)>> {
        let mut stmt = store.conn().prepare(
            "SELECT task_id, depends_on_task_id
             FROM task_edges ORDER BY task_id, depends_on_task_id",
        )?;

        let edges = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(edges)
    }
}

fn validate_task_graph_acyclic(edges: &[(String, String)]) -> Result<()> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum VisitState {
        Visiting,
        Visited,
    }

    fn walk(
        node: &str,
        adjacency: &BTreeMap<String, Vec<String>>,
        states: &mut HashMap<String, VisitState>,
        stack: &mut Vec<String>,
        stack_positions: &mut HashMap<String, usize>,
    ) -> Result<()> {
        states.insert(node.to_string(), VisitState::Visiting);
        stack_positions.insert(node.to_string(), stack.len());
        stack.push(node.to_string());

        if let Some(neighbors) = adjacency.get(node) {
            for next in neighbors {
                match states.get(next.as_str()) {
                    Some(VisitState::Visited) => continue,
                    Some(VisitState::Visiting) => {
                        let start = *stack_positions
                            .get(next.as_str())
                            .context("cycle path construction failed")?;
                        let mut cycle = stack[start..].to_vec();
                        cycle.push(next.clone());
                        anyhow::bail!("task graph cycle detected: {}", cycle.join(" -> "));
                    }
                    None => walk(next, adjacency, states, stack, stack_positions)?,
                }
            }
        }

        stack.pop();
        stack_positions.remove(node);
        states.insert(node.to_string(), VisitState::Visited);
        Ok(())
    }

    let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (task_id, depends_on_task_id) in edges {
        adjacency
            .entry(task_id.clone())
            .or_default()
            .push(depends_on_task_id.clone());
        adjacency.entry(depends_on_task_id.clone()).or_default();
    }
    for neighbors in adjacency.values_mut() {
        neighbors.sort();
        neighbors.dedup();
    }

    let mut states: HashMap<String, VisitState> = HashMap::new();
    let mut stack = Vec::new();
    let mut stack_positions: HashMap<String, usize> = HashMap::new();

    let nodes: Vec<String> = adjacency.keys().cloned().collect();
    for node in nodes {
        if !states.contains_key(node.as_str()) {
            walk(
                &node,
                &adjacency,
                &mut states,
                &mut stack,
                &mut stack_positions,
            )?;
        }
    }

    Ok(())
}

impl TaskAttemptRecord {
    pub fn create(store: &Store, new: NewTaskAttemptRecord) -> Result<TaskAttemptRecord> {
        let state = new.status.parse::<AttemptLifecycleState>()?;
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        store
            .conn()
            .execute(
                "INSERT INTO task_attempts (attempt_id, task_id, session_id, status, recovery_checkpoint, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (
                    &new.attempt_id,
                    &new.task_id,
                    &new.session_id,
                    &state.to_string(),
                    &new.recovery_checkpoint,
                    &now_str,
                    &now_str,
                ),
            )
            .context("failed to insert task attempt")?;

        Ok(TaskAttemptRecord {
            attempt_id: new.attempt_id,
            task_id: new.task_id,
            session_id: new.session_id,
            status: state.to_string(),
            recovery_checkpoint: new.recovery_checkpoint,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn state(&self) -> Result<AttemptLifecycleState> {
        self.status.parse::<AttemptLifecycleState>()
    }

    pub fn claim_runnable_with_lease(
        store: &Store,
        attempt_id: &str,
        owner_id: &str,
        lease_ttl_seconds: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        if lease_ttl_seconds <= 0 {
            anyhow::bail!("lease ttl seconds must be > 0");
        }

        let attempt = Self::get(store, attempt_id)?;
        match attempt.state()? {
            AttemptLifecycleState::Ready => Self::claim_ready_attempt_with_lease(
                store,
                attempt,
                owner_id,
                lease_ttl_seconds,
                now,
            ),
            AttemptLifecycleState::Running => {
                let _ = Self::mark_expired_lease_interrupted_recoverable(store, attempt_id, now)?;
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    pub fn mark_expired_lease_interrupted_recoverable(
        store: &Store,
        attempt_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let attempt = Self::get(store, attempt_id)?;
        if attempt.state()? != AttemptLifecycleState::Running {
            return Ok(false);
        }

        let Some(mut lease) =
            AttemptLeaseMetadata::from_recovery_checkpoint(attempt.recovery_checkpoint.as_deref())?
        else {
            return Ok(false);
        };

        if lease.lease_expires_at > now {
            return Ok(false);
        }

        lease.interrupted_at = Some(now);
        lease.recoverable = true;
        let checkpoint_with_expired_lease =
            lease_checkpoint_json(attempt.recovery_checkpoint.as_deref(), &lease)?;

        let next_updated_at = now.to_rfc3339();
        let previous_updated_at = attempt.updated_at.to_rfc3339();
        let rows = store
            .conn()
            .execute(
                "UPDATE task_attempts
                 SET status = ?1, recovery_checkpoint = ?2, updated_at = ?3
                 WHERE attempt_id = ?4 AND status = ?5 AND updated_at = ?6",
                (
                    AttemptLifecycleState::Interrupted.to_string(),
                    &checkpoint_with_expired_lease,
                    &next_updated_at,
                    attempt_id,
                    AttemptLifecycleState::Running.to_string(),
                    &previous_updated_at,
                ),
            )
            .context("failed to mark expired lease interrupted/recoverable")?;

        if rows == 0 {
            return Ok(false);
        }

        TaskEventRecord::append(
            store,
            NewTaskEventRecord {
                task_id: attempt.task_id.clone(),
                attempt_id: attempt.attempt_id.clone(),
                session_id: attempt.session_id.clone(),
                event_type: "attempt.state.transition".to_string(),
                payload: serde_json::json!({
                    "from": AttemptLifecycleState::Running.to_string(),
                    "to": AttemptLifecycleState::Interrupted.to_string(),
                })
                .to_string(),
            },
        )?;

        TaskEventRecord::append(
            store,
            NewTaskEventRecord {
                task_id: attempt.task_id,
                attempt_id: attempt.attempt_id,
                session_id: attempt.session_id,
                event_type: "attempt.lease.expired".to_string(),
                payload: serde_json::json!({
                    "owner_id": lease.owner_id,
                    "lease_expires_at": lease.lease_expires_at.to_rfc3339(),
                    "interrupted_at": now.to_rfc3339(),
                    "recoverable": true,
                })
                .to_string(),
            },
        )?;

        Ok(true)
    }

    fn claim_ready_attempt_with_lease(
        store: &Store,
        attempt: TaskAttemptRecord,
        owner_id: &str,
        lease_ttl_seconds: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let lease = AttemptLeaseMetadata {
            owner_id: owner_id.to_string(),
            leased_at: now,
            lease_expires_at: now + chrono::Duration::seconds(lease_ttl_seconds),
            interrupted_at: None,
            recoverable: false,
        };
        let checkpoint_with_lease =
            lease_checkpoint_json(attempt.recovery_checkpoint.as_deref(), &lease)?;

        let next_updated_at = now.to_rfc3339();
        let previous_updated_at = attempt.updated_at.to_rfc3339();
        let rows = store
            .conn()
            .execute(
                "UPDATE task_attempts
                 SET status = ?1, recovery_checkpoint = ?2, updated_at = ?3
                 WHERE attempt_id = ?4 AND status = ?5 AND updated_at = ?6",
                (
                    AttemptLifecycleState::Running.to_string(),
                    &checkpoint_with_lease,
                    &next_updated_at,
                    attempt.attempt_id.clone(),
                    AttemptLifecycleState::Ready.to_string(),
                    &previous_updated_at,
                ),
            )
            .context("failed to claim runnable attempt")?;

        if rows == 0 {
            return Ok(false);
        }

        TaskEventRecord::append(
            store,
            NewTaskEventRecord {
                task_id: attempt.task_id.clone(),
                attempt_id: attempt.attempt_id.clone(),
                session_id: attempt.session_id.clone(),
                event_type: "attempt.state.transition".to_string(),
                payload: serde_json::json!({
                    "from": AttemptLifecycleState::Ready.to_string(),
                    "to": AttemptLifecycleState::Running.to_string(),
                })
                .to_string(),
            },
        )?;

        TaskEventRecord::append(
            store,
            NewTaskEventRecord {
                task_id: attempt.task_id,
                attempt_id: attempt.attempt_id,
                session_id: attempt.session_id,
                event_type: "attempt.lease.claimed".to_string(),
                payload: serde_json::json!({
                    "owner_id": lease.owner_id,
                    "leased_at": lease.leased_at.to_rfc3339(),
                    "lease_expires_at": lease.lease_expires_at.to_rfc3339(),
                })
                .to_string(),
            },
        )?;

        Ok(true)
    }

    pub fn transition_state(
        store: &Store,
        attempt_id: &str,
        next_state: AttemptLifecycleState,
    ) -> Result<TaskAttemptRecord> {
        let attempt = Self::get(store, attempt_id)?;
        let current_state = attempt.state()?;
        if !current_state.can_transition_to(next_state) {
            anyhow::bail!(
                "invalid attempt transition: {} -> {}",
                current_state,
                next_state
            );
        }

        let now = Utc::now().to_rfc3339();
        store
            .conn()
            .execute(
                "UPDATE task_attempts SET status = ?1, updated_at = ?2 WHERE attempt_id = ?3",
                (&next_state.to_string(), &now, attempt_id),
            )
            .context("failed to update task attempt status")?;

        TaskEventRecord::append(
            store,
            NewTaskEventRecord {
                task_id: attempt.task_id.clone(),
                attempt_id: attempt.attempt_id.clone(),
                session_id: attempt.session_id,
                event_type: "attempt.state.transition".to_string(),
                payload: serde_json::json!({
                    "from": current_state.to_string(),
                    "to": next_state.to_string(),
                })
                .to_string(),
            },
        )?;

        Self::get(store, attempt_id)
    }

    pub fn get(store: &Store, attempt_id: &str) -> Result<TaskAttemptRecord> {
        store
            .conn()
            .query_row(
                "SELECT attempt_id, task_id, session_id, status, recovery_checkpoint, created_at, updated_at
                 FROM task_attempts WHERE attempt_id = ?1",
                [attempt_id],
                Self::from_row,
            )
            .context("task attempt not found")
    }

    pub fn list_for_task(store: &Store, task_id: &str) -> Result<Vec<TaskAttemptRecord>> {
        let mut stmt = store.conn().prepare(
            "SELECT attempt_id, task_id, session_id, status, recovery_checkpoint, created_at, updated_at
             FROM task_attempts WHERE task_id = ?1 ORDER BY created_at",
        )?;

        let attempts = stmt
            .query_map([task_id], Self::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(attempts)
    }

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskAttemptRecord> {
        let created_at: String = row.get(5)?;
        let updated_at: String = row.get(6)?;

        Ok(TaskAttemptRecord {
            attempt_id: row.get(0)?,
            task_id: row.get(1)?,
            session_id: row.get(2)?,
            status: row.get(3)?,
            recovery_checkpoint: row.get(4)?,
            created_at: DateTime::parse_from_rfc3339(&created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            updated_at: DateTime::parse_from_rfc3339(&updated_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
        })
    }
}

#[derive(Debug, Clone)]
struct AttemptLeaseMetadata {
    owner_id: String,
    leased_at: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
    interrupted_at: Option<DateTime<Utc>>,
    recoverable: bool,
}

impl AttemptLeaseMetadata {
    fn from_recovery_checkpoint(recovery_checkpoint: Option<&str>) -> Result<Option<Self>> {
        let Some(raw_checkpoint) = recovery_checkpoint else {
            return Ok(None);
        };
        let parsed: serde_json::Value =
            serde_json::from_str(raw_checkpoint).context("failed to parse recovery checkpoint")?;
        let Some(lease) = parsed.get("scheduler_lease") else {
            return Ok(None);
        };

        let owner_id = lease
            .get("owner_id")
            .and_then(serde_json::Value::as_str)
            .context("scheduler_lease.owner_id missing")?
            .to_string();
        let leased_at = lease
            .get("leased_at")
            .and_then(serde_json::Value::as_str)
            .context("scheduler_lease.leased_at missing")?;
        let lease_expires_at = lease
            .get("lease_expires_at")
            .and_then(serde_json::Value::as_str)
            .context("scheduler_lease.lease_expires_at missing")?;
        let interrupted_at = lease
            .get("interrupted_at")
            .and_then(serde_json::Value::as_str)
            .map(|value| DateTime::parse_from_rfc3339(value).map(|dt| dt.with_timezone(&Utc)))
            .transpose()
            .context("failed to parse scheduler_lease.interrupted_at")?;

        Ok(Some(Self {
            owner_id,
            leased_at: DateTime::parse_from_rfc3339(leased_at)
                .map(|dt| dt.with_timezone(&Utc))
                .context("failed to parse scheduler_lease.leased_at")?,
            lease_expires_at: DateTime::parse_from_rfc3339(lease_expires_at)
                .map(|dt| dt.with_timezone(&Utc))
                .context("failed to parse scheduler_lease.lease_expires_at")?,
            interrupted_at,
            recoverable: lease
                .get("recoverable")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        }))
    }
}

fn lease_checkpoint_json(
    prior_checkpoint: Option<&str>,
    lease: &AttemptLeaseMetadata,
) -> Result<String> {
    let mut checkpoint_obj = match prior_checkpoint {
        Some(raw) => {
            let parsed: serde_json::Value =
                serde_json::from_str(raw).context("failed to parse recovery checkpoint")?;
            match parsed {
                serde_json::Value::Object(map) => map,
                other => {
                    let mut map = serde_json::Map::new();
                    map.insert("legacy_recovery_checkpoint".to_string(), other);
                    map
                }
            }
        }
        None => serde_json::Map::new(),
    };

    checkpoint_obj.insert(
        "scheduler_lease".to_string(),
        serde_json::json!({
            "owner_id": lease.owner_id,
            "leased_at": lease.leased_at.to_rfc3339(),
            "lease_expires_at": lease.lease_expires_at.to_rfc3339(),
            "interrupted_at": lease.interrupted_at.as_ref().map(DateTime::to_rfc3339),
            "recoverable": lease.recoverable,
        }),
    );

    Ok(serde_json::Value::Object(checkpoint_obj).to_string())
}

impl TaskEventRecord {
    pub fn append(store: &Store, new: NewTaskEventRecord) -> Result<TaskEventRecord> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        store
            .conn()
            .execute(
                "INSERT INTO task_events (task_id, attempt_id, session_id, event_type, payload, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (
                    &new.task_id,
                    &new.attempt_id,
                    &new.session_id,
                    &new.event_type,
                    &new.payload,
                    &now_str,
                ),
            )
            .context("failed to insert task event")?;

        let sequence = store.conn().last_insert_rowid();

        Ok(TaskEventRecord {
            sequence,
            task_id: new.task_id,
            attempt_id: new.attempt_id,
            session_id: new.session_id,
            event_type: new.event_type,
            payload: new.payload,
            recorded_at: now,
        })
    }

    pub fn list_for_task(
        store: &Store,
        task_id: &str,
        after_sequence: i64,
    ) -> Result<Vec<TaskEventRecord>> {
        Self::validate_replay_cursor(store, task_id, after_sequence)?;

        let mut stmt = store.conn().prepare(
            "SELECT sequence, task_id, attempt_id, session_id, event_type, payload, recorded_at
             FROM task_events WHERE task_id = ?1 AND sequence > ?2 ORDER BY sequence",
        )?;

        let events = stmt
            .query_map((task_id, after_sequence), Self::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(events)
    }

    fn validate_replay_cursor(store: &Store, task_id: &str, after_sequence: i64) -> Result<()> {
        if after_sequence < 0 {
            anyhow::bail!("task event cursor must be >= 0, got {after_sequence}");
        }

        let task_exists: i64 = store
            .conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id = ?1)",
                [task_id],
                |row| row.get(0),
            )
            .context("failed to validate task event stream")?;

        if task_exists == 0 {
            anyhow::bail!("task event stream not found for task {task_id}");
        }

        if after_sequence == 0 {
            return Ok(());
        }

        let cursor_matches_task: i64 = store
            .conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM task_events WHERE task_id = ?1 AND sequence = ?2)",
                (task_id, after_sequence),
                |row| row.get(0),
            )
            .context("failed to validate task event cursor")?;

        if cursor_matches_task == 0 {
            anyhow::bail!("task event cursor {after_sequence} is invalid for task {task_id}");
        }

        Ok(())
    }

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskEventRecord> {
        let recorded_at: String = row.get(6)?;

        Ok(TaskEventRecord {
            sequence: row.get(0)?,
            task_id: row.get(1)?,
            attempt_id: row.get(2)?,
            session_id: row.get(3)?,
            event_type: row.get(4)?,
            payload: row.get(5)?,
            recorded_at: DateTime::parse_from_rfc3339(&recorded_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
        })
    }
}
