use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use uuid::Uuid;

use super::settings::{InheritedSettingsSnapshot, spawn_autonomous_child_task_with_policy};
use super::{SessionSettingsOverrides, canonical_settings_json};
use crate::auth::ResolvedAuth;
use crate::compact::CompactConfig;
use crate::events::{AgentEvent, EventEmitter};
use crate::lsp::{builtin_server_for_extension, builtin_server_for_path, resolve_workspace_root};
use crate::provider::{Provider, SendContext, TokenUsage, ToolCall, TurnResult, merge_token_usage};
use crate::store::{
    AttemptLifecycleState, NewSession, NewTaskAttemptRecord, NewTaskEdgeRecord, NewTaskEventRecord,
    NewTurn, Session, SessionStatus, SharedStore, Store, TaskAttemptRecord, TaskEdgeRecord,
    TaskEventRecord, TaskLifecycleState, TaskRecord, Turn,
};
use crate::text::truncate_with_ascii_ellipsis;
use crate::tools::ToolRegistry;
use crate::tools::editing::EditObservation;

const DEFAULT_OPENAI_MODEL: &str = "gpt-5.3-codex-spark";
const DEFAULT_ZAI_MODEL: &str = "glm-4.7";
const REQUEST_COMPACTION_MARGIN_CHARS: usize = 8_192;
const CONTEXT_OVERFLOW_RETRY_LIMIT: usize = 2;
const CHILD_HANDOFF_SUMMARY_MAX_CHARS: usize = 12_000;
const CHILD_HANDOFF_ARTIFACT_LIMIT: usize = 16;
const CHILD_HANDOFF_ARTIFACT_ID_MAX_CHARS: usize = 256;

/// Re-export Message from the ZAI provider for backwards compatibility.
pub use crate::provider::zai::Message;

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    SessionCreated {
        session_id: String,
    },
    SessionResumed {
        session_id: String,
    },
    HistoryLoaded {
        turns: usize,
    },
    ToolCallStarted {
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolCallCompleted {
        call_id: String,
        name: String,
        output_preview: String,
        edit_observation: Option<Box<EditObservation>>,
    },
}

type DeltaHook = Arc<dyn Fn(&str) + Send + Sync>;
type EventHook = Arc<dyn Fn(RuntimeEvent) + Send + Sync>;
type ToolApprovalHook = Arc<dyn Fn(&ToolCall) -> bool + Send + Sync>;

