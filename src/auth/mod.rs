pub mod openai;
pub mod zai;

use std::collections::HashMap;
use std::path::PathBuf;

use age::secrecy::SecretString;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::events::{AgentEvent, EventEmitter};

/// Stored credentials for all providers.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Credentials {
    /// Which provider is currently active
    pub active_provider: Option<String>,
    /// OpenAI OAuth credentials
    pub openai: Option<OpenAiCredentials>,
    #[serde(default)]
    pub openai_api_key: Option<OpenAiApiKeyCredentials>,
    /// ZAI API key
    pub zai: Option<ZaiCredentials>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCredentials {
    pub access_token: String,
    pub refresh_token: String,
    /// Milliseconds since epoch when the access token expires
    pub expires_at_ms: u64,
    pub account_id: String,
    /// ID token from the OAuth flow (needed for token exchange / refresh)
    #[serde(default)]
    pub id_token: Option<String>,
    /// API key obtained via token exchange (this is what we actually use for API calls)
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiApiKeyCredentials {
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZaiCredentials {
    pub api_key: String,
}

// ── Storage trait ───────────────────────────────────────────────────────────

/// Pluggable backend for credential storage.
pub trait SecretBackend: Send + Sync {
    fn load(&self) -> Result<Option<Credentials>>;
    fn save(&self, creds: &Credentials) -> Result<()>;
}

// ── Vault backend ───────────────────────────────────────────────────────────

/// HashiCorp Vault KV v2 backend. Reads `VAULT_ADDR` and `VAULT_TOKEN` from environment.
#[derive(Debug)]
pub struct VaultBackend {
    addr: String,
    token: String,
    mount: String,
    path: String,
}

impl VaultBackend {
    /// Create from environment variables. Returns None if VAULT_ADDR or VAULT_TOKEN are unset.
    pub fn from_env() -> Option<Self> {
        let addr = std::env::var("VAULT_ADDR").ok()?;
        let token = std::env::var("VAULT_TOKEN").ok()?;
        Some(Self {
            addr,
            token,
            mount: "secret".into(),
            path: "kley/credentials".into(),
        })
    }
}

impl SecretBackend for VaultBackend {
    fn load(&self) -> Result<Option<Credentials>> {
        // Use blocking reqwest since the trait is sync
        let url = format!("{}/v1/{}/data/{}", self.addr, self.mount, self.path);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .context("vault request failed")?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("vault read error: {status}\n{body}");
        }

        let body: serde_json::Value = resp.json().context("vault response parse failed")?;
        let data = body
            .get("data")
            .and_then(|d: &serde_json::Value| d.get("data"))
            .context("unexpected vault response structure")?;

        let creds: Credentials =
            serde_json::from_value(data.clone()).context("failed to parse vault secret")?;
        Ok(Some(creds))
    }

    fn save(&self, creds: &Credentials) -> Result<()> {
        let url = format!("{}/v1/{}/data/{}", self.addr, self.mount, self.path);
        let mut payload = HashMap::new();
        payload.insert("data", serde_json::to_value(creds)?);

        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(&url)
            .header("X-Vault-Token", &self.token)
            .json(&payload)
            .send()
            .context("vault write failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("vault write error: {status}\n{body}");
        }

        Ok(())
    }
}

// ── Age-encrypted file backend ──────────────────────────────────────────────

/// Passphrase-encrypted file backend using age (scrypt).
pub struct AgeFileBackend {
    path: PathBuf,
    passphrase: SecretString,
    max_work_factor: u8,
}

const DEFAULT_AGE_MAX_WORK_FACTOR: u8 = 20;
const MAX_ALLOWED_AGE_WORK_FACTOR: u8 = 20;
const AGE_MAX_WORK_FACTOR_ENV: &str = "KLEY_AGE_MAX_WORK_FACTOR";

fn ensure_supported_age_max_work_factor(max_work_factor: u8) {
    assert!((1..=MAX_ALLOWED_AGE_WORK_FACTOR).contains(&max_work_factor));
}

impl AgeFileBackend {
    #[allow(dead_code)]
    pub fn new(path: PathBuf, passphrase: SecretString) -> Self {
        Self::with_max_work_factor(path, passphrase, DEFAULT_AGE_MAX_WORK_FACTOR)
    }

    #[allow(dead_code)]
    pub fn with_max_work_factor(
        path: PathBuf,
        passphrase: SecretString,
        max_work_factor: u8,
    ) -> Self {
        ensure_supported_age_max_work_factor(max_work_factor);
        Self {
            path,
            passphrase,
            max_work_factor,
        }
    }

    /// Prompt for a passphrase interactively.
    pub fn open_interactive(path: PathBuf, prompt: &str, max_work_factor: u8) -> Result<Self> {
        let pp = rpassword::prompt_password(prompt).context("failed to read passphrase")?;
        Ok(Self::with_max_work_factor(
            path,
            SecretString::from(pp),
            max_work_factor,
        ))
    }
}

fn parse_age_max_work_factor_from_env() -> Result<Option<u8>> {
    let raw = match std::env::var(AGE_MAX_WORK_FACTOR_ENV) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{AGE_MAX_WORK_FACTOR_ENV} must be valid UTF-8")
        }
    };

    parse_age_max_work_factor_from_str(&raw)
}

