//! Minimal chat agent — WebSocket Responses API for OpenAI (with SSE fallback), SSE for ZAI.
//!
//! Follows the same fallback strategy as codex-rs:
//! - Each turn opens a fresh WebSocket to `wss://api.openai.com/v1/responses`
//! - If WS connect fails, permanently fall back to HTTP SSE `POST /v1/responses` for this session
//! - No reconnect logic: if WS dies mid-turn, that's a turn error
//! - ZAI always uses SSE via `POST /chat/completions`

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite;

use crate::auth::{self, CredentialStore, ResolvedAuth};
use crate::compact::CompactConfig;
use crate::events::{AgentEvent, EventEmitter, Transport};
use crate::store::{NewSession, NewTurn, Session, SessionStatus, Store, Turn};
use crate::tools::ToolRegistry;

/// Default models per provider.
const DEFAULT_OPENAI_MODEL: &str = "gpt-5.3-codex-spark";
const DEFAULT_ZAI_MODEL: &str = "glm-4.7";

const RESPONSES_WS_BETA_HEADER: &str = "responses_websockets=2026-02-06";

// ── Shared message types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

// ── Tool call types ─────────────────────────────────────────────────────────

/// A function call requested by the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String, // raw JSON string
}

/// Result of a single model turn — either text or tool calls.
#[derive(Debug)]
pub enum TurnResult {
    /// The model produced a text response.
    Text(String),
    /// The model wants to call one or more tools.
    ToolCalls(Vec<ToolCall>),
}

/// Controls whether the agent loop runs interactively or autonomously.
#[derive(Debug, Clone)]
pub enum RunMode {
    /// Normal interactive mode — wait for user input between turns.
    Interactive,
    /// Autonomous mode — the agent works continuously, checking in via
    /// `report_status`. After each text response, a nudge is injected so
    /// the model keeps going. Stops after `max_turns` turns.
    Autonomous {
        initial_prompt: String,
        max_turns: usize,
    },
}

// ── OpenAI Responses API types (shared by WS and SSE) ───────────────────────

