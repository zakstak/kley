//! OpenAI provider — Responses API via WebSocket with SSE fallback.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::tungstenite;

use crate::auth::ResolvedAuth;
use crate::events::{AgentEvent, Transport};
use crate::provider::{
    SendContext, TokenUsage, ToolCall, TurnResult, build_input_items, parse_token_usage,
    wait_for_abort_signal,
};

const RESPONSES_WS_BETA_HEADER: &str = "responses_websockets=2026-02-06";

pub struct OpenAiProvider {
    ws_disabled: AtomicBool,
}

impl Default for OpenAiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiProvider {
    pub fn new() -> Self {
        Self {
            ws_disabled: AtomicBool::new(false),
        }
    }
}

impl crate::provider::Provider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn send<'a>(
        &'a self,
        auth: &'a ResolvedAuth,
        ctx: SendContext<'a>,
        token_usage: &'a mut Option<TokenUsage>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<TurnResult>> + Send + 'a>> {
        Box::pin(async move {
            if self.ws_disabled.load(Ordering::Relaxed) {
                ctx.events.emit(AgentEvent::TransportSelected {
                    session_id: Some(ctx.session_id.to_string()),
                    turn_id: Some(ctx.turn_id.to_string()),
                    provider: "openai".into(),
                    transport: Transport::Sse,
                });
                return send_sse(auth, &ctx, token_usage).await;
            }

            ctx.events.emit(AgentEvent::TransportSelected {
                session_id: Some(ctx.session_id.to_string()),
                turn_id: Some(ctx.turn_id.to_string()),
                provider: "openai".into(),
                transport: Transport::WebSocket,
            });

            match send_ws(auth, &ctx, token_usage).await {
                Ok(response) => Ok(response),
                Err(err) => {
                    let reason = format!("{err:#}");
                    self.ws_disabled.store(true, Ordering::Relaxed);
                    ctx.events.emit(AgentEvent::TransportFallback {
                        session_id: Some(ctx.session_id.to_string()),
                        turn_id: Some(ctx.turn_id.to_string()),
                        from: Transport::WebSocket,
                        to: Transport::Sse,
                        reason,
                    });
                    send_sse(auth, &ctx, token_usage).await
                }
            }
        })
    }
}

// ── Protocol types ──────────────────────────────────────────────────────────

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

// ── WebSocket transport ─────────────────────────────────────────────────────

async fn send_ws(
    auth: &ResolvedAuth,
    ctx: &SendContext<'_>,
    token_usage: &mut Option<TokenUsage>,
) -> Result<TurnResult> {
    if ctx.abort_signal.load(Ordering::Relaxed) {
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
        _ = wait_for_abort_signal(ctx.abort_signal) => return Ok(TurnResult::Aborted),
        result = tokio_tungstenite::connect_async(request) => {
            result.context("WebSocket connect failed")?
        }
    };

    let tools = ctx.registry.to_api_tools();
    let mut create_payload = serde_json::json!({
        "type": "response.create",
        "model": ctx.model,
        "instructions": ctx.instructions,
        "input": build_input_items(ctx.history),
        "stream": true,
        "store": false,
        "tool_choice": "auto"
    });
    if !tools.is_empty() {
        create_payload["tools"] = serde_json::json!(tools);
    }
    if let Some(effort) = ctx.reasoning_effort {
        create_payload["reasoning"] = serde_json::json!({ "effort": effort });
    }

    tokio::select! {
        _ = wait_for_abort_signal(ctx.abort_signal) => {
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
            _ = wait_for_abort_signal(ctx.abort_signal) => {
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
                if ctx.abort_signal.load(Ordering::Relaxed) {
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
                            if let Some(hook) = ctx.output_hook {
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
                    "response.completed" => {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                            *token_usage = parse_token_usage(&value);
                        }
                        break;
                    }
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

// ── SSE transport ───────────────────────────────────────────────────────────

async fn send_sse(
    auth: &ResolvedAuth,
    ctx: &SendContext<'_>,
    token_usage: &mut Option<TokenUsage>,
) -> Result<TurnResult> {
    if ctx.abort_signal.load(Ordering::Relaxed) {
        return Ok(TurnResult::Aborted);
    }

    let url = format!("{}/responses", auth.base_url);
    let tools = ctx.registry.to_api_tools();
    let mut body = serde_json::json!({
        "model": ctx.model,
        "instructions": ctx.instructions,
        "input": build_input_items(ctx.history),
        "stream": true,
        "store": false,
        "tool_choice": "auto"
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::json!(tools);
    }
    if let Some(effort) = ctx.reasoning_effort {
        body["reasoning"] = serde_json::json!({ "effort": effort });
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
        _ = wait_for_abort_signal(ctx.abort_signal) => return Ok(TurnResult::Aborted),
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
            _ = wait_for_abort_signal(ctx.abort_signal) => return Ok(TurnResult::Aborted),
            chunk = stream.next() => chunk,
        }) else {
            break;
        };
        let chunk = chunk.context("stream error")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        let mut done = false;
        while let Some(block_end) = buffer.find("\n\n") {
            if ctx.abort_signal.load(Ordering::Relaxed) {
                return Ok(TurnResult::Aborted);
            }
            let block = buffer[..block_end].to_string();
            buffer = buffer[block_end + 2..].to_string();

            if process_sse_block_with_tools(
                &block,
                &mut full_response,
                &mut tool_calls,
                &mut current_call_args,
                &mut current_call_id,
                &mut current_call_name,
                ctx.output_hook,
                token_usage,
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

// ── SSE block parsing ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn process_sse_block_with_tools(
    block: &str,
    full_response: &mut String,
    tool_calls: &mut Vec<ToolCall>,
    current_call_args: &mut String,
    current_call_id: &mut String,
    current_call_name: &mut String,
    output_hook: Option<&(dyn Fn(&str) + Send + Sync)>,
    token_usage: &mut Option<TokenUsage>,
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
        "response.completed" => {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) {
                *token_usage = parse_token_usage(&value);
            }
            return Ok(true);
        }
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

/// Public SSE block parser used by tests and other modules.
pub fn process_openai_sse_block(block: &str, full_response: &mut String) -> Result<bool> {
    let mut dummy_calls = Vec::new();
    let mut dummy_args = String::new();
    let mut dummy_id = String::new();
    let mut dummy_name = String::new();
    let mut dummy_usage = None;
    process_sse_block_with_tools(
        block,
        full_response,
        &mut dummy_calls,
        &mut dummy_args,
        &mut dummy_id,
        &mut dummy_name,
        None,
        &mut dummy_usage,
    )
}