fn parse_age_max_work_factor_from_str(raw: &str) -> Result<Option<u8>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }

    let value: u8 = raw
        .parse()
        .with_context(|| format!("{AGE_MAX_WORK_FACTOR_ENV} must be an integer"))?;
    if !(1..=MAX_ALLOWED_AGE_WORK_FACTOR).contains(&value) {
        anyhow::bail!(
            "{AGE_MAX_WORK_FACTOR_ENV} must be between 1 and {MAX_ALLOWED_AGE_WORK_FACTOR}"
        );
    }

    Ok(Some(value))
}

fn configured_age_max_work_factor() -> Result<u8> {
    Ok(parse_age_max_work_factor_from_env()?.unwrap_or(DEFAULT_AGE_MAX_WORK_FACTOR))
}

fn decrypt_with_passphrase(
    passphrase: SecretString,
    encrypted: &[u8],
    max_work_factor: u8,
) -> std::result::Result<Vec<u8>, age::DecryptError> {
    let mut identity = age::scrypt::Identity::new(passphrase);
    identity.set_max_work_factor(max_work_factor);
    age::decrypt(&identity, encrypted)
}

fn map_decrypt_error(err: age::DecryptError, max_work_factor: u8) -> anyhow::Error {
    match err {
        age::DecryptError::ExcessiveWork { required, .. } => {
            if required <= MAX_ALLOWED_AGE_WORK_FACTOR {
                anyhow::anyhow!(
                    "decryption failed: credentials.age requires scrypt work factor {required}, but Kley allows up to {max_work_factor}; set {AGE_MAX_WORK_FACTOR_ENV}={required} if you trust this file, or recreate credentials.age with current Kley defaults"
                )
            } else {
                anyhow::anyhow!(
                    "decryption failed: credentials.age requires scrypt work factor {required}, which exceeds Kley's supported maximum of {MAX_ALLOWED_AGE_WORK_FACTOR}; recreate credentials.age with current Kley defaults"
                )
            }
        }
        other => anyhow::anyhow!(
            "decryption failed (wrong passphrase?): {other}; make sure KLEY_PASSPHRASE matches the value used when credentials were created, or recreate credentials (for disposable dev state, remove ~/.config/kley/credentials.age and log in again)"
        ),
    }
}

