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
use crate::events::{AgentEvent, EventEmitter, Transport};
use crate::store::{NewSession, NewTurn, Session, SessionStatus, Store, Turn};

/// Default models per provider.
const DEFAULT_OPENAI_MODEL: &str = "gpt-4.1";
const DEFAULT_ZAI_MODEL: &str = "glm-4.7";

const RESPONSES_WS_BETA_HEADER: &str = "responses_websockets=2026-02-06";

// ── Shared message types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Message {
    pub(crate) role: String,
    pub(crate) content: String,
}

// ── OpenAI Responses API types (shared by WS and SSE) ───────────────────────

/// Server-sent event from the Responses API (both WS and SSE).
#[derive(Deserialize)]
struct ResponseEvent {
    r#type: String,
    #[serde(default)]
    delta: Option<String>,
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
) -> Result<()> {
    let cred_store = CredentialStore::open()?;
    let resolved = auth::resolve_auth(&cred_store, &events).await?;

    let model = model_override
        .map(String::from)
        .unwrap_or_else(|| default_model(&resolved.provider));

    // Session-scoped WS fallback flag (same pattern as codex-rs `disable_websockets`)
    let ws_disabled = AtomicBool::new(false);

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
    let mut history = history_from_turns(&existing_turns);

    if !existing_turns.is_empty() {
        eprintln!("Loaded {} previous turns", existing_turns.len());
    }

    eprintln!("kley v0 — {provider}/{model}", provider = resolved.provider);
    eprintln!("Type a message and press Enter. Ctrl+D to quit.\n");

    let stdin = io::stdin();
    let mut turn_number: usize = existing_turns.len();

    loop {
        eprint!("> ");
        io::stderr().flush().ok();

        let mut input = String::new();
        let bytes_read = stdin.lock().read_line(&mut input)?;
        if bytes_read == 0 {
            // Mark session as completed on clean exit
            Session::update_status(store, &session.id, SessionStatus::Completed)?;
            eprintln!("\nGoodbye.");
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

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

        history.push(Message {
            role: "user".into(),
            content: input.to_string(),
        });

        events.emit(AgentEvent::TurnStart {
            model: model.clone(),
            turn_number,
        });

        let result = match resolved.provider.as_str() {
            "openai" => send_openai(&resolved, &model, &history, &ws_disabled, &events).await,
            _ => {
                events.emit(AgentEvent::TransportSelected {
                    provider: resolved.provider.clone(),
                    transport: Transport::Sse,
                });
                send_zai_sse(&resolved, &model, &history).await
            }
        };

        match result {
            Ok(response) => {
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

                history.push(Message {
                    role: "assistant".into(),
                    content: response,
                });
            }
            Err(err) => {
                events.emit(AgentEvent::TurnError {
                    model: model.clone(),
                    turn_number,
                    error: format!("{err:#}"),
                });
                // Remove the user message that failed (from both memory and DB)
                history.pop();
                // Note: we leave the user turn in the DB — it's an append-only log.
                // A failed response is still useful history.
                eprintln!("Error: {err:#}");
            }
        }

        println!(); // blank line after response
    }

    Ok(())
}

fn default_model(provider: &str) -> String {
    match provider {
        "openai" => DEFAULT_OPENAI_MODEL.to_string(),
        "zai" => DEFAULT_ZAI_MODEL.to_string(),
        _ => "gpt-4.1".to_string(),
    }
}

fn build_input_items(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "type": "message",
                "role": m.role,
                "content": m.content,
            })
        })
        .collect()
}

pub(crate) fn history_from_turns(turns: &[Turn]) -> Vec<Message> {
    turns
        .iter()
        .map(|t| Message {
            role: t.role.clone(),
            content: t.content.clone(),
        })
        .collect()
}

// ── OpenAI: try WS, fallback to SSE ────────────────────────────────────────

async fn send_openai(
    auth: &ResolvedAuth,
    model: &str,
    messages: &[Message],
    ws_disabled: &AtomicBool,
    events: &EventEmitter,
) -> Result<String> {
    // If WS has been disabled for this session, go straight to SSE
    if ws_disabled.load(Ordering::Relaxed) {
        events.emit(AgentEvent::TransportSelected {
            provider: "openai".into(),
            transport: Transport::Sse,
        });
        return send_openai_sse(auth, model, messages).await;
    }

    // Try WebSocket first
    events.emit(AgentEvent::TransportSelected {
        provider: "openai".into(),
        transport: Transport::WebSocket,
    });

    match send_openai_ws(auth, model, messages).await {
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
            send_openai_sse(auth, model, messages).await
        }
    }
}

// ── OpenAI WebSocket transport ──────────────────────────────────────────────

async fn send_openai_ws(auth: &ResolvedAuth, model: &str, messages: &[Message]) -> Result<String> {
    let ws_url = format!(
        "wss://api.openai.com/v1/responses?model={}",
        urlencoding::encode(model)
    );

    let request = tungstenite::http::Request::builder()
        .uri(&ws_url)
        .header("Authorization", format!("Bearer {}", auth.api_key))
        .header("OpenAI-Beta", RESPONSES_WS_BETA_HEADER)
        .header(
            "openai-organization",
            auth.account_id.as_deref().unwrap_or(""),
        )
        .body(())
        .context("failed to build WebSocket request")?;

    let (mut ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .context("WebSocket connect failed")?;

    // Send response.create
    let create_msg = serde_json::json!({
        "type": "response.create",
        "response": {
            "model": model,
            "instructions": "You are a helpful coding assistant.",
            "input": build_input_items(messages),
            "stream": true
        }
    });

    ws.send(tungstenite::Message::Text(create_msg.to_string()))
        .await
        .context("failed to send response.create")?;

    // Read streaming events
    let mut full_response = String::new();

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
    Ok(full_response)
}

// ── OpenAI SSE transport (fallback) ─────────────────────────────────────────

async fn send_openai_sse(auth: &ResolvedAuth, model: &str, messages: &[Message]) -> Result<String> {
    let url = format!("{}/responses", auth.base_url);

    let body = serde_json::json!({
        "model": model,
        "instructions": "You are a helpful coding assistant.",
        "input": build_input_items(messages),
        "stream": true
    });

    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth.api_key))
        .header("Content-Type", "application/json");

    if let Some(ref account_id) = auth.account_id {
        req = req.header("openai-organization", account_id);
    }

    let resp = req.json(&body).send().await.context("SSE request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI SSE error: {status}\n{body}");
    }

    // Parse SSE event stream (Responses API uses `event: <type>\ndata: <json>` format)
    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("stream error")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Process complete double-newline-delimited SSE blocks
            let mut done = false;
            while let Some(block_end) = buffer.find("\n\n") {
                let block = buffer[..block_end].to_string();
                buffer = buffer[block_end + 2..].to_string();

                if process_openai_sse_block(&block, &mut full_response)? {
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

pub(crate) fn process_openai_sse_block(block: &str, full_response: &mut String) -> Result<bool> {
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

pub(crate) fn process_zai_sse_line(line: &str, full_response: &mut String) -> Result<bool> {
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
