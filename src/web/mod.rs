pub mod config;
pub mod mock;
pub mod protocol;
pub mod router;
pub mod state;
pub mod ui;
pub mod ws;

use anyhow::{Context, Result};

use config::WebConfig;
use state::WebAppState;

pub async fn serve(config: WebConfig) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("failed to bind web server on {}", config.bind_addr))?;

    let state = WebAppState::for_web_mode().context("failed to initialize web state")?;

    axum::serve(listener, router::app_with_state(state))
        .await
        .context("web server error")
}
