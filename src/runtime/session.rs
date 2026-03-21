use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite;
use uuid::Uuid;

use crate::auth::ResolvedAuth;
use crate::compact::CompactConfig;
use crate::events::{AgentEvent, EventEmitter, Transport};
use crate::store::{NewSession, NewTurn, Session, SessionStatus, SharedStore, Store, Turn};
use crate::text::truncate_with_ascii_ellipsis;
use crate::tools::ToolRegistry;

const DEFAULT_OPENAI_MODEL: &str = "gpt-5.3-codex-spark";
const DEFAULT_ZAI_MODEL: &str = "glm-4.7";
const RESPONSES_WS_BETA_HEADER: &str = "responses_websockets=2026-02-06";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug)]
enum TurnResult {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    Aborted,
}

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
    ws_disabled: AtomicBool,
    registry: ToolRegistry,
    instructions: String,
    hooks: RuntimeHooks,
    abort_signal: Arc<AtomicBool>,
    active_turn: bool,
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
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_abort_signal(
        store: &'a Store,
        resolved: ResolvedAuth,
        model_override: Option<&str>,
        resume_session_id: Option<&str>,
        events: EventEmitter,
        compact_config: CompactConfig,
        registry: ToolRegistry,
        instructions: String,
        hooks: RuntimeHooks,
        abort_signal: Arc<AtomicBool>,
    ) -> Result<Self> {
        let model = model_override
            .map(String::from)
            .unwrap_or_else(|| Self::default_model(&resolved.provider));

        let session = match resume_session_id {
            Some(id) => {
                let s = Session::get(store, id)?;
                if let Some(hook) = &hooks.on_event {
                    hook(RuntimeEvent::SessionResumed {
                        session_id: s.id.clone(),
                    });
                }
                s
            }
            None => {
                let s = Session::create(
                    store,
                    NewSession {
                        model: model.clone(),
                        provider: resolved.provider.clone(),
                    },
                )?;
                if let Some(hook) = &hooks.on_event {
                    hook(RuntimeEvent::SessionCreated {
                        session_id: s.id.clone(),
                    });
                }
                s
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

        Ok(Self {
            store: RuntimeStore::Borrowed(store),
            events,
            compact_config,
            resolved,
            model,
            session,
            history,
            turn_number: existing_turns.len(),
            ws_disabled: AtomicBool::new(false),
            registry,
            instructions,
            abort_signal,
            hooks,
            active_turn: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_shared_store_and_abort_signal(
        shared_store: SharedStore,
        resolved: ResolvedAuth,
        model_override: Option<&str>,
        resume_session_id: Option<&str>,
        events: EventEmitter,
        compact_config: CompactConfig,
        registry: ToolRegistry,
        instructions: String,
        hooks: RuntimeHooks,
        abort_signal: Arc<AtomicBool>,
    ) -> Result<Self> {
        let model = model_override
            .map(String::from)
            .unwrap_or_else(|| Self::default_model(&resolved.provider));

        let store_guard = shared_store
            .lock()
            .map_err(|e| anyhow::anyhow!("store mutex poisoned: {e}"))?;

        let session = match resume_session_id {
            Some(id) => {
                let s = Session::get(&store_guard, id)?;
                if let Some(hook) = &hooks.on_event {
                    hook(RuntimeEvent::SessionResumed {
                        session_id: s.id.clone(),
                    });
                }
                s
            }
            None => {
                let s = Session::create(
                    &store_guard,
                    NewSession {
                        model: model.clone(),
                        provider: resolved.provider.clone(),
                    },
                )?;
                if let Some(hook) = &hooks.on_event {
                    hook(RuntimeEvent::SessionCreated {
                        session_id: s.id.clone(),
                    });
                }
                s
            }
        };

        let existing_turns = Turn::list_for_session(&store_guard, &session.id)?;
        drop(store_guard);

        let history = history_items_from_turns(&existing_turns);
        if !existing_turns.is_empty()
            && let Some(hook) = &hooks.on_event
        {
            hook(RuntimeEvent::HistoryLoaded {
                turns: existing_turns.len(),
            });
        }

        Ok(Self {
            store: RuntimeStore::Shared(shared_store),
            events,
            compact_config,
            resolved,
            model,
            session,
            history,
            turn_number: existing_turns.len(),
            ws_disabled: AtomicBool::new(false),
            registry,
            instructions,
            abort_signal,
            hooks,
            active_turn: false,
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
        let (context_used_chars, context_max_chars) =
            context_usage_chars(&self.history, self.compact_config.threshold_chars);

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
        let final_text = loop {
            crate::compact::maybe_compact(
                &self.resolved,
                &self.model,
                &mut self.history,
                &self.compact_config,
                &self.events,
            )
            .await
            .ok();

            if self.abort_signal.load(Ordering::Relaxed) {
                return self.finish_aborted_submit(&turn_id, &message_id, turn_number);
            }

            let output_hook = |delta: &str| {
                self.emit_output_delta(delta);
                self.events.emit(AgentEvent::MessageDelta {
                    session_id: self.session.id.clone(),
                    turn_id: turn_id.clone(),
                    message_id: message_id.clone(),
                    delta: delta.to_string(),
                });
            };

            let result = match self.resolved.provider.as_str() {
                "openai" => {
                    send_openai(
                        &self.resolved,
                        OpenaiRequestContext {
                            model: &self.model,
                            session_id: &self.session.id,
                            turn_id: &turn_id,
                            history: &self.history,
                            registry: &self.registry,
                            instructions: &self.instructions,
                            ws_disabled: &self.ws_disabled,
                            abort_signal: &self.abort_signal,
                            events: &self.events,
                            output_hook: Some(&output_hook),
                        },
                    )
                    .await
                }
                "zai" => {
                    self.events.emit(AgentEvent::TransportSelected {
                        session_id: Some(self.session.id.clone()),
                        turn_id: Some(turn_id.clone()),
                        provider: self.resolved.provider.clone(),
                        transport: Transport::Sse,
                    });
                    send_zai_sse(
                        &self.resolved,
                        &self.model,
                        &messages_from_history(&self.history),
                        &self.abort_signal,
                        Some(&output_hook),
                    )
                    .await
                }
                "test" => match test_provider_result(&self.history) {
                    TurnResult::ToolCalls(calls) => Ok(TurnResult::ToolCalls(calls)),
                    _ => {
                        run_test_provider(&self.history, Some(&output_hook), &self.abort_signal)
                            .await
                    }
                },
                other => anyhow::bail!("unsupported provider in runtime: {other}"),
            };

            match result {
                Ok(TurnResult::Text(text)) => break Ok(text),
                Ok(TurnResult::Aborted) => {
                    return self.finish_aborted_submit(&turn_id, &message_id, turn_number);
                }
                Ok(TurnResult::ToolCalls(calls)) => {
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

                        self.emit_runtime_event(RuntimeEvent::ToolCallCompleted {
                            call_id: call.call_id.clone(),
                            name: call.name.clone(),
                            output_preview: truncate(
                                output.lines().next().unwrap_or("(empty)"),
                                80,
                            ),
                        });
                        self.events.emit(AgentEvent::ToolCallCompleted {
                            session_id: self.session.id.clone(),
                            turn_id: turn_id.clone(),
                            message_id: message_id.clone(),
                            tool_call_id: call.call_id.clone(),
                            tool_name: call.name.clone(),
                            output_preview: truncate(
                                output.lines().next().unwrap_or("(empty)"),
                                80,
                            ),
                            success,
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
                    }
                    continue;
                }
                Err(err) => break Err(err),
            }
        };

        self.active_turn = false;
        self.abort_signal.store(false, Ordering::Relaxed);

        match final_text {
            Ok(response) => {
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
                            tokens_in: None,
                            tokens_out: None,
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

                let (context_used_chars, context_max_chars) =
                    context_usage_chars(&self.history, self.compact_config.threshold_chars);
                self.events.emit(AgentEvent::TurnCompleted {
                    session_id: self.session.id.clone(),
                    turn_id: turn_id.clone(),
                    model: self.model.clone(),
                    turn_number,
                    message_id: message_id.clone(),
                    context_used_chars,
                    context_max_chars,
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

fn test_provider_response(history: &[serde_json::Value]) -> String {
    let latest_user = history.iter().rev().find_map(|item| {
        let item_type = item.get("type")?.as_str()?;
        if item_type == "message" && item.get("role")?.as_str()? == "user" {
            item.get("content")?.as_str().map(String::from)
        } else {
            None
        }
    });

    match latest_user {
        Some(user) => format!("Test assistant reply: {user}"),
        None => "Test assistant reply".to_string(),
    }
}

fn test_provider_result(history: &[serde_json::Value]) -> TurnResult {
    let latest_type = history
        .last()
        .and_then(|item| item.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    if latest_type == "function_call_output" {
        return TurnResult::Text(test_provider_response(history));
    }

    let latest_user = history.iter().rev().find_map(|item| {
        let item_type = item.get("type")?.as_str()?;
        if item_type == "message" && item.get("role")?.as_str()? == "user" {
            item.get("content")?.as_str().map(String::from)
        } else {
            None
        }
    });

    if latest_user
        .as_deref()
        .unwrap_or_default()
        .to_lowercase()
        .contains("tool")
    {
        return TurnResult::ToolCalls(vec![ToolCall {
            call_id: format!("call-{}", Uuid::new_v4()),
            name: "unknown_tool".to_string(),
            arguments: "{}".to_string(),
        }]);
    }

    TurnResult::Text(test_provider_response(history))
}

async fn run_test_provider(
    history: &[serde_json::Value],
    output_hook: Option<&dyn Fn(&str)>,
    abort_signal: &AtomicBool,
) -> Result<TurnResult> {
    let response = test_provider_response(history);
    let latest_user = latest_test_user_prompt(history).unwrap_or_default();
    let slow_stream = latest_user.contains("hold-open") || latest_user.contains("abortable");

    if !slow_stream {
        if let Some(hook) = output_hook {
            hook(&response);
        }
        return Ok(TurnResult::Text(response));
    }

    let delay_ms = if latest_user.contains("hold-open") {
        150
    } else {
        25
    };

    for chunk in response_chunks(&response, 4) {
        if abort_signal.load(Ordering::Relaxed) {
            return Ok(TurnResult::Aborted);
        }
        if let Some(hook) = output_hook {
            hook(&chunk);
        }
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }

    if latest_user.contains("hold-open") {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    if abort_signal.load(Ordering::Relaxed) {
        return Ok(TurnResult::Aborted);
    }

    Ok(TurnResult::Text(response))
}

async fn wait_for_abort_signal(abort_signal: &AtomicBool) {
    while !abort_signal.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn latest_test_user_prompt(history: &[serde_json::Value]) -> Option<String> {
    history.iter().rev().find_map(|item| {
        let item_type = item.get("type")?.as_str()?;
        if item_type == "message" && item.get("role")?.as_str()? == "user" {
            item.get("content")?
                .as_str()
                .map(|content| content.to_lowercase())
        } else {
            None
        }
    })
}

fn response_chunks(response: &str, target_parts: usize) -> Vec<String> {
    let words: Vec<&str> = response.split_inclusive(' ').collect();
    if words.len() <= 1 || target_parts <= 1 {
        return vec![response.to_string()];
    }

    let chunk_size = words.len().div_ceil(target_parts);
    words
        .chunks(chunk_size.max(1))
        .map(|chunk| chunk.concat())
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    truncate_with_ascii_ellipsis(s, max)
}

#[cfg(test)]
mod tests {
    use super::truncate;

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
}

fn build_input_items(history: &[serde_json::Value]) -> Vec<serde_json::Value> {
    history.to_vec()
}

fn context_usage_chars(history: &[serde_json::Value], max_chars: usize) -> (usize, usize) {
    let used_chars = crate::compact::estimate_history_chars(history);
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

fn messages_from_history(history: &[serde_json::Value]) -> Vec<Message> {
    history
        .iter()
        .filter_map(|item| {
            let item_type = item.get("type")?.as_str()?;
            match item_type {
                "message" => Some(Message {
                    role: item.get("role")?.as_str()?.to_string(),
                    content: item.get("content")?.as_str()?.to_string(),
                }),
                "function_call_output" => Some(Message {
                    role: "assistant".to_string(),
                    content: format!(
                        "[Tool result: {}]",
                        item.get("output")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(empty)")
                    ),
                }),
                _ => None,
            }
        })
        .collect()
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

#[derive(Deserialize)]
struct ResponseEvent {
    r#type: String,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    item: Option<OutputItem>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct OutputItem {
    r#type: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
}

#[derive(Deserialize)]
struct SseChunk {
    choices: Option<Vec<SseChoice>>,
}

#[derive(Deserialize)]
struct SseChoice {
    delta: Option<SseDelta>,
}

#[derive(Deserialize)]
struct SseDelta {
    content: Option<String>,
}

struct OpenaiRequestContext<'a> {
    model: &'a str,
    session_id: &'a str,
    turn_id: &'a str,
    history: &'a [serde_json::Value],
    registry: &'a ToolRegistry,
    instructions: &'a str,
    ws_disabled: &'a AtomicBool,
    abort_signal: &'a AtomicBool,
    events: &'a EventEmitter,
    output_hook: Option<&'a dyn Fn(&str)>,
}

async fn send_openai(auth: &ResolvedAuth, ctx: OpenaiRequestContext<'_>) -> Result<TurnResult> {
    if ctx.ws_disabled.load(Ordering::Relaxed) {
        ctx.events.emit(AgentEvent::TransportSelected {
            session_id: Some(ctx.session_id.to_string()),
            turn_id: Some(ctx.turn_id.to_string()),
            provider: "openai".into(),
            transport: Transport::Sse,
        });
        return send_openai_sse(
            auth,
            ctx.model,
            ctx.history,
            ctx.registry,
            ctx.instructions,
            ctx.abort_signal,
            ctx.output_hook,
        )
        .await;
    }

    ctx.events.emit(AgentEvent::TransportSelected {
        session_id: Some(ctx.session_id.to_string()),
        turn_id: Some(ctx.turn_id.to_string()),
        provider: "openai".into(),
        transport: Transport::WebSocket,
    });

    match send_openai_ws(
        auth,
        ctx.model,
        ctx.history,
        ctx.registry,
        ctx.instructions,
        ctx.abort_signal,
        ctx.output_hook,
    )
    .await
    {
        Ok(response) => Ok(response),
        Err(err) => {
            let reason = format!("{err:#}");
            ctx.ws_disabled.store(true, Ordering::Relaxed);
            ctx.events.emit(AgentEvent::TransportFallback {
                session_id: Some(ctx.session_id.to_string()),
                turn_id: Some(ctx.turn_id.to_string()),
                from: Transport::WebSocket,
                to: Transport::Sse,
                reason,
            });
            send_openai_sse(
                auth,
                ctx.model,
                ctx.history,
                ctx.registry,
                ctx.instructions,
                ctx.abort_signal,
                ctx.output_hook,
            )
            .await
        }
    }
}

async fn send_openai_ws(
    auth: &ResolvedAuth,
    model: &str,
    history: &[serde_json::Value],
    registry: &ToolRegistry,
    instructions: &str,
    abort_signal: &AtomicBool,
    output_hook: Option<&dyn Fn(&str)>,
) -> Result<TurnResult> {
    if abort_signal.load(Ordering::Relaxed) {
        return Ok(TurnResult::Aborted);
    }

    let ws_url = if let Some(stripped) = auth.base_url.strip_prefix("http://") {
        format!("ws://{stripped}/responses")
    } else if let Some(stripped) = auth.base_url.strip_prefix("https://") {
        format!("wss://{stripped}/responses")
    } else {
        format!("{}/responses", auth.base_url)
    };

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = ws_url
        .into_client_request()
        .context("failed to build WebSocket request")?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", auth.api_key)
            .parse()
            .context("invalid auth header value")?,
    );
    if let Some(account_id) = &auth.account_id {
        request.headers_mut().insert(
            "ChatGPT-Account-ID",
            account_id.parse().context("invalid account id header")?,
        );
    }
    request.headers_mut().insert(
        "OpenAI-Beta",
        RESPONSES_WS_BETA_HEADER
            .parse()
            .context("invalid beta header value")?,
    );

    let (mut ws, _response) = tokio::select! {
        _ = wait_for_abort_signal(abort_signal) => return Ok(TurnResult::Aborted),
        result = tokio_tungstenite::connect_async(request) => {
            result.context("WebSocket connect failed")?
        }
    };

    let tools = registry.to_api_tools();
    let mut create_payload = serde_json::json!({
        "type": "response.create",
        "model": model,
        "instructions": instructions,
        "input": build_input_items(history),
        "stream": true,
        "store": false,
        "tool_choice": "auto"
    });
    if !tools.is_empty() {
        create_payload["tools"] = serde_json::json!(tools);
    }

    tokio::select! {
        _ = wait_for_abort_signal(abort_signal) => {
            let _ = ws.close(None).await;
            return Ok(TurnResult::Aborted);
        }
        result = ws.send(tungstenite::Message::Text(create_payload.to_string())) => {
            result.context("failed to send response.create")?;
        }
    }

    let mut full_response = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut current_call_args = String::new();
    let mut current_call_id = String::new();
    let mut current_call_name = String::new();

    loop {
        let Some(msg) = (tokio::select! {
            _ = wait_for_abort_signal(abort_signal) => {
                let _ = ws.close(None).await;
                return Ok(TurnResult::Aborted);
            }
            msg = ws.next() => msg
        }) else {
            break;
        };
        let msg = msg.context("WebSocket read error")?;
        match msg {
            tungstenite::Message::Text(text) => {
                if abort_signal.load(Ordering::Relaxed) {
                    let _ = ws.close(None).await;
                    return Ok(TurnResult::Aborted);
                }
                let event: ResponseEvent = match serde_json::from_str(&text) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                match event.r#type.as_str() {
                    "response.output_text.delta" => {
                        if let Some(delta) = event.delta {
                            if let Some(hook) = output_hook {
                                hook(&delta);
                            }
                            full_response.push_str(&delta);
                        }
                    }
                    "response.output_item.added" => {
                        if let Some(item) = &event.item
                            && item.r#type.as_deref() == Some("function_call")
                        {
                            current_call_id = item.call_id.clone().unwrap_or_default();
                            current_call_name = item.name.clone().unwrap_or_default();
                            current_call_args.clear();
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        if let Some(delta) = event.delta {
                            current_call_args.push_str(&delta);
                        }
                    }
                    "response.function_call_arguments.done" => {
                        tool_calls.push(ToolCall {
                            call_id: event.call_id.unwrap_or_else(|| current_call_id.clone()),
                            name: event.name.unwrap_or_else(|| current_call_name.clone()),
                            arguments: event.arguments.unwrap_or_else(|| current_call_args.clone()),
                        });
                        current_call_args.clear();
                        current_call_id.clear();
                        current_call_name.clear();
                    }
                    "response.completed" => break,
                    "error" => {
                        let err_value: serde_json::Value =
                            serde_json::from_str(&text).unwrap_or_default();
                        let err_msg = err_value
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown error");
                        anyhow::bail!("OpenAI WebSocket error: {err_msg}");
                    }
                    _ => {}
                }
            }
            tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }

    let _ = ws.close(None).await;

    if !tool_calls.is_empty() {
        Ok(TurnResult::ToolCalls(tool_calls))
    } else {
        Ok(TurnResult::Text(full_response))
    }
}

async fn send_openai_sse(
    auth: &ResolvedAuth,
    model: &str,
    history: &[serde_json::Value],
    registry: &ToolRegistry,
    instructions: &str,
    abort_signal: &AtomicBool,
    output_hook: Option<&dyn Fn(&str)>,
) -> Result<TurnResult> {
    if abort_signal.load(Ordering::Relaxed) {
        return Ok(TurnResult::Aborted);
    }

    let url = format!("{}/responses", auth.base_url);
    let tools = registry.to_api_tools();
    let mut body = serde_json::json!({
        "model": model,
        "instructions": instructions,
        "input": build_input_items(history),
        "stream": true,
        "store": false,
        "tool_choice": "auto"
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::json!(tools);
    }

    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth.api_key))
        .header("Content-Type", "application/json");
    if let Some(account_id) = &auth.account_id {
        req = req.header("ChatGPT-Account-ID", account_id);
    }
    let resp = tokio::select! {
        _ = wait_for_abort_signal(abort_signal) => return Ok(TurnResult::Aborted),
        result = req.json(&body).send() => result.context("SSE request failed")?,
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI SSE error: {status}\n{body}");
    }

    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut current_call_args = String::new();
    let mut current_call_id = String::new();
    let mut current_call_name = String::new();
    let mut buffer = String::new();

    loop {
        let Some(chunk) = (tokio::select! {
            _ = wait_for_abort_signal(abort_signal) => return Ok(TurnResult::Aborted),
            chunk = stream.next() => chunk,
        }) else {
            break;
        };
        let chunk = chunk.context("stream error")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        let mut done = false;
        while let Some(block_end) = buffer.find("\n\n") {
            if abort_signal.load(Ordering::Relaxed) {
                return Ok(TurnResult::Aborted);
            }
            let block = buffer[..block_end].to_string();
            buffer = buffer[block_end + 2..].to_string();

            if process_openai_sse_block_with_tools(
                &block,
                &mut full_response,
                &mut tool_calls,
                &mut current_call_args,
                &mut current_call_id,
                &mut current_call_name,
                output_hook,
            )? {
                done = true;
                break;
            }
        }

        if done {
            break;
        }
    }

    if !tool_calls.is_empty() {
        Ok(TurnResult::ToolCalls(tool_calls))
    } else {
        Ok(TurnResult::Text(full_response))
    }
}

fn process_openai_sse_block_with_tools(
    block: &str,
    full_response: &mut String,
    tool_calls: &mut Vec<ToolCall>,
    current_call_args: &mut String,
    current_call_id: &mut String,
    current_call_name: &mut String,
    output_hook: Option<&dyn Fn(&str)>,
) -> Result<bool> {
    let mut event_type = String::new();
    let mut data = String::new();
    for line in block.lines() {
        if let Some(t) = line.strip_prefix("event: ") {
            event_type = t.to_string();
        } else if let Some(d) = line.strip_prefix("data: ") {
            data = d.to_string();
        }
    }

    if data.is_empty() {
        return Ok(false);
    }

    match event_type.as_str() {
        "response.output_text.delta" => {
            if let Ok(event) = serde_json::from_str::<ResponseEvent>(&data)
                && let Some(delta) = event.delta
            {
                if let Some(hook) = output_hook {
                    hook(&delta);
                }
                full_response.push_str(&delta);
            }
        }
        "response.output_item.added" => {
            if let Ok(event) = serde_json::from_str::<ResponseEvent>(&data)
                && let Some(item) = &event.item
                && item.r#type.as_deref() == Some("function_call")
            {
                *current_call_id = item.call_id.clone().unwrap_or_default();
                *current_call_name = item.name.clone().unwrap_or_default();
                current_call_args.clear();
            }
        }
        "response.function_call_arguments.delta" => {
            if let Ok(event) = serde_json::from_str::<ResponseEvent>(&data)
                && let Some(delta) = event.delta
            {
                current_call_args.push_str(&delta);
            }
        }
        "response.function_call_arguments.done" => {
            if let Ok(event) = serde_json::from_str::<ResponseEvent>(&data) {
                tool_calls.push(ToolCall {
                    call_id: event.call_id.unwrap_or_else(|| current_call_id.clone()),
                    name: event.name.unwrap_or_else(|| current_call_name.clone()),
                    arguments: event.arguments.unwrap_or_else(|| current_call_args.clone()),
                });
                current_call_args.clear();
                current_call_id.clear();
                current_call_name.clear();
            }
        }
        "response.completed" => return Ok(true),
        "error" => {
            let err_value: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
            let err_msg = err_value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("OpenAI SSE error: {err_msg}");
        }
        _ => {}
    }

    Ok(false)
}

pub fn process_openai_sse_block(block: &str, full_response: &mut String) -> Result<bool> {
    let mut dummy_calls = Vec::new();
    let mut dummy_args = String::new();
    let mut dummy_id = String::new();
    let mut dummy_name = String::new();
    process_openai_sse_block_with_tools(
        block,
        full_response,
        &mut dummy_calls,
        &mut dummy_args,
        &mut dummy_id,
        &mut dummy_name,
        None,
    )
}

async fn send_zai_sse(
    auth: &ResolvedAuth,
    model: &str,
    messages: &[Message],
    abort_signal: &AtomicBool,
    output_hook: Option<&dyn Fn(&str)>,
) -> Result<TurnResult> {
    if abort_signal.load(Ordering::Relaxed) {
        return Ok(TurnResult::Aborted);
    }

    let url = format!("{}/chat/completions", auth.base_url);
    let body = ChatCompletionsRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        stream: true,
    };

    let resp = tokio::select! {
        _ = wait_for_abort_signal(abort_signal) => return Ok(TurnResult::Aborted),
        result = reqwest::Client::new()
            .post(&url)
            .header("Authorization", format!("Bearer {}", auth.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send() => result.context("ZAI request failed")?,
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("ZAI API error: {status}\n{body}");
    }

    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();
    let mut buffer = String::new();

    loop {
        let Some(chunk) = (tokio::select! {
            _ = wait_for_abort_signal(abort_signal) => return Ok(TurnResult::Aborted),
            chunk = stream.next() => chunk,
        }) else {
            break;
        };
        let chunk = chunk.context("stream error")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        let mut done = false;
        while let Some(line_end) = buffer.find('\n') {
            if abort_signal.load(Ordering::Relaxed) {
                return Ok(TurnResult::Aborted);
            }
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if process_zai_sse_line_with_hook(&line, &mut full_response, output_hook)? {
                done = true;
                break;
            }
        }

        if done {
            break;
        }
    }

    Ok(TurnResult::Text(full_response))
}

fn process_zai_sse_line_with_hook(
    line: &str,
    full_response: &mut String,
    output_hook: Option<&dyn Fn(&str)>,
) -> Result<bool> {
    if line.is_empty() || line.starts_with(':') {
        return Ok(false);
    }

    if let Some(data) = line.strip_prefix("data: ") {
        if data == "[DONE]" {
            return Ok(true);
        }

        if let Ok(chunk) = serde_json::from_str::<SseChunk>(data)
            && let Some(choices) = chunk.choices
        {
            for choice in choices {
                if let Some(delta) = choice.delta
                    && let Some(content) = delta.content
                {
                    if let Some(hook) = output_hook {
                        hook(&content);
                    }
                    full_response.push_str(&content);
                }
            }
        }
    }

    Ok(false)
}

pub fn process_zai_sse_line(line: &str, full_response: &mut String) -> Result<bool> {
    process_zai_sse_line_with_hook(line, full_response, None)
}