#[derive(Default, Clone)]
pub struct RuntimeHooks {
    pub on_output_delta: Option<DeltaHook>,
    pub on_event: Option<EventHook>,
    pub on_tool_approval: Option<ToolApprovalHook>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortResult {
    Aborted { session_id: String },
    NoActiveTurn { session_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitResult {
    Completed {
        turn_id: String,
        message_id: String,
        turn_number: usize,
        response: String,
    },
    Failed {
        turn_id: String,
        message_id: String,
        turn_number: usize,
        error: String,
    },
    Aborted {
        turn_id: String,
        message_id: String,
        turn_number: usize,
        result: AbortResult,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnCorrelation {
    pub turn_id: String,
    pub message_id: String,
}

enum RuntimeStore<'a> {
    Borrowed(&'a Store),
    Shared(SharedStore),
}

pub struct SessionRuntime<'a> {
    store: RuntimeStore<'a>,
    events: EventEmitter,
    compact_config: CompactConfig,
    resolved: ResolvedAuth,
    model: String,
    session: Session,
    history: Vec<serde_json::Value>,
    turn_number: usize,
    provider: Box<dyn Provider>,
    registry: ToolRegistry,
    instructions: String,
    hooks: RuntimeHooks,
    abort_signal: Arc<AtomicBool>,
    active_turn: bool,
    reasoning_effort: Option<String>,
}

struct InitializedSessionState {
    session: Session,
    history: Vec<serde_json::Value>,
    turn_number: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildSessionBootstrapMode {
    CreateNew,
    LinkExisting { session_id: String },
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChildHandoffContract {
    pub summary: String,
    pub artifact_ids: Vec<String>,
    pub inherited_settings: InheritedSettingsSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegatedChildBootstrapOutcome {
    Started {
        child_session_id: Option<String>,
        handoff: ChildHandoffContract,
    },
    InterruptedRetryable {
        handoff: ChildHandoffContract,
        error: String,
    },
}

#[derive(Debug, serde::Deserialize)]
struct DelegateTaskArgs {
    parent_task_id: String,
    #[serde(default)]
    child_task_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default = "default_delegate_priority")]
    priority: i64,
    handoff_brief: String,
    #[serde(default)]
    artifact_ids: Vec<String>,
    #[serde(default)]
    requested_policy_json: Option<String>,
    #[serde(default)]
    after_sequence: Option<i64>,
}

#[derive(Debug, serde::Deserialize)]
struct ReportStatusArgs {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    after_sequence: Option<i64>,
}

struct LspToolTarget {
    server_id: String,
    command: Vec<String>,
    workspace_root: PathBuf,
    last_file: Option<String>,
}

fn default_delegate_priority() -> i64 {
    0
}

#[allow(clippy::too_many_arguments)]
pub fn bootstrap_delegated_child_session(
    store: &Store,
    task_id: &str,
    attempt_id: &str,
    parent_session_id: Option<&str>,
    inherited_settings_override: Option<InheritedSettingsSnapshot>,
    handoff_summary: &str,
    artifact_ids: Vec<String>,
    child_session_mode: ChildSessionBootstrapMode,
) -> Result<DelegatedChildBootstrapOutcome> {
    let attempt = TaskAttemptRecord::get(store, attempt_id)?;
    if attempt.task_id != task_id {
        anyhow::bail!("attempt {attempt_id} does not belong to task {task_id}");
    }

    let inherited_settings = if let Some(snapshot) = inherited_settings_override {
        snapshot
    } else if let Some(snapshot) =
        inherited_settings_from_checkpoint(attempt.recovery_checkpoint.as_deref())?
    {
        snapshot
    } else {
        let parent_session_id = parent_session_id.context(
            "delegated child bootstrap requires parent_session_id when checkpoint has no inherited_settings",
        )?;
        let parent_session = Session::get(store, parent_session_id)?;
        InheritedSettingsSnapshot::from_parent_session(&parent_session)
    };

    let handoff = ChildHandoffContract {
        summary: truncate(handoff_summary, CHILD_HANDOFF_SUMMARY_MAX_CHARS),
        artifact_ids: bounded_artifact_ids(artifact_ids),
        inherited_settings,
    };

    ensure_task_attempt_running(store, task_id, attempt_id)?;
    persist_child_bootstrap_checkpoint(store, attempt_id, None, &handoff, "starting", None, false)?;

    let child_session_result = (|| -> Result<Option<String>> {
        match child_session_mode {
            ChildSessionBootstrapMode::CreateNew => {
                let mut child_session = Session::create(
                    store,
                    NewSession {
                        model: handoff.inherited_settings.model.clone(),
                        provider: handoff.inherited_settings.provider.clone(),
                    },
                )?;
                let settings_json = handoff.inherited_settings.canonical_settings_json();
                Session::update_settings(store, &child_session.id, &settings_json)?;
                child_session.settings = Some(settings_json);

                Turn::append(
                    store,
                    NewTurn {
                        session_id: child_session.id.clone(),
                        kind: "message".to_string(),
                        role: "user".to_string(),
                        content: child_bootstrap_message(&handoff),
                        model: None,
                        tokens_in: None,
                        tokens_out: None,
                    },
                )?;

                Ok(Some(child_session.id))
            }
            ChildSessionBootstrapMode::LinkExisting { session_id } => Ok(Some(session_id)),
            ChildSessionBootstrapMode::Skip => Ok(None),
        }
    })();

    match child_session_result {
        Ok(child_session_id) => {
            if let Some(session_id) = child_session_id.as_deref()
                && let Err(err) = link_attempt_session(store, task_id, attempt_id, session_id)
            {
                let error = format!("{err:#}");
                mark_bootstrap_interrupted_retryable(store, task_id, attempt_id, &handoff, &error)?;
                return Ok(DelegatedChildBootstrapOutcome::InterruptedRetryable { handoff, error });
            }

            persist_child_bootstrap_checkpoint(
                store,
                attempt_id,
                child_session_id.as_deref(),
                &handoff,
                "ready",
                None,
                false,
            )?;
            append_child_bootstrap_ready_event(
                store,
                task_id,
                attempt_id,
                child_session_id.as_deref(),
            )?;

            Ok(DelegatedChildBootstrapOutcome::Started {
                child_session_id,
                handoff,
            })
        }
        Err(err) => {
            let error = format!("{err:#}");
            mark_bootstrap_interrupted_retryable(store, task_id, attempt_id, &handoff, &error)?;
            Ok(DelegatedChildBootstrapOutcome::InterruptedRetryable { handoff, error })
        }
    }
}

fn inherited_settings_from_checkpoint(
    recovery_checkpoint: Option<&str>,
) -> Result<Option<InheritedSettingsSnapshot>> {
    let Some(raw) = recovery_checkpoint else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_str(raw)
        .context("failed to parse delegated child recovery checkpoint JSON")?;
    let Some(inherited_settings) = parsed
        .get("child_bootstrap")
        .and_then(|value| value.get("handoff"))
        .and_then(|value| value.get("inherited_settings"))
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };

    let snapshot: InheritedSettingsSnapshot = serde_json::from_value(inherited_settings.clone())
        .context("failed to decode delegated child inherited_settings from checkpoint")?;
    Ok(Some(snapshot))
}

fn run_immediate_transaction<T>(store: &Store, f: impl FnOnce() -> Result<T>) -> Result<T> {
    store
        .conn()
        .execute_batch("BEGIN IMMEDIATE TRANSACTION")
        .context("failed to begin transaction")?;

    match f() {
        Ok(result) => {
            store
                .conn()
                .execute_batch("COMMIT TRANSACTION")
                .context("failed to commit transaction")?;
            Ok(result)
        }
        Err(err) => {
            let _ = store.conn().execute_batch("ROLLBACK TRANSACTION");
            Err(err)
        }
    }
}

fn bounded_artifact_ids(artifact_ids: Vec<String>) -> Vec<String> {
    artifact_ids
        .into_iter()
        .map(|id| truncate(id.trim(), CHILD_HANDOFF_ARTIFACT_ID_MAX_CHARS))
        .filter(|id| !id.is_empty())
        .take(CHILD_HANDOFF_ARTIFACT_LIMIT)
        .collect()
}

fn child_bootstrap_message(handoff: &ChildHandoffContract) -> String {
    let mut message = crate::compact::format_handoff_summary(&handoff.summary);
    if !handoff.artifact_ids.is_empty() {
        message.push_str("\n\nReferenced artifacts:\n");
        for artifact_id in &handoff.artifact_ids {
            message.push_str(&format!("- {artifact_id}\n"));
        }
    }
    message.trim_end().to_string()
}

fn ensure_task_attempt_running(store: &Store, task_id: &str, attempt_id: &str) -> Result<()> {
    let task_state = TaskRecord::current_state(store, task_id)?;
    let attempt_state = TaskAttemptRecord::get(store, attempt_id)?.state()?;

    if attempt_state == AttemptLifecycleState::Running && task_state != TaskLifecycleState::Running
    {
        TaskRecord::transition_state(store, task_id, attempt_id, TaskLifecycleState::Running)?;
    }

    Ok(())
}

fn mark_bootstrap_interrupted_retryable(
    store: &Store,
    task_id: &str,
    attempt_id: &str,
    handoff: &ChildHandoffContract,
    error: &str,
) -> Result<()> {
    let attempt_state = TaskAttemptRecord::get(store, attempt_id)?.state()?;
    if matches!(
        attempt_state,
        AttemptLifecycleState::Queued
            | AttemptLifecycleState::Ready
            | AttemptLifecycleState::Running
            | AttemptLifecycleState::Blocked
            | AttemptLifecycleState::CancelRequested
    ) {
        TaskAttemptRecord::transition_state(store, attempt_id, AttemptLifecycleState::Interrupted)?;
    }

    let task_state = TaskRecord::current_state(store, task_id)?;
    if matches!(
        task_state,
        TaskLifecycleState::Queued
            | TaskLifecycleState::Ready
            | TaskLifecycleState::Running
            | TaskLifecycleState::Blocked
            | TaskLifecycleState::CancelRequested
    ) {
        TaskRecord::transition_state(store, task_id, attempt_id, TaskLifecycleState::Interrupted)?;
    }

    persist_child_bootstrap_checkpoint(
        store,
        attempt_id,
        None,
        handoff,
        "interrupted",
        Some(error),
        true,
    )?;

    Ok(())
}

fn link_attempt_session(
    store: &Store,
    task_id: &str,
    attempt_id: &str,
    session_id: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    store
        .conn()
        .execute(
            "UPDATE task_attempts SET session_id = ?1, updated_at = ?2 WHERE attempt_id = ?3",
            (session_id, &now, attempt_id),
        )
        .context("failed to link child session to task attempt")?;

    TaskEventRecord::append(
        store,
        NewTaskEventRecord {
            task_id: task_id.to_string(),
            attempt_id: attempt_id.to_string(),
            session_id: Some(session_id.to_string()),
            event_type: "attempt.child_session.linked".to_string(),
            payload: serde_json::json!({
                "session_id": session_id,
            })
            .to_string(),
        },
    )?;

    Ok(())
}

fn append_child_bootstrap_ready_event(
    store: &Store,
    task_id: &str,
    attempt_id: &str,
    child_session_id: Option<&str>,
) -> Result<()> {
    TaskEventRecord::append(
        store,
        NewTaskEventRecord {
            task_id: task_id.to_string(),
            attempt_id: attempt_id.to_string(),
            session_id: child_session_id.map(str::to_string),
            event_type: "attempt.child_session.bootstrap.ready".to_string(),
            payload: serde_json::json!({
                "status": "ready",
                "child_session_id": child_session_id,
            })
            .to_string(),
        },
    )?;

    Ok(())
}

fn persist_child_bootstrap_checkpoint(
    store: &Store,
    attempt_id: &str,
    child_session_id: Option<&str>,
    handoff: &ChildHandoffContract,
    status: &str,
    error: Option<&str>,
    retryable: bool,
) -> Result<()> {
    let existing = TaskAttemptRecord::get(store, attempt_id)?.recovery_checkpoint;
    let checkpoint = merge_child_bootstrap_checkpoint(
        existing.as_deref(),
        child_session_id,
        handoff,
        status,
        error,
        retryable,
    )?;

    let now = chrono::Utc::now().to_rfc3339();
    store
        .conn()
        .execute(
            "UPDATE task_attempts SET recovery_checkpoint = ?1, updated_at = ?2 WHERE attempt_id = ?3",
            (&checkpoint, &now, attempt_id),
        )
        .context("failed to update delegated child bootstrap checkpoint")?;
    Ok(())
}

fn merge_child_bootstrap_checkpoint(
    existing: Option<&str>,
    child_session_id: Option<&str>,
    handoff: &ChildHandoffContract,
    status: &str,
    error: Option<&str>,
    retryable: bool,
) -> Result<String> {
    let mut checkpoint = existing
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !checkpoint.is_object() {
        checkpoint = serde_json::json!({});
    }

    checkpoint["child_bootstrap"] = serde_json::json!({
        "status": status,
        "retryable": retryable,
        "child_session_id": child_session_id,
        "handoff": handoff,
        "error": error,
    });

    serde_json::to_string(&checkpoint).context("failed to serialize child bootstrap checkpoint")
}

impl<'a> SessionRuntime<'a> {
    pub fn default_model(provider: &str) -> String {
        match provider {
            "openai" => DEFAULT_OPENAI_MODEL.to_string(),
            "zai" => DEFAULT_ZAI_MODEL.to_string(),
            _ => DEFAULT_OPENAI_MODEL.to_string(),
        }
    }

    pub fn emit_runtime_event(&self, event: RuntimeEvent) {
        if let Some(hook) = &self.hooks.on_event {
            hook(event);
        }
    }

    pub fn emit_output_delta(&self, delta: &str) {
        if let Some(hook) = &self.hooks.on_output_delta {
            hook(delta);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: &'a Store,
        resolved: ResolvedAuth,
        model_override: Option<&str>,
        resume_session_id: Option<&str>,
        events: EventEmitter,
        compact_config: CompactConfig,
        registry: ToolRegistry,
        instructions: String,
        hooks: RuntimeHooks,
    ) -> Result<Self> {
        Self::new_with_abort_signal(
            store,
            resolved,
            model_override,
            resume_session_id,
            events,
            compact_config,
            registry,
            instructions,
            hooks,
            Arc::new(AtomicBool::new(false)),
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_abort_signal(
        store: &'a Store,
        resolved: ResolvedAuth,
        model_override: Option<&str>,
        resume_session_id: Option<&str>,
        events: EventEmitter,
        mut compact_config: CompactConfig,
        mut registry: ToolRegistry,
        instructions: String,
        hooks: RuntimeHooks,
        abort_signal: Arc<AtomicBool>,
        reasoning_effort: Option<String>,
    ) -> Result<Self> {
        let model = model_override
            .map(String::from)
            .unwrap_or_else(|| Self::default_model(&resolved.provider));
        let InitializedSessionState {
            session,
            history,
            turn_number,
        } = initialize_session_state(
            store,
            &model,
            &resolved.provider,
            resume_session_id,
            &mut compact_config,
            &hooks,
        )?;
        registry.bind_session_context(&session.id);

        let provider = create_provider(&resolved.provider);
        Ok(Self {
            store: RuntimeStore::Borrowed(store),
            events,
            compact_config,
            resolved,
            model,
            session,
            history,
            turn_number,
            provider,
            registry,
            instructions,
            abort_signal,
            hooks,
            active_turn: false,
            reasoning_effort,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_shared_store_and_abort_signal(
        shared_store: SharedStore,
        resolved: ResolvedAuth,
        model_override: Option<&str>,
        resume_session_id: Option<&str>,
        events: EventEmitter,
        mut compact_config: CompactConfig,
        mut registry: ToolRegistry,
        instructions: String,
        hooks: RuntimeHooks,
        abort_signal: Arc<AtomicBool>,
        reasoning_effort: Option<String>,
    ) -> Result<Self> {
        let model = model_override
            .map(String::from)
            .unwrap_or_else(|| Self::default_model(&resolved.provider));

        let store_guard = shared_store
            .lock()
            .map_err(|e| anyhow::anyhow!("store mutex poisoned: {e}"))?;
        let InitializedSessionState {
            session,
            history,
            turn_number,
        } = initialize_session_state(
            &store_guard,
            &model,
            &resolved.provider,
            resume_session_id,
            &mut compact_config,
            &hooks,
        )?;
        registry.bind_session_context(&session.id);
        drop(store_guard);

        let provider = create_provider(&resolved.provider);
        Ok(Self {
            store: RuntimeStore::Shared(shared_store),
            events,
            compact_config,
            resolved,
            model,
            session,
            history,
            turn_number,
            provider,
            registry,
            instructions,
            abort_signal,
            hooks,
            active_turn: false,
            reasoning_effort,
        })
    }

    fn with_store<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Store) -> Result<T>,
    {
        match &self.store {
            RuntimeStore::Borrowed(store) => f(store),
            RuntimeStore::Shared(shared) => {
                let guard = shared
                    .lock()
                    .map_err(|e| anyhow::anyhow!("store mutex poisoned: {e}"))?;
                f(&guard)
            }
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn provider(&self) -> &str {
        &self.resolved.provider
    }

    pub fn session_id(&self) -> &str {
        &self.session.id
    }

    fn execute_delegate_task_entrypoint(&self, args: &serde_json::Value) -> Result<String> {
        let parsed: DelegateTaskArgs =
            serde_json::from_value(args.clone()).context("invalid delegate_task arguments")?;
        let parent_task_id = parsed.parent_task_id.trim();
        if parent_task_id.is_empty() {
            anyhow::bail!("delegate_task.parent_task_id must not be empty");
        }

        let handoff_brief = parsed.handoff_brief.trim();
        if handoff_brief.is_empty() {
            anyhow::bail!("delegate_task.handoff_brief must not be empty");
        }

        let child_task_id = parsed
            .child_task_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("task-{}", Uuid::new_v4()));

        let requested_policy = parsed
            .requested_policy_json
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let after_sequence = parsed.after_sequence.unwrap_or(0);

        self.with_store(|store| {
            run_immediate_transaction(store, || {
                let _ =
                    TaskRecord::ensure_owned_by_session(store, parent_task_id, &self.session.id)?;
                let task = spawn_autonomous_child_task_with_policy(
                    store,
                    &self.registry,
                    parent_task_id,
                    &child_task_id,
                    parsed.title.clone(),
                    parsed.priority,
                    requested_policy.as_deref(),
                )?;

                TaskEdgeRecord::create(
                    store,
                    NewTaskEdgeRecord {
                        task_id: task.task_id.clone(),
                        depends_on_task_id: parent_task_id.to_string(),
                    },
                )?;

                let attempt_id = Uuid::new_v4().to_string();
                TaskAttemptRecord::create(
                    store,
                    NewTaskAttemptRecord {
                        attempt_id: attempt_id.clone(),
                        task_id: task.task_id.clone(),
                        session_id: None,
                        status: AttemptLifecycleState::Queued.to_string(),
                        recovery_checkpoint: None,
                    },
                )?;

                let bootstrap_outcome = bootstrap_delegated_child_session(
                    store,
                    &task.task_id,
                    &attempt_id,
                    Some(&self.session.id),
                    None,
                    handoff_brief,
                    parsed.artifact_ids.clone(),
                    ChildSessionBootstrapMode::CreateNew,
                )?;

                let events = TaskEventRecord::list_for_task(store, &task.task_id, after_sequence)?;
                let latest_sequence = events
                    .last()
                    .map(|event| event.sequence)
                    .unwrap_or(after_sequence);
                let event_payloads = events.iter().map(task_event_payload).collect::<Vec<_>>();
                let task_state = TaskRecord::current_state(store, &task.task_id)?;

                let bootstrap_json = match bootstrap_outcome {
                    DelegatedChildBootstrapOutcome::Started {
                        child_session_id,
                        handoff,
                    } => serde_json::json!({
                        "status": "started",
                        "child_session_id": child_session_id,
                        "handoff": handoff,
                    }),
                    DelegatedChildBootstrapOutcome::InterruptedRetryable { handoff, error } => {
                        serde_json::json!({
                            "status": "interrupted_retryable",
                            "handoff": handoff,
                            "error": error,
                        })
                    }
                };

                Ok(serde_json::json!({
                    "task_id": task.task_id,
                    "attempt_id": attempt_id,
                    "task_state": task_state.to_string(),
                    "bootstrap": bootstrap_json,
                    "event_stream": {
                        "after_sequence": after_sequence,
                        "next_after_sequence": latest_sequence,
                        "events": event_payloads,
                    }
                })
                .to_string())
            })
        })
    }

    fn maybe_execute_task_status_report(&self, args: &serde_json::Value) -> Result<Option<String>> {
        let parsed: ReportStatusArgs =
            serde_json::from_value(args.clone()).context("invalid report_status arguments")?;

        let Some(task_id) = parsed
            .task_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };

        let after_sequence = parsed.after_sequence.unwrap_or(0);
        let summary = parsed.summary.unwrap_or_default();
        self.with_store(|store| {
            let _ = TaskRecord::ensure_owned_by_session(store, task_id, &self.session.id)?;
            let task_state = TaskRecord::current_state(store, task_id)?;
            let events = TaskEventRecord::list_for_task(store, task_id, after_sequence)?;
            let next_after_sequence = events
                .last()
                .map(|event| event.sequence)
                .unwrap_or(after_sequence);
            let event_payloads = events.iter().map(task_event_payload).collect::<Vec<_>>();

            Ok(Some(
                serde_json::json!({
                    "summary": summary,
                    "task_id": task_id,
                    "task_state": task_state.to_string(),
                    "after_sequence": after_sequence,
                    "next_after_sequence": next_after_sequence,
                    "events": event_payloads,
                })
                .to_string(),
            ))
        })
    }

    fn detect_lsp_tool_target(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Option<LspToolTarget> {
        let project_dir = std::env::current_dir().ok()?;
        detect_lsp_tool_target(&project_dir, tool_name, args)
    }

    fn emit_lsp_detected_status(&self, target: &LspToolTarget) {
        let detail = target
            .last_file
            .as_deref()
            .map(|last_file| format!("matched {last_file} for {}", target.server_id))
            .unwrap_or_else(|| format!("matched {}", target.server_id));
        self.events.emit(AgentEvent::StatusReport {
            session_id: Some(self.session.id.clone()),
            turn_id: None,
            status: "lsp.detected".to_string(),
            detail,
            turn_number: self.turn_number,
            server_id: Some(target.server_id.clone()),
            command: Some(target.command.clone()),
            workspace_root: Some(target.workspace_root.display().to_string()),
            last_file: target.last_file.clone(),
            last_error: None,
        });
    }

    pub fn loaded_turns(&self) -> usize {
        self.turn_number
    }

    pub fn abort_turn(&mut self) -> Result<AbortResult> {
        if self.active_turn {
            self.abort_signal.store(true, Ordering::Relaxed);
            self.with_store(|store| {
                Session::update_status(store, &self.session.id, SessionStatus::Aborted)
            })?;
            Ok(AbortResult::Aborted {
                session_id: self.session.id.clone(),
            })
        } else {
            self.abort_signal.store(false, Ordering::Relaxed);
            Ok(AbortResult::NoActiveTurn {
                session_id: self.session.id.clone(),
            })
        }
    }

    pub fn mark_completed(&self) -> Result<()> {
        self.with_store(|store| {
            Session::update_status(store, &self.session.id, SessionStatus::Completed)
        })
    }

    pub async fn submit_prompt(&mut self, input: String) -> Result<SubmitResult> {
        let correlation = TurnCorrelation {
            turn_id: format!("turn-{}", Uuid::new_v4()),
            message_id: format!("msg-{}", Uuid::new_v4()),
        };
        self.submit_prompt_with_ids(input, correlation).await
    }

    pub async fn submit_prompt_with_ids(
        &mut self,
        input: String,
        correlation: TurnCorrelation,
    ) -> Result<SubmitResult> {
        self.abort_signal.store(false, Ordering::Relaxed);
        self.turn_number += 1;
        let turn_number = self.turn_number;
        let turn_id = correlation.turn_id;
        let message_id = correlation.message_id;

        self.with_store(|store| {
            Session::update_status(store, &self.session.id, SessionStatus::Active)?;
            Turn::append(
                store,
                NewTurn {
                    session_id: self.session.id.clone(),
                    kind: "message".into(),
                    role: "user".into(),
                    content: input.clone(),
                    model: None,
                    tokens_in: None,
                    tokens_out: None,
                },
            )?;
            Ok(())
        })?;

        self.history.push(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": input,
        }));

        compact_history_to_budget(
            &self.resolved,
            &self.model,
            &mut self.history,
            &self.compact_config,
            &self.events,
            &RequestBudgetContext {
                provider_name: self.provider.name(),
                instructions: &self.instructions,
                registry: &self.registry,
            },
        )
        .await;

        let (context_used_chars, context_max_chars) = context_usage_chars(
            &self.history,
            self.compact_config.threshold_chars,
            &RequestBudgetContext {
                provider_name: self.provider.name(),
                instructions: &self.instructions,
                registry: &self.registry,
            },
        );

        if self.abort_signal.load(Ordering::Relaxed) {
            return self.finish_aborted_submit(&turn_id, &message_id, turn_number);
        }

        self.events.emit(AgentEvent::TurnStarted {
            session_id: self.session.id.clone(),
            turn_id: turn_id.clone(),
            model: self.model.clone(),
            turn_number,
            context_used_chars,
            context_max_chars,
        });
        self.events.emit(AgentEvent::MessageStarted {
            session_id: self.session.id.clone(),
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
        });

        self.active_turn = true;
        let mut aggregated_usage: Option<TokenUsage> = None;
        let mut context_overflow_retries = 0usize;
        let final_text = loop {
            compact_history_to_budget(
                &self.resolved,
                &self.model,
                &mut self.history,
                &self.compact_config,
                &self.events,
                &RequestBudgetContext {
                    provider_name: self.provider.name(),
                    instructions: &self.instructions,
                    registry: &self.registry,
                },
            )
            .await;

            if self.abort_signal.load(Ordering::Relaxed) {
                return self.finish_aborted_submit(&turn_id, &message_id, turn_number);
            }

            // Extract references from self before creating the closure, so the
            // closure doesn't capture &SessionRuntime (which contains non-Sync
            // rusqlite::Connection).
            let hooks = &self.hooks;
            let events = &self.events;
            let session_id = &self.session.id;
            let output_hook = |delta: &str| {
                if let Some(hook) = &hooks.on_output_delta {
                    hook(delta);
                }
                events.emit(AgentEvent::MessageDelta {
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    message_id: message_id.clone(),
                    delta: delta.to_string(),
                });
            };

            let result = {
                let mut token_usage = None;
                let ctx = SendContext {
                    model: &self.model,
                    session_id: &self.session.id,
                    turn_id: &turn_id,
                    history: &self.history,
                    registry: &self.registry,
                    instructions: &self.instructions,
                    abort_signal: &self.abort_signal,
                    events: &self.events,
                    output_hook: Some(&output_hook),
                    reasoning_effort: self.reasoning_effort.as_deref(),
                };
                self.provider
                    .send(&self.resolved, ctx, &mut token_usage)
                    .await
                    .map(|result| (result, token_usage))
            };

            match result {
                Ok((TurnResult::Text(text), usage)) => {
                    merge_token_usage(&mut aggregated_usage, usage);
                    break Ok((text, aggregated_usage.clone()));
                }
                Ok((TurnResult::Aborted, _)) => {
                    return self.finish_aborted_submit(&turn_id, &message_id, turn_number);
                }
                Ok((TurnResult::ToolCalls(calls), usage)) => {
                    merge_token_usage(&mut aggregated_usage, usage);
                    for call in &calls {
                        if self.abort_signal.load(Ordering::Relaxed) {
                            return self.finish_aborted_submit(&turn_id, &message_id, turn_number);
                        }

                        self.emit_runtime_event(RuntimeEvent::ToolCallStarted {
                            call_id: call.call_id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        });
                        self.events.emit(AgentEvent::ToolCallStarted {
                            session_id: self.session.id.clone(),
                            turn_id: turn_id.clone(),
                            message_id: message_id.clone(),
                            tool_call_id: call.call_id.clone(),
                            tool_name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        });

                        self.with_store(|store| {
                            Turn::append(
                                store,
                                NewTurn {
                                    session_id: self.session.id.clone(),
                                    kind: "function_call".into(),
                                    role: "assistant".into(),
                                    content: serde_json::json!({
                                        "call_id": call.call_id,
                                        "name": call.name,
                                        "arguments": call.arguments,
                                    })
                                    .to_string(),
                                    model: Some(self.model.clone()),
                                    tokens_in: None,
                                    tokens_out: None,
                                },
                            )?;
                            Ok(())
                        })?;

                        let approved = match &self.hooks.on_tool_approval {
                            Some(approve) => approve(call),
                            None => true,
                        };

                        let (output, edit_observation, success) = if !approved {
                            (
                                "Tool execution denied by user. Re-run with --tool-approval auto to allow execution.".to_string(),
                                None,
                                false,
                            )
                        } else {
                            let args: serde_json::Value =
                                serde_json::from_str(&call.arguments).unwrap_or_default();
                            if self.abort_signal.load(Ordering::Relaxed) {
                                return self.finish_aborted_submit(
                                    &turn_id,
                                    &message_id,
                                    turn_number,
                                );
                            }

                            if let Some(lsp_target) = self.detect_lsp_tool_target(&call.name, &args)
                                && lsp_target.last_file.is_some()
                            {
                                self.emit_lsp_detected_status(&lsp_target);
                            }

                            let runtime_handled: Option<Result<String>> = match call.name.as_str() {
                                "delegate_task" => {
                                    Some(self.execute_delegate_task_entrypoint(&args))
                                }
                                "report_status" => {
                                    self.maybe_execute_task_status_report(&args)?.map(Ok)
                                }
                                _ => None,
                            };

                            if let Some(result) = runtime_handled {
                                match result {
                                    Ok(output) => (output, None, true),
                                    Err(err) => (format!("Tool error: {err:#}"), None, false),
                                }
                            } else {
                                match self.registry.get(&call.name) {
                                    Some(tool) => match tool.execute_with_result(args) {
                                        Ok(result) => (
                                            result.output,
                                            result.edit_observations.first().cloned(),
                                            true,
                                        ),
                                        Err(e) => (format!("Tool error: {e:#}"), None, false),
                                    },
                                    None => (
                                        format!("Error: unknown tool '{}'", call.name),
                                        None,
                                        false,
                                    ),
                                }
                            }
                        };

                        let output_preview =
                            tool_output_preview(&output, edit_observation.as_ref());
                        self.emit_runtime_event(RuntimeEvent::ToolCallCompleted {
                            call_id: call.call_id.clone(),
                            name: call.name.clone(),
                            output_preview: output_preview.clone(),
                            edit_observation: edit_observation.clone().map(Box::new),
                        });

                        self.with_store(|store| {
                            let mut content = serde_json::json!({
                                "call_id": call.call_id,
                                "output": output.clone(),
                            });
                            if let Some(observation) = &edit_observation {
                                content["edit_observation"] =
                                    serialize_edit_observation_best_effort(observation);
                            }
                            Turn::append(
                                store,
                                NewTurn {
                                    session_id: self.session.id.clone(),
                                    kind: "function_call_output".into(),
                                    role: "tool".into(),
                                    content: content.to_string(),
                                    model: None,
                                    tokens_in: None,
                                    tokens_out: None,
                                },
                            )?;
                            Ok(())
                        })?;

                        self.history.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": call.call_id,
                            "name": call.name,
                            "arguments": call.arguments,
                        }));
                        let output_item = serde_json::json!({
                            "type": "function_call_output",
                            "call_id": call.call_id,
                            "output": output,
                        });
                        self.history.push(output_item);

                        let (context_used_chars, context_max_chars) = context_usage_chars(
                            &self.history,
                            self.compact_config.threshold_chars,
                            &RequestBudgetContext {
                                provider_name: self.provider.name(),
                                instructions: &self.instructions,
                                registry: &self.registry,
                            },
                        );
                        self.events.emit(AgentEvent::ToolCallCompleted {
                            session_id: self.session.id.clone(),
                            turn_id: turn_id.clone(),
                            message_id: message_id.clone(),
                            tool_call_id: call.call_id.clone(),
                            tool_name: call.name.clone(),
                            output_preview,
                            edit_observation: edit_observation.map(Box::new),
                            success,
                            context_used_chars,
                            context_max_chars,
                        });
                    }
                    continue;
                }
                Err(err)
                    if is_context_window_error(&err)
                        && context_overflow_retries < CONTEXT_OVERFLOW_RETRY_LIMIT =>
                {
                    context_overflow_retries += 1;
                    let retry_config =
                        retry_compact_config(&self.compact_config, context_overflow_retries);

                    let before_retry_request_chars = estimated_request_chars(
                        &self.history,
                        &RequestBudgetContext {
                            provider_name: self.provider.name(),
                            instructions: &self.instructions,
                            registry: &self.registry,
                        },
                    );
                    let before_retry_len = self.history.len();

                    compact_history_to_budget(
                        &self.resolved,
                        &self.model,
                        &mut self.history,
                        &retry_config,
                        &self.events,
                        &RequestBudgetContext {
                            provider_name: self.provider.name(),
                            instructions: &self.instructions,
                            registry: &self.registry,
                        },
                    )
                    .await;

                    let after_retry_request_chars = estimated_request_chars(
                        &self.history,
                        &RequestBudgetContext {
                            provider_name: self.provider.name(),
                            instructions: &self.instructions,
                            registry: &self.registry,
                        },
                    );
                    if after_retry_request_chars >= before_retry_request_chars
                        && self.history.len() == before_retry_len
                    {
                        let removed_items = force_shrink_history_for_overflow_retry(
                            &mut self.history,
                            context_overflow_retries,
                        );
                        if removed_items > 0 {
                            self.events.emit(AgentEvent::HistoryCompacted {
                                session_id: Some(self.session.id.clone()),
                                turn_id: Some(turn_id.clone()),
                                old_items: removed_items,
                                new_chars: crate::compact::estimate_history_chars(&self.history),
                            });
                        }
                    }
                    continue;
                }
                Err(err) => break Err(err),
            }
        };

        self.active_turn = false;
        self.abort_signal.store(false, Ordering::Relaxed);

        match final_text {
            Ok((response, token_usage)) => {
                self.events.emit(AgentEvent::MessageCompleted {
                    session_id: self.session.id.clone(),
                    turn_id: turn_id.clone(),
                    message_id: message_id.clone(),
                    content: response.clone(),
                });

                self.with_store(|store| {
                    Turn::append(
                        store,
                        NewTurn {
                            session_id: self.session.id.clone(),
                            kind: "message".into(),
                            role: "assistant".into(),
                            content: response.clone(),
                            model: Some(self.model.clone()),
                            tokens_in: token_usage
                                .as_ref()
                                .and_then(|u| i64::try_from(u.input_tokens).ok()),
                            tokens_out: token_usage
                                .as_ref()
                                .and_then(|u| i64::try_from(u.output_tokens).ok()),
                        },
                    )?;
                    Ok(())
                })?;

                if turn_number == 1 {
                    let title: String = response.chars().take(80).collect();
                    let title = title.lines().next().unwrap_or(&title);
                    self.with_store(|store| Session::update_title(store, &self.session.id, title))?;
                }

                self.history.push(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": response.clone(),
                }));

                let (context_used_chars, context_max_chars) = context_usage_chars(
                    &self.history,
                    self.compact_config.threshold_chars,
                    &RequestBudgetContext {
                        provider_name: self.provider.name(),
                        instructions: &self.instructions,
                        registry: &self.registry,
                    },
                );
                self.events.emit(AgentEvent::TurnCompleted {
                    session_id: self.session.id.clone(),
                    turn_id: turn_id.clone(),
                    model: self.model.clone(),
                    turn_number,
                    message_id: message_id.clone(),
                    context_used_chars,
                    context_max_chars,
                    input_tokens: token_usage.as_ref().map(|u| u.input_tokens),
                    output_tokens: token_usage.as_ref().map(|u| u.output_tokens),
                    total_tokens: token_usage.as_ref().map(|u| u.total_tokens),
                });

                Ok(SubmitResult::Completed {
                    turn_id,
                    message_id,
                    turn_number,
                    response,
                })
            }
            Err(err) => {
                let error = format!("{err:#}");
                self.events.emit(AgentEvent::TurnFailed {
                    session_id: self.session.id.clone(),
                    turn_id: turn_id.clone(),
                    model: self.model.clone(),
                    turn_number,
                    error: error.clone(),
                });
                Ok(SubmitResult::Failed {
                    turn_id,
                    message_id,
                    turn_number,
                    error,
                })
            }
        }
    }

    fn finish_aborted_submit(
        &mut self,
        turn_id: &str,
        message_id: &str,
        turn_number: usize,
    ) -> Result<SubmitResult> {
        let result = self.abort_turn()?;
        self.events.emit(AgentEvent::TurnFailed {
            session_id: self.session.id.clone(),
            turn_id: turn_id.to_string(),
            model: self.model.clone(),
            turn_number,
            error: "aborted".to_string(),
        });
        self.active_turn = false;
        self.abort_signal.store(false, Ordering::Relaxed);
        Ok(SubmitResult::Aborted {
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
            turn_number,
            result,
        })
    }
}

fn detect_lsp_tool_target(
    project_dir: &std::path::Path,
    tool_name: &str,
    args: &serde_json::Value,
) -> Option<LspToolTarget> {
    let path_key = match tool_name {
        "lsp_prepare_rename" | "lsp_rename" => "filePath",
        "lsp_diagnostics" | "lsp_symbols" | "lsp_goto_definition" | "lsp_find_references" => {
            "file_path"
        }
        _ => return None,
    };

    let raw_path = args.get(path_key)?.as_str()?.trim();
    if raw_path.is_empty() {
        return None;
    }

    let file_path = resolve_tool_path(project_dir, raw_path);
    if file_path.is_file() {
        let server = builtin_server_for_path(&file_path)?;
        return Some(LspToolTarget {
            server_id: server.id.to_string(),
            command: server
                .command
                .iter()
                .map(|part| (*part).to_string())
                .collect(),
            workspace_root: resolve_workspace_root(&file_path, server.id),
            last_file: Some(file_path.display().to_string()),
        });
    }

    if tool_name == "lsp_diagnostics" && file_path.is_dir() {
        let extension = args.get("extension")?.as_str()?;
        let server = builtin_server_for_extension(extension)?;
        return Some(LspToolTarget {
            server_id: server.id.to_string(),
            command: server
                .command
                .iter()
                .map(|part| (*part).to_string())
                .collect(),
            workspace_root: file_path,
            last_file: None,
        });
    }

    None
}

fn resolve_tool_path(project_dir: &std::path::Path, raw_path: &str) -> PathBuf {
    let path = PathBuf::from(raw_path);
    if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    }
}

fn initialize_session_state(
    store: &Store,
    model: &str,
    provider: &str,
    resume_session_id: Option<&str>,
    compact_config: &mut CompactConfig,
    hooks: &RuntimeHooks,
) -> Result<InitializedSessionState> {
    let session = match resume_session_id {
        Some(id) => {
            let session = Session::get(store, id)?;
            restore_compact_threshold_from_settings(compact_config, &session);
            if let Some(hook) = &hooks.on_event {
                hook(RuntimeEvent::SessionResumed {
                    session_id: session.id.clone(),
                });
            }
            session
        }
        None => {
            let mut session = Session::create(
                store,
                NewSession {
                    model: model.to_string(),
                    provider: provider.to_string(),
                },
            )?;
            let settings_json =
                canonical_settings_json(model, provider, compact_config.threshold_chars);
            Session::update_settings(store, &session.id, &settings_json)?;
            session.settings = Some(settings_json);
            if let Some(hook) = &hooks.on_event {
                hook(RuntimeEvent::SessionCreated {
                    session_id: session.id.clone(),
                });
            }
            session
        }
    };

    let existing_turns = Turn::list_for_session(store, &session.id)?;
    let history = history_items_from_turns(&existing_turns);
    if !existing_turns.is_empty()
        && let Some(hook) = &hooks.on_event
    {
        hook(RuntimeEvent::HistoryLoaded {
            turns: existing_turns.len(),
        });
    }

    Ok(InitializedSessionState {
        session,
        history,
        turn_number: existing_turns.len(),
    })
}

fn restore_compact_threshold_from_settings(compact_config: &mut CompactConfig, session: &Session) {
    if let Some(settings) = SessionSettingsOverrides::from_session(session) {
        settings.apply_compact_threshold(compact_config);
    }
}

fn truncate(s: &str, max: usize) -> String {
    truncate_with_ascii_ellipsis(s, max)
}

fn tool_output_preview(output: &str, edit_observation: Option<&EditObservation>) -> String {
    if let Some(observation) = edit_observation
        && let Some(artifact_id) = observation.artifact_id.as_deref()
    {
        return truncate(&format!("artifact_id={artifact_id}"), 80);
    }

    if let Some(artifact_line) = output
        .lines()
        .find(|line| line.trim_start().starts_with("artifact_id="))
    {
        return truncate(artifact_line.trim(), 80);
    }

    truncate(output.lines().next().unwrap_or("(empty)"), 80)
}

fn task_event_payload(event: &TaskEventRecord) -> serde_json::Value {
    let payload = serde_json::from_str::<serde_json::Value>(&event.payload)
        .unwrap_or_else(|_| serde_json::json!({ "raw": event.payload }));
    serde_json::json!({
        "sequence": event.sequence,
        "task_id": &event.task_id,
        "attempt_id": &event.attempt_id,
        "session_id": &event.session_id,
        "event_type": &event.event_type,
        "payload": payload,
        "recorded_at": event.recorded_at.to_rfc3339(),
    })
}

fn serialize_edit_observation_best_effort(observation: &EditObservation) -> serde_json::Value {
    serde_json::to_value(observation).unwrap_or_else(|_| {
        serde_json::json!({
            "engine": observation.engine.clone(),
            "tool_name": observation.tool_name.clone(),
            "path": observation.path.clone(),
            "failure_kind": observation.failure_kind.clone(),
            "model_output_bounded": observation.model_output_bounded,
        })
    })
}

/// Create a boxed provider for the given provider name.
fn create_provider(provider_name: &str) -> Box<dyn Provider> {
    match provider_name {
        "openai" => Box::new(crate::provider::openai::OpenAiProvider::new()),
        "zai" => Box::new(crate::provider::zai::ZaiProvider::new()),
        "test" => Box::new(crate::provider::test::TestProvider::new()),
        _ => Box::new(crate::provider::openai::OpenAiProvider::new()),
    }
}

struct RequestBudgetContext<'a> {
    provider_name: &'a str,
    instructions: &'a str,
    registry: &'a ToolRegistry,
}

async fn compact_history_to_budget(
    auth: &ResolvedAuth,
    model: &str,
    history: &mut Vec<serde_json::Value>,
    config: &CompactConfig,
    events: &EventEmitter,
    request_budget: &RequestBudgetContext<'_>,
) {
    if history.len() < 2 {
        return;
    }

    for pass in 0..=CONTEXT_OVERFLOW_RETRY_LIMIT {
        let request_chars = estimated_request_chars(history, request_budget);
        let target_chars = config.threshold_chars.max(1);
        if request_chars <= target_chars {
            return;
        }

        let history_chars = crate::compact::estimate_history_chars(history);
        if history_chars == 0 {
            return;
        }

        let overshoot = request_chars.saturating_sub(target_chars);
        let threshold_chars = history_chars
            .saturating_sub(overshoot.saturating_add(REQUEST_COMPACTION_MARGIN_CHARS))
            .max(1);
        let keep_recent = compact_keep_recent(
            config.keep_recent,
            history.len(),
            overshoot,
            history_chars,
            pass,
        );
        let pass_config = CompactConfig {
            threshold_chars,
            keep_recent,
        };
        let before_request_chars = request_chars;
        let before_history_chars = history_chars;
        let before_len = history.len();
        if crate::compact::maybe_compact(auth, model, history, &pass_config, events)
            .await
            .is_err()
        {
            return;
        }

        let after_request_chars = estimated_request_chars(history, request_budget);
        let after_history_chars = crate::compact::estimate_history_chars(history);
        if after_request_chars >= before_request_chars
            && after_history_chars >= before_history_chars
            && history.len() == before_len
        {
            return;
        }
    }
}

fn force_shrink_history_for_overflow_retry(
    history: &mut Vec<serde_json::Value>,
    retry_count: usize,
) -> usize {
    if history.len() < 2 {
        return 0;
    }

    let max_removable = history.len().saturating_sub(1);
    let target_removal = match retry_count {
        0 | 1 => (history.len() / 4).max(1),
        _ => (history.len() / 2).max(1),
    };
    let require_minimum_trim = retry_count >= 2;
    let remove_count =
        exchange_aligned_remove_count(history, target_removal, max_removable, require_minimum_trim)
            .unwrap_or_else(|| target_removal.min(max_removable));
    history.drain(..remove_count);
    remove_count
}

fn exchange_aligned_remove_count(
    history: &[serde_json::Value],
    target_removal: usize,
    max_removable: usize,
    require_minimum_trim: bool,
) -> Option<usize> {
    let target_removal = target_removal.min(max_removable);
    if target_removal == 0 {
        return None;
    }

    if !require_minimum_trim {
        let previous_cut = history
            .iter()
            .enumerate()
            .skip(1)
            .take_while(|(idx, _)| *idx <= target_removal)
            .filter(|(_, item)| is_user_message(item))
            .map(|(idx, _)| idx)
            .last();

        if previous_cut.is_some() {
            return previous_cut;
        }
    }

    let next_cut = history
        .iter()
        .enumerate()
        .skip(target_removal + 1)
        .take_while(|(idx, _)| *idx <= max_removable)
        .find(|(_, item)| is_user_message(item))
        .map(|(idx, _)| idx);

    if let Some(next_cut) = next_cut
        && (require_minimum_trim
            || next_cut.saturating_sub(target_removal) <= target_removal.max(1))
    {
        return Some(next_cut);
    }

    let fallback = tool_pair_safe_remove_count(history, target_removal);
    if require_minimum_trim
        && fallback < target_removal
        && let Some(minimum_safe) =
            safe_remove_count_at_or_after(history, target_removal, max_removable)
    {
        return Some(minimum_safe);
    }

    Some(fallback)
}

fn safe_remove_count_at_or_after(
    history: &[serde_json::Value],
    start: usize,
    max_removable: usize,
) -> Option<usize> {
    let start = start.min(max_removable);
    (start..=max_removable).find(|remove_count| {
        history
            .get(*remove_count)
            .and_then(|item| item.get("type"))
            .and_then(|value| value.as_str())
            != Some("function_call_output")
    })
}

fn is_user_message(item: &serde_json::Value) -> bool {
    item.get("type").and_then(|v| v.as_str()) == Some("message")
        && item.get("role").and_then(|v| v.as_str()) == Some("user")
}

fn tool_pair_safe_remove_count(history: &[serde_json::Value], remove_count: usize) -> usize {
    if remove_count == 0 || remove_count >= history.len() {
        return remove_count;
    }

    let candidate = &history[remove_count];
    if candidate.get("type").and_then(|v| v.as_str()) == Some("function_call_output") {
        remove_count.saturating_sub(1).max(1)
    } else {
        remove_count
    }
}

fn retry_compact_config(config: &CompactConfig, retry_count: usize) -> CompactConfig {
    let threshold_chars = match retry_count {
        0 => config.threshold_chars,
        1 => config.threshold_chars.saturating_mul(3) / 4,
        _ => config.threshold_chars / 2,
    }
    .max(1);
    CompactConfig {
        threshold_chars,
        keep_recent: config.keep_recent,
    }
}

fn compact_keep_recent(
    base_keep_recent: usize,
    history_len: usize,
    overshoot: usize,
    history_chars: usize,
    pass: usize,
) -> usize {
    let max_keep_recent = history_len.saturating_sub(1).max(1);
    let mut keep_recent = base_keep_recent.max(1).min(max_keep_recent);

    if overshoot.saturating_mul(2) >= history_chars {
        keep_recent = keep_recent.min((history_len / 2).max(1));
    } else if overshoot.saturating_mul(4) >= history_chars {
        keep_recent = keep_recent.min((history_len * 2 / 3).max(1));
    } else if overshoot.saturating_mul(8) >= history_chars {
        keep_recent = keep_recent.min((history_len * 3 / 4).max(1));
    }

    match pass {
        0 => keep_recent,
        1 => keep_recent.min((history_len / 2).max(1)),
        _ => keep_recent.min((history_len / 3).max(1)),
    }
}

fn estimated_request_chars(
    history: &[serde_json::Value],
    request_budget: &RequestBudgetContext<'_>,
) -> usize {
    match request_budget.provider_name {
        "openai" => {
            let tools = request_budget.registry.to_api_tools();
            crate::compact::estimate_request_chars(history, request_budget.instructions, &tools)
        }
        "zai" | "test" => crate::compact::estimate_history_chars(history),
        _ => {
            let tools = request_budget.registry.to_api_tools();
            crate::compact::estimate_request_chars(history, request_budget.instructions, &tools)
        }
    }
}

fn is_context_window_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    [
        "context window",
        "input exceeds",
        "maximum context length",
        "too many tokens",
        "input is too long",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn context_usage_chars(
    history: &[serde_json::Value],
    max_chars: usize,
    request_budget: &RequestBudgetContext<'_>,
) -> (usize, usize) {
    let used_chars = estimated_request_chars(history, request_budget);
    (used_chars, max_chars.max(1))
}

pub fn history_items_from_turns(turns: &[Turn]) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    for t in turns {
        match t.kind.as_str() {
            "function_call" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t.content) {
                    let call_id = v
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if call_id.is_empty() {
                        continue;
                    }
                    items.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": call_id,
                        "name": v.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "arguments": v.get("arguments").and_then(|v| v.as_str()).unwrap_or(""),
                    }));
                }
            }
            "function_call_output" => {
                let parsed = serde_json::from_str::<serde_json::Value>(&t.content)
                    .unwrap_or_else(|_| serde_json::json!({ "output": t.content }));
                let call_id = parsed
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if call_id.is_empty() {
                    continue;
                }
                let output = parsed
                    .get("output")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&t.content)
                    .to_string();
                let item = serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                });
                items.push(item);
            }
            _ => {
                items.push(serde_json::json!({
                    "type": "message",
                    "role": t.role,
                    "content": t.content,
                }));
            }
        }
    }
    items
}

