//! ZAI (ZhipuAI) authentication — simple API key from environment.

use anyhow::{Context, Result};

use super::{CredentialStore, ZaiCredentials};

/// Store a ZAI API key. Reads from `ZAI_API_KEY` env var.
pub fn login_interactive() -> Result<()> {
    let api_key = std::env::var("ZAI_API_KEY")
        .context("ZAI_API_KEY environment variable not set\n\nSet it and re-run:\n  export ZAI_API_KEY=\"your-key-here\"\n  kley login zai")?;

    if api_key.is_empty() {
        anyhow::bail!("ZAI_API_KEY is empty");
    }

    let store = CredentialStore::open()?;
    let mut creds = store.load()?;
    creds.active_provider = Some("zai".into());
    creds.zai = Some(ZaiCredentials { api_key });
    store.save(&creds)?;

    eprintln!("✓ ZAI API key saved.");
    Ok(())
}