impl SecretBackend for AgeFileBackend {
    fn load(&self) -> Result<Option<Credentials>> {
        match std::fs::read(&self.path) {
            Ok(encrypted) => {
                let decrypted = decrypt_with_passphrase(
                    self.passphrase.clone(),
                    &encrypted,
                    self.max_work_factor,
                )
                .map_err(|err| map_decrypt_error(err, self.max_work_factor))?;
                let creds: Credentials = serde_json::from_slice(&decrypted)
                    .context("failed to parse decrypted credentials")?;
                Ok(Some(creds))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", self.path.display())),
        }
    }

    fn save(&self, creds: &Credentials) -> Result<()> {
        let plaintext = serde_json::to_string_pretty(creds)?;
        let mut recipient = age::scrypt::Recipient::new(self.passphrase.clone());
        recipient.set_work_factor(self.max_work_factor);
        let encrypted = age::encrypt(&recipient, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&self.path)
            .with_context(|| format!("failed to write {}", self.path.display()))?;
        file.write_all(&encrypted)?;

        Ok(())
    }
}

// ── CredentialStore (auto-selects backend) ───────────────────────────────────

/// Credential store with pluggable backend.
///
/// Tries Vault first (if `VAULT_ADDR` + `VAULT_TOKEN` are set),
/// then falls back to age-encrypted file.
pub struct CredentialStore {
    backend: Box<dyn SecretBackend>,
    #[allow(dead_code)]
    backend_name: String,
}

enum CredentialBackendSelection {
    Vault(VaultBackend),
    AgeFile { path: PathBuf, prompt: &'static str },
}

fn select_backend() -> Result<CredentialBackendSelection> {
    if let Some(vault) = VaultBackend::from_env() {
        return Ok(CredentialBackendSelection::Vault(vault));
    }

    let config_dir = dirs::config_dir()
        .context("could not determine config directory")?
        .join("kley");
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("failed to create {}", config_dir.display()))?;
    let path = config_dir.join("credentials.age");

    let prompt = if path.exists() {
        "Passphrase: "
    } else {
        "Choose passphrase for credentials: "
    };

    Ok(CredentialBackendSelection::AgeFile { path, prompt })
}

impl CredentialStore {
    /// Open the credential store, auto-selecting the best available backend.
    pub fn open() -> Result<Self> {
        match select_backend()? {
            CredentialBackendSelection::Vault(vault) => Ok(Self {
                backend: Box::new(vault),
                backend_name: "vault".into(),
            }),
            CredentialBackendSelection::AgeFile { path, prompt } => {
                let max_work_factor = configured_age_max_work_factor()?;
                let backend = if let Ok(pp) = std::env::var("KLEY_PASSPHRASE") {
                    AgeFileBackend::with_max_work_factor(
                        path,
                        SecretString::from(pp),
                        max_work_factor,
                    )
                } else {
                    AgeFileBackend::open_interactive(path, prompt, max_work_factor)?
                };
                Ok(Self {
                    backend: Box::new(backend),
                    backend_name: "age-file".into(),
                })
            }
        }
    }

    pub fn open_noninteractive() -> Result<Self> {
        match select_backend()? {
            CredentialBackendSelection::Vault(vault) => Ok(Self {
                backend: Box::new(vault),
                backend_name: "vault".into(),
            }),
            CredentialBackendSelection::AgeFile { path, .. } => {
                let max_work_factor = configured_age_max_work_factor()?;
                let passphrase = std::env::var("KLEY_PASSPHRASE").context(
                    "web auth requires KLEY_PASSPHRASE to access the credential store, or VAULT_ADDR/VAULT_TOKEN for Vault-backed credentials",
                )?;

                if passphrase.trim().is_empty() {
                    anyhow::bail!(
                        "web auth requires a non-empty KLEY_PASSPHRASE, or VAULT_ADDR/VAULT_TOKEN for Vault-backed credentials"
                    );
                }

                Ok(Self {
                    backend: Box::new(AgeFileBackend::with_max_work_factor(
                        path,
                        SecretString::from(passphrase),
                        max_work_factor,
                    )),
                    backend_name: "age-file".into(),
                })
            }
        }
    }

    /// Which backend is active.
    #[allow(dead_code)]
    pub fn backend_name(&self) -> &str {
        &self.backend_name
    }

    /// Load credentials. Returns default if nothing stored yet.
    pub fn load(&self) -> Result<Credentials> {
        Ok(self.backend.load()?.unwrap_or_default())
    }