/// Server-sent event from the Responses API (both WS and SSE).
/// Extended to handle function_call events.
#[derive(Deserialize)]
struct ResponseEvent {
    r#type: String,
    #[serde(default)]
    delta: Option<String>,
    // For response.output_item.added — tells us the output item type
    #[serde(default)]
    item: Option<OutputItem>,
    // For function_call_arguments.done — the final call_id and name
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

// ── ZAI chat/completions types ──────────────────────────────────────────────

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

// ── Chat loop ───────────────────────────────────────────────────────────────

pub async fn chat_loop(
    model_override: Option<&str>,
    resume_session_id: Option<&str>,
    store: &Store,
    events: EventEmitter,
    run_mode: RunMode,
    compact_config: CompactConfig,
) -> Result<()> {
    let cred_store = CredentialStore::open()?;
    let resolved = auth::resolve_auth(&cred_store, &events).await?;

    let model = model_override
        .map(String::from)
        .unwrap_or_else(|| default_model(&resolved.provider));

    // Session-scoped WS fallback flag (same pattern as codex-rs `disable_websockets`)
    let ws_disabled = AtomicBool::new(false);

    // Discover skills and rules, build system prompt
    let project_dir = std::env::current_dir().unwrap_or_default();

    // Build the tool registry (needs project_dir for read_skill)
    let registry = crate::tools::default_registry(project_dir.clone());

    let rules = crate::skills::discover_rules(&project_dir);
    let skills = crate::skills::discover_skills(&project_dir);
    let instructions = crate::skills::build_system_prompt(&rules, &skills);

    // Resolve or create the session
    let session = match resume_session_id {
        Some(id) => {
            let s = Session::get(store, id)?;
            eprintln!("Resuming session {}", &s.id[..8]);
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
            eprintln!("Session {}", &s.id[..8]);
            s
        }
    };

    // Load existing history (for resumed sessions) or start fresh
    let existing_turns = Turn::list_for_session(store, &session.id)?;
    let mut history: Vec<serde_json::Value> = history_items_from_turns(&existing_turns);

    if !existing_turns.is_empty() {
        eprintln!("Loaded {} previous turns", existing_turns.len());
    }

    eprintln!("kley v0 — {provider}/{model}", provider = resolved.provider);
    eprintln!("Type a message and press Enter. Ctrl+D to quit.\n");

    let stdin = io::stdin();
    let mut turn_number: usize = existing_turns.len();

    // Autonomous mode state
    let (is_autonomous, mut remaining_turns) = match &run_mode {
        RunMode::Interactive => (false, usize::MAX),
        RunMode::Autonomous { max_turns, .. } => (true, *max_turns),
    };
    // In autonomous mode, the first prompt comes from the CLI
    let mut pending_input: Option<String> = match &run_mode {
        RunMode::Autonomous { initial_prompt, .. } => Some(initial_prompt.clone()),
        RunMode::Interactive => None,
    };
    let mut consecutive_errors: usize = 0;

    loop {
        // Get the next input — either from pending (autonomous) or stdin
        let input = if let Some(queued) = pending_input.take() {
            queued
        } else {
            eprint!("> ");
            io::stderr().flush().ok();

            let mut line = String::new();
            let bytes_read = stdin.lock().read_line(&mut line)?;
            if bytes_read == 0 {
                Session::update_status(store, &session.id, SessionStatus::Completed)?;
                eprintln!("\nGoodbye.");
                break;
            }
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            trimmed
        };

        turn_number += 1;

        // Persist the user turn
        Turn::append(
            store,
            NewTurn {
                session_id: session.id.clone(),
                kind: "message".into(),
                role: "user".into(),
                content: input.to_string(),
                model: None,
                tokens_in: None,
                tokens_out: None,
            },
        )?;

        history.push(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": &input,
        }));

        events.emit(AgentEvent::TurnStart {
            model: model.clone(),
            turn_number,
        });

