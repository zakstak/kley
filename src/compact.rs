//! Context-window compaction for long-running autonomous sessions.
//!
//! When the in-memory history grows beyond a configurable character budget, the
//! oldest items are summarized via a model call and replaced with a single
//! recap message. This keeps the context window within model limits while
//! preserving continuity.

use anyhow::{Context, Result};

use crate::auth::ResolvedAuth;
use crate::events::{AgentEvent, EventEmitter};

/// Compaction configuration.
#[derive(Debug, Clone)]
pub struct CompactConfig {
    /// Estimated character budget (~4 chars per token). When the serialized
    /// history exceeds this, compaction is triggered.
    pub threshold_chars: usize,
    /// Number of most-recent history items to keep verbatim (these represent
    /// the current cycle's context and should not be summarized).
    pub keep_recent: usize,
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            // ~200k tokens worth of characters
            threshold_chars: 800_000,
            keep_recent: 20,
        }
    }
}

/// Estimate the total character count of the serialized history.
pub fn estimate_history_chars(history: &[serde_json::Value]) -> usize {
    history
        .iter()
        .map(|v| serde_json::to_string(v).map(|s| s.len()).unwrap_or(0))
        .sum()
}

pub fn estimate_effective_history_chars(
    history: &[serde_json::Value],
    config: &CompactConfig,
) -> usize {
    let raw_chars = estimate_history_chars(history);
    if raw_chars <= config.threshold_chars || history.len() <= config.keep_recent {
        return raw_chars;
    }

    let split_at = history.len() - config.keep_recent;
    let old_items = &history[..split_at];
    let recent_items = &history[split_at..];
    let old_chars = estimate_history_chars(old_items);
    let recent_chars = estimate_history_chars(recent_items);
    let summary_placeholder = serde_json::json!({
        "type": "message",
        "role": "user",
        "content": format!(
            "{SUMMARY_PREFIX}\n\n[Recovered compacted history estimate for {split_at} earlier items ({old_chars} serialized chars).]"
        ),
    });

    recent_chars + estimate_history_chars(&[summary_placeholder])
}

/// Check whether the history exceeds the compaction threshold.
pub fn needs_compaction(history: &[serde_json::Value], threshold: usize) -> bool {
    estimate_history_chars(history) > threshold
}

/// Compact the history if it exceeds the configured threshold.
///
/// When compaction is triggered:
/// 1. The oldest items (everything except the most recent `keep_recent` items)
///    are serialized and sent to the model for summarization.
/// 2. The model returns a concise recap.
/// 3. The old items are replaced with a single synthetic system message
///    containing the recap.
///
/// If the compaction API call fails, falls back to hard truncation — the old
/// items are replaced with a brief "history truncated" notice rather than
/// crashing the autonomous loop.
pub async fn maybe_compact(
    auth: &ResolvedAuth,
    model: &str,
    history: &mut Vec<serde_json::Value>,
    config: &CompactConfig,
    events: &EventEmitter,
) -> Result<()> {
    if !needs_compaction(history, config.threshold_chars) {
        return Ok(());
    }

    let total_items = history.len();

    // Don't compact if there aren't enough items to split
    if total_items <= config.keep_recent {
        return Ok(());
    }

    let split_at = total_items - config.keep_recent;
    let old_items: Vec<serde_json::Value> = history.drain(..split_at).collect();
    let old_chars: usize = old_items
        .iter()
        .map(|v| serde_json::to_string(v).map(|s| s.len()).unwrap_or(0))
        .sum();

    eprintln!(
        "  📦 compacting history: {total_items} items → {} kept, {split_at} summarized ({old_chars} chars)",
        config.keep_recent
    );

    // Try to summarize via model call
    let summary = match summarize_history(auth, model, &old_items).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  ⚠ compaction summary failed, falling back to truncation: {e:#}");
            format!(
                "[History truncated: {split_at} earlier items removed to stay within context limits. \
                 The conversation included tool calls and responses that are no longer in context.]"
            )
        }
    };

    let new_chars = summary.len();

    // Prepend the summary as a synthetic message using codex-rs's handoff
    // framing: present it as another LLM's work for this LLM to continue.
    history.insert(
        0,
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": format!(
                "{SUMMARY_PREFIX}\n\n{summary}"
            ),
        }),
    );

    events.emit(AgentEvent::HistoryCompacted {
        session_id: None,
        turn_id: None,
        old_items: split_at,
        new_chars,
    });

    Ok(())
}

// ── Prompt templates (inspired by codex-rs prompt.md and omo structured handoff) ─

/// Prefix injected before the compaction summary in the replacement history.
/// Frames the summary as another LLM's work (codex-rs pattern) so the model
/// builds on it rather than treating it as user instructions.
const SUMMARY_PREFIX: &str = "\
Another language model started working on this task and produced the summary \
below. You also have access to the current state of the tools and repository. \
Use this summary to build on the work already done — avoid duplicating effort.";

/// The compaction instruction sent to the model. Combines:
/// - codex-rs: "handoff summary for another LLM" framing
/// - omo: structured sections (work completed, remaining tasks, active context)
/// - omo #1485 lesson: explicit guard against hallucinating constraints
const COMPACTION_INSTRUCTION: &str = "\
You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary \
for another LLM that will resume this task.

Organize your summary with these sections:

## Progress
- Key decisions made and their rationale
- Branches created, commits pushed, PRs opened (include names/URLs)
- Changes made to source files
- Validation results (cargo fmt, clippy, test, build)

## Remaining Work
- Clear next steps, in priority order
- Any blocked or failed items that need attention

## Active Context
- Relevant file paths, function names, or line numbers
- Important observations about the codebase
- Errors encountered and how they were (or were not) resolved

