use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use tokio::task;
use tokio::time::{Duration, timeout};

use super::super::protocol::{
    ActiveTurnSnapshot, AuthStateSnapshot, SelectedSession, SessionSummary, StateSnapshotData,
    TaskAttemptSnapshot, TaskDetailSnapshot, TaskDetailSnapshotData, TaskGraphEdgeSnapshot,
    TaskGraphNodeSnapshot, TaskGraphSnapshot, TaskListSnapshotData, TaskWatchCursor,
    TranscriptEntry,
};
use super::super::state::WebAppState;
use super::context_usage::{
    context_usage_from_chars, estimate_context_breakdown, estimate_persisted_context_usage,
};
use crate::compact::CompactConfig;
use crate::runtime::{ActiveTurnReplay, SessionSettingsOverrides};
use crate::store::{
    self, Session, Store, TaskAttemptRecord, TaskEdgeRecord, TaskEventRecord, TaskRecord, Turn,
};

const SNAPSHOT_AUTH_TIMEOUT: Duration = Duration::from_millis(250);

pub(super) struct TaskWatchBootstrapData {
    pub list_snapshot: TaskListSnapshotData,
    pub detail_snapshot: TaskDetailSnapshotData,
    pub replay_events: Vec<TaskEventRecord>,
}

pub(super) async fn snapshot_data(
    state: &WebAppState,
    session_id: &str,
    controller_id: &str,
) -> Result<StateSnapshotData> {
    let selected_session = load_selected_session(state, session_id).await?;
    let auth = snapshot_auth_summary(state, controller_id).await;
    let sessions = vec![SessionSummary {
        session_id: selected_session.session_id.clone(),
        title: selected_session.title.clone(),
        updated_at: selected_session.updated_at.clone(),
    }];
    let turns = load_turns(state, session_id).await?;
    let transcript = turns_to_transcript(&turns);
    let active_turn = state
        .runtime_manager
        .active_turn(session_id)
        .map(active_turn_snapshot);
    let system_prompt_chars = state
        .runtime_manager
        .system_prompt_chars(session_id)
        .unwrap_or(0);
    let context_usage = if let Some((used_chars, max_chars)) =
        state.runtime_manager.context_usage_chars(session_id)
    {
        let (input_tokens, output_tokens, total_tokens) = state
            .runtime_manager
            .token_usage(session_id)
            .unwrap_or((None, None, None));
        context_usage_from_chars(
            used_chars,
            max_chars,
            input_tokens,
            output_tokens,
            total_tokens,
            estimate_context_breakdown(
                &turns,
                used_chars,
                input_tokens,
                output_tokens,
                total_tokens,
                system_prompt_chars,
            ),
        )
    } else {
        let compact_threshold = state
            .runtime_manager
            .compact_threshold_chars(session_id)
            .unwrap_or_else(|| CompactConfig::default().threshold_chars);
        estimate_persisted_context_usage(&turns, compact_threshold, system_prompt_chars)
    };
    Ok(StateSnapshotData {
        protocol_version: super::super::protocol::PROTOCOL_VERSION,
        session_id: session_id.to_string(),
        selected_session,
        auth,
        sessions,
        transcript,
        active_turn,
        context_usage,
    })
}

pub(super) async fn bootstrap_snapshot_data(
    state: &WebAppState,
    session_id: &str,
    controller_id: &str,
) -> Result<StateSnapshotData> {
    let selected_session = load_selected_session(state, session_id).await?;
    let auth = snapshot_auth_summary(state, controller_id).await;
    let sessions = vec![SessionSummary {
        session_id: selected_session.session_id.clone(),
        title: selected_session.title.clone(),
        updated_at: selected_session.updated_at.clone(),
    }];
    let active_turn = state
        .runtime_manager
        .active_turn(session_id)
        .map(active_turn_snapshot);
    let context_usage = bootstrap_context_usage(state, session_id);

    Ok(StateSnapshotData {
        protocol_version: super::super::protocol::PROTOCOL_VERSION,
        session_id: session_id.to_string(),
        selected_session,
        auth,
        sessions,
        transcript: Vec::new(),
        active_turn,
        context_usage,
    })
}

