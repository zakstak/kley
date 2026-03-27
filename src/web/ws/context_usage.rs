use crate::compact::{CompactConfig, estimate_effective_history_chars};
use crate::store::Turn;
use crate::web::protocol::ContextUsage;

pub(super) fn estimate_persisted_context_usage(
    turns: &[Turn],
    compact_threshold: usize,
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
    )
}

pub(super) fn context_usage_from_chars(
    used_chars: usize,
    max_chars: usize,
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
    total_tokens: Option<usize>,
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
    )
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
        let usage =
            estimate_persisted_context_usage(&turns, CompactConfig::default().threshold_chars);

        assert_eq!(usage.used_chars, expected_chars);
        assert!(usage.used_chars < raw_chars);
        assert_eq!(usage.max_chars, CompactConfig::default().threshold_chars);
    }

    #[test]
    fn persisted_context_usage_drops_tokens_when_tail_is_not_assistant() {
        let mut assistant = make_turn("message", "assistant", "done");
        assistant.tokens_in = Some(120);
        assistant.tokens_out = Some(30);

        let turns = vec![assistant, make_turn("message", "user", "follow-up")];
        let usage =
            estimate_persisted_context_usage(&turns, CompactConfig::default().threshold_chars);

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

        let usage = estimate_persisted_context_usage(&turns, 120_000);

        assert_eq!(usage.max_chars, 120_000);
        assert!(usage.percent_used <= 100);
    }
}
