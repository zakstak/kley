//! ZAI provider — Chat Completions API via SSE.

use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::auth::ResolvedAuth;
use crate::http_client;
use crate::provider::{SendContext, TokenUsage, TurnResult, UsagePayload, wait_for_abort_signal};

/// A message in the Chat Completions format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

pub struct ZaiProvider;

impl Default for ZaiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ZaiProvider {
    pub fn new() -> Self {
        Self
    }
}

impl crate::provider::Provider for ZaiProvider {
    fn name(&self) -> &str {
        "zai"
    }

    fn send<'a>(
        &'a self,
        auth: &'a ResolvedAuth,
        ctx: SendContext<'a>,
        token_usage: &'a mut Option<TokenUsage>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<TurnResult>> + Send + 'a>> {
        Box::pin(async move {
            let messages = messages_from_history(ctx.history);
            send_sse(
                auth,
                ctx.model,
                &messages,
                ctx.abort_signal,
                ctx.output_hook,
                token_usage,
            )
            .await
        })
    }
}

// ── History → Messages conversion ──────────────────────────────────────────

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

// ── SSE transport ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
}

async fn send_sse(
    auth: &ResolvedAuth,
    model: &str,
    messages: &[Message],
    abort_signal: &std::sync::atomic::AtomicBool,
    output_hook: Option<&(dyn Fn(&str) + Send + Sync)>,
    token_usage: &mut Option<TokenUsage>,
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
        result = http_client::client()
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

            if process_sse_line_with_hook(&line, &mut full_response, output_hook, token_usage)? {
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

// ── SSE line parsing ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SseChunk {
    choices: Option<Vec<SseChoice>>,
    #[serde(default)]
    usage: Option<UsagePayload>,
}

#[derive(Deserialize)]
struct SseChoice {
    delta: Option<SseDelta>,
}

#[derive(Deserialize)]
struct SseDelta {
    content: Option<String>,
}

fn process_sse_line_with_hook(
    line: &str,
    full_response: &mut String,
    output_hook: Option<&(dyn Fn(&str) + Send + Sync)>,
    token_usage: &mut Option<TokenUsage>,
) -> Result<bool> {
    if line.is_empty() || line.starts_with(':') {
        return Ok(false);
    }

    if let Some(data) = line.strip_prefix("data: ") {
        if data == "[DONE]" {
            return Ok(true);
        }

        if let Ok(chunk) = serde_json::from_str::<SseChunk>(data) {
            if let Some(usage) = chunk.usage {
                *token_usage = usage.into_token_usage();
            }
            if let Some(choices) = chunk.choices {
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
    }

    Ok(false)
}

/// Public SSE parser used by tests and other modules.
pub fn process_zai_sse_line(line: &str, full_response: &mut String) -> Result<bool> {
    let mut dummy_usage = None;
    process_sse_line_with_hook(line, full_response, None, &mut dummy_usage)
}