pub(super) async fn task_watch_bootstrap_data(
    state: &WebAppState,
    session_id: &str,
    request_id: &str,
    task_id: &str,
    after_sequence: i64,
) -> Result<TaskWatchBootstrapData> {
    let session_id = session_id.to_string();
    let request_id = request_id.to_string();
    let task_id = task_id.to_string();
    let store_ref = state.store.clone();

    store::store_run(&store_ref, move |store| {
        let selected_task = TaskRecord::get_owned_by_session(store, &task_id, &session_id)?;
        let all_tasks = TaskRecord::list(store)?
            .into_iter()
            .filter(|task| task.owner_session_id.as_deref() == Some(session_id.as_str()))
            .collect::<Vec<_>>();
        let owned_task_ids = all_tasks
            .iter()
            .map(|task| task.task_id.clone())
            .collect::<HashSet<_>>();
        let all_edges = TaskEdgeRecord::list(store)?
            .into_iter()
            .filter(|edge| {
                owned_task_ids.contains(&edge.task_id)
                    && owned_task_ids.contains(&edge.depends_on_task_id)
            })
            .collect::<Vec<_>>();
        let related_task_ids = collect_related_task_ids(&task_id, &all_tasks, &all_edges);

        let related_tasks = all_tasks
            .into_iter()
            .filter(|task| related_task_ids.contains(&task.task_id))
            .collect::<Vec<_>>();
        let related_edges = all_edges
            .into_iter()
            .filter(|edge| {
                related_task_ids.contains(&edge.task_id)
                    && related_task_ids.contains(&edge.depends_on_task_id)
            })
            .collect::<Vec<_>>();

        let all_events = TaskEventRecord::list_for_task(store, &task_id, 0)?;
        let replay_events = if after_sequence == 0 {
            all_events.clone()
        } else {
            TaskEventRecord::list_for_task(store, &task_id, after_sequence)?
        };
        let latest_sequence = all_events.last().map(|event| event.sequence).unwrap_or(0);
        let cursor = TaskWatchCursor {
            after_sequence,
            latest_sequence,
        };

        let graph = build_task_graph_snapshot(store, &related_tasks, &related_edges)?;
        let attempts = TaskAttemptRecord::list_for_task(store, &task_id)?;
        let task_detail = build_task_detail_snapshot(store, &selected_task, &attempts)?;
        let attempt_snapshots = attempts
            .iter()
            .map(task_attempt_snapshot)
            .collect::<Vec<_>>();

        Ok(TaskWatchBootstrapData {
            list_snapshot: TaskListSnapshotData {
                request_id: request_id.clone(),
                session_id: session_id.clone(),
                task_id: task_id.clone(),
                cursor: cursor.clone(),
                graph: graph.clone(),
            },
            detail_snapshot: TaskDetailSnapshotData {
                request_id,
                session_id,
                task_id,
                cursor,
                graph,
                task: task_detail,
                attempts: attempt_snapshots,
            },
            replay_events,
        })
    })
    .await
}

pub(super) async fn task_event_records(
    state: &WebAppState,
    task_id: &str,
    after_sequence: i64,
) -> Result<Vec<TaskEventRecord>> {
    let task_id = task_id.to_string();
    let store_ref = state.store.clone();
    store::store_run(&store_ref, move |store| {
        TaskEventRecord::list_for_task(store, &task_id, after_sequence)
    })
    .await
}

