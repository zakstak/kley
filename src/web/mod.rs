mod callback;
pub mod config;
pub mod mock;
mod origin;
pub mod protocol;
pub mod router;
pub mod state;
pub mod ui;
pub mod ws;

use anyhow::{Context, Result};
use axum::{Router, routing::get};

use config::WebConfig;
use state::WebAppState;

async fn serve_openai_callback_bridge() -> Result<()> {
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:1455").await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("warning: failed to start OpenAI callback bridge on 127.0.0.1:1455: {error}");
            return Ok(());
        }
    };

    let app = Router::new().route("/auth/callback", get(callback::openai_callback));

    axum::serve(listener, app)
        .await
        .context("callback bridge server error")
}

pub async fn serve(config: WebConfig) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("failed to bind web server on {}", config.bind_addr))?;

    let state = WebAppState::for_web_mode().context("failed to initialize web state")?;
    let callback_bridge = tokio::spawn(async {
        if let Err(error) = serve_openai_callback_bridge().await {
            eprintln!("warning: OpenAI callback bridge exited: {error}");
        }
    });

    let result = axum::serve(listener, router::app_with_state(state))
        .await
        .context("web server error");

    callback_bridge.abort();
    result
}
