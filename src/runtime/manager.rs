use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, ensure};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinError;

use crate::auth::{CredentialStore, ResolvedAuth};
use crate::compact::CompactConfig;
use crate::events::{AgentEvent, event_channel};
use crate::runtime::{RuntimeHooks, SessionRuntime, SubmitResult, TurnCorrelation};
use crate::store::{Session, SharedStore};

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

#[derive(Debug, Clone)]
struct RuntimeWorker {
    session_id: String,
    model: String,
    provider: String,
    project_dir: PathBuf,
    instructions: String,
    compact_config: CompactConfig,
}

impl RuntimeWorker {
    fn from_session(session: &Session) -> Self {
        let mut model = session.model.clone();
        let mut provider = session.provider.clone();
        let mut compact_config = CompactConfig::default();
        if let Some(settings) = &session.settings
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(settings)
        {
            if let Some(settings_model) = json.get("model").and_then(|v| v.as_str()) {
                model = settings_model.to_string();
            }
            if let Some(settings_provider) = json.get("provider").and_then(|v| v.as_str()) {
                provider = settings_provider.to_string();
            }
            if let Some(threshold) = json.get("compact_threshold").and_then(|v| v.as_u64())
                && let Ok(threshold_chars) = usize::try_from(threshold)
            {
                compact_config.threshold_chars = threshold_chars.max(1);
            }
        }

        let project_dir = std::env::current_dir().unwrap_or_default();
        let rules = crate::skills::discover_rules(&project_dir);
        let skills = crate::skills::discover_skills(&project_dir);
        let instructions = crate::skills::build_system_prompt(&rules, &skills);

        Self {
            session_id: session.id.clone(),
            model,
            provider,
            project_dir,
            instructions,
            compact_config,
        }
    }

    fn resolved_auth(
        &self,
        runtime_rt: &tokio::runtime::Runtime,
        events: &crate::events::EventEmitter,
    ) -> Result<ResolvedAuth> {
        if self.provider == "test" {
            return Ok(ResolvedAuth {
                provider: "test".to_string(),
                api_key: "test-key".to_string(),
                base_url: "http://unused".to_string(),
                account_id: None,
            });
        }

        let store = CredentialStore::open().context("failed to open credential store")?;
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
            let registry = crate::tools::default_registry(worker.project_dir.clone());
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
            )?;

            let submit = runtime_rt
                .block_on(runtime.submit_prompt_with_ids(prompt, correlation))
                .context("runtime submit failed")?;

            drop(runtime);
            drop(events);
            let _ = forwarder.join();

            Ok(submit)
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
}

#[derive(Debug, Default)]
pub struct RuntimeManager {
    sessions: Mutex<HashMap<String, ManagedSession>>,
}

impl RuntimeManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_sessions(&self) -> MutexGuard<'_, HashMap<String, ManagedSession>> {
        match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub fn attach_controller(
        &self,
        session: &Session,
        controller_id: &str,
    ) -> Result<(), AttachControllerError> {
        let mut sessions = self.lock_sessions();
        let entry = sessions
            .entry(session.id.clone())
            .or_insert_with(|| ManagedSession {
                runtime: RuntimeWorker::from_session(session),
                settings: session.settings.clone(),
                controller_id: None,
                active_turn: None,
                last_context_usage: None,
                last_token_usage: None,
                active_prompt: None,
                events: broadcast::channel(256).0,
            });

        entry.settings = session.settings.clone();
        entry.runtime = RuntimeWorker::from_session(session);

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

    pub fn submit_prompt(
        self: &Arc<Self>,
        shared_store: SharedStore,
        session_id: &str,
        prompt: String,
    ) -> std::result::Result<SubmitPromptOutcome, SubmitPromptError> {
        let (worker, active_turn, abort_signal) = {
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

            (entry.runtime.clone(), active_turn, abort_signal)
        };

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
            let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
            let request_id_for_events = request_id.clone();
            let session_id_for_events = session_id_owned.clone();
            let event_manager = Arc::clone(&manager);

            let worker_task = tokio::spawn(async move {
                worker
                    .submit_prompt_stream(
                        shared_store,
                        prompt,
                        correlation,
                        abort_signal,
                        stream_tx,
                    )
                    .await
            });

            while let Some(event) = stream_rx.recv().await {
                event_manager.publish_runtime_event(
                    &session_id_for_events,
                    &request_id_for_events,
                    event,
                );
            }

            let result = match worker_task.await {
                Ok(result) => result,
                Err(err) => Err(join_error(err)),
            };

            manager.finish_prompt(&session_id_owned, &request_id, &finish_turn_id, result);
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

        let _ = entry.events.send(RuntimeEventEnvelope {
            request_id: request_id.to_string(),
            event: event.clone(),
        });

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewSession, Session};
    use anyhow::anyhow;

    #[test]
    fn finish_prompt_err_clears_active_state_and_emits_event() {
        let store = crate::store::Store::open_memory().expect("failed to open in-memory store");
        let session = Session::create(
            &store,
            NewSession {
                model: "test-model".to_string(),
                provider: "test".to_string(),
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
                model: "test-model".to_string(),
                provider: "test".to_string(),
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
                model: "test-model".to_string(),
                provider: "test".to_string(),
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
                model: "test-model".to_string(),
                provider: "test".to_string(),
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
                model: "test-model".to_string(),
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
}
fn join_error(err: JoinError) -> anyhow::Error {
    anyhow::anyhow!("runtime worker join error: {err}")
}
