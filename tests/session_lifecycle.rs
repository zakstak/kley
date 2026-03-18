//! Integration tests for the full session lifecycle.
//!
//! These tests exercise session + turn APIs across module boundaries,
//! verifying the complete create → populate → query → update → resume flow.

mod harness;

use harness::{SessionBuilder, TurnBuilder};
use kley::agent::history_from_turns;
use kley::store::{Session, SessionStatus, Store, Turn};

// ── Create and retrieve ─────────────────────────────────────────────────────

#[test]
fn create_session_and_retrieve_by_id() {
    let store = Store::open_memory().unwrap();

    let session = SessionBuilder::new()
        .model("gpt-4.1")
        .provider("openai")
        .create(&store);

    let fetched = Session::get(&store, &session.id).unwrap();
    assert_eq!(fetched.id, session.id);
    assert_eq!(fetched.model, "gpt-4.1");
    assert_eq!(fetched.provider, "openai");
    assert_eq!(fetched.status, SessionStatus::Active);
}

// ── Session appears in list ─────────────────────────────────────────────────

#[test]
fn session_appears_in_list() {
    let store = Store::open_memory().unwrap();

    let s1 = SessionBuilder::new().model("m1").create(&store);
    let s2 = SessionBuilder::new().model("m2").create(&store);

    let list = Session::list(&store, 10).unwrap();
    assert_eq!(list.len(), 2);
    // Most recent first
    assert_eq!(list[0].id, s2.id);
    assert_eq!(list[1].id, s1.id);
}

// ── Turn ordering and numbering ─────────────────────────────────────────────

#[test]
fn turns_are_numbered_sequentially() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new().create(&store);

    TurnBuilder::new(&session.id)
        .role("user")
        .content("Hello")
        .append(&store);

    TurnBuilder::new(&session.id)
        .role("assistant")
        .content("Hi!")
        .model("gpt-4.1")
        .tokens(10, 25)
        .append(&store);

    TurnBuilder::new(&session.id)
        .role("user")
        .content("How are you?")
        .append(&store);

    let turns = Turn::list_for_session(&store, &session.id).unwrap();
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[0].turn_number, 1);
    assert_eq!(turns[1].turn_number, 2);
    assert_eq!(turns[2].turn_number, 3);
    assert_eq!(turns[0].role, "user");
    assert_eq!(turns[1].role, "assistant");
    assert_eq!(turns[1].tokens_in, Some(10));
    assert_eq!(turns[1].tokens_out, Some(25));
}

// ── Status updates ──────────────────────────────────────────────────────────

#[test]
fn status_update_persists() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new().create(&store);

    Session::update_status(&store, &session.id, SessionStatus::Completed).unwrap();
    let fetched = Session::get(&store, &session.id).unwrap();
    assert_eq!(fetched.status, SessionStatus::Completed);
}

// ── Title auto-update ───────────────────────────────────────────────────────

#[test]
fn title_update_persists() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new().create(&store);

    Session::update_title(&store, &session.id, "My awesome chat").unwrap();
    let fetched = Session::get(&store, &session.id).unwrap();
    assert_eq!(fetched.title.as_deref(), Some("My awesome chat"));
}

// ── Latest session ──────────────────────────────────────────────────────────

#[test]
fn get_latest_returns_newest_session() {
    let store = Store::open_memory().unwrap();

    let _s1 = SessionBuilder::new().model("old").create(&store);
    let s2 = SessionBuilder::new().model("new").create(&store);

    let latest = Session::get_latest(&store).unwrap().unwrap();
    assert_eq!(latest.id, s2.id);
    assert_eq!(latest.model, "new");
}

// ── Resume: history reconstruction ──────────────────────────────────────────

#[test]
fn resume_session_reconstructs_history() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new().create(&store);

    TurnBuilder::new(&session.id)
        .role("user")
        .content("What is Rust?")
        .append(&store);

    TurnBuilder::new(&session.id)
        .role("assistant")
        .content("Rust is a systems programming language.")
        .append(&store);

    // Simulate "resume": re-fetch from store and reconstruct history
    let turns = Turn::list_for_session(&store, &session.id).unwrap();
    let history = history_from_turns(&turns);

    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, "user");
    assert_eq!(history[0].content, "What is Rust?");
    assert_eq!(history[1].role, "assistant");
    assert_eq!(
        history[1].content,
        "Rust is a systems programming language."
    );
}

// ── Turns are isolated between sessions ─────────────────────────────────────

#[test]
fn turns_are_scoped_to_session() {
    let store = Store::open_memory().unwrap();

    let s1 = SessionBuilder::new().create(&store);
    let s2 = SessionBuilder::new().create(&store);

    TurnBuilder::new(&s1.id).content("s1-only").append(&store);
    TurnBuilder::new(&s2.id).content("s2-only").append(&store);

    let t1 = Turn::list_for_session(&store, &s1.id).unwrap();
    let t2 = Turn::list_for_session(&store, &s2.id).unwrap();

    assert_eq!(t1.len(), 1);
    assert_eq!(t2.len(), 1);
    assert_eq!(t1[0].content, "s1-only");
    assert_eq!(t2[0].content, "s2-only");
}

// ── Settings round-trip ─────────────────────────────────────────────────────

#[test]
fn settings_round_trip() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new().create(&store);

    let settings_json = r#"{"temperature":0.7,"top_p":0.9}"#;
    Session::update_settings(&store, &session.id, settings_json).unwrap();

    let fetched = Session::get(&store, &session.id).unwrap();
    assert_eq!(fetched.settings.as_deref(), Some(settings_json));
}
