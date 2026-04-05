use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinError;
use uuid::Uuid;

use super::SessionSettingsOverrides;
use super::session::{
    ChildSessionBootstrapMode, DelegatedChildBootstrapOutcome, bootstrap_delegated_child_session,
};
use super::settings::{
    InheritedSettingsSnapshot,
    spawn_autonomous_child_task_with_policy as spawn_autonomous_child_task_with_policy_impl,
};
use crate::auth::{CredentialStore, ResolvedAuth};
use crate::compact::CompactConfig;
use crate::diagnostics::lsp_status;
use crate::events::{AgentEvent, event_channel};
use crate::lsp::{LspClientFactory, LspManager, LspService, builtin_catalog};
use crate::runtime::{RuntimeHooks, SessionRuntime, SubmitResult, TurnCorrelation};
use crate::store::{
    AttemptLifecycleState, NewTaskAttemptRecord, Session, SharedStore, Store, TaskAttemptRecord,
    TaskEdgeRecord, TaskLifecycleState, TaskRecord,
};
use crate::tools::ToolRegistry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRuntime {
    pub session_id: String,
    pub model: String,
    pub provider: String,
    pub settings: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTurnReplay {
    pub request_id: String,
    pub turn_id: String,
    pub message_id: String,
    pub content: String,
    pub context_used_chars: usize,
    pub context_max_chars: usize,
    pub context_usage_initialized: bool,
    pub input_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub total_tokens: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEventEnvelope {
    pub request_id: String,
    pub event: AgentEvent,
}

struct PromptExecutionIdentifiers<'a> {
    request_id: &'a str,
    session_id: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachControllerError {
    SessionBusy {
        session_id: String,
        active_controller_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitPromptError {
    NoRuntime { session_id: String },
    NoActiveTurn { session_id: String },
    TurnInProgress { session_id: String, turn_id: String },
    RuntimeFailed { session_id: String, error: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitPromptOutcome {
    Accepted { turn_id: String, message_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortTurnError {
    NoRuntime {
        session_id: String,
    },
    NoActiveTurn {
        session_id: String,
    },
    TurnMismatch {
        session_id: String,
        expected_turn_id: String,
        requested_turn_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortTurnOutcome {
    Requested { session_id: String, turn_id: String },
}

#[derive(Clone)]
struct RuntimeWorker {
    session_id: String,
    model: String,
    provider: String,
    project_dir: PathBuf,
    instructions: String,
    compact_config: CompactConfig,
    lsp_manager: Arc<LspManager>,
}

impl std::fmt::Debug for RuntimeWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeWorker")
            .field("session_id", &self.session_id)
            .field("model", &self.model)
            .field("provider", &self.provider)
            .field("project_dir", &self.project_dir)
            .field("instructions", &self.instructions)
            .field("compact_config", &self.compact_config)
            .finish()
    }
}

impl RuntimeWorker {
    fn from_session(session: &Session, lsp_factory: Option<Arc<dyn LspClientFactory>>) -> Self {
        let lsp_manager = match lsp_factory {
            Some(factory) => Arc::new(LspManager::with_test_factory(factory)),
            None => Arc::new(LspManager::new()),
        };

        let mut worker = Self {
            session_id: session.id.clone(),
            model: session.model.clone(),
            provider: session.provider.clone(),
            project_dir: std::env::current_dir().unwrap_or_default(),
            instructions: String::new(),
            compact_config: CompactConfig::default(),
            lsp_manager,
        };
        worker.refresh_from_session(session);
        worker
    }

    fn refresh_from_session(&mut self, session: &Session) {
        let mut model = session.model.clone();
        let mut provider = session.provider.clone();
        let mut compact_config = CompactConfig::default();
        if let Some(settings) = SessionSettingsOverrides::from_session(session) {
            settings.apply_model_provider_overrides(&mut model, &mut provider);
            settings.apply_compact_threshold(&mut compact_config);
        }

        let project_dir = std::env::current_dir().unwrap_or_default();
        let rules = crate::skills::discover_rules(&project_dir);
        let skills = crate::skills::discover_skills(&project_dir);
        let instructions = crate::skills::build_system_prompt(&rules, &skills);
        self.session_id = session.id.clone();
        self.model = model;
        self.provider = provider;
        self.project_dir = project_dir;
        self.instructions = instructions;
        self.compact_config = compact_config;
    }

    fn resolved_auth(
        &self,
        runtime_rt: &tokio::runtime::Runtime,
        events: &crate::events::EventEmitter,
    ) -> Result<ResolvedAuth> {
        if self.provider == "openai"
            && let Ok(api_key) = std::env::var("OPENAI_API_KEY")
            && !api_key.is_empty()
        {
            return Ok(ResolvedAuth {
                provider: "openai".to_string(),
                api_key,
                base_url: std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
                account_id: None,
            });
        }

        let store =
            CredentialStore::open_noninteractive().context("failed to open credential store")?;
        let resolved = runtime_rt
            .block_on(crate::auth::resolve_auth(&store, events))
            .context("failed to resolve provider auth")?;

        ensure!(
            resolved.provider == self.provider,
            "session provider '{}' does not match resolved auth provider '{}'; switch active provider or credentials",
            self.provider,
            resolved.provider
        );

        Ok(resolved)
    }

    async fn submit_prompt_stream(
        &self,
        shared_store: SharedStore,
        prompt: String,
        correlation: TurnCorrelation,
        abort_signal: Arc<AtomicBool>,
        stream_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<SubmitResult> {
        let worker = self.clone();
        tokio::task::spawn_blocking(move || -> Result<SubmitResult> {
            let runtime_rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build runtime worker tokio runtime")?;

            let (events, receiver) = event_channel();
            let forward_tx = stream_tx.clone();
            let forwarder = std::thread::spawn(move || {
                while let Ok(event) = receiver.recv_blocking() {
                    if forward_tx.send(event).is_err() {
                        break;
                    }
                }
            });

            let resolved_auth = worker.resolved_auth(&runtime_rt, &events)?;
            worker.lsp_manager.set_event_emitter(events.clone());
            let submit_result = (|| -> Result<SubmitResult> {
                let registry =
                    runtime_registry(worker.project_dir.clone(), worker.lsp_manager.clone());
                let mut runtime = SessionRuntime::new_with_shared_store_and_abort_signal(
                    shared_store,
                    resolved_auth,
                    Some(&worker.model),
                    Some(&worker.session_id),
                    events.clone(),
                    worker.compact_config.clone(),
                    registry,
                    worker.instructions.clone(),
                    RuntimeHooks::default(),
                    abort_signal,
                    None,
                )?;

                let submit = runtime_rt
                    .block_on(runtime.submit_prompt_with_ids(prompt, correlation))
                    .context("runtime submit failed")?;

                drop(runtime);
                Ok(submit)
            })();

            worker.lsp_manager.clear_event_emitter();
            drop(events);
            let _ = forwarder.join();

            submit_result
        })
        .await
        .map_err(join_error)?
    }
}

#[derive(Debug)]
struct ActivePrompt {
    turn_id: String,
    abort_signal: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLspSupport {
    pub server_id: String,
    pub command: Vec<String>,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLspServerState {
    pub server_id: String,
    pub status: String,
    pub command: Vec<String>,
    pub workspace_root: String,
    pub last_file: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLspSnapshot {
    pub supported: Vec<RuntimeLspSupport>,
    pub active: Vec<RuntimeLspServerState>,
}

#[derive(Debug)]
struct ManagedSession {
    runtime: RuntimeWorker,
    settings: Option<String>,
    controller_id: Option<String>,
    active_turn: Option<ActiveTurnReplay>,
    last_context_usage: Option<(usize, usize)>,
    last_token_usage: Option<(Option<usize>, Option<usize>, Option<usize>)>,
    active_prompt: Option<ActivePrompt>,
    events: broadcast::Sender<RuntimeEventEnvelope>,
    lsp_servers: HashMap<(String, String), RuntimeLspServerState>,
}

const DEFAULT_SCHEDULER_OWNER_ID: &str = "runtime-manager";
const DEFAULT_SCHEDULER_LEASE_TTL_SECONDS: i64 = 60;

pub struct RuntimeManager {
    sessions: Mutex<HashMap<String, ManagedSession>>,
    bound_store: Mutex<Option<SharedStore>>,
    lsp_test_factory: Mutex<Option<Arc<dyn LspClientFactory>>>,
}

impl Default for RuntimeManager {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            bound_store: Mutex::new(None),
            lsp_test_factory: Mutex::new(None),
        }
    }
}

pub fn spawn_autonomous_child_task_with_policy(
    store: &Store,
    parent_task_id: &str,
    child_task_id: &str,
    title: Option<String>,
    priority: i64,
    requested_policy_json: Option<&str>,
) -> Result<TaskRecord> {
    let project_dir = std::env::current_dir().unwrap_or_default();
    let registry = crate::tools::default_registry(project_dir);
    spawn_autonomous_child_task_with_policy_impl(
        store,
        &registry,
        parent_task_id,
        child_task_id,
        title,
        priority,
        requested_policy_json,
    )
}

impl RuntimeManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind_shared_store(&self, shared_store: SharedStore) {
        let mut bound_store = self
            .bound_store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *bound_store = Some(shared_store);
    }

    fn bound_store(&self) -> Option<SharedStore> {
        self.bound_store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn lsp_test_factory(&self) -> Option<Arc<dyn LspClientFactory>> {
        self.lsp_test_factory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set_lsp_test_factory(&self, factory: Arc<dyn LspClientFactory>) {
        let mut slot = self
            .lsp_test_factory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(factory);
    }

    fn lock_sessions(&self) -> MutexGuard<'_, HashMap<String, ManagedSession>> {
        match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn ensure_managed_session(&self, session: &Session) {
        let lsp_factory = self.lsp_test_factory();
        let mut sessions = self.lock_sessions();
        let entry = sessions
            .entry(session.id.clone())
            .or_insert_with(|| ManagedSession {
                runtime: RuntimeWorker::from_session(session, lsp_factory.clone()),
                settings: session.settings.clone(),
                controller_id: None,
                active_turn: None,
                last_context_usage: None,
                last_token_usage: None,
                active_prompt: None,
                events: broadcast::channel(256).0,
                lsp_servers: HashMap::new(),
            });

        entry.settings = session.settings.clone();
        if entry.runtime.session_id != session.id {
            entry.runtime = RuntimeWorker::from_session(session, lsp_factory);
        } else {
            entry.runtime.refresh_from_session(session);
        }
    }

    pub fn attach_controller(
        &self,
        session: &Session,
        controller_id: &str,
    ) -> Result<(), AttachControllerError> {
        self.ensure_managed_session(session);
        let mut sessions = self.lock_sessions();
        let entry = sessions
            .get_mut(&session.id)
            .expect("managed session should exist after ensure_managed_session");

        if let Some(active) = &entry.controller_id
            && active != controller_id
        {
            return Err(AttachControllerError::SessionBusy {
                session_id: session.id.clone(),
                active_controller_id: active.clone(),
            });
        }

        entry.controller_id = Some(controller_id.to_string());
        Ok(())
    }

    fn reserve_prompt_execution(
        &self,
        session_id: &str,
    ) -> std::result::Result<(RuntimeWorker, ActiveTurnReplay, Arc<AtomicBool>), SubmitPromptError>
    {
        let mut sessions = self.lock_sessions();
        let Some(entry) = sessions.get_mut(session_id) else {
            return Err(SubmitPromptError::NoRuntime {
                session_id: session_id.to_string(),
            });
        };
        let Some(active_turn) = entry.active_turn.clone() else {
            return Err(SubmitPromptError::NoActiveTurn {
                session_id: session_id.to_string(),
            });
        };
        if let Some(active_prompt) = &entry.active_prompt {
            return Err(SubmitPromptError::TurnInProgress {
                session_id: session_id.to_string(),
                turn_id: active_prompt.turn_id.clone(),
            });
        }

        let abort_signal = Arc::new(AtomicBool::new(false));
        entry.active_prompt = Some(ActivePrompt {
            turn_id: active_turn.turn_id.clone(),
            abort_signal: abort_signal.clone(),
        });

        Ok((entry.runtime.clone(), active_turn, abort_signal))
    }

    async fn execute_reserved_prompt<'a>(
        &self,
        worker: RuntimeWorker,
        shared_store: SharedStore,
        prompt: String,
        correlation: TurnCorrelation,
        abort_signal: Arc<AtomicBool>,
        identifiers: PromptExecutionIdentifiers<'a>,
    ) -> Result<SubmitResult> {
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        let request_id_for_events = identifiers.request_id.to_string();
        let session_id_for_events = identifiers.session_id.to_string();

        let worker_task = tokio::spawn(async move {
            worker
                .submit_prompt_stream(shared_store, prompt, correlation, abort_signal, stream_tx)
                .await
        });

        while let Some(event) = stream_rx.recv().await {
            self.publish_runtime_event(&session_id_for_events, &request_id_for_events, event);
        }

        match worker_task.await {
            Ok(result) => result,
            Err(err) => Err(join_error(err)),
        }
    }

    pub fn kick_scheduler_for_owner_session(self: &Arc<Self>, owner_session_id: &str) {
        let Some(shared_store) = self.bound_store() else {
            return;
        };

        let owner_session_id = owner_session_id.to_string();
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = manager
                .execute_scheduler_ready_graph_nodes(
                    shared_store,
                    &owner_session_id,
                    DEFAULT_SCHEDULER_OWNER_ID,
                    DEFAULT_SCHEDULER_LEASE_TTL_SECONDS,
                )
                .await
            {
                eprintln!(
                    "warning: failed to execute delegated task scheduler for session {owner_session_id}: {error:#}"
                );
            }
        });
    }

    pub async fn recover_bound_store_on_startup(&self) -> Result<usize> {
        let Some(shared_store) = self.bound_store() else {
            return Ok(0);
        };

        let owner_session_ids = {
            let store = lock_shared_store(&shared_store)?;
            TaskRecord::list(&store)?
                .into_iter()
                .filter_map(|task| task.owner_session_id)
                .collect::<BTreeSet<_>>()
        };

        let mut recovered = 0usize;
        for owner_session_id in owner_session_ids {
            recovered = recovered.saturating_add(
                self.recover_nonterminal_attempts_on_startup(
                    Arc::clone(&shared_store),
                    &owner_session_id,
                    DEFAULT_SCHEDULER_OWNER_ID,
                    DEFAULT_SCHEDULER_LEASE_TTL_SECONDS,
                )
                .await?,
            );
        }

        Ok(recovered)
    }

    pub fn release_controller(&self, session_id: &str, controller_id: &str) {
        let mut sessions = self.lock_sessions();
        if let Some(entry) = sessions.get_mut(session_id)
            && entry.controller_id.as_deref() == Some(controller_id)
        {
            entry.controller_id = None;
        }
    }

    pub fn subscribe(&self, session_id: &str) -> Option<broadcast::Receiver<RuntimeEventEnvelope>> {
        let sessions = self.lock_sessions();
        sessions
            .get(session_id)
            .map(|entry| entry.events.subscribe())
    }

    pub fn runtime(&self, session_id: &str) -> Option<ManagedRuntime> {
        let sessions = self.lock_sessions();
        sessions.get(session_id).map(|entry| ManagedRuntime {
            session_id: entry.runtime.session_id.clone(),
            model: entry.runtime.model.clone(),
            provider: entry.runtime.provider.clone(),
            settings: entry.settings.clone(),
        })
    }

    pub fn start_active_turn(
        &self,
        session_id: &str,
        request_id: String,
        turn_id: String,
        message_id: String,
    ) -> Result<(), SubmitPromptError> {
        let mut sessions = self.lock_sessions();
        let Some(entry) = sessions.get_mut(session_id) else {
            return Err(SubmitPromptError::NoRuntime {
                session_id: session_id.to_string(),
            });
        };

        if let Some(active_prompt) = &entry.active_prompt {
            return Err(SubmitPromptError::TurnInProgress {
                session_id: session_id.to_string(),
                turn_id: active_prompt.turn_id.clone(),
            });
        }

        let (baseline_used_chars, baseline_max_chars) = entry
            .last_context_usage
            .unwrap_or((0, entry.runtime.compact_config.threshold_chars));

        entry.active_turn = Some(ActiveTurnReplay {
            request_id,
            turn_id,
            message_id,
            content: String::new(),
            context_used_chars: baseline_used_chars,
            context_max_chars: baseline_max_chars.max(1),
            context_usage_initialized: false,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
        });
        Ok(())
    }

    pub fn context_usage_chars(&self, session_id: &str) -> Option<(usize, usize)> {
        let sessions = self.lock_sessions();
        let entry = sessions.get(session_id)?;
        if let Some(active_turn) = &entry.active_turn {
            if !active_turn.context_usage_initialized {
                return None;
            }
            return Some((
                active_turn.context_used_chars,
                active_turn.context_max_chars,
            ));
        }
        entry.last_context_usage
    }

    pub fn compact_threshold_chars(&self, session_id: &str) -> Option<usize> {
        let sessions = self.lock_sessions();
        let entry = sessions.get(session_id)?;
        Some(entry.runtime.compact_config.threshold_chars.max(1))
    }

    pub fn system_prompt_chars(&self, session_id: &str) -> Option<usize> {
        let sessions = self.lock_sessions();
        let entry = sessions.get(session_id)?;
        Some(entry.runtime.instructions.len())
    }

    pub fn token_usage(
        &self,
        session_id: &str,
    ) -> Option<(Option<usize>, Option<usize>, Option<usize>)> {
        let sessions = self.lock_sessions();
        let entry = sessions.get(session_id)?;
        if let Some(active_turn) = &entry.active_turn {
            if active_turn.input_tokens.is_some()
                || active_turn.output_tokens.is_some()
                || active_turn.total_tokens.is_some()
            {
                return Some((
                    active_turn.input_tokens,
                    active_turn.output_tokens,
                    active_turn.total_tokens,
                ));
            }
            return None;
        }
        entry.last_token_usage
    }

    pub fn active_turn(&self, session_id: &str) -> Option<ActiveTurnReplay> {
        let sessions = self.lock_sessions();
        sessions
            .get(session_id)
            .and_then(|entry| entry.active_turn.clone())
    }

    pub fn lsp_snapshot(&self, session_id: &str) -> RuntimeLspSnapshot {
        let sessions = self.lock_sessions();
        let mut active = sessions
            .get(session_id)
            .map(|entry| entry.lsp_servers.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        drop(sessions);

        active.sort_by(|left, right| {
            left.server_id
                .cmp(&right.server_id)
                .then_with(|| left.workspace_root.cmp(&right.workspace_root))
        });

        RuntimeLspSnapshot {
            supported: builtin_catalog()
                .iter()
                .map(|server| RuntimeLspSupport {
                    server_id: server.id.to_string(),
                    command: server
                        .command
                        .iter()
                        .map(|part| (*part).to_string())
                        .collect(),
                    extensions: server
                        .extensions
                        .iter()
                        .map(|extension| (*extension).to_string())
                        .collect(),
                })
                .collect(),
            active,
        }
    }

    pub fn clear_active_turn(&self, session_id: &str, turn_id: &str) {
        let mut sessions = self.lock_sessions();
        if let Some(entry) = sessions.get_mut(session_id) {
            let should_clear = entry
                .active_turn
                .as_ref()
                .map(|active| active.turn_id == turn_id)
                .unwrap_or(false);
            if should_clear {
                entry.active_turn = None;
            }
        }
    }

    pub async fn execute_scheduler_ready_graph_nodes(
        &self,
        shared_store: SharedStore,
        parent_session_id: &str,
        owner_id: &str,
        lease_ttl_seconds: i64,
    ) -> Result<usize> {
        let mut completed_nodes = 0usize;

        loop {
            let Some(candidate) =
                self.select_next_runnable_candidate(&shared_store, parent_session_id)?
            else {
                break;
            };

            let claim_succeeded = {
                let store = lock_shared_store(&shared_store)?;
                TaskAttemptRecord::claim_runnable_with_lease(
                    &store,
                    &candidate.attempt_id,
                    owner_id,
                    lease_ttl_seconds,
                    Utc::now(),
                )?
            };
            if !claim_succeeded {
                continue;
            }

            {
                let store = lock_shared_store(&shared_store)?;
                if TaskRecord::current_state(&store, &candidate.task_id)?
                    == TaskLifecycleState::Ready
                {
                    TaskRecord::transition_state(
                        &store,
                        &candidate.task_id,
                        &candidate.attempt_id,
                        TaskLifecycleState::Running,
                    )?;
                }
            }

            let child_session_id = {
                let store = lock_shared_store(&shared_store)?;
                let bootstrap_mode = candidate
                    .recovery_child_session_id
                    .as_ref()
                    .map(|session_id| ChildSessionBootstrapMode::LinkExisting {
                        session_id: session_id.clone(),
                    })
                    .unwrap_or(ChildSessionBootstrapMode::CreateNew);
                let bootstrap = bootstrap_delegated_child_session(
                    &store,
                    &candidate.task_id,
                    &candidate.attempt_id,
                    Some(&candidate.owner_session_id),
                    candidate.recovery_inherited_settings.clone(),
                    &candidate.handoff_summary,
                    candidate.handoff_artifact_ids.clone(),
                    bootstrap_mode,
                )?;
                match bootstrap {
                    DelegatedChildBootstrapOutcome::Started {
                        child_session_id: Some(session_id),
                        ..
                    } => Some(session_id),
                    DelegatedChildBootstrapOutcome::Started {
                        child_session_id: None,
                        ..
                    }
                    | DelegatedChildBootstrapOutcome::InterruptedRetryable { .. } => None,
                }
            };

            let Some(child_session_id) = child_session_id else {
                continue;
            };

            let child_session = {
                let store = lock_shared_store(&shared_store)?;
                Session::get(&store, &child_session_id)?
            };

            self.ensure_managed_session(&child_session);
            let request_id = format!("scheduler-{}", Uuid::new_v4());
            let turn_id = Uuid::new_v4().to_string();
            let message_id = Uuid::new_v4().to_string();
            self.start_active_turn(&child_session.id, request_id.clone(), turn_id, message_id)
                .map_err(|error| {
                    anyhow::anyhow!("failed to start child scheduler turn: {error:?}")
                })?;

            let (worker, active_turn, abort_signal) = self
                .reserve_prompt_execution(&child_session.id)
                .map_err(|error| {
                    anyhow::anyhow!("failed to reserve child scheduler prompt: {error:?}")
                })?;
            let correlation = TurnCorrelation {
                turn_id: active_turn.turn_id.clone(),
                message_id: active_turn.message_id.clone(),
            };

            let result = self
                .execute_reserved_prompt(
                    worker,
                    Arc::clone(&shared_store),
                    candidate.execution_prompt.clone(),
                    correlation.clone(),
                    abort_signal,
                    PromptExecutionIdentifiers {
                        request_id: &request_id,
                        session_id: &child_session.id,
                    },
                )
                .await;
            let finish_result = match &result {
                Ok(submit_result) => Ok(submit_result.clone()),
                Err(error) => Err(anyhow::anyhow!(format!("{error:#}"))),
            };
            self.finish_prompt(
                &child_session.id,
                &request_id,
                &correlation.turn_id,
                finish_result,
            );

            let store = lock_shared_store(&shared_store)?;
            match result {
                Ok(SubmitResult::Completed { .. }) => {
                    transition_attempt_if_needed(
                        &store,
                        &candidate.attempt_id,
                        AttemptLifecycleState::Completed,
                    )?;
                    transition_task_if_needed(
                        &store,
                        &candidate.task_id,
                        &candidate.attempt_id,
                        TaskLifecycleState::Completed,
                    )?;
                    completed_nodes = completed_nodes.saturating_add(1);
                }
                Ok(SubmitResult::Failed { .. }) => {
                    transition_attempt_if_needed(
                        &store,
                        &candidate.attempt_id,
                        AttemptLifecycleState::Failed,
                    )?;
                    transition_task_if_needed(
                        &store,
                        &candidate.task_id,
                        &candidate.attempt_id,
                        TaskLifecycleState::Failed,
                    )?;
                }
                Ok(SubmitResult::Aborted { .. }) | Err(_) => {
                    transition_attempt_if_needed(
                        &store,
                        &candidate.attempt_id,
                        AttemptLifecycleState::Interrupted,
                    )?;
                    transition_task_if_needed(
                        &store,
                        &candidate.task_id,
                        &candidate.attempt_id,
                        TaskLifecycleState::Interrupted,
                    )?;
                }
            }
        }

        Ok(completed_nodes)
    }

    pub fn cancel_task_graph(
        &self,
        shared_store: &SharedStore,
        task_id: &str,
    ) -> Result<Vec<String>> {
        let store = lock_shared_store(shared_store)?;
        let task_ids = collect_descendant_task_ids(&store, task_id)?;

        for current_task_id in &task_ids {
            cancel_single_task(&store, current_task_id)?;
        }

        Ok(task_ids)
    }

    pub fn retry_task(&self, shared_store: &SharedStore, task_id: &str) -> Result<String> {
        let store = lock_shared_store(shared_store)?;
        create_fresh_attempt_for_control(
            &store,
            task_id,
            &[TaskLifecycleState::Failed, TaskLifecycleState::Retryable],
            TaskLifecycleState::Failed,
            "retry",
            false,
        )
    }

    pub fn resume_task(&self, shared_store: &SharedStore, task_id: &str) -> Result<String> {
        let store = lock_shared_store(shared_store)?;
        create_fresh_attempt_for_control(
            &store,
            task_id,
            &[
                TaskLifecycleState::Interrupted,
                TaskLifecycleState::Retryable,
            ],
            TaskLifecycleState::Interrupted,
            "resume",
            true,
        )
    }

    pub fn reprioritize_task(
        &self,
        shared_store: &SharedStore,
        task_id: &str,
        priority: i64,
    ) -> Result<()> {
        let store = lock_shared_store(shared_store)?;
        let _ = TaskRecord::get(&store, task_id)?;
        let state = TaskRecord::current_state(&store, task_id)?;
        if !matches!(
            state,
            TaskLifecycleState::Queued | TaskLifecycleState::Ready
        ) {
            anyhow::bail!(
                "reprioritize is only allowed for queued/ready tasks: {task_id} is {state}",
            );
        }

        let now = Utc::now().to_rfc3339();
        store
            .conn()
            .execute(
                "UPDATE tasks SET priority = ?1, updated_at = ?2 WHERE task_id = ?3",
                (priority, &now, task_id),
            )
            .context("failed to update task priority")?;

        Ok(())
    }

    pub fn submit_prompt(
        self: &Arc<Self>,
        shared_store: SharedStore,
        session_id: &str,
        prompt: String,
    ) -> std::result::Result<SubmitPromptOutcome, SubmitPromptError> {
        let (worker, active_turn, abort_signal) = self.reserve_prompt_execution(session_id)?;

        let manager = Arc::clone(self);
        let session_id_owned = session_id.to_string();
        let request_id = active_turn.request_id.clone();
        let turn_id = active_turn.turn_id.clone();
        let message_id = active_turn.message_id.clone();
        let finish_turn_id = turn_id.clone();
        let correlation = TurnCorrelation {
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
        };

        tokio::spawn(async move {
            let result = manager
                .execute_reserved_prompt(
                    worker,
                    shared_store,
                    prompt,
                    correlation,
                    abort_signal,
                    PromptExecutionIdentifiers {
                        request_id: &request_id,
                        session_id: &session_id_owned,
                    },
                )
                .await;

            manager.finish_prompt(&session_id_owned, &request_id, &finish_turn_id, result);
            manager.kick_scheduler_for_owner_session(&session_id_owned);
        });

        Ok(SubmitPromptOutcome::Accepted {
            turn_id,
            message_id,
        })
    }

    pub fn request_abort(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> std::result::Result<AbortTurnOutcome, AbortTurnError> {
        let sessions = self.lock_sessions();
        let Some(entry) = sessions.get(session_id) else {
            return Err(AbortTurnError::NoRuntime {
                session_id: session_id.to_string(),
            });
        };
        let Some(active_turn) = &entry.active_turn else {
            return Err(AbortTurnError::NoActiveTurn {
                session_id: session_id.to_string(),
            });
        };
        if active_turn.turn_id != turn_id {
            return Err(AbortTurnError::TurnMismatch {
                session_id: session_id.to_string(),
                expected_turn_id: active_turn.turn_id.clone(),
                requested_turn_id: turn_id.to_string(),
            });
        }
        let Some(active_prompt) = &entry.active_prompt else {
            return Err(AbortTurnError::NoActiveTurn {
                session_id: session_id.to_string(),
            });
        };

        active_prompt.abort_signal.store(true, Ordering::Relaxed);
        Ok(AbortTurnOutcome::Requested {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
        })
    }

    fn publish_runtime_event(&self, session_id: &str, request_id: &str, event: AgentEvent) {
        let mut sessions = self.lock_sessions();
        let Some(entry) = sessions.get_mut(session_id) else {
            return;
        };

        if let AgentEvent::MessageDelta { delta, .. } = &event
            && let Some(active_turn) = entry.active_turn.as_mut()
        {
            let prior_chars = assistant_message_context_chars(&active_turn.content);
            active_turn.content.push_str(delta);
            if active_turn.context_usage_initialized {
                let updated_chars = assistant_message_context_chars(&active_turn.content);
                active_turn.context_used_chars = active_turn
                    .context_used_chars
                    .saturating_sub(prior_chars)
                    .saturating_add(updated_chars);
                entry.last_context_usage = Some((
                    active_turn.context_used_chars,
                    active_turn.context_max_chars,
                ));
            }
        }

        if let AgentEvent::ToolCallCompleted {
            context_used_chars,
            context_max_chars,
            ..
        } = &event
            && let Some(active_turn) = entry.active_turn.as_mut()
        {
            active_turn.context_used_chars = *context_used_chars;
            active_turn.context_max_chars = (*context_max_chars).max(1);
            active_turn.context_usage_initialized = true;
            active_turn.content.clear();
            entry.last_context_usage = Some((
                active_turn.context_used_chars,
                active_turn.context_max_chars,
            ));
        }

        if let AgentEvent::TurnStarted {
            turn_id,
            context_used_chars,
            context_max_chars,
            ..
        } = &event
            && let Some(active_turn) = entry.active_turn.as_mut()
            && &active_turn.turn_id == turn_id
        {
            active_turn.context_used_chars = *context_used_chars;
            active_turn.context_max_chars = (*context_max_chars).max(1);
            active_turn.context_usage_initialized = true;
            active_turn.input_tokens = None;
            active_turn.output_tokens = None;
            active_turn.total_tokens = None;
            entry.last_context_usage = Some((
                active_turn.context_used_chars,
                active_turn.context_max_chars,
            ));
            entry.last_token_usage = None;
        }

        if let AgentEvent::TurnCompleted {
            context_used_chars,
            context_max_chars,
            input_tokens,
            output_tokens,
            total_tokens,
            ..
        } = &event
        {
            entry.last_context_usage = Some((*context_used_chars, (*context_max_chars).max(1)));
            entry.last_token_usage = Some((*input_tokens, *output_tokens, *total_tokens));
            if let Some(active_turn) = entry.active_turn.as_mut() {
                active_turn.input_tokens = *input_tokens;
                active_turn.output_tokens = *output_tokens;
                active_turn.total_tokens = *total_tokens;
            }
        }

        let should_publish = match &event {
            AgentEvent::StatusReport {
                status,
                server_id,
                command,
                workspace_root,
                last_file,
                last_error,
                ..
            } if status.starts_with("lsp.") => apply_lsp_status_report(
                &mut entry.lsp_servers,
                status,
                server_id.as_deref(),
                command.as_deref(),
                workspace_root.as_deref(),
                last_file.as_deref(),
                last_error.as_deref(),
            ),
            _ => true,
        };

        if should_publish {
            let _ = entry.events.send(RuntimeEventEnvelope {
                request_id: request_id.to_string(),
                event: event.clone(),
            });
        }

        match event {
            AgentEvent::TurnCompleted { .. } => {
                entry.active_turn = None;
            }
            AgentEvent::TurnFailed { .. } => {
                entry.active_turn = None;
                entry.last_context_usage = None;
                entry.last_token_usage = None;
            }
            _ => {}
        }
    }

    fn finish_prompt(
        &self,
        session_id: &str,
        request_id: &str,
        turn_id: &str,
        result: Result<SubmitResult>,
    ) {
        let mut sessions = self.lock_sessions();
        let Some(entry) = sessions.get_mut(session_id) else {
            return;
        };

        match result {
            Ok(SubmitResult::Completed { .. }) => {
                entry.active_prompt = None;
                if entry
                    .active_turn
                    .as_ref()
                    .map(|active| active.turn_id == turn_id)
                    .unwrap_or(false)
                {
                    entry.active_turn = None;
                }
            }
            Ok(SubmitResult::Failed { .. }) | Ok(SubmitResult::Aborted { .. }) => {
                entry.active_prompt = None;
                if entry
                    .active_turn
                    .as_ref()
                    .map(|active| active.turn_id == turn_id)
                    .unwrap_or(false)
                {
                    entry.active_turn = None;
                }
                entry.last_context_usage = None;
                entry.last_token_usage = None;
            }
            Err(error) => {
                let error = format!("{}", error);
                let _ = entry.events.send(RuntimeEventEnvelope {
                    request_id: request_id.to_string(),
                    event: AgentEvent::TurnFailed {
                        session_id: session_id.to_string(),
                        turn_id: turn_id.to_string(),
                        model: entry.runtime.model.clone(),
                        turn_number: 0,
                        error,
                    },
                });
                entry.active_prompt = None;
                entry.active_turn = None;
                entry.last_context_usage = None;
                entry.last_token_usage = None;
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SchedulerCandidate {
    task_id: String,
    attempt_id: String,
    owner_session_id: String,
    handoff_summary: String,
    handoff_artifact_ids: Vec<String>,
    recovery_child_session_id: Option<String>,
    recovery_inherited_settings: Option<InheritedSettingsSnapshot>,
    execution_prompt: String,
}

impl RuntimeManager {
    pub async fn recover_nonterminal_attempts_on_startup(
        &self,
        shared_store: SharedStore,
        parent_session_id: &str,
        owner_id: &str,
        lease_ttl_seconds: i64,
    ) -> Result<usize> {
        self.reconcile_nonterminal_attempts(&shared_store)?;
        self.execute_scheduler_ready_graph_nodes(
            shared_store,
            parent_session_id,
            owner_id,
            lease_ttl_seconds,
        )
        .await
    }

    fn reconcile_nonterminal_attempts(&self, shared_store: &SharedStore) -> Result<()> {
        let now = Utc::now();
        let store = lock_shared_store(shared_store)?;
        let tasks = TaskRecord::list(&store)?;

        for task in tasks {
            let mut task_state = TaskRecord::current_state(&store, &task.task_id)?;
            if is_task_terminal(task_state) {
                continue;
            }

            if task.parent_close_policy == "request_cancel_descendants"
                && task_state == TaskLifecycleState::CancelRequested
            {
                request_descendant_cancellation_before_recovery(&store, &task.task_id)?;
            }

            let latest_attempt = latest_or_new_attempt(&store, &task.task_id)?;
            let mut attempt_state = latest_attempt.state()?;

            if attempt_state == AttemptLifecycleState::Running {
                let expired = TaskAttemptRecord::mark_expired_lease_interrupted_recoverable(
                    &store,
                    &latest_attempt.attempt_id,
                    now,
                )?;
                if expired {
                    attempt_state = AttemptLifecycleState::Interrupted;
                    task_state = TaskRecord::current_state(&store, &task.task_id)?;
                    if task_state == TaskLifecycleState::Running {
                        TaskRecord::transition_state(
                            &store,
                            &task.task_id,
                            &latest_attempt.attempt_id,
                            TaskLifecycleState::Interrupted,
                        )?;
                        task_state = TaskLifecycleState::Interrupted;
                    }
                }
            }

            if task_state == TaskLifecycleState::CancelRequested
                || attempt_state == AttemptLifecycleState::CancelRequested
            {
                continue;
            }

            if attempt_state == AttemptLifecycleState::Interrupted {
                if matches!(
                    task_state,
                    TaskLifecycleState::Queued
                        | TaskLifecycleState::Ready
                        | TaskLifecycleState::Running
                        | TaskLifecycleState::Blocked
                ) {
                    TaskRecord::transition_state(
                        &store,
                        &task.task_id,
                        &latest_attempt.attempt_id,
                        TaskLifecycleState::Interrupted,
                    )?;
                    task_state = TaskLifecycleState::Interrupted;
                }

                if matches!(
                    task_state,
                    TaskLifecycleState::Interrupted | TaskLifecycleState::Retryable
                ) {
                    let _ = create_fresh_attempt_for_control(
                        &store,
                        &task.task_id,
                        &[
                            TaskLifecycleState::Interrupted,
                            TaskLifecycleState::Retryable,
                        ],
                        TaskLifecycleState::Interrupted,
                        "recover",
                        true,
                    )?;
                }
            }
        }

        Ok(())
    }

    fn select_next_runnable_candidate(
        &self,
        shared_store: &SharedStore,
        owner_session_id: &str,
    ) -> Result<Option<SchedulerCandidate>> {
        let store = lock_shared_store(shared_store)?;
        let mut tasks = TaskRecord::list(&store)?;
        tasks.retain(|task| task.owner_session_id.as_deref() == Some(owner_session_id));
        tasks.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });

        for task in tasks {
            let task_state = TaskRecord::current_state(&store, &task.task_id)?;
            if is_task_terminal(task_state) {
                continue;
            }
            if task_state == TaskLifecycleState::CancelRequested {
                continue;
            }

            let dependencies = TaskEdgeRecord::list_for_task(&store, &task.task_id)?;
            let mut dependencies_satisfied = true;
            for edge in &dependencies {
                let dep_state = TaskRecord::current_state(&store, &edge.depends_on_task_id)?;
                if dep_state != TaskLifecycleState::Completed {
                    dependencies_satisfied = false;
                    break;
                }
            }

            let attempt = latest_or_new_attempt(&store, &task.task_id)?;
            let attempt_state = attempt.state()?;

            if !dependencies_satisfied {
                if matches!(
                    task_state,
                    TaskLifecycleState::Ready | TaskLifecycleState::Running
                ) {
                    TaskRecord::transition_state(
                        &store,
                        &task.task_id,
                        &attempt.attempt_id,
                        TaskLifecycleState::Blocked,
                    )?;
                }
                if matches!(
                    attempt_state,
                    AttemptLifecycleState::Ready | AttemptLifecycleState::Running
                ) {
                    TaskAttemptRecord::transition_state(
                        &store,
                        &attempt.attempt_id,
                        AttemptLifecycleState::Blocked,
                    )?;
                }
                continue;
            }

            let task_state = TaskRecord::current_state(&store, &task.task_id)?;
            if matches!(
                task_state,
                TaskLifecycleState::Queued
                    | TaskLifecycleState::Blocked
                    | TaskLifecycleState::Retryable
            ) {
                TaskRecord::transition_state(
                    &store,
                    &task.task_id,
                    &attempt.attempt_id,
                    TaskLifecycleState::Ready,
                )?;
            }

            let refreshed_attempt = TaskAttemptRecord::get(&store, &attempt.attempt_id)?;
            let refreshed_attempt_state = refreshed_attempt.state()?;
            if matches!(
                refreshed_attempt_state,
                AttemptLifecycleState::Queued
                    | AttemptLifecycleState::Blocked
                    | AttemptLifecycleState::Retryable
            ) {
                TaskAttemptRecord::transition_state(
                    &store,
                    &refreshed_attempt.attempt_id,
                    AttemptLifecycleState::Ready,
                )?;
            }

            let runnable_attempt = TaskAttemptRecord::get(&store, &attempt.attempt_id)?;
            if runnable_attempt.state()? != AttemptLifecycleState::Ready {
                continue;
            }

            let recovery_bootstrap =
                recovery_bootstrap_from_checkpoint(runnable_attempt.recovery_checkpoint.as_deref());

            let prompt = task
                .title
                .clone()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| format!("Execute task {}", task.task_id));
            let summary = recovery_bootstrap
                .as_ref()
                .and_then(|checkpoint| checkpoint.handoff_summary.clone())
                .filter(|summary| !summary.trim().is_empty())
                .unwrap_or_else(|| format!("Delegated DAG node {}", task.task_id));

            let task_owner_session_id = task.owner_session_id.clone().context(
                "scheduler candidate is missing owner_session_id after owner-scoped filtering",
            )?;

            return Ok(Some(SchedulerCandidate {
                owner_session_id: task_owner_session_id,
                task_id: task.task_id,
                attempt_id: runnable_attempt.attempt_id,
                handoff_summary: summary,
                handoff_artifact_ids: recovery_bootstrap
                    .as_ref()
                    .map(|checkpoint| checkpoint.handoff_artifact_ids.clone())
                    .unwrap_or_default(),
                recovery_child_session_id: recovery_bootstrap
                    .as_ref()
                    .and_then(|checkpoint| checkpoint.child_session_id.clone()),
                recovery_inherited_settings: recovery_bootstrap
                    .and_then(|checkpoint| checkpoint.inherited_settings),
                execution_prompt: prompt,
            }));
        }

        Ok(None)
    }
}

fn latest_or_new_attempt(store: &Store, task_id: &str) -> Result<TaskAttemptRecord> {
    let attempts = TaskAttemptRecord::list_for_task(store, task_id)?;
    if let Some(latest) = attempts.last() {
        return Ok(latest.clone());
    }

    TaskAttemptRecord::create(
        store,
        NewTaskAttemptRecord {
            attempt_id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            session_id: None,
            status: AttemptLifecycleState::Queued.to_string(),
            recovery_checkpoint: None,
        },
    )
}

fn lock_shared_store(shared_store: &SharedStore) -> Result<MutexGuard<'_, Store>> {
    shared_store
        .lock()
        .map_err(|err| anyhow::anyhow!("store mutex poisoned: {err}"))
}

fn is_task_terminal(state: TaskLifecycleState) -> bool {
    matches!(
        state,
        TaskLifecycleState::Completed | TaskLifecycleState::Failed | TaskLifecycleState::Cancelled
    )
}

fn is_attempt_terminal(state: AttemptLifecycleState) -> bool {
    matches!(
        state,
        AttemptLifecycleState::Completed
            | AttemptLifecycleState::Failed
            | AttemptLifecycleState::Cancelled
    )
}

fn collect_descendant_task_ids(store: &Store, root_task_id: &str) -> Result<Vec<String>> {
    let _ = TaskRecord::get(store, root_task_id)?;
    let edges = TaskEdgeRecord::list(store)?;

    let mut visited = HashSet::new();
    visited.insert(root_task_id.to_string());

    let mut queue = vec![root_task_id.to_string()];
    let mut index = 0usize;
    while index < queue.len() {
        let current = queue[index].clone();
        index = index.saturating_add(1);

        let mut direct_children = edges
            .iter()
            .filter(|edge| edge.depends_on_task_id == current)
            .map(|edge| edge.task_id.clone())
            .collect::<Vec<_>>();
        direct_children.sort();

        for child in direct_children {
            if visited.insert(child.clone()) {
                queue.push(child);
            }
        }
    }

    let mut descendants = queue;
    descendants.sort();
    Ok(descendants)
}

fn request_descendant_cancellation_before_recovery(
    store: &Store,
    root_task_id: &str,
) -> Result<()> {
    let task_ids = collect_descendant_task_ids(store, root_task_id)?;
    for descendant_task_id in task_ids {
        if descendant_task_id == root_task_id {
            continue;
        }
        cancel_single_task(store, &descendant_task_id)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct RecoveryBootstrapCheckpoint {
    handoff_summary: Option<String>,
    handoff_artifact_ids: Vec<String>,
    child_session_id: Option<String>,
    inherited_settings: Option<InheritedSettingsSnapshot>,
}

fn recovery_bootstrap_from_checkpoint(
    recovery_checkpoint: Option<&str>,
) -> Option<RecoveryBootstrapCheckpoint> {
    let raw = recovery_checkpoint?;
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let child_bootstrap = parsed.get("child_bootstrap")?;

    let handoff = child_bootstrap.get("handoff");
    let handoff_summary = handoff
        .and_then(|value| value.get("summary"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let handoff_artifact_ids = handoff
        .and_then(|value| value.get("artifact_ids"))
        .and_then(serde_json::Value::as_array)
        .map(|artifact_ids| {
            artifact_ids
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let child_session_id = child_bootstrap
        .get("child_session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty());
    let inherited_settings = handoff
        .and_then(|value| value.get("inherited_settings"))
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value::<InheritedSettingsSnapshot>(value.clone()).ok());

    Some(RecoveryBootstrapCheckpoint {
        handoff_summary,
        handoff_artifact_ids,
        child_session_id,
        inherited_settings,
    })
}

fn cancel_single_task(store: &Store, task_id: &str) -> Result<()> {
    let latest_attempt = latest_or_new_attempt(store, task_id)?;
    let attempt_state = latest_attempt.state()?;
    let task_state = TaskRecord::current_state(store, task_id)?;

    if is_task_terminal(task_state) || is_attempt_terminal(attempt_state) {
        return Ok(());
    }

    if task_state == TaskLifecycleState::CancelRequested
        || attempt_state == AttemptLifecycleState::CancelRequested
    {
        if task_state != TaskLifecycleState::CancelRequested {
            TaskRecord::transition_state(
                store,
                task_id,
                &latest_attempt.attempt_id,
                TaskLifecycleState::CancelRequested,
            )?;
        }
        if attempt_state != AttemptLifecycleState::CancelRequested {
            TaskAttemptRecord::transition_state(
                store,
                &latest_attempt.attempt_id,
                AttemptLifecycleState::CancelRequested,
            )?;
        }
        return Ok(());
    }

    if task_state == TaskLifecycleState::Running || attempt_state == AttemptLifecycleState::Running
    {
        if task_state != TaskLifecycleState::CancelRequested {
            TaskRecord::transition_state(
                store,
                task_id,
                &latest_attempt.attempt_id,
                TaskLifecycleState::CancelRequested,
            )?;
        }
        if attempt_state != AttemptLifecycleState::CancelRequested {
            TaskAttemptRecord::transition_state(
                store,
                &latest_attempt.attempt_id,
                AttemptLifecycleState::CancelRequested,
            )?;
        }
        return Ok(());
    }

    if task_state != TaskLifecycleState::Cancelled {
        TaskRecord::transition_state(
            store,
            task_id,
            &latest_attempt.attempt_id,
            TaskLifecycleState::Cancelled,
        )?;
    }
    if attempt_state != AttemptLifecycleState::Cancelled {
        TaskAttemptRecord::transition_state(
            store,
            &latest_attempt.attempt_id,
            AttemptLifecycleState::Cancelled,
        )?;
    }

    Ok(())
}

fn create_fresh_attempt_for_control(
    store: &Store,
    task_id: &str,
    allowed_states: &[TaskLifecycleState],
    transition_source_state: TaskLifecycleState,
    action: &str,
    carry_checkpoint: bool,
) -> Result<String> {
    let task_state = TaskRecord::current_state(store, task_id)?;
    if !allowed_states.contains(&task_state) {
        anyhow::bail!(
            "{action} is only allowed from {:?}: task {task_id} is {task_state}",
            allowed_states
        );
    }

    let latest_attempt = latest_or_new_attempt(store, task_id)?;
    let latest_attempt_state = latest_attempt.state()?;

    if task_state == transition_source_state {
        TaskRecord::transition_state(
            store,
            task_id,
            &latest_attempt.attempt_id,
            TaskLifecycleState::Retryable,
        )?;
    }

    if matches!(
        latest_attempt_state,
        AttemptLifecycleState::Failed | AttemptLifecycleState::Interrupted
    ) {
        TaskAttemptRecord::transition_state(
            store,
            &latest_attempt.attempt_id,
            AttemptLifecycleState::Retryable,
        )?;
    }

    let new_attempt = TaskAttemptRecord::create(
        store,
        NewTaskAttemptRecord {
            attempt_id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            session_id: None,
            status: AttemptLifecycleState::Queued.to_string(),
            recovery_checkpoint: if carry_checkpoint {
                latest_attempt.recovery_checkpoint.clone()
            } else {
                None
            },
        },
    )?;

    TaskRecord::transition_state(
        store,
        task_id,
        &new_attempt.attempt_id,
        TaskLifecycleState::Queued,
    )?;

    Ok(new_attempt.attempt_id)
}

fn transition_attempt_if_needed(
    store: &Store,
    attempt_id: &str,
    next: AttemptLifecycleState,
) -> Result<()> {
    if TaskAttemptRecord::get(store, attempt_id)?.state()? != next {
        TaskAttemptRecord::transition_state(store, attempt_id, next)?;
    }
    Ok(())
}

fn transition_task_if_needed(
    store: &Store,
    task_id: &str,
    attempt_id: &str,
    next: TaskLifecycleState,
) -> Result<()> {
    if TaskRecord::current_state(store, task_id)? != next {
        TaskRecord::transition_state(store, task_id, attempt_id, next)?;
    }
    Ok(())
}

fn assistant_message_context_chars(content: &str) -> usize {
    if content.is_empty() {
        return 0;
    }

    crate::compact::estimate_history_chars(&[serde_json::json!({
        "type": "message",
        "role": "assistant",
        "content": content,
    })])
}

fn runtime_registry(project_dir: PathBuf, lsp_service: Arc<dyn LspService>) -> ToolRegistry {
    crate::tools::registry_with_lsp_service(project_dir, lsp_service)
}

fn apply_lsp_status_report(
    states: &mut HashMap<(String, String), RuntimeLspServerState>,
    status: &str,
    server_id: Option<&str>,
    command: Option<&[String]>,
    workspace_root: Option<&str>,
    last_file: Option<&str>,
    last_error: Option<&str>,
) -> bool {
    let Some(server_id) = server_id else {
        return true;
    };
    let Some(workspace_root) = workspace_root else {
        return true;
    };

    let key = (server_id.to_string(), workspace_root.to_string());
    let is_new = !states.contains_key(&key);
    let entry = states.entry(key).or_insert_with(|| RuntimeLspServerState {
        server_id: server_id.to_string(),
        status: lsp_status::DETECTED.to_string(),
        command: command.map(|parts| parts.to_vec()).unwrap_or_default(),
        workspace_root: workspace_root.to_string(),
        last_file: last_file.map(str::to_string),
        last_error: last_error.map(str::to_string),
    });

    if let Some(command) = command
        && !command.is_empty()
    {
        entry.command = command.to_vec();
    }
    if let Some(last_file) = last_file {
        entry.last_file = Some(last_file.to_string());
    }
    if let Some(last_error) = last_error {
        entry.last_error = Some(last_error.to_string());
    }

    match status {
        lsp_status::DETECTED => {
            if is_new {
                return true;
            }
            if entry.status == lsp_status::DETECTED {
                return false;
            }
            if matches!(
                entry.status.as_str(),
                lsp_status::STARTING | lsp_status::READY | lsp_status::FAILED
            ) {
                return false;
            }
            entry.status = status.to_string();
            true
        }
        lsp_status::STARTING => {
            if matches!(
                entry.status.as_str(),
                lsp_status::READY | lsp_status::FAILED
            ) {
                return false;
            }
            if entry.status == status {
                return false;
            }
            entry.status = status.to_string();
            true
        }
        lsp_status::READY => {
            if entry.status == status {
                return false;
            }
            entry.status = status.to_string();
            entry.last_error = None;
            true
        }
        lsp_status::FAILED => {
            if entry.status == status {
                return false;
            }
            entry.status = status.to_string();
            true
        }
        _ => true,
    }
}

fn join_error(err: JoinError) -> anyhow::Error {
    anyhow::anyhow!("runtime worker join error: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::{LspClient, LspClientError};
    use crate::store::{NewSession, Session, SessionStatus};
    use crate::test_openai::{self, ControlledResponse, TEST_MODEL};
    use anyhow::anyhow;
    use axum::Router;
    use axum::body::Bytes;
    use axum::http::header;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use serde_json::{Value, json};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicUsize, Ordering};

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
                Some(previous) => unsafe { std::env::set_var(self.key, previous) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
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

    async fn controlled_openai_handler(body: Bytes) -> impl IntoResponse {
        let payload: Value = serde_json::from_slice(&body).unwrap_or_default();
        let prompt = payload
            .get("input")
            .and_then(Value::as_array)
            .and_then(|items| items.last())
            .and_then(|item| item.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        let response =
            test_openai::parse_controlled_prompt(prompt).unwrap_or(ControlledResponse::Text {
                content: format!("Mock assistant reply: {prompt}"),
            });

        let body = match response {
            ControlledResponse::ToolCall { name, arguments } => {
                test_openai::tool_call_sse(&name, &arguments)
            }
            ControlledResponse::Text { content } => test_openai::text_sse(&content),
        };

        ([(header::CONTENT_TYPE, "text/event-stream")], body)
    }

    #[test]
    fn finish_prompt_err_clears_active_state_and_emits_event() {
        let store = crate::store::Store::open_memory().expect("failed to open in-memory store");
        let session = Session::create(
            &store,
            NewSession {
                model: TEST_MODEL.to_string(),
                provider: "openai".to_string(),
            },
        )
        .expect("failed to create session");

        let manager = RuntimeManager::new();
        manager
            .attach_controller(&session, "controller-1")
            .expect("failed to attach controller");

        manager
            .start_active_turn(
                &session.id,
                "request-id".to_string(),
                "turn-1".to_string(),
                "message-1".to_string(),
            )
            .expect("failed to start active turn");

        let mut receiver = manager
            .subscribe(&session.id)
            .expect("failed to subscribe to runtime events");

        {
            let mut sessions = manager.lock_sessions();
            if let Some(entry) = sessions.get_mut(&session.id) {
                entry.active_prompt = Some(ActivePrompt {
                    turn_id: "turn-1".to_string(),
                    abort_signal: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                });
            }
        }

        manager.finish_prompt(
            &session.id,
            "request-id",
            "turn-1",
            Err(anyhow!("runtime worker failed")),
        );

        assert!(manager.active_turn(&session.id).is_none());

        let runtime_sessions = manager.lock_sessions();
        let entry = runtime_sessions
            .get(&session.id)
            .expect("manager session should exist");
        assert!(entry.active_prompt.is_none());

        let emitted = receiver
            .try_recv()
            .expect("expected runtime manager event to be emitted")
            .event;
        match emitted {
            AgentEvent::TurnFailed { error, .. } => {
                assert!(error.contains("runtime worker failed"));
            }
            _ => panic!("unexpected event type"),
        }
    }

    #[test]
    fn lock_sessions_recovers_from_poisoned_mutex() {
        let store = crate::store::Store::open_memory().expect("failed to open in-memory store");
        let session = Session::create(
            &store,
            NewSession {
                model: TEST_MODEL.to_string(),
                provider: "openai".to_string(),
            },
        )
        .expect("failed to create session");

        let manager = RuntimeManager::new();
        manager
            .attach_controller(&session, "controller-1")
            .expect("failed to attach controller");

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = manager.lock_sessions();
            panic!("intentional lock poisoning for regression coverage");
        }));

        assert!(
            manager
                .start_active_turn(
                    &session.id,
                    "request-id".to_string(),
                    "turn-1".to_string(),
                    "message-1".to_string(),
                )
                .is_ok()
        );
    }

    #[test]
    fn active_turn_context_is_unknown_before_turn_started() {
        let store = crate::store::Store::open_memory().expect("failed to open in-memory store");
        let session = Session::create(
            &store,
            NewSession {
                model: TEST_MODEL.to_string(),
                provider: "openai".to_string(),
            },
        )
        .expect("failed to create session");

        let manager = RuntimeManager::new();
        manager
            .attach_controller(&session, "controller-1")
            .expect("failed to attach controller");

        manager
            .start_active_turn(
                &session.id,
                "request-id".to_string(),
                "turn-1".to_string(),
                "message-1".to_string(),
            )
            .expect("failed to start active turn");

        assert_eq!(manager.context_usage_chars(&session.id), None);
    }

    #[test]
    fn message_delta_uses_serialized_assistant_message_size() {
        let store = crate::store::Store::open_memory().expect("failed to open in-memory store");
        let session = Session::create(
            &store,
            NewSession {
                model: TEST_MODEL.to_string(),
                provider: "openai".to_string(),
            },
        )
        .expect("failed to create session");

        let manager = RuntimeManager::new();
        manager
            .attach_controller(&session, "controller-1")
            .expect("failed to attach controller");
        manager
            .start_active_turn(
                &session.id,
                "request-id".to_string(),
                "turn-1".to_string(),
                "message-1".to_string(),
            )
            .expect("failed to start active turn");

        manager.publish_runtime_event(
            &session.id,
            "request-id",
            AgentEvent::TurnStarted {
                session_id: session.id.clone(),
                turn_id: "turn-1".to_string(),
                model: TEST_MODEL.to_string(),
                turn_number: 1,
                context_used_chars: 100,
                context_max_chars: 1000,
            },
        );

        manager.publish_runtime_event(
            &session.id,
            "request-id",
            AgentEvent::MessageDelta {
                session_id: session.id.clone(),
                turn_id: "turn-1".to_string(),
                message_id: "message-1".to_string(),
                delta: "line 1\n\"quoted\"".to_string(),
            },
        );

        let first_expected = 100 + assistant_message_context_chars("line 1\n\"quoted\"");
        assert_eq!(
            manager.context_usage_chars(&session.id),
            Some((first_expected, 1000))
        );

        manager.publish_runtime_event(
            &session.id,
            "request-id",
            AgentEvent::MessageDelta {
                session_id: session.id.clone(),
                turn_id: "turn-1".to_string(),
                message_id: "message-1".to_string(),
                delta: " plus more".to_string(),
            },
        );

        let second_expected = 100 + assistant_message_context_chars("line 1\n\"quoted\" plus more");
        assert_eq!(
            manager.context_usage_chars(&session.id),
            Some((second_expected, 1000))
        );
    }

    #[test]
    fn runtime_worker_uses_session_settings_overrides() {
        let session = Session {
            id: "session-1".to_string(),
            title: None,
            status: SessionStatus::Active,
            model: "stored-model".to_string(),
            provider: "stored-provider".to_string(),
            policy: None,
            settings: Some(
                serde_json::json!({
                    "model": "settings-model",
                    "provider": "settings-provider",
                    "compact_threshold": 12_345,
                })
                .to_string(),
            ),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let worker = RuntimeWorker::from_session(&session, None);

        assert_eq!(worker.model, "settings-model");
        assert_eq!(worker.provider, "settings-provider");
        assert_eq!(worker.compact_config.threshold_chars, 12_345);
    }

    #[test]
    fn runtime_worker_keeps_default_compact_threshold_for_legacy_settings() {
        let session = Session {
            id: "session-2".to_string(),
            title: None,
            status: SessionStatus::Active,
            model: "stored-model".to_string(),
            provider: "stored-provider".to_string(),
            policy: None,
            settings: Some(
                serde_json::json!({
                    "model": "settings-model",
                    "provider": "settings-provider",
                })
                .to_string(),
            ),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let worker = RuntimeWorker::from_session(&session, None);

        assert_eq!(worker.model, "settings-model");
        assert_eq!(worker.provider, "settings-provider");
        assert_eq!(
            worker.compact_config.threshold_chars,
            CompactConfig::default().threshold_chars
        );
    }

    struct CountingLspClient;

    impl LspClient for CountingLspClient {
        fn request(&self, method: &str, _params: Value) -> Result<Value, LspClientError> {
            match method {
                "initialize" => Ok(json!({})),
                "textDocument/diagnostic" => Ok(json!({ "items": [] })),
                _ => Ok(json!([])),
            }
        }
    }

    struct CountingLspFactory {
        create_calls: AtomicUsize,
    }

    impl CountingLspFactory {
        fn new() -> Self {
            Self {
                create_calls: AtomicUsize::new(0),
            }
        }

        fn create_calls(&self) -> usize {
            self.create_calls.load(Ordering::Relaxed)
        }
    }

    impl LspClientFactory for CountingLspFactory {
        fn create(
            &self,
            _command: &[String],
            _workspace_root: &Path,
        ) -> std::result::Result<Arc<dyn LspClient>, String> {
            self.create_calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(CountingLspClient))
        }
    }

    #[tokio::test]
    async fn runtime_worker_reuses_lsp_manager_across_prompt_submissions() {
        let shared_store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
        let session = {
            let store = shared_store.lock().unwrap();
            Session::create(
                &store,
                NewSession {
                    model: TEST_MODEL.to_string(),
                    provider: "openai".to_string(),
                },
            )
            .unwrap()
        };
        let server =
            spawn_app(Router::new().route("/responses", post(controlled_openai_handler))).await;

        let fixture = tempfile::tempdir().unwrap();
        let file_path = fixture.path().join("sample.rs");
        std::fs::write(&file_path, "fn main() {}\n").unwrap();

        let factory = Arc::new(CountingLspFactory::new());
        let worker = RuntimeWorker::from_session(&session, Some(factory.clone()));
        let tool_args = json!({
            "file_path": file_path.display().to_string(),
            "severity": "all",
            "extension": null,
        });
        let prompt = test_openai::controlled_response_prompt(ControlledResponse::ToolCall {
            name: "lsp_diagnostics".to_string(),
            arguments: tool_args,
        });

        let _api_key = EnvVarGuard::set("OPENAI_API_KEY", "test-key");
        let _base_url = EnvVarGuard::set("OPENAI_BASE_URL", &format!("http://{}", server.addr));

        let mut worker = worker;
        worker.provider = "openai".to_string();
        worker.model = TEST_MODEL.to_string();
        worker.session_id = session.id.clone();

        let (stream_tx_one, _stream_rx_one) = mpsc::unbounded_channel();
        let first = worker
            .submit_prompt_stream(
                Arc::clone(&shared_store),
                prompt.clone(),
                TurnCorrelation {
                    turn_id: "turn-1".to_string(),
                    message_id: "message-1".to_string(),
                },
                Arc::new(StdAtomicBool::new(false)),
                stream_tx_one,
            )
            .await
            .unwrap();

        let (stream_tx_two, _stream_rx_two) = mpsc::unbounded_channel();
        let _server_auth = test_openai::auth(format!("http://{}", server.addr));
        let second = worker
            .submit_prompt_stream(
                Arc::clone(&shared_store),
                prompt,
                TurnCorrelation {
                    turn_id: "turn-2".to_string(),
                    message_id: "message-2".to_string(),
                },
                Arc::new(StdAtomicBool::new(false)),
                stream_tx_two,
            )
            .await
            .unwrap();

        assert!(matches!(first, SubmitResult::Completed { .. }));
        assert!(matches!(second, SubmitResult::Completed { .. }));
        assert_eq!(factory.create_calls(), 1);
    }
}