fn bootstrap_context_usage(
    state: &WebAppState,
    session_id: &str,
) -> super::super::protocol::ContextUsage {
    if let Some((used_chars, max_chars)) = state.runtime_manager.context_usage_chars(session_id) {
        let (input_tokens, output_tokens, total_tokens) = state
            .runtime_manager
            .token_usage(session_id)
            .unwrap_or((None, None, None));
        return context_usage_from_chars(
            used_chars,
            max_chars,
            input_tokens,
            output_tokens,
            total_tokens,
            None,
        );
    }

    let max_chars = state
        .runtime_manager
        .compact_threshold_chars(session_id)
        .unwrap_or_else(|| CompactConfig::default().threshold_chars);
    context_usage_from_chars(0, max_chars, None, None, None, None)
}

fn build_task_graph_snapshot(
    store: &Store,
    tasks: &[TaskRecord],
    edges: &[TaskEdgeRecord],
) -> Result<TaskGraphSnapshot> {
    let nodes = tasks
        .iter()
        .map(|task| task_graph_node_snapshot(store, task))
        .collect::<Result<Vec<_>>>()?;
    let edges = edges
        .iter()
        .map(|edge| TaskGraphEdgeSnapshot {
            task_id: edge.task_id.clone(),
            depends_on_task_id: edge.depends_on_task_id.clone(),
        })
        .collect();

    Ok(TaskGraphSnapshot { nodes, edges })
}

fn task_graph_node_snapshot(store: &Store, task: &TaskRecord) -> Result<TaskGraphNodeSnapshot> {
    let state = TaskRecord::current_state(store, &task.task_id)?.to_string();
    let attempts = TaskAttemptRecord::list_for_task(store, &task.task_id)?;
    let latest_attempt = attempts.last();

    Ok(TaskGraphNodeSnapshot {
        task_id: task.task_id.clone(),
        parent_task_id: task.parent_task_id.clone(),
        title: task.title.clone(),
        priority: task.priority,
        state,
        latest_attempt_id: latest_attempt.map(|attempt| attempt.attempt_id.clone()),
        latest_attempt_state: latest_attempt.map(|attempt| attempt.status.clone()),
        child_session_id: latest_attempt.and_then(|attempt| attempt.session_id.clone()),
    })
}

fn build_task_detail_snapshot(
    store: &Store,
    task: &TaskRecord,
    attempts: &[TaskAttemptRecord],
) -> Result<TaskDetailSnapshot> {
    let state = TaskRecord::current_state(store, &task.task_id)?.to_string();
    let latest_attempt = attempts.last();

    Ok(TaskDetailSnapshot {
        task_id: task.task_id.clone(),
        parent_task_id: task.parent_task_id.clone(),
        title: task.title.clone(),
        priority: task.priority,
        state,
        policy_snapshot: parse_json_value(&task.policy_snapshot),
        parent_close_policy: task.parent_close_policy.clone(),
        recovery_checkpoint: task.recovery_checkpoint.as_deref().map(parse_json_value),
        latest_attempt_id: latest_attempt.map(|attempt| attempt.attempt_id.clone()),
        latest_attempt_state: latest_attempt.map(|attempt| attempt.status.clone()),
        child_session_id: latest_attempt.and_then(|attempt| attempt.session_id.clone()),
        created_at: task.created_at.to_rfc3339(),
        updated_at: task.updated_at.to_rfc3339(),
    })
}

fn task_attempt_snapshot(attempt: &TaskAttemptRecord) -> TaskAttemptSnapshot {
    TaskAttemptSnapshot {
        attempt_id: attempt.attempt_id.clone(),
        state: attempt.status.clone(),
        child_session_id: attempt.session_id.clone(),
        recovery_checkpoint: attempt.recovery_checkpoint.as_deref().map(parse_json_value),
        created_at: attempt.created_at.to_rfc3339(),
        updated_at: attempt.updated_at.to_rfc3339(),
    }
}

