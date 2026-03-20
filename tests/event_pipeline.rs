//! Integration tests for the event pipeline.

mod harness;

use harness::EventCollector;
use kley::events::{event_channel, AgentEvent, Transport};

// ── Events arrive in order ──────────────────────────────────────────────────

#[test]
fn events_arrive_in_emission_order() {
    let (emitter, receiver) = event_channel();
    let collector = EventCollector::start(receiver);

    emitter.emit(AgentEvent::TransportSelected {
        session_id: Some("sess-1".into()),
        turn_id: Some("turn-1".into()),
        provider: "openai".into(),
        transport: Transport::WebSocket,
    });

    emitter.emit(AgentEvent::TurnStarted {
        session_id: "sess-1".into(),
        turn_id: "turn-1".into(),
        model: "gpt-4.1".into(),
        turn_number: 1,
    });

    emitter.emit(AgentEvent::TurnCompleted {
        session_id: "sess-1".into(),
        turn_id: "turn-1".into(),
        model: "gpt-4.1".into(),
        turn_number: 1,
        message_id: "msg-1".into(),
    });

    // Drop emitter to close the channel
    drop(emitter);

    let events = collector.collect();
    assert_eq!(events.len(), 3);

    assert!(
        matches!(&events[0], AgentEvent::TransportSelected { provider, transport, .. }
        if provider == "openai" && *transport == Transport::WebSocket)
    );
    assert!(matches!(
        &events[1],
        AgentEvent::TurnStarted { turn_number: 1, .. }
    ));
    assert!(matches!(
        &events[2],
        AgentEvent::TurnCompleted { turn_number: 1, .. }
    ));
}

// ── Channel closes cleanly when emitter is dropped ──────────────────────────

#[test]
fn channel_closes_on_emitter_drop() {
    let (emitter, receiver) = event_channel();
    let collector = EventCollector::start(receiver);

    emitter.emit(AgentEvent::TokenRefreshed {
        session_id: None,
        provider: "zai".into(),
    });

    drop(emitter);

    let events = collector.collect();
    assert_eq!(events.len(), 1);
}

// ── Drain captures all pending events ───────────────────────────────────────

#[test]
fn drain_captures_pending_events() {
    let (emitter, receiver) = event_channel();

    emitter.emit(AgentEvent::TurnStarted {
        session_id: "sess-1".into(),
        turn_id: "turn-1".into(),
        model: "m".into(),
        turn_number: 1,
    });
    emitter.emit(AgentEvent::TurnStarted {
        session_id: "sess-1".into(),
        turn_id: "turn-2".into(),
        model: "m".into(),
        turn_number: 2,
    });
    emitter.emit(AgentEvent::TurnStarted {
        session_id: "sess-1".into(),
        turn_id: "turn-3".into(),
        model: "m".into(),
        turn_number: 3,
    });

    // Give the channel a moment to buffer
    std::thread::sleep(std::time::Duration::from_millis(50));

    let drained = receiver.drain();
    assert_eq!(drained.len(), 3);
}

// ── Emitting after receiver is dropped doesn't panic ────────────────────────

#[test]
fn emit_after_receiver_dropped_is_silent() {
    let (emitter, receiver) = event_channel();
    drop(receiver);

    // This should not panic
    emitter.emit(AgentEvent::TurnFailed {
        session_id: "sess-1".into(),
        turn_id: "turn-1".into(),
        model: "m".into(),
        turn_number: 1,
        error: "test error".into(),
    });
}

// ── Transport fallback event captures reason ────────────────────────────────

#[test]
fn transport_fallback_captures_reason() {
    let (emitter, receiver) = event_channel();
    let collector = EventCollector::start(receiver);

    emitter.emit(AgentEvent::TransportFallback {
        session_id: Some("sess-1".into()),
        turn_id: Some("turn-1".into()),
        from: Transport::WebSocket,
        to: Transport::Sse,
        reason: "connection refused".into(),
    });

    drop(emitter);

    let events = collector.collect();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::TransportFallback {
            from, to, reason, ..
        } => {
            assert_eq!(*from, Transport::WebSocket);
            assert_eq!(*to, Transport::Sse);
            assert_eq!(reason, "connection refused");
        }
        _ => panic!("expected TransportFallback"),
    }
}

// ── Display formatting ──────────────────────────────────────────────────────

#[test]
fn event_display_formatting() {
    let event = AgentEvent::TurnStarted {
        session_id: "sess-1".into(),
        turn_id: "turn-3".into(),
        model: "gpt-4.1".into(),
        turn_number: 3,
    };
    let s = format!("{event}");
    assert!(s.contains("turn 3"), "expected turn number in display: {s}");
    assert!(s.contains("gpt-4.1"), "expected model in display: {s}");
}
