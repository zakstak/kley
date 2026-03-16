//! OpenAI Codex OAuth — PKCE flow with local callback server.
//!
//! Mirrors the JS implementation in packages/ai/dist/utils/oauth/openai-codex.js.

use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use super::{CredentialStore, OpenAiCredentials};

// ── Constants (verbatim from the JS) ────────────────────────────────────────

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const CALLBACK_PORT: u16 = 1455;

// ── PKCE ────────────────────────────────────────────────────────────────────

/// Generate PKCE verifier (43-char base64url) and S256 challenge.
fn generate_pkce() -> (String, String) {
    let mut rng = rand::thread_rng();
    let mut verifier_bytes = [0u8; 32];
    rng.fill(&mut verifier_bytes);
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

    let challenge_hash = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(challenge_hash);

    (verifier, challenge)
}

/// Generate random hex state string.
fn generate_state() -> String {
    let mut rng = rand::thread_rng();
    let mut buf = [0u8; 16];
    rng.fill(&mut buf);
    hex::encode(buf)
}

// ── Authorize URL ───────────────────────────────────────────────────────────

fn build_authorize_url(challenge: &str, state: &str) -> String {
    let mut url = reqwest::Url::parse(AUTHORIZE_URL).unwrap();
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "kley");
    url.to_string()
}

// ── Token exchange ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

async fn exchange_code(code: &str, verifier: &str) -> Result<OpenAiCredentials> {
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", REDIRECT_URI),
        ])
        .send()
        .await
        .context("token exchange request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("token exchange failed: {status} {body}");
    }

    let token: TokenResponse = resp
        .json()
        .await
        .context("failed to parse token response")?;
    let access = token.access_token.context("missing access_token")?;
    let refresh = token.refresh_token.context("missing refresh_token")?;
    let expires_in = token.expires_in.context("missing expires_in")?;

    let account_id = extract_account_id(&access)?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    Ok(OpenAiCredentials {
        access_token: access,
        refresh_token: refresh,
        expires_at_ms: now_ms + expires_in * 1000,
        account_id,
    })
}

/// Refresh an OpenAI token using the refresh_token grant.
pub async fn refresh_token(refresh_tok: &str) -> Result<OpenAiCredentials> {
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_tok),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .context("token refresh request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("token refresh failed: {status} {body}");
    }

    let token: TokenResponse = resp
        .json()
        .await
        .context("failed to parse refresh response")?;
    let access = token.access_token.context("missing access_token")?;
    let refresh = token.refresh_token.context("missing refresh_token")?;
    let expires_in = token.expires_in.context("missing expires_in")?;

    let account_id = extract_account_id(&access)?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    Ok(OpenAiCredentials {
        access_token: access,
        refresh_token: refresh,
        expires_at_ms: now_ms + expires_in * 1000,
        account_id,
    })
}

// ── JWT decode ──────────────────────────────────────────────────────────────

fn extract_account_id(access_token: &str) -> Result<String> {
    let payload = decode_jwt_payload(access_token).context("failed to decode JWT")?;
    let auth = payload
        .get(JWT_CLAIM_PATH)
        .context("JWT missing auth claim")?;
    let account_id = auth
        .get("chatgpt_account_id")
        .and_then(|v| v.as_str())
        .context("JWT missing chatgpt_account_id")?;
    Ok(account_id.to_string())
}

fn decode_jwt_payload(token: &str) -> Result<serde_json::Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        anyhow::bail!("JWT does not have 3 parts");
    }
    // JWT uses standard base64url (no padding)
    let decoded = URL_SAFE_NO_PAD
        .decode(parts[1])
        .context("invalid base64 in JWT payload")?;
    serde_json::from_slice(&decoded).context("invalid JSON in JWT payload")
}

// ── Local callback server ───────────────────────────────────────────────────

const SUCCESS_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Authentication successful</title>
</head>
<body>
  <p>Authentication successful. Return to your terminal to continue.</p>
</body>
</html>"#;

