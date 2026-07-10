use axum::extract::ws::{Message, WebSocket};

use crate::web::protocol::{UiEvent, WebResponse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WsIoErrorKind {
    SerializeResponse,
    SerializeEvent,
    SendResponse,
    SendEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WsIoError {
    pub kind: WsIoErrorKind,
    pub message: String,
}

impl WsIoError {
    fn new(kind: WsIoErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

pub(super) async fn send_response(
    socket: &mut WebSocket,
    response: WebResponse,
) -> Result<(), WsIoError> {
    let text = serde_json::to_string(&response)
        .map_err(|error| WsIoError::new(WsIoErrorKind::SerializeResponse, error.to_string()))?;
    socket
        .send(Message::text(text))
        .await
        .map_err(|error| WsIoError::new(WsIoErrorKind::SendResponse, error.to_string()))
}

pub(super) async fn send_event(socket: &mut WebSocket, event: UiEvent) -> Result<(), WsIoError> {
    let text = serde_json::to_string(&event)
        .map_err(|error| WsIoError::new(WsIoErrorKind::SerializeEvent, error.to_string()))?;
    socket
        .send(Message::text(text))
        .await
        .map_err(|error| WsIoError::new(WsIoErrorKind::SendEvent, error.to_string()))
}