pub fn history_from_turns(turns: &[Turn]) -> Vec<Message> {
    turns
        .iter()
        .map(|t| Message {
            role: t.role.clone(),
            content: t.content.clone(),
        })
        .collect()
}

/// Re-export public SSE parsers for backwards compatibility.
pub use crate::provider::openai::process_openai_sse_block;
pub use crate::provider::zai::process_zai_sse_line;

#[cfg(test)]
mod tests {
    use super::truncate;
    use super::*;
    use crate::auth::ResolvedAuth;
    use crate::compact::CompactConfig;
    use crate::events::event_channel;
    use crate::provider::test::{CONTROL_BLOCK_END, CONTROL_BLOCK_START};
    use crate::store::{NewTaskRecord, Session, Store, TaskRecord, Turn};
    use crate::tools::editing::EditObservation;
    use crate::tools::{Tool, ToolExecutionResult, ToolRegistry};
    use serde_json::{Value, json};

    struct ObservedTool;

    impl Tool for ObservedTool {
        fn name(&self) -> &str {
            "edit_tool"
        }

        fn description(&self) -> &str {
            "Test tool that emits an edit observation"
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false,
            })
        }

        fn execute(&self, _args: Value) -> Result<String> {
            Ok("execute called".to_string())
        }

        fn execute_with_result(&self, _args: Value) -> Result<ToolExecutionResult> {
            let observation = EditObservation {
                engine: "test-engine".to_string(),
                tool_name: self.name().to_string(),
                path: "src/lib.rs".to_string(),
                edit_count: 1,
                applied_count: 1,
                stale_reference_count: 0,
                noop_count: 0,
                failure_kind: None,
                duration_ms: 12,
                artifact_path: Some("/tmp/artifact.json".to_string()),
                artifact_id: Some("artifact-123".to_string()),
                model_output_bounded: true,
            };
            Ok(ToolExecutionResult {
                output: "applied edits".to_string(),
                edit_observations: vec![observation],
            })
        }
    }

    #[tokio::test]
    async fn model_history_excludes_edit_metadata() {
        let store = Store::open_memory().unwrap();
        let (emitter, receiver) = event_channel();

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(ObservedTool));

        let mut runtime = SessionRuntime::new(
            &store,
            ResolvedAuth {
                provider: "test".to_string(),
                api_key: "test-key".to_string(),
                base_url: "http://unused".to_string(),
                account_id: None,
            },
            Some("test-model"),
            None,
            emitter,
            CompactConfig::default(),
            registry,
            "system".to_string(),
            RuntimeHooks::default(),
        )
        .unwrap();

        let control_block = format!(
            "{CONTROL_BLOCK_START}{{\"type\":\"tool_call\",\"name\":\"edit_tool\",\"arguments\":{{}}}}{CONTROL_BLOCK_END}"
        );
        runtime
            .submit_prompt(format!("run the edit tool {control_block}"))
            .await
            .unwrap();

        let function_call_outputs: Vec<_> = runtime
            .history
            .iter()
            .filter(|item| {
                item.get("type").and_then(|v| v.as_str()) == Some("function_call_output")
            })
            .collect();
        assert!(!function_call_outputs.is_empty());
        let latest_output = function_call_outputs.last().unwrap();
        assert!(latest_output.get("edit_observation").is_none());
        assert!(latest_output.get("artifact_id").is_none());
        assert!(latest_output.get("artifact_path").is_none());

        let turns = Turn::list_for_session(&store, runtime.session_id()).unwrap();
        let function_output_turn = turns
            .iter()
            .find(|turn| turn.kind == "function_call_output")
            .expect("function_call_output turn expected");
        let payload: Value = serde_json::from_str(&function_output_turn.content).unwrap();
        let observation = payload
            .get("edit_observation")
            .and_then(|value| value.as_object())
            .expect("edit_observation should remain in persisted turn");
        assert_eq!(
            observation
                .get("artifact_id")
                .and_then(|value| value.as_str()),
            Some("artifact-123")
        );
        assert_eq!(
            observation
                .get("artifact_path")
                .and_then(|value| value.as_str()),
            Some("/tmp/artifact.json")
        );

        let tool_event = receiver.drain().into_iter().find_map(|event| match event {
            AgentEvent::ToolCallCompleted {
                edit_observation, ..
            } => edit_observation,
            _ => None,
        });
        assert!(tool_event.is_some());
    }

    #[test]
    fn truncate_preserves_utf8_boundaries() {
        let input = "🦀 test";
        assert_eq!(truncate(input, 1), "🦀...");
    }

    #[test]
    fn truncate_uses_ellipsis_when_truncating() {
        assert_eq!(truncate("hello", 3), "hel...");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn resume_restores_compact_threshold_from_session_settings() {
        let store = Store::open_memory().unwrap();
        let session = Session::create(
            &store,
            NewSession {
                model: "test-model".to_string(),
                provider: "test".to_string(),
            },
        )
        .unwrap();
        Session::update_settings(
            &store,
            &session.id,
            &serde_json::json!({
                "model": "test-model",
                "provider": "test",
                "compact_threshold": 12345,
            })
            .to_string(),
        )
        .unwrap();

        let (events, _rx) = event_channel();
        let runtime = SessionRuntime::new(
            &store,
            ResolvedAuth {
                provider: "test".to_string(),
                api_key: "test-key".to_string(),
                base_url: "http://unused".to_string(),
                account_id: None,
            },
            None,
            Some(&session.id),
            events,
            CompactConfig::default(),
            crate::tools::default_registry(std::env::current_dir().unwrap()),
            "system".to_string(),
            RuntimeHooks::default(),
        )
        .unwrap();

        assert_eq!(runtime.compact_config.threshold_chars, 12_345);
    }

    #[test]
    fn restore_compact_threshold_handles_zero_and_invalid_values() {
        let mut compact = CompactConfig::default();
        let mut session = Session {
            id: "s1".to_string(),
            model: "m".to_string(),
            provider: "test".to_string(),
            title: None,
            status: SessionStatus::Active,
            policy: None,
            settings: Some("{\"compact_threshold\":0}".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        restore_compact_threshold_from_settings(&mut compact, &session);
        assert_eq!(compact.threshold_chars, 1);

        compact.threshold_chars = 777;
        session.settings = Some("{\"compact_threshold\":\"nope\"}".to_string());
        restore_compact_threshold_from_settings(&mut compact, &session);
        assert_eq!(compact.threshold_chars, 777);
    }

    #[test]
    fn immediate_transaction_rolls_back_failed_delegate_steps() {
        let store = Store::open_memory().unwrap();

        let result: anyhow::Result<()> = run_immediate_transaction(&store, || {
            TaskRecord::create(
                &store,
                NewTaskRecord {
                    task_id: "tx-rollback-task".to_string(),
                    parent_task_id: None,
                    title: Some("tx-rollback-task".to_string()),
                    priority: 1,
                    policy_snapshot: r#"{"mode":"auto"}"#.to_string(),
                    parent_close_policy: "request_cancel_descendants".to_string(),
                    recovery_checkpoint: None,
                    owner_session_id: None,
                },
            )?;
            anyhow::bail!("forced failure")
        });

        assert!(format!("{:#}", result.unwrap_err()).contains("forced failure"));
        assert!(TaskRecord::get(&store, "tx-rollback-task").is_err());
    }

    #[test]
    fn retry_compact_config_tightens_budget() {
        let base = CompactConfig {
            threshold_chars: 800,
            keep_recent: 20,
        };

        assert_eq!(retry_compact_config(&base, 0).threshold_chars, 800);
        assert_eq!(retry_compact_config(&base, 1).threshold_chars, 600);
        assert_eq!(retry_compact_config(&base, 2).threshold_chars, 400);
    }

    #[test]
    fn context_window_error_matches_provider_messages() {
        assert!(is_context_window_error(&anyhow::anyhow!(
            "OpenAI SSE error: Your input exceeds the context window of this model"
        )));
        assert!(is_context_window_error(&anyhow::anyhow!(
            "maximum context length exceeded"
        )));
        assert!(!is_context_window_error(&anyhow::anyhow!(
            "rate limit exceeded"
        )));
    }

    #[test]
    fn context_usage_chars_includes_request_overhead() {
        let history = vec![serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "hello",
        })];
        let registry = crate::tools::default_registry(std::env::current_dir().unwrap());
        let request_budget = RequestBudgetContext {
            provider_name: "openai",
            instructions: "system prompt",
            registry: &registry,
        };

        let (used_chars, max_chars) = context_usage_chars(&history, 1_000, &request_budget);

        assert!(used_chars > crate::compact::estimate_history_chars(&history));
        assert_eq!(max_chars, 1_000);
    }

    #[test]
    fn context_usage_chars_matches_zai_payload_shape() {
        let history = vec![serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "hello",
        })];
        let registry = crate::tools::default_registry(std::env::current_dir().unwrap());
        let request_budget = RequestBudgetContext {
            provider_name: "zai",
            instructions: "system prompt",
            registry: &registry,
        };

        let (used_chars, max_chars) = context_usage_chars(&history, 1_000, &request_budget);

        assert_eq!(used_chars, crate::compact::estimate_history_chars(&history));
        assert_eq!(max_chars, 1_000);
    }

    #[test]
    fn force_shrink_history_for_overflow_retry_removes_oldest_items() {
        let mut history = vec![
            serde_json::json!({"type": "message", "role": "user", "content": "m1"}),
            serde_json::json!({"type": "message", "role": "assistant", "content": "m2"}),
            serde_json::json!({"type": "message", "role": "user", "content": "m3"}),
            serde_json::json!({"type": "message", "role": "assistant", "content": "m4"}),
        ];

        let removed = force_shrink_history_for_overflow_retry(&mut history, 1);

        assert_eq!(removed, 2);
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[0].get("content").and_then(|v| v.as_str()),
            Some("m3")
        );
    }

    #[test]
    fn force_shrink_history_for_overflow_retry_progressively_trims_tool_exchange() {
        let mut history = vec![
            serde_json::json!({"type": "message", "role": "user", "content": "u1"}),
            serde_json::json!({"type": "function_call", "call_id": "c1", "name": "t", "arguments": "{}"}),
            serde_json::json!({"type": "function_call_output", "call_id": "c1", "output": "ok"}),
            serde_json::json!({"type": "message", "role": "assistant", "content": "a1"}),
            serde_json::json!({"type": "message", "role": "user", "content": "u2"}),
            serde_json::json!({"type": "message", "role": "assistant", "content": "a2"}),
        ];

        let removed = force_shrink_history_for_overflow_retry(&mut history, 1);

        assert_eq!(removed, 1);
        assert_eq!(history.len(), 5);
        assert_eq!(
            history[0].get("type").and_then(|v| v.as_str()),
            Some("function_call")
        );
        assert_eq!(
            history[1].get("type").and_then(|v| v.as_str()),
            Some("function_call_output")
        );
    }

    #[test]
    fn history_items_from_turns_skips_function_call_without_call_id() {
        let turns = vec![
            Turn {
                id: 1,
                session_id: "s1".to_string(),
                turn_number: 1,
                kind: "function_call".to_string(),
                role: "assistant".to_string(),
                content: serde_json::json!({
                    "name": "shell",
                    "arguments": "{}",
                })
                .to_string(),
                model: None,
                tokens_in: None,
                tokens_out: None,
                created_at: chrono::Utc::now(),
            },
            Turn {
                id: 2,
                session_id: "s1".to_string(),
                turn_number: 2,
                kind: "function_call_output".to_string(),
                role: "tool".to_string(),
                content: serde_json::json!({
                    "output": "ok",
                })
                .to_string(),
                model: None,
                tokens_in: None,
                tokens_out: None,
                created_at: chrono::Utc::now(),
            },
            Turn {
                id: 3,
                session_id: "s1".to_string(),
                turn_number: 3,
                kind: "message".to_string(),
                role: "user".to_string(),
                content: "hello".to_string(),
                model: None,
                tokens_in: None,
                tokens_out: None,
                created_at: chrono::Utc::now(),
            },
        ];

        let items = history_items_from_turns(&turns);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].get("type").and_then(|v| v.as_str()),
            Some("message")
        );
    }

    #[test]
    fn force_shrink_history_for_overflow_retry_avoids_orphaning_tool_output() {
        let mut history = vec![
            serde_json::json!({"type": "message", "role": "user", "content": "u1"}),
            serde_json::json!({"type": "function_call", "call_id": "c1", "name": "t", "arguments": "{}"}),
            serde_json::json!({"type": "function_call_output", "call_id": "c1", "output": "ok"}),
        ];

        let removed = force_shrink_history_for_overflow_retry(&mut history, 2);

        assert_eq!(removed, 1);
        assert_eq!(
            history[0].get("type").and_then(|v| v.as_str()),
            Some("function_call")
        );
    }

    #[test]
    fn force_shrink_history_for_overflow_retry_keeps_at_least_one_item() {
        let mut history = vec![
            serde_json::json!({"type": "message", "role": "user", "content": "m1"}),
            serde_json::json!({"type": "message", "role": "assistant", "content": "m2"}),
        ];

        let removed = force_shrink_history_for_overflow_retry(&mut history, 2);

        assert_eq!(removed, 1);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn force_shrink_history_for_final_retry_meets_minimum_target() {
        let mut history = vec![
            serde_json::json!({"type": "message", "role": "user", "content": "u1"}),
            serde_json::json!({"type": "message", "role": "assistant", "content": "a1"}),
            serde_json::json!({"type": "message", "role": "user", "content": "u2"}),
            serde_json::json!({"type": "function_call", "call_id": "c1", "name": "t", "arguments": "{}"}),
            serde_json::json!({"type": "function_call_output", "call_id": "c1", "output": "ok"}),
            serde_json::json!({"type": "message", "role": "assistant", "content": "a2"}),
            serde_json::json!({"type": "message", "role": "user", "content": "u3"}),
            serde_json::json!({"type": "message", "role": "assistant", "content": "a3"}),
        ];

        let removed = force_shrink_history_for_overflow_retry(&mut history, 2);

        assert_eq!(removed, 6);
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[0].get("content").and_then(|v| v.as_str()),
            Some("u3")
        );
    }
}