/// Start a tiny HTTP server on 127.0.0.1:1455 that waits for the OAuth callback.
/// Returns the authorization code when received.
async fn wait_for_callback(expected_state: &str) -> Result<String> {
    let expected_state = expected_state.to_string();
    let (tx, rx) = oneshot::channel::<String>();
    let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));

    let app = axum::Router::new().route(
        "/auth/callback",
        axum::routing::get({
            let expected_state = expected_state.clone();
            let tx = tx.clone();
            move |query: axum::extract::Query<std::collections::HashMap<String, String>>| {
                let tx = tx.clone();
                let expected_state = expected_state.clone();
                async move {
                    let state = query.get("state").cloned().unwrap_or_default();
                    if state != expected_state {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            axum::response::Html("State mismatch".to_string()),
                        );
                    }
                    let code = match query.get("code") {
                        Some(c) => c.clone(),
                        None => {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::response::Html("Missing authorization code".to_string()),
                            );
                        }
                    };
                    if let Some(tx) = tx.lock().unwrap().take() {
                        let _ = tx.send(code);
                    }
                    (
                        axum::http::StatusCode::OK,
                        axum::response::Html(SUCCESS_HTML.to_string()),
                    )
                }
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
        .await
        .context("failed to bind callback server on 127.0.0.1:1455")?;

    // Run the server in the background, shut down once we get the code
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("callback server error");
    });

    // Wait for the code (with a 60s timeout)
    let code = tokio::time::timeout(std::time::Duration::from_secs(60), rx)
        .await
        .context("timed out waiting for OAuth callback (60s)")?
        .context("callback channel dropped")?;

    server.abort();
    Ok(code)
}

// ── Interactive login ───────────────────────────────────────────────────────

/// Run the full interactive OpenAI OAuth login flow.
pub async fn login_interactive() -> Result<()> {
    let (verifier, challenge) = generate_pkce();
    let state = generate_state();
    let url = build_authorize_url(&challenge, &state);

    eprintln!("Opening browser for OpenAI login...");
    eprintln!("If the browser doesn't open, visit:\n  {url}\n");

    // Try to open the browser (non-fatal if it fails)
    let _ = open::that(&url);

    eprintln!("Waiting for callback on http://127.0.0.1:{CALLBACK_PORT}/auth/callback ...");
    let code = wait_for_callback(&state).await?;

    eprintln!("Exchanging authorization code for tokens...");
    let creds = exchange_code(&code, &verifier).await?;

    // Save to credential store
    let store = CredentialStore::open()?;
    let mut all = store.load()?;
    all.active_provider = Some("openai".into());
    all.openai = Some(creds);
    store.save(&all)?;

    eprintln!("✓ OpenAI login successful. Credentials saved.");
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_generation() {
        let (verifier, challenge) = generate_pkce();
        // base64url of 32 bytes = 43 chars (no padding)
        assert_eq!(verifier.len(), 43);
        // challenge = sha256(verifier) base64url = 43 chars
        assert_eq!(challenge.len(), 43);
        // Verify the challenge is correct
        let hash = Sha256::digest(verifier.as_bytes());
        let expected = URL_SAFE_NO_PAD.encode(hash);
        assert_eq!(challenge, expected);
    }

    #[test]
    fn test_build_authorize_url() {
        let url = build_authorize_url("test_challenge", "test_state");
        assert!(url.starts_with(AUTHORIZE_URL));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("code_challenge=test_challenge"));
        assert!(url.contains("state=test_state"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri="));
        assert!(url.contains("originator=kley"));
    }

    #[test]
    fn test_jwt_decode() {
        // Build a test JWT: header.payload.signature
        let payload = serde_json::json!({
            JWT_CLAIM_PATH: {
                "chatgpt_account_id": "acct-test-123"
            }
        });
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        let header_b64 = URL_SAFE_NO_PAD.encode(b"{}");
        let sig_b64 = URL_SAFE_NO_PAD.encode(b"sig");
        let token = format!("{header_b64}.{payload_b64}.{sig_b64}");

        let account_id = extract_account_id(&token).unwrap();
        assert_eq!(account_id, "acct-test-123");
    }

    #[test]
    fn test_generate_state() {
        let state = generate_state();
        // 16 random bytes = 32 hex chars
        assert_eq!(state.len(), 32);
        assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
