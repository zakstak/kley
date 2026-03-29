use std::collections::HashMap;

use crate::compact::{CompactConfig, estimate_effective_history_chars};
use crate::store::Turn;
use crate::web::protocol::{ContextUsage, ContextUsageBreakdown, ContextUsageBucket};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BreakdownKind {
    SystemPrompt,
    UserInput,
    AssistantOutput,
    SkillCalls,
    McpCalls,
    ToolCalls,
    Other,
}

#[derive(Debug, Default, Clone)]
struct BreakdownChars {
    system_prompt: usize,
    user_input: usize,
    assistant_output: usize,
    skill_calls: usize,
    mcp_calls: usize,
    tool_calls: usize,
    other: usize,
}

impl BreakdownChars {
    fn as_weights_all(&self) -> [usize; 7] {
        [
            self.system_prompt,
            self.user_input,
            self.assistant_output,
            self.skill_calls,
            self.mcp_calls,
            self.tool_calls,
            self.other,
        ]
    }
}

pub(super) fn estimate_persisted_context_usage(
    turns: &[Turn],
    compact_threshold: usize,
    system_prompt_chars: usize,
) -> ContextUsage {
    let history_items = crate::runtime::history_items_from_turns(turns);
    let compact_config = CompactConfig {
        threshold_chars: compact_threshold.max(1),
        ..CompactConfig::default()
    };
    let used_chars = estimate_effective_history_chars(&history_items, &compact_config);
    let max_chars = compact_config.threshold_chars;
    let trailing_assistant = turns
        .last()
        .filter(|turn| turn.role == "assistant" && turn.kind == "message");
    let input_tokens = trailing_assistant
        .and_then(|turn| turn.tokens_in)
        .and_then(|value| usize::try_from(value).ok());
    let output_tokens = trailing_assistant
        .and_then(|turn| turn.tokens_out)
        .and_then(|value| usize::try_from(value).ok());
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(input), Some(output)) => Some(input.saturating_add(output)),
        _ => None,
    };
    context_usage_from_chars(
        used_chars,
        max_chars,
        input_tokens,
        output_tokens,
        total_tokens,
        estimate_context_breakdown(
            turns,
            used_chars,
            input_tokens,
            output_tokens,
            total_tokens,
            system_prompt_chars,
        ),
    )
}

pub(super) fn context_usage_from_chars(
    used_chars: usize,
    max_chars: usize,
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
    total_tokens: Option<usize>,
    breakdown: Option<ContextUsageBreakdown>,
) -> ContextUsage {
    let clamped_max = max_chars.max(1);
    let percent_used = ((used_chars.saturating_mul(100)) / clamped_max).min(100) as u8;

    ContextUsage {
        used_chars,
        max_chars: clamped_max,
        percent_used,
        input_tokens,
        output_tokens,
        total_tokens,
        breakdown,
    }
}

pub(super) fn context_usage_from_event(
    used_chars: usize,
    max_chars: usize,
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
    total_tokens: Option<usize>,
) -> ContextUsage {
    context_usage_from_chars(
        used_chars,
        max_chars,
        input_tokens,
        output_tokens,
        total_tokens,
        None,
    )
}

pub(super) fn estimate_context_breakdown(
    turns: &[Turn],
    used_chars: usize,
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
    total_tokens: Option<usize>,
    system_prompt_chars: usize,
) -> Option<ContextUsageBreakdown> {
    let raw = classify_turn_chars(turns, system_prompt_chars);
    let raw_weights = raw.as_weights_all();
    let char_alloc = allocate_by_weight(&raw_weights, used_chars);

    let token_alloc = match (input_tokens, output_tokens, total_tokens) {
        (Some(input), Some(output), _) => {
            let non_assistant_weights = [
                char_alloc[0],
                char_alloc[1],
                char_alloc[3],
                char_alloc[4],
                char_alloc[5],
                char_alloc[6],
            ];
            let non_assistant_alloc = allocate_by_weight(&non_assistant_weights, input);
            Some([
                Some(non_assistant_alloc[0]),
                Some(non_assistant_alloc[1]),
                Some(output),
                Some(non_assistant_alloc[2]),
                Some(non_assistant_alloc[3]),
                Some(non_assistant_alloc[4]),
                Some(non_assistant_alloc[5]),
            ])
        }
        (_, _, Some(total)) => {
            let alloc = allocate_by_weight(&char_alloc, total);
            Some([
                Some(alloc[0]),
                Some(alloc[1]),
                Some(alloc[2]),
                Some(alloc[3]),
                Some(alloc[4]),
                Some(alloc[5]),
                Some(alloc[6]),
            ])
        }
        _ => None,
    };

    Some(ContextUsageBreakdown {
        system_prompt: ContextUsageBucket {
            chars_estimate: char_alloc[0],
            tokens_estimate: token_alloc.as_ref().and_then(|alloc| alloc[0]),
        },
        user_input: ContextUsageBucket {
            chars_estimate: char_alloc[1],
            tokens_estimate: token_alloc.as_ref().and_then(|alloc| alloc[1]),
        },
        assistant_output: ContextUsageBucket {
            chars_estimate: char_alloc[2],
            tokens_estimate: token_alloc.as_ref().and_then(|alloc| alloc[2]),
        },
        skill_calls: ContextUsageBucket {
            chars_estimate: char_alloc[3],
            tokens_estimate: token_alloc.as_ref().and_then(|alloc| alloc[3]),
        },
        mcp_calls: ContextUsageBucket {
            chars_estimate: char_alloc[4],
            tokens_estimate: token_alloc.as_ref().and_then(|alloc| alloc[4]),
        },
        tool_calls: ContextUsageBucket {
            chars_estimate: char_alloc[5],
            tokens_estimate: token_alloc.as_ref().and_then(|alloc| alloc[5]),
        },
        other: ContextUsageBucket {
            chars_estimate: char_alloc[6],
            tokens_estimate: token_alloc.as_ref().and_then(|alloc| alloc[6]),
        },
    })
}