        // ── Tool-call loop: keep going until the model returns text ──
        let final_text = loop {
            // Compact history if it has grown too large for the context window
            crate::compact::maybe_compact(
                &resolved, &model, &mut history, &compact_config, &events,
            )
            .await
            .ok(); // log but don't fail the turn on compaction errors

            let result = match resolved.provider.as_str() {
                "openai" => {
                    send_openai(
                        &resolved,
                        &model,
                        &history,
                        &registry,
                        &instructions,
                        &ws_disabled,
                        &events,
                    )
                    .await
                }
                _ => {
                    events.emit(AgentEvent::TransportSelected {
                        provider: resolved.provider.clone(),
                        transport: Transport::Sse,
                    });
                    // ZAI doesn't support tool calls yet — text only
                    let text =
                        send_zai_sse(&resolved, &model, &messages_from_history(&history)).await?;
                    Ok(TurnResult::Text(text))
                }
            };

            match result {
                Ok(TurnResult::Text(text)) => break Ok(text),
                Ok(TurnResult::ToolCalls(calls)) => {
                    // Execute each tool call and feed results back
                    for call in &calls {
                        eprintln!("  ⚙ {}({})", call.name, truncate(&call.arguments, 80));

                        // Persist the function_call turn
                        Turn::append(
                            store,
                            NewTurn {
                                session_id: session.id.clone(),
                                kind: "function_call".into(),
                                role: "assistant".into(),
                                content: serde_json::json!({
                                    "call_id": call.call_id,
                                    "name": call.name,
                                    "arguments": call.arguments,
                                })
                                .to_string(),
                                model: Some(model.clone()),
                                tokens_in: None,
                                tokens_out: None,
                            },
                        )?;

                        // Execute the tool
                        let output = match registry.get(&call.name) {
                            Some(tool) => {
                                let args: serde_json::Value =
                                    serde_json::from_str(&call.arguments).unwrap_or_default();
                                match tool.execute(args) {
                                    Ok(result) => result,
                                    Err(e) => format!("Tool error: {e:#}"),
                                }
                            }
                            None => format!("Error: unknown tool '{}'", call.name),
                        };

                        eprintln!(
                            "  ⚙ → {}",
                            truncate(output.lines().next().unwrap_or("(empty)"), 80)
                        );

                        // Persist the function_call_output turn
                        Turn::append(
                            store,
                            NewTurn {
                                session_id: session.id.clone(),
                                kind: "function_call_output".into(),
                                role: "tool".into(),
                                content: output.clone(),
                                model: None,
                                tokens_in: None,
                                tokens_out: None,
                            },
                        )?;

                        // Add to history for the next API call
                        history.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": call.call_id,
                            "name": call.name,
                            "arguments": call.arguments,
                        }));
                        history.push(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": call.call_id,
                            "output": output,
                        }));
                    }
                    // Loop back to call the API again with tool results
                    continue;
                }
                Err(err) => break Err(err),
            }
        };

        // Track whether this turn succeeded (for autonomous error handling)
        let turn_ok;

        match final_text {
            Ok(response) => {
                turn_ok = true;
                events.emit(AgentEvent::TurnComplete {
                    model: model.clone(),
                    turn_number,
                });

                // Persist the assistant turn
                Turn::append(
                    store,
                    NewTurn {
                        session_id: session.id.clone(),
                        kind: "message".into(),
                        role: "assistant".into(),
                        content: response.clone(),
                        model: Some(model.clone()),
                        tokens_in: None,
                        tokens_out: None,
                    },
                )?;

                // Auto-title after first assistant response
                if turn_number == 1 {
                    let title: String = response.chars().take(80).collect();
                    let title = title.lines().next().unwrap_or(&title);
                    Session::update_title(store, &session.id, title)?;
                }

                history.push(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": response,
                }));
            }
            Err(err) => {
                turn_ok = false;
                events.emit(AgentEvent::TurnError {
                    model: model.clone(),
                    turn_number,
                    error: format!("{err:#}"),
                });
                eprintln!("Error: {err:#}");
            }
        }

        println!(); // blank line after response

        // ── Autonomous re-injection ──────────────────────────────────────
        if is_autonomous {
            if turn_ok {
                consecutive_errors = 0;
            } else {
                consecutive_errors += 1;
                if consecutive_errors >= 3 {
                    eprintln!(
                        "\n🛑 Autonomous mode: {} consecutive errors. Stopping.",
                        consecutive_errors
                    );
                    Session::update_status(store, &session.id, SessionStatus::Completed)?;
                    break;
                }
            }

            remaining_turns = remaining_turns.saturating_sub(1);
            if remaining_turns == 0 {
                eprintln!("\n🛑 Autonomous mode: max turns reached. Stopping.");
                Session::update_status(store, &session.id, SessionStatus::Completed)?;
                break;
            }
            // Inject a synthetic nudge so the model continues
            let nudge = "Acknowledged. Continue to the next improvement.".to_string();
            pending_input = Some(nudge);
        }
    }

    Ok(())
}

fn default_model(provider: &str) -> String {
    match provider {
        "openai" => DEFAULT_OPENAI_MODEL.to_string(),
        "zai" => DEFAULT_ZAI_MODEL.to_string(),
        _ => "gpt-5.3-codex-spark".to_string(),
    }
}

/// Build Responses API input items from raw JSON history values.
fn build_input_items(history: &[serde_json::Value]) -> Vec<serde_json::Value> {
    history.to_vec()
}

