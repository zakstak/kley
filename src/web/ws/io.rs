use axum::extract::ws::{Message, WebSocket};

use crate::web::protocol::{UiEvent, WebResponse};

pub(super) async fn send_response(
    socket: &mut WebSocket,
    response: WebResponse,
) -> std::result::Result<(), ()> {
    let text = serde_json::to_string(&response).map_err(|_| ())?;
    socket.send(Message::Text(text)).await.map_err(|_| ())
}

pub(super) async fn send_event(
    socket: &mut WebSocket,
    event: UiEvent,
) -> std::result::Result<(), ()> {
    let text = serde_json::to_string(&event).map_err(|_| ())?;
    socket.send(Message::Text(text)).await.map_err(|_| ())
}
