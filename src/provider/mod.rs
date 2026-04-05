//! Provider abstraction layer.
//!
//! Each LLM provider (OpenAI, ZAI) implements the `Provider` trait,
//! which `SessionRuntime` calls to send prompts and receive responses.

pub mod openai;
pub mod zai;

use std::sync::atomic::AtomicBool;

use anyhow::Result;

use crate::auth::ResolvedAuth;
use crate::events::EventEmitter;
use crate::tools::ToolRegistry;

/// Result of a single model turn — either text, tool calls, or an abort.
#[derive(Debug)]
pub enum TurnResult {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    Aborted,
}

/// A tool call requested by the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

/// Aggregated token usage from a model response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
}

/// Merge incremental token usage into an aggregate.
pub fn merge_token_usage(aggregate: &mut Option<TokenUsage>, usage: Option<TokenUsage>) {
    let Some(usage) = usage else {
        return;
    };
    if let Some(current) = aggregate.as_mut() {
        current.input_tokens = current.input_tokens.saturating_add(usage.input_tokens);
        current.output_tokens = current.output_tokens.saturating_add(usage.output_tokens);
        current.total_tokens = current.total_tokens.saturating_add(usage.total_tokens);
    } else {
        *aggregate = Some(usage);
    }
}

/// Context passed to a provider's `send` method.
pub struct SendContext<'a> {
    pub model: &'a str,
    pub session_id: &'a str,
    pub turn_id: &'a str,
    pub history: &'a [serde_json::Value],
    pub registry: &'a ToolRegistry,
    pub instructions: &'a str,
    pub abort_signal: &'a AtomicBool,
    pub events: &'a EventEmitter,
    pub output_hook: Option<&'a (dyn Fn(&str) + Send + Sync)>,
    /// OpenAI reasoning effort level (e.g. "low", "medium", "high").
    pub reasoning_effort: Option<&'a str>,
}

/// Trait for LLM providers. Each provider knows how to send a prompt and
/// stream back a response.
///
/// Uses boxed futures to support dynamic dispatch (`dyn Provider`).
pub trait Provider: Send + Sync {
    /// Provider name (e.g. "openai", "zai").
    fn name(&self) -> &str;

    /// Send a prompt to the model and return the result.
    ///
    /// Implementations must respect `ctx.abort_signal` and return
    /// `TurnResult::Aborted` when it fires.
    fn send<'a>(
        &'a self,
        auth: &'a ResolvedAuth,
        ctx: SendContext<'a>,
        token_usage: &'a mut Option<TokenUsage>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<TurnResult>> + Send + 'a>>;
}

// ── Shared types used by multiple providers ────────────────────────────────

use serde::Deserialize;

/// Parse token usage from a response envelope (OpenAI format).
pub fn parse_token_usage(value: &serde_json::Value) -> Option<TokenUsage> {
    let envelope = serde_json::from_value::<UsageEnvelope>(value.clone()).ok()?;
    envelope
        .usage
        .and_then(UsagePayload::into_token_usage)
        .or_else(|| {
            envelope
                .response
                .and_then(|response| response.usage)
                .and_then(UsagePayload::into_token_usage)
        })
}

#[derive(Deserialize)]
struct UsageEnvelope {
    #[serde(default)]
    usage: Option<UsagePayload>,
    #[serde(default)]
    response: Option<UsageEnvelopeResponse>,
}

#[derive(Deserialize)]
struct UsageEnvelopeResponse {
    #[serde(default)]
    usage: Option<UsagePayload>,
}

#[derive(Deserialize)]
pub(crate) struct UsagePayload {
    #[serde(default)]
    pub input_tokens: Option<usize>,
    #[serde(default)]
    pub output_tokens: Option<usize>,
    #[serde(default)]
    pub prompt_tokens: Option<usize>,
    #[serde(default)]
    pub completion_tokens: Option<usize>,
    #[serde(default)]
    pub total_tokens: Option<usize>,
}

impl UsagePayload {
    pub fn into_token_usage(self) -> Option<TokenUsage> {
        let input = self.input_tokens.or(self.prompt_tokens);
        let output = self.output_tokens.or(self.completion_tokens);
        let total = self.total_tokens.or_else(|| match (input, output) {
            (Some(i), Some(o)) => Some(i + o),
            _ => None,
        });
        match (input, output, total) {
            (Some(input_tokens), Some(output_tokens), Some(total_tokens)) => Some(TokenUsage {
                input_tokens,
                output_tokens,
                total_tokens,
            }),
            _ => None,
        }
    }
}

/// Build input items from history (passes through for now).
pub fn build_input_items(history: &[serde_json::Value]) -> Vec<serde_json::Value> {
    history.to_vec()
}

/// Helper: poll until abort_signal is set.
pub async fn wait_for_abort_signal(abort_signal: &AtomicBool) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    while !abort_signal.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
