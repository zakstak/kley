use anyhow::Result;

use super::super::protocol::{
    ActiveTurnSnapshot, SelectedSession, SessionSummary, StateSnapshotData, TranscriptEntry,
};
use super::super::state::WebAppState;
use super::context_usage::{context_usage_from_chars, estimate_persisted_context_usage};
use crate::compact::CompactConfig;
use crate::runtime::ActiveTurnReplay;
use crate::store::{self, Session, Turn};

pub(super) async fn snapshot_data(
    state: &WebAppState,
    session_id: &str,
) -> Result<StateSnapshotData> {
    let selected_session = load_selected_session(state, session_id).await?;
    let sessions = list_sessions(state, Some(session_id)).await?;
    let turns = load_turns(state, session_id).await?;
    let transcript = turns_to_transcript(&turns);
    let active_turn = state
        .runtime_manager
        .active_turn(session_id)
        .map(active_turn_snapshot);
    let context_usage = if let Some((used_chars, max_chars)) =
        state.runtime_manager.context_usage_chars(session_id)
    {
        let (input_tokens, output_tokens, total_tokens) = state
            .runtime_manager
            .token_usage(session_id)
            .unwrap_or((None, None, None));
        context_usage_from_chars(
            used_chars,
            max_chars,
            input_tokens,
            output_tokens,
            total_tokens,
        )
    } else {
        let compact_threshold = state
            .runtime_manager
            .compact_threshold_chars(session_id)
            .unwrap_or_else(|| CompactConfig::default().threshold_chars);
        estimate_persisted_context_usage(&turns, compact_threshold)
    };
    Ok(StateSnapshotData {
        protocol_version: super::super::protocol::PROTOCOL_VERSION,
        session_id: session_id.to_string(),
        selected_session,
        sessions,
        transcript,
        active_turn,
        context_usage,
    })
}

pub(super) async fn list_sessions(
    state: &WebAppState,
    selected_session_id: Option<&str>,
) -> Result<Vec<SessionSummary>> {
    let store_ref = state.store.clone();
    let mut sessions = store::store_run(&store_ref, |store| Session::list(store, 50)).await?;

    if let Some(selected_session_id) = selected_session_id {
        let contains_selected = sessions
            .iter()
            .any(|session| session.id == selected_session_id);
        if !contains_selected {
            let selected_session_id = selected_session_id.to_string();
            if let Some(selected_session) = store::store_run(&store_ref, move |store| {
                Session::find(store, &selected_session_id)
            })
            .await?
            {
                sessions.insert(0, selected_session);
            }
        }
    }

    Ok(sessions
        .into_iter()
        .map(|session| SessionSummary {
            session_id: session.id,
            title: session
                .title
                .unwrap_or_else(|| "Untitled session".to_string()),
            updated_at: session.updated_at.to_rfc3339(),
        })
        .collect())
}

async fn load_selected_session(state: &WebAppState, session_id: &str) -> Result<SelectedSession> {
    let session_id = session_id.to_string();
    let store_ref = state.store.clone();
    let session = store::store_run(&store_ref, move |store| {
        Session::find(store, &session_id)?.ok_or_else(|| anyhow::anyhow!("session not found"))
    })
    .await?;

    Ok(SelectedSession {
        session_id: session.id,
        title: session
            .title
            .unwrap_or_else(|| "Untitled session".to_string()),
        status: session.status.to_string(),
        created_at: session.created_at.to_rfc3339(),
        updated_at: session.updated_at.to_rfc3339(),
    })
}

async fn load_turns(state: &WebAppState, session_id: &str) -> Result<Vec<Turn>> {
    let session_id = session_id.to_string();
    let store_ref = state.store.clone();
    store::store_run(&store_ref, move |store| {
        Turn::list_for_session(store, &session_id)
    })
    .await
}

fn turns_to_transcript(turns: &[Turn]) -> Vec<TranscriptEntry> {
    turns
        .iter()
        .map(|turn| TranscriptEntry {
            turn_number: turn.turn_number,
            kind: turn.kind.clone(),
            role: turn.role.clone(),
            content: turn.content.clone(),
        })
        .collect()
}

fn active_turn_snapshot(active: ActiveTurnReplay) -> ActiveTurnSnapshot {
    ActiveTurnSnapshot {
        request_id: active.request_id,
        turn_id: active.turn_id,
        message_id: active.message_id,
        content: active.content,
    }
}