fn parse_json_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn collect_related_task_ids(
    selected_task_id: &str,
    tasks: &[TaskRecord],
    edges: &[TaskEdgeRecord],
) -> HashSet<String> {
    let mut by_parent = HashMap::<String, Vec<String>>::new();
    for task in tasks {
        if let Some(parent_task_id) = &task.parent_task_id {
            by_parent
                .entry(parent_task_id.clone())
                .or_default()
                .push(task.task_id.clone());
        }
    }

    let mut related = HashSet::from([selected_task_id.to_string()]);
    let mut queue = VecDeque::from([selected_task_id.to_string()]);

    while let Some(task_id) = queue.pop_front() {
        for edge in edges {
            let adjacent = if edge.task_id == task_id {
                Some(edge.depends_on_task_id.clone())
            } else if edge.depends_on_task_id == task_id {
                Some(edge.task_id.clone())
            } else {
                None
            };

            if let Some(adjacent) = adjacent
                && related.insert(adjacent.clone())
            {
                queue.push_back(adjacent);
            }
        }

        if let Some(task) = tasks.iter().find(|task| task.task_id == task_id)
            && let Some(parent_task_id) = &task.parent_task_id
            && related.insert(parent_task_id.clone())
        {
            queue.push_back(parent_task_id.clone());
        }

        if let Some(children) = by_parent.get(&task_id) {
            for child_task_id in children {
                if related.insert(child_task_id.clone()) {
                    queue.push_back(child_task_id.clone());
                }
            }
        }
    }

    related
}

async fn snapshot_auth_summary(state: &WebAppState, controller_id: &str) -> AuthStateSnapshot {
    let fallback = AuthStateSnapshot {
        storage_available: true,
        storage_error: None,
        active_provider: None,
        openai_logged_in: false,
        zai_logged_in: false,
        pending_openai_login: state.pending_openai_login(controller_id),
    };

    let state = state.clone();
    let controller_id = controller_id.to_string();
    match timeout(
        SNAPSHOT_AUTH_TIMEOUT,
        task::spawn_blocking(move || state.auth_summary(&controller_id)),
    )
    .await
    {
        Ok(Ok(summary)) => summary,
        Ok(Err(_)) | Err(_) => fallback,
    }
}

async fn load_selected_session(state: &WebAppState, session_id: &str) -> Result<SelectedSession> {
    let session_id = session_id.to_string();
    let store_ref = state.store.clone();
    let session = store::store_run(&store_ref, move |store| {
        Session::find(store, &session_id)?.ok_or_else(|| anyhow::anyhow!("session not found"))
    })
    .await?;
    let (model, provider) = effective_model_provider(&session);

    Ok(SelectedSession {
        session_id: session.id,
        title: session
            .title
            .unwrap_or_else(|| "Untitled session".to_string()),
        status: session.status.to_string(),
        provider,
        model,
        created_at: session.created_at.to_rfc3339(),
        updated_at: session.updated_at.to_rfc3339(),
    })
}

fn effective_model_provider(session: &Session) -> (String, String) {
    let mut model = session.model.clone();
    let mut provider = session.provider.clone();
    if let Some(settings) = SessionSettingsOverrides::from_session(session) {
        settings.apply_model_provider_overrides(&mut model, &mut provider);
    }
    (model, provider)
}

async fn load_turns(state: &WebAppState, session_id: &str) -> Result<Vec<Turn>> {
    let session_id = session_id.to_string();
    let store_ref = state.store.clone();
    store::store_run(&store_ref, move |store| {
        Turn::list_for_session(store, &session_id)
    })
    .await
}

fn turns_to_transcript(turns: &[Turn]) -> Vec<TranscriptEntry> {
    turns
        .iter()
        .map(|turn| TranscriptEntry {
            turn_number: turn.turn_number,
            kind: turn.kind.clone(),
            role: turn.role.clone(),
            content: turn.content.clone(),
        })
        .collect()
}

fn active_turn_snapshot(active: ActiveTurnReplay) -> ActiveTurnSnapshot {
    ActiveTurnSnapshot {
        request_id: active.request_id,
        turn_id: active.turn_id,
        message_id: active.message_id,
        content: active.content,
    }
}