    /// Save credentials.
    pub fn save(&self, creds: &Credentials) -> Result<()> {
        self.backend.save(creds)
    }
}

/// Resolved authentication info for making API calls.
#[derive(Debug)]
pub struct ResolvedAuth {
    pub provider: String,
    pub api_key: String,
    pub base_url: String,
    /// Optional organization/account header (OpenAI only)
    pub account_id: Option<String>,
}

pub fn save_openai_oauth_credentials(
    store: &CredentialStore,
    credentials: OpenAiCredentials,
) -> Result<()> {
    let mut all = store.load()?;
    all.active_provider = Some("openai".into());
    all.openai = Some(credentials);
    all.openai_api_key = None;
    store.save(&all)
}

/// Resolve the current auth, refreshing tokens if necessary.
///
/// For OpenAI, checks `OPENAI_API_KEY` env var first (platform API keys).
/// Falls back to stored OAuth credentials if the env var is not set.
pub async fn resolve_auth(store: &CredentialStore, events: &EventEmitter) -> Result<ResolvedAuth> {
    // Check for OPENAI_API_KEY env var first — this is the simplest path
    // and avoids the ChatGPT OAuth scope limitations.
    if let Ok(api_key) = std::env::var("OPENAI_API_KEY")
        && !api_key.is_empty()
    {
        return Ok(ResolvedAuth {
            provider: "openai".into(),
            api_key,
            base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            account_id: None,
        });
    }

    let mut creds = store.load()?;
    let provider = creds
        .active_provider
        .as_deref()
        .context("no active provider — run `kley login openai` or `kley login zai` first, or set OPENAI_API_KEY")?;

    match provider {
        "openai" => {
            if let Some(openai_api_key) = creds.openai_api_key.as_ref() {
                let api_key = openai_api_key.api_key.trim();
                if !api_key.is_empty() {
                    return Ok(ResolvedAuth {
                        provider: "openai".into(),
                        api_key: api_key.to_string(),
                        base_url: std::env::var("OPENAI_BASE_URL")
                            .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
                        account_id: None,
                    });
                }
            }

            let oa = creds
                .openai
                .as_mut()
                .context("openai credentials missing")?;

            // Refresh if expired (with 60s buffer)
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;

            if now_ms + 60_000 >= oa.expires_at_ms {
                let refreshed = openai::refresh_token(&oa.refresh_token).await?;
                *oa = refreshed;
                store.save(&creds)?;
                events.emit(AgentEvent::TokenRefreshed {
                    session_id: None,
                    provider: "openai".into(),
                });
            }

            let oa = creds.openai.as_ref().unwrap();
            // If we have an exchanged API key, use the standard API endpoint.
            // Otherwise fall back to the ChatGPT backend (access_token auth),
            // matching codex-rs's behavior.
            let (key, base_url) = if let Some(ref api_key) = oa.api_key {
                (api_key.clone(), "https://api.openai.com/v1".to_string())
            } else {
                (
                    oa.access_token.clone(),
                    "https://chatgpt.com/backend-api/codex".to_string(),
                )
            };
            Ok(ResolvedAuth {
                provider: "openai".into(),
                api_key: key,
                base_url,
                account_id: Some(oa.account_id.clone()),
            })
        }
        "zai" => {
            let z = creds.zai.as_ref().context("zai credentials missing")?;
            Ok(ResolvedAuth {
                provider: "zai".into(),
                api_key: z.api_key.clone(),
                base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
                account_id: None,
            })
        }
        other => anyhow::bail!("unknown provider: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const TEST_AGE_MAX_WORK_FACTOR: u8 = 1;

    // ── In-memory backend for contract tests ────────────────────────────────
    // Tests should verify behavior through the trait, not specific backends.

    struct InMemoryBackend {
        data: Mutex<Option<Credentials>>,
    }

    impl InMemoryBackend {
        fn new() -> Self {
            Self {
                data: Mutex::new(None),
            }
        }
    }

    impl SecretBackend for InMemoryBackend {
        fn load(&self) -> Result<Option<Credentials>> {
            Ok(self.data.lock().unwrap().clone())
        }
        fn save(&self, creds: &Credentials) -> Result<()> {
            *self.data.lock().unwrap() = Some(creds.clone());
            Ok(())
        }
    }

    fn sample_openai_creds() -> Credentials {
        Credentials {
            active_provider: Some("openai".into()),
            openai: Some(OpenAiCredentials {
                access_token: "sk-test-token".into(),
                refresh_token: "rt-test-refresh".into(),
                expires_at_ms: u64::MAX, // far future
                account_id: "acct-abc".into(),
                id_token: None,
                api_key: None,
            }),
            openai_api_key: None,
            zai: None,
        }
    }

    fn sample_zai_creds() -> Credentials {
        Credentials {
            active_provider: Some("zai".into()),
            openai: None,
            openai_api_key: None,
            zai: Some(ZaiCredentials {
                api_key: "zai-secret-key".into(),
            }),
        }
    }

    // ── SecretBackend contract: any backend must satisfy these ───────────────

    /// Run the full contract suite against any backend.
    fn run_backend_contract(backend: &dyn SecretBackend) {
        // 1. Empty store returns None
        assert!(
            backend.load().unwrap().is_none(),
            "empty store should return None"
        );

        // 2. Save then load returns identical data
        let creds = sample_openai_creds();
        backend.save(&creds).unwrap();
        let loaded = backend
            .load()
            .unwrap()
            .expect("should return Some after save");
        assert_eq!(loaded.active_provider, creds.active_provider);
        let oa = loaded.openai.unwrap();
        assert_eq!(oa.access_token, "sk-test-token");
        assert_eq!(oa.refresh_token, "rt-test-refresh");
        assert_eq!(oa.account_id, "acct-abc");

        // 3. Save overwrites previous data
        let creds2 = sample_zai_creds();
        backend.save(&creds2).unwrap();
        let loaded2 = backend.load().unwrap().unwrap();
        assert_eq!(loaded2.active_provider.as_deref(), Some("zai"));
        assert!(loaded2.openai.is_none(), "previous openai should be gone");
        assert_eq!(loaded2.zai.unwrap().api_key, "zai-secret-key");

        // 4. Multiple providers can coexist in one save
        let both = Credentials {
            active_provider: Some("openai".into()),
            openai: Some(OpenAiCredentials {
                access_token: "tok".into(),
                refresh_token: "ref".into(),
                expires_at_ms: 1000,
                account_id: "acct".into(),
                id_token: None,
                api_key: None,
            }),
            openai_api_key: None,
            zai: Some(ZaiCredentials {
                api_key: "key".into(),
            }),
        };
        backend.save(&both).unwrap();
        let loaded3 = backend.load().unwrap().unwrap();
        assert!(loaded3.openai.is_some());
        assert!(loaded3.zai.is_some());
    }

    #[test]
    fn backend_contract_in_memory() {
        run_backend_contract(&InMemoryBackend::new());
    }

    #[test]
    fn backend_contract_age_file() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("creds.age");
        let backend = AgeFileBackend::with_max_work_factor(
            path,
            SecretString::from("test-passphrase".to_owned()),
            TEST_AGE_MAX_WORK_FACTOR,
        );
        run_backend_contract(&backend);
    }

    // ── Behavioral: secrets must be confidential at rest ────────────────────

    #[test]
    fn secrets_are_not_readable_on_disk() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("creds.age");
        let backend = AgeFileBackend::with_max_work_factor(
            path.clone(),
            SecretString::from("my-passphrase".to_owned()),
            TEST_AGE_MAX_WORK_FACTOR,
        );

        backend.save(&sample_openai_creds()).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        let raw_bytes = std::fs::read(&path).unwrap();
        let raw_lossy = String::from_utf8_lossy(&raw_bytes);

        // None of the secret values should appear in the file
        assert!(!raw.contains("sk-test-token") && !raw_lossy.contains("sk-test-token"));
        assert!(!raw.contains("rt-test-refresh") && !raw_lossy.contains("rt-test-refresh"));
        assert!(!raw.contains("acct-abc") && !raw_lossy.contains("acct-abc"));
    }

    // ── Behavioral: wrong credentials must fail ─────────────────────────────

    #[test]
    fn wrong_passphrase_is_rejected() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("creds.age");

        let writer = AgeFileBackend::with_max_work_factor(
            path.clone(),
            SecretString::from("correct".to_owned()),
            TEST_AGE_MAX_WORK_FACTOR,
        );
        let reader = AgeFileBackend::with_max_work_factor(
            path,
            SecretString::from("incorrect".to_owned()),
            TEST_AGE_MAX_WORK_FACTOR,
        );

        writer.save(&sample_openai_creds()).unwrap();
        let err = reader.load().unwrap_err().to_string();
        assert!(err.contains("wrong passphrase"), "{err}");
        assert!(err.contains("KLEY_PASSPHRASE"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn age_file_backend_writes_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("creds.age");
        let backend = AgeFileBackend::with_max_work_factor(
            path.clone(),
            SecretString::from("correct".to_owned()),
            TEST_AGE_MAX_WORK_FACTOR,
        );

        backend.save(&sample_openai_creds()).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn parse_age_max_work_factor_accepts_empty_as_unset() {
        assert_eq!(parse_age_max_work_factor_from_str("   ").unwrap(), None);
    }

    #[test]
    fn parse_age_max_work_factor_rejects_values_above_supported_max() {
        let err = parse_age_max_work_factor_from_str("21")
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be between 1 and 20"));
    }

    #[test]
    fn map_decrypt_error_does_not_suggest_impossible_override() {
        let err = map_decrypt_error(
            age::DecryptError::ExcessiveWork {
                required: 21,
                target: 20,
            },
            20,
        )
        .to_string();

        assert!(err.contains("exceeds Kley's supported maximum of 20"));
        assert!(!err.contains("KLEY_AGE_MAX_WORK_FACTOR=21"));
    }

    // ── Behavioral: CredentialStore resolves providers correctly ─────────────

    #[test]
    fn store_returns_default_when_empty() {
        let backend = InMemoryBackend::new();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };
        let creds = store.load().unwrap();
        assert!(creds.active_provider.is_none());
        assert!(creds.openai.is_none());
        assert!(creds.openai_api_key.is_none());
        assert!(creds.zai.is_none());
    }

    #[test]
    fn store_roundtrips_through_any_backend() {
        let backend = InMemoryBackend::new();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };

        let creds = sample_openai_creds();
        store.save(&creds).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.active_provider.as_deref(), Some("openai"));
        assert_eq!(loaded.openai.unwrap().access_token, "sk-test-token");
    }

