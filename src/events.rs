use std::fmt;
use std::sync::mpsc;

use crate::text::truncate_with_ascii_ellipsis;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    TransportSelected {
        session_id: Option<String>,
        turn_id: Option<String>,
        provider: String,
        transport: Transport,
    },
    TransportFallback {
        session_id: Option<String>,
        turn_id: Option<String>,
        from: Transport,
        to: Transport,
        reason: String,
    },
    TokenRefreshed {
        session_id: Option<String>,
        provider: String,
    },
    TurnStarted {
        session_id: String,
        turn_id: String,
        model: String,
        turn_number: usize,
        context_used_chars: usize,
        context_max_chars: usize,
    },
    MessageStarted {
        session_id: String,
        turn_id: String,
        message_id: String,
    },
    MessageDelta {
        session_id: String,
        turn_id: String,
        message_id: String,
        delta: String,
    },
    MessageCompleted {
        session_id: String,
        turn_id: String,
        message_id: String,
        content: String,
    },
    ToolCallStarted {
        session_id: String,
        turn_id: String,
        message_id: String,
        tool_call_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolCallCompleted {
        session_id: String,
        turn_id: String,
        message_id: String,
        tool_call_id: String,
        tool_name: String,
        output_preview: String,
        success: bool,
    },
    TurnCompleted {
        session_id: String,
        turn_id: String,
        model: String,
        turn_number: usize,
        message_id: String,
        context_used_chars: usize,
        context_max_chars: usize,
        input_tokens: Option<usize>,
        output_tokens: Option<usize>,
        total_tokens: Option<usize>,
    },
    TurnFailed {
        session_id: String,
        turn_id: String,
        model: String,
        turn_number: usize,
        error: String,
    },
    StatusReport {
        session_id: Option<String>,
        turn_id: Option<String>,
        summary: String,
        turn_number: usize,
    },
    HistoryCompacted {
        session_id: Option<String>,
        turn_id: Option<String>,
        old_items: usize,
        new_chars: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    WebSocket,
    Sse,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Transport::WebSocket => write!(f, "WebSocket"),
            Transport::Sse => write!(f, "SSE"),
        }
    }
}

impl fmt::Display for AgentEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentEvent::TransportSelected {
                provider,
                transport,
                turn_id,
                ..
            } => {
                if let Some(turn_id) = turn_id {
                    write!(f, "[{provider}] using {transport} transport ({turn_id})")
                } else {
                    write!(f, "[{provider}] using {transport} transport")
                }
            }
            AgentEvent::TransportFallback {
                from,
                to,
                reason,
                turn_id,
                ..
            } => {
                if let Some(turn_id) = turn_id {
                    write!(
                        f,
                        "transport fallback: {from} -> {to} ({reason}) [{turn_id}]"
                    )
                } else {
                    write!(f, "transport fallback: {from} -> {to} ({reason})")
                }
            }
            AgentEvent::TokenRefreshed { provider, .. } => {
                write!(f, "[{provider}] token refreshed")
            }
            AgentEvent::TurnStarted {
                model,
                turn_number,
                turn_id,
                context_used_chars,
                context_max_chars,
                ..
            } => {
                let pct = ((*context_used_chars).saturating_mul(100) / (*context_max_chars).max(1))
                    .min(100);
                write!(
                    f,
                    "turn {turn_number} -> {model} ({turn_id}) [ctx {pct}% | tok n/a]"
                )
            }
            AgentEvent::MessageStarted { .. } => Ok(()),
            AgentEvent::MessageDelta { delta, .. } => write!(f, "{delta}"),
            AgentEvent::MessageCompleted { .. } => Ok(()),
            AgentEvent::ToolCallStarted {
                tool_name,
                arguments,
                ..
            } => write!(f, "  [tool] {}({})", tool_name, truncate(arguments, 80)),
            AgentEvent::ToolCallCompleted {
                output_preview,
                success,
                ..
            } => {
                if *success {
                    write!(f, "  [tool] -> {output_preview}")
                } else {
                    write!(f, "  [tool] ! {output_preview}")
                }
            }
            AgentEvent::TurnCompleted {
                model,
                turn_number,
                context_used_chars,
                context_max_chars,
                input_tokens,
                output_tokens,
                total_tokens,
                ..
            } => {
                let pct = ((*context_used_chars).saturating_mul(100) / (*context_max_chars).max(1))
                    .min(100);
                if let (Some(total), Some(input), Some(output)) =
                    (total_tokens, input_tokens, output_tokens)
                {
                    write!(
                        f,
                        "turn {turn_number} ok ({model}) [ctx {pct}% | tok {total} ({input} in/{output} out)]"
                    )
                } else if let Some(tokens) = total_tokens {
                    write!(
                        f,
                        "turn {turn_number} ok ({model}) [ctx {pct}% | tok {tokens}]"
                    )
                } else {
                    write!(f, "turn {turn_number} ok ({model}) [ctx {pct}% | tok n/a]")
                }
            }
            AgentEvent::TurnFailed {
                turn_number, error, ..
            } => {
                write!(f, "turn {turn_number} failed {error}")
            }
            AgentEvent::StatusReport {
                summary,
                turn_number,
                ..
            } => {
                write!(f, "turn {turn_number} status {summary}")
            }
            AgentEvent::HistoryCompacted {
                old_items,
                new_chars,
                ..
            } => {
                write!(
                    f,
                    "compacted history: {old_items} items -> {new_chars} char summary"
                )
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    truncate_with_ascii_ellipsis(s, max)
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_respects_utf8_char_boundaries() {
        let s = "A shell tool — execute commands";
        assert_eq!(truncate(s, 15), "A shell tool — ...");
    }

    #[test]
    fn truncate_handles_zero_max() {
        assert_eq!(truncate("hello", 0), "...");
    }
}

#[derive(Clone)]
pub struct EventEmitter {
    tx: mpsc::Sender<AgentEvent>,
}

pub struct EventReceiver {
    rx: mpsc::Receiver<AgentEvent>,
}

pub fn event_channel() -> (EventEmitter, EventReceiver) {
    let (tx, rx) = mpsc::channel();
    (EventEmitter { tx }, EventReceiver { rx })
}

impl EventEmitter {
    pub fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }
}

impl EventReceiver {
    pub fn recv_blocking(&self) -> std::result::Result<AgentEvent, mpsc::RecvError> {
        self.rx.recv()
    }

    #[allow(dead_code)]
    pub fn try_recv(&self) -> Option<AgentEvent> {
        self.rx.try_recv().ok()
    }

    #[allow(dead_code)]
    pub fn drain(&self) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
        }
        events
    }
}