Rules:
- Be concise and structured. Use bullet points.
- Focus on FACTS from the conversation history only.
- Do NOT include tool call arguments or file contents verbatim.
- Do NOT invent constraints, rules, or preferences that were not explicitly \
  stated by the user. Only report what actually happened.
- Do NOT include system-prompt instructions or workflow rules as constraints.";

/// Call the model to summarize old history items into a concise recap.
async fn summarize_history(
    auth: &ResolvedAuth,
    model: &str,
    old_items: &[serde_json::Value],
) -> Result<String> {
    let serialized = serde_json::to_string(old_items).unwrap_or_default();

    // Truncate if the history itself is enormous (the summary call has its
    // own context limit). Keep the last portion which is most relevant.
    let max_summary_input = 400_000; // ~100k tokens for the summary call
    let input = if serialized.len() > max_summary_input {
        // Take the tail (most recent old items)
        let start = serialized.len() - max_summary_input;
        format!("[...truncated...]{}", &serialized[start..])
    } else {
        serialized
    };

    let body = serde_json::json!({
        "model": model,
        "instructions": COMPACTION_INSTRUCTION,
        "input": [{
            "type": "message",
            "role": "user",
            "content": input,
        }],
        "stream": false,
        "store": false,
    });

    let url = format!("{}/responses", auth.base_url);
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth.api_key))
        .header("Content-Type", "application/json");
    if let Some(account_id) = &auth.account_id {
        req = req.header("ChatGPT-Account-ID", account_id);
    }

    let resp = req
        .json(&body)
        .send()
        .await
        .context("compaction summary request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("compaction summary API error: {status}\n{body}");
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse summary response")?;

    // The Responses API returns output as an array of output items
    let text = json
        .get("output")
        .and_then(|o| o.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|item| {
                if item.get("type")?.as_str()? == "message" {
                    item.get("content")?
                        .as_array()?
                        .iter()
                        .find_map(|c| c.get("text")?.as_str().map(String::from))
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| "[Summary unavailable]".to_string());

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_history_item(content: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": content,
        })
    }

    #[test]
    fn test_estimate_history_chars() {
        let history = vec![make_history_item("hello"), make_history_item("world")];
        let chars = estimate_history_chars(&history);
        // Should be > 0 and roughly the serialized size
        assert!(chars > 0);
        assert!(chars > 10); // at minimum the JSON overhead
    }

    #[test]
    fn test_needs_compaction_below_threshold() {
        let history = vec![make_history_item("small")];
        assert!(!needs_compaction(&history, 1_000_000));
    }

    #[test]
    fn test_needs_compaction_above_threshold() {
        let big_content = "x".repeat(1000);
        let history = vec![make_history_item(&big_content)];
        // threshold smaller than the item
        assert!(needs_compaction(&history, 100));
    }

    #[test]
    fn test_empty_history_no_compaction() {
        let history: Vec<serde_json::Value> = vec![];
        assert!(!needs_compaction(&history, 0));
    }

    #[test]
    fn test_estimate_effective_history_chars_reduces_large_history() {
        let history: Vec<serde_json::Value> = (0..30)
            .map(|i| make_history_item(&format!("message-{i}-{}", "x".repeat(50))))
            .collect();
        let config = CompactConfig {
            threshold_chars: 100,
            keep_recent: 10,
        };

        let raw_chars = estimate_history_chars(&history);
        let effective_chars = estimate_effective_history_chars(&history, &config);

        assert!(effective_chars < raw_chars);
        assert!(effective_chars > estimate_history_chars(&history[20..]));
    }

    #[tokio::test]
    async fn test_compact_keeps_recent_items() {
        // Build a history with 30 items, each ~100 chars serialized
        let mut history: Vec<serde_json::Value> = (0..30)
            .map(|i| make_history_item(&format!("message-{i}-{}", "x".repeat(50))))
            .collect();

        let config = CompactConfig {
            threshold_chars: 100, // very low to trigger compaction
            keep_recent: 10,
        };

        let (emitter, _receiver) = crate::events::event_channel();

        // We can't actually call the API in tests, so the summarize call
        // will fail and we'll get the truncation fallback — which is fine,
        // it tests the compaction logic itself.
        let auth = ResolvedAuth {
            provider: "openai".into(),
            api_key: "test-key".into(),
            base_url: "http://localhost:1".into(), // will fail to connect
            account_id: None,
        };

        let _ = maybe_compact(&auth, "test-model", &mut history, &config, &emitter).await;

        // Should have keep_recent items + 1 summary message = 11
        assert_eq!(history.len(), 11);

        // First item should be the compaction recap
        let first = &history[0];
        let content = first.get("content").and_then(|c| c.as_str()).unwrap_or("");
        assert!(
            content.contains("Context recap") || content.contains("truncated"),
            "first item should be the recap, got: {content}"
        );

        // Last item should be the most recent original message
        let last = &history[10];
        let last_content = last.get("content").and_then(|c| c.as_str()).unwrap_or("");
        assert!(
            last_content.contains("message-29"),
            "last item should be the most recent, got: {last_content}"
        );
    }

    #[tokio::test]
    async fn test_no_compact_below_threshold() {
        let mut history: Vec<serde_json::Value> = vec![make_history_item("small")];

        let config = CompactConfig {
            threshold_chars: 1_000_000,
            keep_recent: 10,
        };

        let (emitter, _receiver) = crate::events::event_channel();
        let auth = ResolvedAuth {
            provider: "openai".into(),
            api_key: "test".into(),
            base_url: "http://localhost:1".into(),
            account_id: None,
        };

        let _ = maybe_compact(&auth, "test-model", &mut history, &config, &emitter).await;

        // Should remain unchanged
        assert_eq!(history.len(), 1);
    }
}
