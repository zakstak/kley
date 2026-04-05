use axum::{
    Router,
    routing::{any, get},
};

use super::{callback, mock, state::WebAppState, ui, ws};

pub fn app() -> Router {
    let state = WebAppState::for_web_mode().expect("web state should initialize");
    app_with_state(state)
}

pub fn app_with_state(state: WebAppState) -> Router {
    Router::new()
        .route("/", get(ui::root))
        .route("/auth/callback", get(callback::openai_callback))
        .route("/assets/bindery-icon.svg", get(ui::bindery_icon))
        .route("/healthz", get(healthz))
        .route("/ws", any(ws::ws_handler))
        .route("/ws/mock", any(mock::ws_handler_with_headers))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}
