use anyhow::Result;

use super::super::state::WebAppState;
use super::{DEFAULT_WEB_MODEL, DEFAULT_WEB_PROVIDER};
use crate::compact::CompactConfig;
use crate::runtime::AttachControllerError;
use crate::runtime::canonical_settings_json;
use crate::store::{self, NewSession, Session};

pub(super) enum SelectSessionError {
    Busy(AttachControllerError),
    Store,
}

pub(super) async fn attach_or_select_session(
    state: &WebAppState,
    controller_id: &str,
    preferred_session_id: Option<&str>,
) -> std::result::Result<Session, SelectSessionError> {
    let store_ref = state.store.clone();
    let mut sessions = store::store_run(&store_ref, |store| Session::list(store, 50))
        .await
        .map_err(|_| SelectSessionError::Store)?;

    if sessions.is_empty() {
        let session = create_default_session(state)
            .await
            .map_err(|_| SelectSessionError::Store)?;
        state
            .runtime_manager
            .attach_controller(&session, controller_id)
            .map_err(SelectSessionError::Busy)?;
        return Ok(session);
    }

    if let Some(session_id) = preferred_session_id {
        if let Some(index) = sessions.iter().position(|session| session.id == session_id) {
            let preferred = sessions.remove(index);
            sessions.insert(0, preferred);
        } else {
            let preferred_session_id = session_id.to_string();
            if let Some(preferred) = store::store_run(&store_ref, move |store| {
                Session::find(store, &preferred_session_id)
            })
            .await
            .map_err(|_| SelectSessionError::Store)?
            {
                sessions.insert(0, preferred);
            }
        }
    }

    let mut busy_error = None;
    for session in sessions {
        let session = ensure_session_settings(state, session)
            .await
            .map_err(|_| SelectSessionError::Store)?;
        let is_requested_session = preferred_session_id == Some(session.id.as_str());
        match state
            .runtime_manager
            .attach_controller(&session, controller_id)
        {
            Ok(()) => return Ok(session),
            Err(err) => {
                if is_requested_session {
                    return Err(SelectSessionError::Busy(err));
                }
                busy_error = Some(err);
            }
        }
    }

    match busy_error {
        Some(err) => Err(SelectSessionError::Busy(err)),
        None => Err(SelectSessionError::Store),
    }
}

async fn create_default_session(state: &WebAppState) -> Result<Session> {
    let store_ref = state.store.clone();
    let session = store::store_run(&store_ref, |store| {
        Session::create(
            store,
            NewSession {
                model: DEFAULT_WEB_MODEL.to_string(),
                provider: DEFAULT_WEB_PROVIDER.to_string(),
            },
        )
    })
    .await?;

    ensure_session_settings(state, session).await
}

pub(super) async fn ensure_session_settings(
    state: &WebAppState,
    mut session: Session,
) -> Result<Session> {
    if session.settings.is_none() {
        let settings_json = default_settings_json(&session.model, &session.provider);
        let id = session.id.clone();
        let store_ref = state.store.clone();
        let update_value = settings_json.clone();
        store::store_run(&store_ref, move |store| {
            Session::update_settings(store, &id, &update_value)?;
            Ok(())
        })
        .await?;
        session.settings = Some(settings_json);
    }

    Ok(session)
}

fn default_settings_json(model: &str, provider: &str) -> String {
    canonical_settings_json(model, provider, CompactConfig::default().threshold_chars)
}

pub(super) enum LoadSessionError {
    Busy(AttachControllerError),
    TurnInProgress { turn_id: String },
    NotFound,
    Store,
}

pub(super) async fn load_session_for_controller(
    state: &WebAppState,
    controller_id: &str,
    previous_session: &Session,
    next_session_id: &str,
) -> std::result::Result<Session, LoadSessionError> {
    let next_session_id = next_session_id.to_string();
    let store_ref = state.store.clone();
    let session = store::store_run(&store_ref, move |store| {
        Session::find(store, &next_session_id)
    })
    .await
    .map_err(|_| LoadSessionError::Store)?
    .ok_or(LoadSessionError::NotFound)?;
    let session = ensure_session_settings(state, session)
        .await
        .map_err(|_| LoadSessionError::Store)?;

    if session.id != previous_session.id
        && let Some(active_turn) = state.runtime_manager.active_turn(&previous_session.id)
    {
        return Err(LoadSessionError::TurnInProgress {
            turn_id: active_turn.turn_id,
        });
    }

    state
        .runtime_manager
        .attach_controller(&session, controller_id)
        .map_err(LoadSessionError::Busy)?;

    if session.id != previous_session.id {
        state
            .runtime_manager
            .release_controller(&previous_session.id, controller_id);
    }
    Ok(session)
}