fn classify_turn_chars(turns: &[Turn], system_prompt_chars: usize) -> BreakdownChars {
    let mut out = BreakdownChars {
        system_prompt: system_prompt_chars,
        ..BreakdownChars::default()
    };
    let mut call_kind_by_id: HashMap<String, BreakdownKind> = HashMap::new();

    for turn in turns {
        match turn.kind.as_str() {
            "function_call" => {
                let parsed = serde_json::from_str::<serde_json::Value>(&turn.content).ok();
                let call_id = parsed
                    .as_ref()
                    .and_then(|value| value.get("call_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let name = parsed
                    .as_ref()
                    .and_then(|value| value.get("name"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let arguments = parsed
                    .as_ref()
                    .and_then(|value| value.get("arguments"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();

                let item = serde_json::json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments,
                });
                let kind = classify_tool_name(name);
                if !call_id.is_empty() {
                    call_kind_by_id.insert(call_id, kind);
                }
                add_chars(&mut out, kind, serialized_chars(&item));
            }
            "function_call_output" => {
                let parsed = serde_json::from_str::<serde_json::Value>(&turn.content)
                    .unwrap_or_else(|_| serde_json::json!({ "output": turn.content }));
                let call_id = parsed
                    .get("call_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let output = parsed
                    .get("output")
                    .and_then(|value| value.as_str())
                    .unwrap_or(&turn.content);
                let item = serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                });

                let kind = call_kind_by_id
                    .get(&call_id)
                    .copied()
                    .unwrap_or(BreakdownKind::ToolCalls);
                add_chars(&mut out, kind, serialized_chars(&item));
            }
            "message" => {
                let item = serde_json::json!({
                    "type": "message",
                    "role": turn.role,
                    "content": turn.content,
                });
                let kind = match turn.role.as_str() {
                    "system" => BreakdownKind::SystemPrompt,
                    "user" => BreakdownKind::UserInput,
                    "assistant" => BreakdownKind::AssistantOutput,
                    _ => BreakdownKind::Other,
                };
                add_chars(&mut out, kind, serialized_chars(&item));
            }
            _ => {
                let item = serde_json::json!({
                    "type": "message",
                    "role": turn.role,
                    "content": turn.content,
                });
                add_chars(&mut out, BreakdownKind::Other, serialized_chars(&item));
            }
        }
    }

    out
}

fn classify_tool_name(name: &str) -> BreakdownKind {
    let normalized = name.trim().to_ascii_lowercase();
    if normalized.contains("mcp") {
        return BreakdownKind::McpCalls;
    }
    if normalized.contains("skill") {
        return BreakdownKind::SkillCalls;
    }
    BreakdownKind::ToolCalls
}

fn add_chars(breakdown: &mut BreakdownChars, kind: BreakdownKind, chars: usize) {
    match kind {
        BreakdownKind::SystemPrompt => {
            breakdown.system_prompt = breakdown.system_prompt.saturating_add(chars)
        }
        BreakdownKind::UserInput => {
            breakdown.user_input = breakdown.user_input.saturating_add(chars)
        }
        BreakdownKind::AssistantOutput => {
            breakdown.assistant_output = breakdown.assistant_output.saturating_add(chars)
        }
        BreakdownKind::SkillCalls => {
            breakdown.skill_calls = breakdown.skill_calls.saturating_add(chars)
        }
        BreakdownKind::McpCalls => breakdown.mcp_calls = breakdown.mcp_calls.saturating_add(chars),
        BreakdownKind::ToolCalls => {
            breakdown.tool_calls = breakdown.tool_calls.saturating_add(chars)
        }
        BreakdownKind::Other => breakdown.other = breakdown.other.saturating_add(chars),
    }
}

fn serialized_chars(value: &serde_json::Value) -> usize {
    serde_json::to_string(value)
        .map(|text| text.len())
        .unwrap_or(0)
}

fn allocate_by_weight(weights: &[usize], total: usize) -> Vec<usize> {
    if weights.is_empty() {
        return Vec::new();
    }
    let weight_sum: usize = weights.iter().sum();
    if total == 0 {
        return vec![0; weights.len()];
    }
    if weight_sum == 0 {
        let mut alloc = vec![0; weights.len()];
        alloc[weights.len() - 1] = total;
        return alloc;
    }

    let mut alloc: Vec<usize> = weights
        .iter()
        .map(|weight| weight.saturating_mul(total) / weight_sum)
        .collect();
    let used: usize = alloc.iter().sum();
    let mut remainder = total.saturating_sub(used);

    if remainder == 0 {
        return alloc;
    }

    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by_key(|idx| std::cmp::Reverse(weights[*idx]));
    for idx in order.into_iter().cycle() {
        if remainder == 0 {
            break;
        }
        alloc[idx] = alloc[idx].saturating_add(1);
        remainder -= 1;
    }

    alloc
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn make_turn(kind: &str, role: &str, content: &str) -> Turn {
        Turn {
            id: 1,
            session_id: "sess-1".to_string(),
            kind: kind.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            model: None,
            tokens_in: None,
            tokens_out: None,
            turn_number: 1,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn persisted_context_usage_uses_compaction_aware_estimate() {
        let turns: Vec<Turn> = (0..30)
            .map(|index| Turn {
                turn_number: (index + 1) as i64,
                ..make_turn(
                    "message",
                    "user",
                    &format!("message-{index}-{}", "x".repeat(40_000)),
                )
            })
            .collect();

        let history_items = crate::runtime::history_items_from_turns(&turns);
        let raw_chars = crate::compact::estimate_history_chars(&history_items);
        let expected_chars = crate::compact::estimate_effective_history_chars(
            &history_items,
            &CompactConfig::default(),
        );
        let usage = estimate_persisted_context_usage(
            &turns,
            CompactConfig::default().threshold_chars,
            1_000,
        );

        assert_eq!(usage.used_chars, expected_chars);
        assert!(usage.used_chars < raw_chars);
        assert_eq!(usage.max_chars, CompactConfig::default().threshold_chars);
        assert!(usage.breakdown.is_some());
    }

    #[test]
    fn persisted_context_usage_drops_tokens_when_tail_is_not_assistant() {
        let mut assistant = make_turn("message", "assistant", "done");
        assistant.tokens_in = Some(120);
        assistant.tokens_out = Some(30);

        let turns = vec![assistant, make_turn("message", "user", "follow-up")];
        let usage =
            estimate_persisted_context_usage(&turns, CompactConfig::default().threshold_chars, 0);

        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.total_tokens, None);
    }

    #[test]
    fn persisted_context_usage_honors_custom_threshold() {
        let turns: Vec<Turn> = (0..25)
            .map(|index| Turn {
                turn_number: (index + 1) as i64,
                ..make_turn(
                    "message",
                    "user",
                    &format!("message-{index}-{}", "x".repeat(8_000)),
                )
            })
            .collect();

        let usage = estimate_persisted_context_usage(&turns, 120_000, 0);

        assert_eq!(usage.max_chars, 120_000);
        assert!(usage.percent_used <= 100);
    }

    #[test]
    fn classify_breakdown_detects_skill_and_mcp_calls() {
        let turns = vec![
            Turn {
                turn_number: 1,
                ..make_turn("message", "user", "hello")
            },
            Turn {
                turn_number: 2,
                content: serde_json::json!({
                    "call_id": "c1",
                    "name": "skill.run",
                    "arguments": "{}"
                })
                .to_string(),
                ..make_turn("function_call", "assistant", "")
            },
            Turn {
                turn_number: 3,
                content: serde_json::json!({
                    "call_id": "c1",
                    "output": "ok"
                })
                .to_string(),
                ..make_turn("function_call_output", "tool", "")
            },
            Turn {
                turn_number: 4,
                content: serde_json::json!({
                    "call_id": "c2",
                    "name": "skill_mcp",
                    "arguments": "{}"
                })
                .to_string(),
                ..make_turn("function_call", "assistant", "")
            },
        ];

        let breakdown = estimate_context_breakdown(&turns, 10_000, Some(900), Some(100), None, 800)
            .expect("breakdown should exist");

        assert!(breakdown.system_prompt.chars_estimate > 0);
        assert!(breakdown.user_input.chars_estimate > 0);
        assert!(breakdown.skill_calls.chars_estimate > 0);
        assert!(breakdown.mcp_calls.chars_estimate > 0);
        assert_eq!(breakdown.assistant_output.tokens_estimate, Some(100));
    }
}
