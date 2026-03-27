//! Test provider — fake model for integration tests.

use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::auth::ResolvedAuth;
use crate::provider::{SendContext, TokenUsage, ToolCall, TurnResult};

pub const CONTROL_BLOCK_START: &str = "<kley-test-provider>";
pub const CONTROL_BLOCK_END: &str = "</kley-test-provider>";

pub struct TestProvider;

impl Default for TestProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TestProvider {
    pub fn new() -> Self {
        Self
    }
}

impl crate::provider::Provider for TestProvider {
    fn name(&self) -> &str {
        "test"
    }

    fn send<'a>(
        &'a self,
        _auth: &'a ResolvedAuth,
        ctx: SendContext<'a>,
        _token_usage: &'a mut Option<TokenUsage>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<TurnResult>> + Send + 'a>> {
        Box::pin(async move {
            // Check if the test scenario should return tool calls first.
            match test_provider_result(ctx.history) {
                TurnResult::ToolCalls(calls) => Ok(TurnResult::ToolCalls(calls)),
                TurnResult::Text(text) if has_control_block(ctx.history) => {
                    Ok(TurnResult::Text(text))
                }
                _ => run_test_provider(ctx.history, ctx.output_hook, ctx.abort_signal).await,
            }
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

    if let Some(controlled) = latest_user.as_deref().and_then(extract_controlled_response) {
        return controlled;
    }

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

fn extract_controlled_response(prompt: &str) -> Option<TurnResult> {
    let start = prompt.find(CONTROL_BLOCK_START)? + CONTROL_BLOCK_START.len();
    let rest = &prompt[start..];
    let end = rest.find(CONTROL_BLOCK_END)?;
    let raw = rest[..end].trim();
    let control = serde_json::from_str::<ControlledResponse>(raw).ok()?;

    Some(match control {
        ControlledResponse::ToolCall { name, arguments } => TurnResult::ToolCalls(vec![ToolCall {
            call_id: format!("call-{}", Uuid::new_v4()),
            name,
            arguments: serde_json::to_string(&arguments).ok()?,
        }]),
        ControlledResponse::Text { content } => TurnResult::Text(content),
    })
}

fn has_control_block(history: &[serde_json::Value]) -> bool {
    history.iter().rev().any(|item| {
        item.get("type").and_then(|v| v.as_str()) == Some("message")
            && item.get("role").and_then(|v| v.as_str()) == Some("user")
            && item
                .get("content")
                .and_then(|v| v.as_str())
                .is_some_and(|content| {
                    content.contains(CONTROL_BLOCK_START) && content.contains(CONTROL_BLOCK_END)
                })
    })
}

async fn run_test_provider(
    history: &[serde_json::Value],
    output_hook: Option<&(dyn Fn(&str) + Send + Sync)>,
    abort_signal: &std::sync::atomic::AtomicBool,
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

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ControlledResponse {
    ToolCall { name: String, arguments: Value },
    Text { content: String },
}
