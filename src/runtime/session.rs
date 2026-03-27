use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use uuid::Uuid;

use super::{SessionSettingsOverrides, canonical_settings_json};
use crate::auth::ResolvedAuth;
use crate::compact::CompactConfig;
use crate::events::{AgentEvent, EventEmitter};
use crate::provider::{Provider, SendContext, TokenUsage, TurnResult, merge_token_usage};
use crate::store::{NewSession, NewTurn, Session, SessionStatus, SharedStore, Store, Turn};
use crate::text::truncate_with_ascii_ellipsis;
use crate::tools::ToolRegistry;

const DEFAULT_OPENAI_MODEL: &str = "gpt-5.3-codex-spark";
const DEFAULT_ZAI_MODEL: &str = "glm-4.7";
const REQUEST_COMPACTION_MARGIN_CHARS: usize = 8_192;
const CONTEXT_OVERFLOW_RETRY_LIMIT: usize = 2;

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
    },
}

type DeltaHook = Arc<dyn Fn(&str) + Send + Sync>;
type EventHook = Arc<dyn Fn(RuntimeEvent) + Send + Sync>;

#[derive(Default, Clone)]
pub struct RuntimeHooks {
    pub on_output_delta: Option<DeltaHook>,
    pub on_event: Option<EventHook>,
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
        registry: ToolRegistry,
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
        registry: ToolRegistry,
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

                        let (output, success) = match self.registry.get(&call.name) {
                            Some(tool) => {
                                if self.abort_signal.load(Ordering::Relaxed) {
                                    return self.finish_aborted_submit(
                                        &turn_id,
                                        &message_id,
                                        turn_number,
                                    );
                                }
                                let args: serde_json::Value =
                                    serde_json::from_str(&call.arguments).unwrap_or_default();
                                match tool.execute(args) {
                                    Ok(result) => (result, true),
                                    Err(e) => (format!("Tool error: {e:#}"), false),
                                }
                            }
                            None => (format!("Error: unknown tool '{}'", call.name), false),
                        };

                        let output_preview =
                            truncate(output.lines().next().unwrap_or("(empty)"), 80);
                        self.emit_runtime_event(RuntimeEvent::ToolCallCompleted {
                            call_id: call.call_id.clone(),
                            name: call.name.clone(),
                            output_preview: output_preview.clone(),
                        });

                        self.with_store(|store| {
                            Turn::append(
                                store,
                                NewTurn {
                                    session_id: self.session.id.clone(),
                                    kind: "function_call_output".into(),
                                    role: "tool".into(),
                                    content: serde_json::json!({
                                        "call_id": call.call_id,
                                        "output": output.clone(),
                                    })
                                    .to_string(),
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
                        self.history.push(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": call.call_id,
                            "output": output,
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
                        self.events.emit(AgentEvent::ToolCallCompleted {
                            session_id: self.session.id.clone(),
                            turn_id: turn_id.clone(),
                            message_id: message_id.clone(),
                            tool_call_id: call.call_id.clone(),
                            tool_name: call.name.clone(),
                            output_preview,
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
                    items.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": v.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
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
                    .to_string();
                let output = parsed
                    .get("output")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&t.content)
                    .to_string();
                items.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
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
    use crate::events::event_channel;
    use crate::store::Session;

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
