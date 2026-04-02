#![allow(dead_code)]
//! Shared test harness for integration tests.
//!
//! Provides `TestContext`, builders, and helper utilities so integration
//! tests stay DRY and expressive.

use kley::events::{AgentEvent, EventEmitter, EventReceiver, event_channel};
use kley::store::{NewSession, NewTurn, Session, Store, Turn};
use std::thread;

// ── TestContext ─────────────────────────────────────────────────────────────

/// One-shot test fixture: in-memory store + event channel.
pub struct TestContext {
    pub store: Store,
    pub emitter: EventEmitter,
    pub receiver: EventReceiver,
}

impl TestContext {
    /// Create a fresh, isolated test context.
    pub fn new() -> Self {
        let store = Store::open_memory().expect("failed to open in-memory store");
        let (emitter, receiver) = event_channel();
        Self {
            store,
            emitter,
            receiver,
        }
    }
}

// ── SessionBuilder ──────────────────────────────────────────────────────────

/// Fluent builder for creating sessions with sensible defaults.
pub struct SessionBuilder {
    model: String,
    provider: String,
}

impl SessionBuilder {
    pub fn new() -> Self {
        Self {
            model: "test-model".into(),
            provider: "test-provider".into(),
        }
    }

    pub fn model(mut self, model: &str) -> Self {
        self.model = model.into();
        self
    }

    pub fn provider(mut self, provider: &str) -> Self {
        self.provider = provider.into();
        self
    }

    pub fn create(self, store: &Store) -> Session {
        Session::create(
            store,
            NewSession {
                model: self.model,
                provider: self.provider,
            },
        )
        .expect("failed to create session")
    }
}

// ── TurnBuilder ─────────────────────────────────────────────────────────────

/// Fluent builder for appending turns with sensible defaults.
pub struct TurnBuilder {
    session_id: String,
    kind: String,
    role: String,
    content: String,
    model: Option<String>,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
}

impl TurnBuilder {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.into(),
            kind: "message".into(),
            role: "user".into(),
            content: "Hello".into(),
            model: None,
            tokens_in: None,
            tokens_out: None,
        }
    }

    pub fn role(mut self, role: &str) -> Self {
        self.role = role.into();
        self
    }

    pub fn content(mut self, content: &str) -> Self {
        self.content = content.into();
        self
    }

    pub fn kind(mut self, kind: &str) -> Self {
        self.kind = kind.into();
        self
    }

    pub fn model(mut self, model: &str) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn tokens(mut self, tokens_in: i64, tokens_out: i64) -> Self {
        self.tokens_in = Some(tokens_in);
        self.tokens_out = Some(tokens_out);
        self
    }

    pub fn append(self, store: &Store) -> Turn {
        Turn::append(
            store,
            NewTurn {
                session_id: self.session_id,
                kind: self.kind,
                role: self.role,
                content: self.content,
                model: self.model,
                tokens_in: self.tokens_in,
                tokens_out: self.tokens_out,
            },
        )
        .expect("failed to append turn")
    }
}

// ── EventCollector ──────────────────────────────────────────────────────────

/// Spawns a background thread to collect events from an `EventReceiver`.
///
/// Call `collect()` to join the thread and retrieve all captured events.
pub struct EventCollector {
    handle: Option<thread::JoinHandle<Vec<AgentEvent>>>,
}

impl EventCollector {
    /// Start collecting. Takes ownership of the receiver.
    pub fn start(receiver: EventReceiver) -> Self {
        let handle = thread::spawn(move || {
            let mut events = Vec::new();
            while let Ok(event) = receiver.recv_blocking() {
                events.push(event);
            }
            events
        });
        Self {
            handle: Some(handle),
        }
    }

    /// Drop the emitter first, then call this to drain all events.
    pub fn collect(mut self) -> Vec<AgentEvent> {
        self.handle
            .take()
            .expect("already collected")
            .join()
            .expect("event collector thread panicked")
    }
}
