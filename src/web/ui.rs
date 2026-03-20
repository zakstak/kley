use askama::Template;
use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse},
};

use crate::web::protocol::PROTOCOL_VERSION;

#[derive(Template)]
#[template(path = "index.html")]
struct ShellTemplate {
    ws_path: &'static str,
    protocol_version: u32,
}

const BINDERY_ICON: &str = include_str!("../../assets/bindery-icon.svg");

pub async fn root() -> Result<Html<String>, (StatusCode, &'static str)> {
    ShellTemplate {
        ws_path: "/ws",
        protocol_version: PROTOCOL_VERSION,
    }
    .render()
    .map(Html)
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to render web shell",
        )
    })
}

pub async fn bindery_icon() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        BINDERY_ICON,
    )
}
