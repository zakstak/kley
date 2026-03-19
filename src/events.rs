//! Structured runtime events for observability.
//!
//! Important state transitions (transport fallback, auth refresh, errors) are
//! emitted as typed `AgentEvent` values, not just log lines. This gives future
//! UI layers a clean seam to hook into.

use std::fmt;
use std::sync::mpsc;

/// A significant runtime event that consumers should be aware of.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Transport negotiation outcome for the current turn.
    TransportSelected {
        provider: String,
        transport: Transport,
    },
    /// WebSocket connection failed; falling back to HTTP SSE for the rest of the session.
    TransportFallback {
        from: Transport,
        to: Transport,
        reason: String,
    },
    /// OAuth token was auto-refreshed.
    TokenRefreshed { provider: String },
    /// A turn is starting (user sent a message).
    TurnStart { model: String, turn_number: usize },
    /// A turn completed successfully.
    TurnComplete { model: String, turn_number: usize },
    /// A turn failed with an error.
    TurnError {
        #[allow(dead_code)]
        model: String,
        turn_number: usize,
        error: String,
    },
    /// The agent reported a status update (heartbeat) during autonomous mode.
    StatusReport { summary: String, turn_number: usize },
    /// History was compacted to stay within context-window limits.
    HistoryCompacted { old_items: usize, new_chars: usize },
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
            } => write!(f, "[{provider}] using {transport} transport"),
            AgentEvent::TransportFallback { from, to, reason } => {
                write!(f, "⚠ transport fallback: {from} → {to} ({reason})")
            }
            AgentEvent::TokenRefreshed { provider } => {
                write!(f, "[{provider}] token refreshed")
            }
            AgentEvent::TurnStart { model, turn_number } => {
                write!(f, "turn {turn_number} → {model}")
            }
            AgentEvent::TurnComplete { model, turn_number } => {
                write!(f, "turn {turn_number} ✓ ({model})")
            }
            AgentEvent::TurnError {
                turn_number, error, ..
            } => {
                write!(f, "turn {turn_number} ✗ {error}")
            }
            AgentEvent::StatusReport {
                summary,
                turn_number,
            } => {
                write!(f, "turn {turn_number} 📋 {summary}")
            }
            AgentEvent::HistoryCompacted {
                old_items,
                new_chars,
            } => {
                write!(
                    f,
                    "📦 compacted history: {old_items} items → {new_chars} char summary"
                )
            }
        }
    }
}

/// Emitter for agent events. Cheaply cloneable.
#[derive(Clone)]
pub struct EventEmitter {
    tx: mpsc::Sender<AgentEvent>,
}

/// Receiver for agent events.
pub struct EventReceiver {
    rx: mpsc::Receiver<AgentEvent>,
}

/// Create a new event channel.
pub fn event_channel() -> (EventEmitter, EventReceiver) {
    let (tx, rx) = mpsc::channel();
    (EventEmitter { tx }, EventReceiver { rx })
}

impl EventEmitter {
    /// Emit an event. Non-blocking; silently drops if the receiver is gone.
    pub fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }
}

impl EventReceiver {
    /// Block until an event is received. Returns Err when the channel closes.
    pub fn recv_blocking(&self) -> std::result::Result<AgentEvent, mpsc::RecvError> {
        self.rx.recv()
    }

    /// Try to receive an event without blocking.
    #[allow(dead_code)]
    pub fn try_recv(&self) -> Option<AgentEvent> {
        self.rx.try_recv().ok()
    }

    /// Drain all pending events.
    #[allow(dead_code)]
    pub fn drain(&self) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
        }
        events
    }
}