/// Convert stored turns into Responses API input items.
pub fn history_items_from_turns(turns: &[Turn]) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    for t in turns {
        match t.kind.as_str() {
            "function_call" => {
                // Content is JSON with {call_id, name, arguments}
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
                // We need the call_id from the preceding function_call turn.
                // Look backward in the items we've built so far.
                let call_id = items
                    .iter()
                    .rev()
                    .find_map(|item| {
                        if item.get("type")?.as_str()? == "function_call" {
                            item.get("call_id")?.as_str().map(String::from)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                items.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": t.content,
                }));
            }
            _ => {
                // Regular message
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

/// Extract simple Messages from history items (for ZAI which uses chat/completions).
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

/// For backward compatibility with existing code that uses Vec<Message>.
pub fn history_from_turns(turns: &[Turn]) -> Vec<Message> {
    turns
        .iter()
        .map(|t| Message {
            role: t.role.clone(),
            content: t.content.clone(),
        })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

// ── OpenAI: try WS, fallback to SSE ────────────────────────────────────────

async fn send_openai(
    auth: &ResolvedAuth,
    model: &str,
    history: &[serde_json::Value],
    registry: &ToolRegistry,
    instructions: &str,
    ws_disabled: &AtomicBool,
    events: &EventEmitter,
) -> Result<TurnResult> {
    // If WS has been disabled for this session, go straight to SSE
    if ws_disabled.load(Ordering::Relaxed) {
        events.emit(AgentEvent::TransportSelected {
            provider: "openai".into(),
            transport: Transport::Sse,
        });
        return send_openai_sse(auth, model, history, registry, instructions).await;
    }

    // Try WebSocket first
    events.emit(AgentEvent::TransportSelected {
        provider: "openai".into(),
        transport: Transport::WebSocket,
    });

    match send_openai_ws(auth, model, history, registry, instructions).await {
        Ok(response) => Ok(response),
        Err(err) => {
            let reason = format!("{err:#}");
            // Permanently disable WS for this session (codex-rs pattern)
            ws_disabled.store(true, Ordering::Relaxed);
            events.emit(AgentEvent::TransportFallback {
                from: Transport::WebSocket,
                to: Transport::Sse,
                reason,
            });
            send_openai_sse(auth, model, history, registry, instructions).await
        }
    }
}

// ── OpenAI WebSocket transport ──────────────────────────────────────────────

async fn send_openai_ws(
    auth: &ResolvedAuth,
    model: &str,
    history: &[serde_json::Value],
    registry: &ToolRegistry,
    instructions: &str,
) -> Result<TurnResult> {
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

    let (mut ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .context("WebSocket connect failed")?;

    // Build tools array
    let tools = registry.to_api_tools();

    // Send response.create — fields must be at the top level alongside "type",
    // matching codex-rs's #[serde(tag = "type")] serialization.
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

    ws.send(tungstenite::Message::Text(create_payload.to_string()))
        .await
        .context("failed to send response.create")?;

    // Read streaming events
    let mut full_response = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut current_call_args = String::new();
    let mut current_call_id = String::new();
    let mut current_call_name = String::new();

    while let Some(msg) = ws.next().await {
        let msg = msg.context("WebSocket read error")?;

        match msg {
            tungstenite::Message::Text(text) => {
                let event: ResponseEvent = match serde_json::from_str(&text) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                match event.r#type.as_str() {
                    "response.output_text.delta" => {
                        if let Some(delta) = event.delta {
                            print!("{delta}");
                            io::stdout().flush().ok();
                            full_response.push_str(&delta);
                        }
                    }
                    "response.output_item.added" => {
                        // A new output item — could be text or function_call
                        if let Some(item) = &event.item {
                            if item.r#type.as_deref() == Some("function_call") {
                                current_call_id = item.call_id.clone().unwrap_or_default();
                                current_call_name = item.name.clone().unwrap_or_default();
                                current_call_args.clear();
                            }
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        if let Some(delta) = event.delta {
                            current_call_args.push_str(&delta);
                        }
                    }
                    "response.function_call_arguments.done" => {
                        let call = ToolCall {
                            call_id: event
                                .call_id
                                .clone()
                                .unwrap_or_else(|| current_call_id.clone()),
                            name: event
                                .name
                                .clone()
                                .unwrap_or_else(|| current_call_name.clone()),
                            arguments: event
                                .arguments
                                .clone()
                                .unwrap_or_else(|| current_call_args.clone()),
                        };
                        tool_calls.push(call);
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

// ── OpenAI SSE transport (fallback) ─────────────────────────────────────────

async fn send_openai_sse(
    auth: &ResolvedAuth,
    model: &str,
    history: &[serde_json::Value],
    registry: &ToolRegistry,
    instructions: &str,
) -> Result<TurnResult> {
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

    let resp = req.json(&body).send().await.context("SSE request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI SSE error: {status}\n{body}");
    }

    // Parse SSE event stream
    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut current_call_args = String::new();
    let mut current_call_id = String::new();
    let mut current_call_name = String::new();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("stream error")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Process complete double-newline-delimited SSE blocks
        let mut done = false;
        while let Some(block_end) = buffer.find("\n\n") {
            let block = buffer[..block_end].to_string();
            buffer = buffer[block_end + 2..].to_string();

            if process_openai_sse_block_with_tools(
                &block,
                &mut full_response,
                &mut tool_calls,
                &mut current_call_args,
                &mut current_call_id,
                &mut current_call_name,
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
            if let Ok(event) = serde_json::from_str::<ResponseEvent>(&data) {
                if let Some(delta) = event.delta {
                    print!("{delta}");
                    io::stdout().flush().ok();
                    full_response.push_str(&delta);
                }
            }
        }
        "response.output_item.added" => {
            if let Ok(event) = serde_json::from_str::<ResponseEvent>(&data) {
                if let Some(item) = &event.item {
                    if item.r#type.as_deref() == Some("function_call") {
                        *current_call_id = item.call_id.clone().unwrap_or_default();
                        *current_call_name = item.name.clone().unwrap_or_default();
                        current_call_args.clear();
                    }
                }
            }
        }
        "response.function_call_arguments.delta" => {
            if let Ok(event) = serde_json::from_str::<ResponseEvent>(&data) {
                if let Some(delta) = event.delta {
                    current_call_args.push_str(&delta);
                }
            }
        }
        "response.function_call_arguments.done" => {
            if let Ok(event) = serde_json::from_str::<ResponseEvent>(&data) {
                let call = ToolCall {
                    call_id: event.call_id.unwrap_or_else(|| current_call_id.clone()),
                    name: event.name.unwrap_or_else(|| current_call_name.clone()),
                    arguments: event.arguments.unwrap_or_else(|| current_call_args.clone()),
                };
                tool_calls.push(call);
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

/// Legacy SSE block processor (no tool support) — kept for backward compatibility
/// with integration tests.
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
    )
}

// ── ZAI SSE (chat/completions, different API format) ────────────────────────

async fn send_zai_sse(auth: &ResolvedAuth, model: &str, messages: &[Message]) -> Result<String> {
    let url = format!("{}/chat/completions", auth.base_url);

    let body = ChatCompletionsRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        stream: true,
    };

    let resp = reqwest::Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("ZAI request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("ZAI API error: {status}\n{body}");
    }

    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("stream error")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        let mut done = false;
        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if process_zai_sse_line(&line, &mut full_response)? {
                done = true;
                break;
            }
        }

        if done {
            break;
        }
    }

    Ok(full_response)
}

pub fn process_zai_sse_line(line: &str, full_response: &mut String) -> Result<bool> {
    if line.is_empty() || line.starts_with(':') {
        return Ok(false);
    }

    if let Some(data) = line.strip_prefix("data: ") {
        if data == "[DONE]" {
            return Ok(true);
        }

        if let Ok(chunk) = serde_json::from_str::<SseChunk>(data) {
            if let Some(choices) = chunk.choices {
                for choice in choices {
                    if let Some(delta) = choice.delta {
                        if let Some(content) = delta.content {
                            print!("{content}");
                            io::stdout().flush().ok();
                            full_response.push_str(&content);
                        }
                    }
                }
            }
        }
    }

    Ok(false)
}