    #[test]
    fn save_openai_oauth_credentials_clears_web_api_key() {
        let backend = InMemoryBackend::new();
        backend
            .save(&Credentials {
                active_provider: Some("openai".into()),
                openai: None,
                openai_api_key: Some(OpenAiApiKeyCredentials {
                    api_key: "sk-web-api-key".into(),
                }),
                zai: None,
            })
            .unwrap();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };

        save_openai_oauth_credentials(
            &store,
            OpenAiCredentials {
                access_token: "oauth-access".into(),
                refresh_token: "oauth-refresh".into(),
                expires_at_ms: u64::MAX,
                account_id: "acct-oauth".into(),
                id_token: Some("oauth-id".into()),
                api_key: Some("sk-oauth".into()),
            },
        )
        .unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.active_provider.as_deref(), Some("openai"));
        assert!(loaded.openai_api_key.is_none());
        assert_eq!(loaded.openai.unwrap().account_id, "acct-oauth");
    }

    // ── Behavioral: resolve_auth produces correct ResolvedAuth ───────────────

    #[tokio::test]
    async fn resolve_auth_openai_with_valid_token() {
        let backend = InMemoryBackend::new();
        backend.save(&sample_openai_creds()).unwrap();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };
        let (emitter, _receiver) = crate::events::event_channel();

        let auth = resolve_auth(&store, &emitter).await.unwrap();
        assert_eq!(auth.provider, "openai");
        // No API key → uses access_token with ChatGPT backend
        assert_eq!(auth.api_key, "sk-test-token");
        assert_eq!(auth.base_url, "https://chatgpt.com/backend-api/codex");
        assert_eq!(auth.account_id.as_deref(), Some("acct-abc"));
    }

    #[tokio::test]
    async fn resolve_auth_openai_with_api_key() {
        let backend = InMemoryBackend::new();
        backend
            .save(&Credentials {
                active_provider: Some("openai".into()),
                openai: Some(OpenAiCredentials {
                    access_token: "raw-token".into(),
                    refresh_token: "rt".into(),
                    expires_at_ms: u64::MAX,
                    account_id: "acct-xyz".into(),
                    id_token: None,
                    api_key: Some("sk-real-api-key".into()),
                }),
                openai_api_key: None,
                zai: None,
            })
            .unwrap();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };
        let (emitter, _receiver) = crate::events::event_channel();

        let auth = resolve_auth(&store, &emitter).await.unwrap();
        assert_eq!(auth.provider, "openai");
        // Has exchanged API key → uses it with standard API URL
        assert_eq!(auth.api_key, "sk-real-api-key");
        assert_eq!(auth.base_url, "https://api.openai.com/v1");
    }

    #[tokio::test]
    async fn resolve_auth_openai_with_stored_web_api_key() {
        let backend = InMemoryBackend::new();
        backend
            .save(&Credentials {
                active_provider: Some("openai".into()),
                openai: None,
                openai_api_key: Some(OpenAiApiKeyCredentials {
                    api_key: "sk-web-ui".into(),
                }),
                zai: None,
            })
            .unwrap();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };
        let (emitter, _receiver) = crate::events::event_channel();

        let auth = resolve_auth(&store, &emitter).await.unwrap();
        assert_eq!(auth.provider, "openai");
        assert_eq!(auth.api_key, "sk-web-ui");
        assert_eq!(auth.base_url, "https://api.openai.com/v1");
        assert!(auth.account_id.is_none());
    }

    #[tokio::test]
    async fn resolve_auth_zai() {
        let backend = InMemoryBackend::new();
        backend.save(&sample_zai_creds()).unwrap();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };
        let (emitter, _receiver) = crate::events::event_channel();

        let auth = resolve_auth(&store, &emitter).await.unwrap();
        assert_eq!(auth.provider, "zai");
        assert_eq!(auth.api_key, "zai-secret-key");
        assert_eq!(auth.base_url, "https://open.bigmodel.cn/api/paas/v4");
        assert!(auth.account_id.is_none());
    }

    #[tokio::test]
    async fn resolve_auth_fails_when_no_provider() {
        let backend = InMemoryBackend::new();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };
        let (emitter, _receiver) = crate::events::event_channel();

        let result = resolve_auth(&store, &emitter).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no active provider"),
            "should mention no active provider, got: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_auth_fails_when_credentials_missing() {
        let backend = InMemoryBackend::new();
        // Active provider set but no credentials for it
        backend
            .save(&Credentials {
                active_provider: Some("openai".into()),
                openai: None,
                openai_api_key: None,
                zai: None,
            })
            .unwrap();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };
        let (emitter, _receiver) = crate::events::event_channel();

        let result = resolve_auth(&store, &emitter).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("credentials missing"),
            "should mention missing credentials, got: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_auth_rejects_unknown_provider() {
        let backend = InMemoryBackend::new();
        backend
            .save(&Credentials {
                active_provider: Some("unknown-provider".into()),
                openai: None,
                openai_api_key: None,
                zai: None,
            })
            .unwrap();
        let store = CredentialStore {
            backend: Box::new(backend),
            backend_name: "test".into(),
        };
        let (emitter, _receiver) = crate::events::event_channel();

        let result = resolve_auth(&store, &emitter).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown provider"),
            "should reject unknown provider, got: {err}"
        );
    }
}
