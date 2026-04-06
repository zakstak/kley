use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::future::Future;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::auth::{
    self, save_openai_oauth_credentials, CredentialStore, Credentials, ZaiCredentials,
};
use crate::runtime::RuntimeManager;
use crate::store::{SharedStore, Store};

#[cfg(feature = "testing")]
use crate::auth::OpenAiCredentials;

use super::protocol::{AuthStateSnapshot, CapacityUsage, PercentUsage, ResourceUsage};

const PENDING_OPENAI_LOGIN_TTL: Duration = Duration::from_secs(600);
const CPU_SAMPLE_WINDOW: Duration = Duration::from_millis(120);
#[cfg(feature = "testing")]
const OPENAI_AUTH_MODE_ENV: &str = "KLEY_WEB_OPENAI_AUTH_MODE";
const WEB_AUTH_AUTO_RESET_ENV: &str = "KLEY_WEB_AUTH_AUTO_RESET";
const LOCAL_WEB_DEFAULT_PASSPHRASE: &str = "kley-dev-passphrase";
#[cfg(feature = "testing")]
const GIB_BYTES: u64 = 1024 * 1024 * 1024;

type WebAuthFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub struct PendingOpenAiLogin {
    pub authorize_url: String,
    pub verifier: String,
    pub state: String,
    pub redirect_uri: String,
    started_at: Instant,
}

impl PendingOpenAiLogin {
    fn new(authorize_url: String, verifier: String, state: String, redirect_uri: String) -> Self {
        Self {
            authorize_url,
            verifier,
            state,
            redirect_uri,
            started_at: Instant::now(),
        }
    }

    fn is_expired(&self) -> bool {
        self.started_at.elapsed() >= PENDING_OPENAI_LOGIN_TTL
    }
}

pub trait WebAuthService: Send + Sync {
    fn summary(&self, pending_openai_login: bool) -> AuthStateSnapshot;
    fn start_openai_login(&self, redirect_uri: &str) -> Result<PendingOpenAiLogin>;
    fn complete_openai_login<'a>(
        &'a self,
        pending: &'a PendingOpenAiLogin,
        callback_input: &'a str,
    ) -> WebAuthFuture<'a>;
    fn login_zai(&self, api_key: &str) -> Result<()>;
}

pub trait WebResourceUsageService: Send + Sync {
    fn summary(&self) -> ResourceUsage;
}

struct LiveWebAuthService;

struct LiveWebResourceUsageService {
    disk_path: PathBuf,
    cpu_sample: Mutex<Option<CpuTimesSample>>,
}

#[derive(Debug, Clone, Copy)]
struct CpuTimesSample {
    total: u64,
    active: u64,
}

impl LiveWebAuthService {
    fn credential_store(&self) -> Result<CredentialStore> {
        CredentialStore::open_noninteractive()
    }

    fn load_credentials(&self, store: &CredentialStore) -> Result<Credentials> {
        match store.load() {
            Ok(credentials) => Ok(credentials),
            Err(error)
                if should_auto_reset_auth_storage()
                    && looks_like_passphrase_mismatch_error(&error) =>
            {
                store.save(&Credentials::default()).with_context(|| {
                    format!(
                        "failed to reset auth storage after passphrase mismatch; disable auto reset with {WEB_AUTH_AUTO_RESET_ENV}=0 to inspect credentials manually"
                    )
                })?;
                store
                    .load()
                    .context("failed to load auth storage after automatic reset")
            }
            Err(error) => Err(error),
        }
    }
}

impl Default for LiveWebResourceUsageService {
    fn default() -> Self {
        Self {
            disk_path: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            cpu_sample: Mutex::new(read_cpu_times_sample().ok()),
        }
    }
}

impl LiveWebResourceUsageService {
    fn snapshot(&self) -> Result<ResourceUsage> {
        Ok(ResourceUsage {
            ram: read_memory_usage()?,
            cpu: PercentUsage {
                percent_used: self.cpu_percent()?,
            },
            disk: read_disk_usage(&self.disk_path)?,
        })
    }

    fn cpu_percent(&self) -> Result<u8> {
        let mut previous = self.cpu_sample.lock().unwrap();
        let prior_sample = match *previous {
            Some(sample) => sample,
            None => {
                let sample = read_cpu_times_sample()?;
                *previous = Some(sample);
                std::thread::sleep(CPU_SAMPLE_WINDOW);
                sample
            }
        };
        let current_sample = read_cpu_times_sample()?;
        *previous = Some(current_sample);

        let total_delta = current_sample.total.saturating_sub(prior_sample.total);
        if total_delta == 0 {
            return Ok(0);
        }

        let active_delta = current_sample
            .active
            .saturating_sub(prior_sample.active)
            .min(total_delta);
        Ok(percent_from_ratio(active_delta, total_delta))
    }
}

impl WebResourceUsageService for LiveWebResourceUsageService {
    fn summary(&self) -> ResourceUsage {
        self.snapshot().unwrap_or_default()
    }
}

fn should_auto_reset_auth_storage() -> bool {
    matches!(
        std::env::var(WEB_AUTH_AUTO_RESET_ENV),
        Ok(value)
            if matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
    )
}

fn ensure_local_web_auth_defaults() {
    if std::env::var_os("KLEY_PASSPHRASE").is_some() {
        return;
    }

    if std::env::var_os("VAULT_ADDR").is_some() && std::env::var_os("VAULT_TOKEN").is_some() {
        return;
    }

    unsafe {
        std::env::set_var("KLEY_PASSPHRASE", LOCAL_WEB_DEFAULT_PASSPHRASE);
    }
}

fn looks_like_passphrase_mismatch_error(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("wrong passphrase")
}

impl WebAuthService for LiveWebAuthService {
    fn summary(&self, pending_openai_login: bool) -> AuthStateSnapshot {
        match self
            .credential_store()
            .and_then(|store| self.load_credentials(&store))
        {
            Ok(credentials) => auth_summary_from_credentials(&credentials, pending_openai_login),
            Err(error) => AuthStateSnapshot {
                storage_available: false,
                storage_error: Some(error.to_string()),
                active_provider: None,
                openai_logged_in: false,
                zai_logged_in: false,
                pending_openai_login,
            },
        }
    }

    fn start_openai_login(&self, redirect_uri: &str) -> Result<PendingOpenAiLogin> {
        let flow = auth::openai::start_login_flow_with_redirect_uri(redirect_uri)?;
        Ok(PendingOpenAiLogin::new(
            flow.authorize_url,
            flow.verifier,
            flow.state,
            flow.redirect_uri,
        ))
    }

    fn complete_openai_login<'a>(
        &'a self,
        pending: &'a PendingOpenAiLogin,
        callback_input: &'a str,
    ) -> WebAuthFuture<'a> {
        Box::pin(async move {
            let credentials = auth::openai::finish_login_flow_with_redirect_uri(
                callback_input,
                &pending.verifier,
                &pending.state,
                &pending.redirect_uri,
            )
            .await?;
            let store = self.credential_store()?;
            save_openai_oauth_credentials(&store, credentials)
        })
    }

    fn login_zai(&self, api_key: &str) -> Result<()> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            anyhow::bail!("API key must not be empty");
        }

        let store = self.credential_store()?;
        let mut credentials = self.load_credentials(&store)?;
        credentials.active_provider = Some("zai".into());
        credentials.zai = Some(ZaiCredentials {
            api_key: api_key.to_string(),
        });
        store.save(&credentials)
    }
}

#[cfg(feature = "testing")]
#[derive(Default)]
pub struct MockWebAuthService {
    credentials: Mutex<Credentials>,
}

#[cfg(feature = "testing")]
impl MockWebAuthService {
    fn mock_openai_credentials() -> OpenAiCredentials {
        OpenAiCredentials {
            access_token: "mock-openai-access-token".into(),
            refresh_token: "mock-openai-refresh-token".into(),
            expires_at_ms: u64::MAX,
            account_id: "acct-web-mock".into(),
            id_token: Some("mock-openai-id-token".into()),
            api_key: Some("sk-web-mock-api-key".into()),
        }
    }
}

#[cfg(feature = "testing")]
impl WebAuthService for MockWebAuthService {
    fn summary(&self, pending_openai_login: bool) -> AuthStateSnapshot {
        let credentials = self.credentials.lock().unwrap().clone();
        auth_summary_from_credentials(&credentials, pending_openai_login)
    }

    fn start_openai_login(&self, redirect_uri: &str) -> Result<PendingOpenAiLogin> {
        let flow = auth::openai::start_login_flow_with_redirect_uri(redirect_uri)?;
        Ok(PendingOpenAiLogin::new(
            "data:text/html,%3Ctitle%3EKley%20OpenAI%20Auth%3C/title%3E%3Cp%3EMock%20OpenAI%20auth%20started.%3C/p%3E".into(),
            flow.verifier,
            flow.state,
            flow.redirect_uri,
        ))
    }

    fn complete_openai_login<'a>(
        &'a self,
        pending: &'a PendingOpenAiLogin,
        callback_input: &'a str,
    ) -> WebAuthFuture<'a> {
        Box::pin(async move {
            let input = callback_input.trim();
            if input.is_empty() {
                anyhow::bail!("missing authorization code");
            }

            if input.starts_with("http") {
                let url = reqwest::Url::parse(input).context("invalid redirect URL pasted")?;
                let state = url
                    .query_pairs()
                    .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
                    .context("Could not find 'state' parameter in the pasted URL")?;
                if state != pending.state {
                    anyhow::bail!("state mismatch");
                }
            }

            let mut credentials = self.credentials.lock().unwrap();
            credentials.active_provider = Some("openai".into());
            credentials.openai = Some(Self::mock_openai_credentials());
            credentials.openai_api_key = None;
            Ok(())
        })
    }

    fn login_zai(&self, api_key: &str) -> Result<()> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            anyhow::bail!("API key must not be empty");
        }

        let mut credentials = self.credentials.lock().unwrap();
        credentials.active_provider = Some("zai".into());
        credentials.zai = Some(ZaiCredentials {
            api_key: api_key.to_string(),
        });
        Ok(())
    }
}

#[cfg(feature = "testing")]
pub struct MockWebResourceUsageService {
    snapshot: Mutex<ResourceUsage>,
}

#[cfg(feature = "testing")]
impl Default for MockWebResourceUsageService {
    fn default() -> Self {
        Self {
            snapshot: Mutex::new(default_mock_resource_usage()),
        }
    }
}

#[cfg(feature = "testing")]
impl WebResourceUsageService for MockWebResourceUsageService {
    fn summary(&self) -> ResourceUsage {
        self.snapshot.lock().unwrap().clone()
    }
}

fn auth_summary_from_credentials(
    credentials: &Credentials,
    pending_openai_login: bool,
) -> AuthStateSnapshot {
    let openai_logged_in = credentials.openai.is_some()
        || credentials
            .openai_api_key
            .as_ref()
            .map(|creds| !creds.api_key.trim().is_empty())
            .unwrap_or(false);
    let zai_logged_in = credentials
        .zai
        .as_ref()
        .map(|creds| !creds.api_key.trim().is_empty())
        .unwrap_or(false);

    AuthStateSnapshot {
        storage_available: true,
        storage_error: None,
        active_provider: credentials.active_provider.clone(),
        openai_logged_in,
        zai_logged_in,
        pending_openai_login,
    }
}

fn read_memory_usage() -> Result<CapacityUsage> {
    let meminfo = fs::read_to_string("/proc/meminfo").context("failed to read /proc/meminfo")?;
    let mut total_kib = None;
    let mut available_kib = None;

    for line in meminfo.lines() {
        if let Some(value) = line.strip_prefix("MemTotal:") {
            total_kib = parse_meminfo_kib(value);
        }
        if let Some(value) = line.strip_prefix("MemAvailable:") {
            available_kib = parse_meminfo_kib(value);
        }
    }

    let total_bytes = total_kib.context("missing MemTotal in /proc/meminfo")? * 1024;
    let available_bytes = available_kib.context("missing MemAvailable in /proc/meminfo")? * 1024;
    let used_bytes = total_bytes.saturating_sub(available_bytes);

    Ok(CapacityUsage {
        used_bytes,
        total_bytes,
        percent_used: percent_from_ratio(used_bytes, total_bytes),
    })
}

fn parse_meminfo_kib(value: &str) -> Option<u64> {
    value.split_whitespace().next()?.parse().ok()
}

fn read_disk_usage(path: &Path) -> Result<CapacityUsage> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .context("disk usage path contains an interior null byte")?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    let status = unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) };
    if status != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to read filesystem usage");
    }
    let stats = unsafe { stats.assume_init() };
    let block_size = if stats.f_frsize > 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    };
    let total_bytes = stats.f_blocks.saturating_mul(block_size);
    let available_bytes = stats.f_bavail.saturating_mul(block_size);
    let used_bytes = total_bytes.saturating_sub(available_bytes);

    Ok(CapacityUsage {
        used_bytes,
        total_bytes,
        percent_used: percent_from_ratio(used_bytes, total_bytes),
    })
}

fn read_cpu_times_sample() -> Result<CpuTimesSample> {
    let stat = fs::read_to_string("/proc/stat").context("failed to read /proc/stat")?;
    let cpu_line = stat.lines().next().context("missing aggregate cpu line")?;
    let mut parts = cpu_line.split_whitespace();
    let label = parts.next().context("missing cpu label")?;
    if label != "cpu" {
        anyhow::bail!("unexpected aggregate cpu label: {label}");
    }

    let values = parts
        .take(8)
        .map(|value| value.parse::<u64>().context("invalid cpu counter"))
        .collect::<Result<Vec<_>>>()?;
    if values.len() < 8 {
        anyhow::bail!("aggregate cpu line missing counters");
    }

    let active = values[0]
        .saturating_add(values[1])
        .saturating_add(values[2])
        .saturating_add(values[5])
        .saturating_add(values[6])
        .saturating_add(values[7]);
    let idle = values[3].saturating_add(values[4]);

    Ok(CpuTimesSample {
        total: active.saturating_add(idle),
        active,
    })
}

fn percent_from_ratio(used: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }

    ((used.saturating_mul(100).saturating_add(total / 2)) / total).min(100) as u8
}

#[cfg(feature = "testing")]
fn default_mock_resource_usage() -> ResourceUsage {
    ResourceUsage {
        ram: CapacityUsage {
            used_bytes: 3 * GIB_BYTES,
            total_bytes: 8 * GIB_BYTES,
            percent_used: 38,
        },
        cpu: PercentUsage { percent_used: 27 },
        disk: CapacityUsage {
            used_bytes: 128 * GIB_BYTES,
            total_bytes: 512 * GIB_BYTES,
            percent_used: 25,
        },
    }
}

fn default_auth_service() -> Arc<dyn WebAuthService> {
    #[cfg(feature = "testing")]
    if matches!(
        std::env::var(OPENAI_AUTH_MODE_ENV),
        Ok(mode) if mode.eq_ignore_ascii_case("mock")
    ) {
        return Arc::new(MockWebAuthService::default());
    }

    Arc::new(LiveWebAuthService)
}

fn default_resource_usage_service() -> Arc<dyn WebResourceUsageService> {
    #[cfg(feature = "testing")]
    {
        return Arc::new(MockWebResourceUsageService::default());
    }

    #[allow(unreachable_code)]
    Arc::new(LiveWebResourceUsageService::default())
}

#[derive(Clone)]
pub struct WebAppState {
    pub store: SharedStore,
    pub runtime_manager: Arc<RuntimeManager>,
    auth_service: Arc<dyn WebAuthService>,
    resource_usage_service: Arc<dyn WebResourceUsageService>,
    pending_openai_logins: Arc<Mutex<HashMap<String, PendingOpenAiLogin>>>,
}

impl WebAppState {
    pub fn new(store: SharedStore) -> Self {
        Self::with_services(
            store,
            default_auth_service(),
            default_resource_usage_service(),
        )
    }

    pub fn with_auth_service(store: SharedStore, auth_service: Arc<dyn WebAuthService>) -> Self {
        Self::with_services(store, auth_service, default_resource_usage_service())
    }

    pub fn with_services(
        store: SharedStore,
        auth_service: Arc<dyn WebAuthService>,
        resource_usage_service: Arc<dyn WebResourceUsageService>,
    ) -> Self {
        let runtime_manager = Arc::new(RuntimeManager::new());
        runtime_manager.bind_shared_store(Arc::clone(&store));
        Self {
            store,
            runtime_manager,
            auth_service,
            resource_usage_service,
            pending_openai_logins: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn from_store(store: Store) -> Self {
        Self::new(Arc::new(Mutex::new(store)))
    }

    pub fn for_web_mode() -> Result<Self> {
        ensure_local_web_auth_defaults();
        Ok(Self::from_store(Store::open()?))
    }

    pub fn auth_summary(&self, controller_id: &str) -> AuthStateSnapshot {
        let pending_openai_login = self.has_pending_openai_login(controller_id);
        self.auth_service.summary(pending_openai_login)
    }

    pub fn resource_usage_summary(&self) -> ResourceUsage {
        self.resource_usage_service.summary()
    }

    pub(crate) fn pending_openai_login(&self, controller_id: &str) -> bool {
        self.has_pending_openai_login(controller_id)
    }

    pub fn start_openai_login(&self, controller_id: &str, redirect_uri: &str) -> Result<String> {
        let pending = self.auth_service.start_openai_login(redirect_uri)?;
        let authorize_url = pending.authorize_url.clone();

        let mut logins = self.pending_openai_logins.lock().unwrap();
        cleanup_expired_logins(&mut logins);
        logins.insert(controller_id.to_string(), pending);
        Ok(authorize_url)
    }

    pub async fn complete_openai_login(
        &self,
        controller_id: &str,
        callback_input: &str,
    ) -> Result<()> {
        let pending = {
            let mut logins = self.pending_openai_logins.lock().unwrap();
            cleanup_expired_logins(&mut logins);
            logins
                .get(controller_id)
                .cloned()
                .context("no OpenAI login is currently pending")?
        };

        self.auth_service
            .complete_openai_login(&pending, callback_input)
            .await?;

        self.clear_openai_login(controller_id);
        Ok(())
    }

    pub async fn complete_openai_login_with_verifier_state(
        &self,
        controller_id: &str,
        callback_input: &str,
        verifier: &str,
        expected_state: &str,
    ) -> Result<()> {
        let pending = {
            let mut logins = self.pending_openai_logins.lock().unwrap();
            cleanup_expired_logins(&mut logins);
            logins
                .get(controller_id)
                .cloned()
                .context("no OpenAI login is currently pending")?
        };

        if pending.verifier != verifier || pending.state != expected_state {
            anyhow::bail!("OpenAI login callback does not match the pending login flow");
        }

        self.auth_service
            .complete_openai_login(&pending, callback_input)
            .await?;
        self.clear_openai_login(controller_id);
        Ok(())
    }

    pub fn login_zai(&self, api_key: &str) -> Result<()> {
        self.auth_service.login_zai(api_key)
    }

    pub fn clear_openai_login(&self, controller_id: &str) {
        let mut logins = self.pending_openai_logins.lock().unwrap();
        logins.remove(controller_id);
    }

    fn has_pending_openai_login(&self, controller_id: &str) -> bool {
        let mut logins = self.pending_openai_logins.lock().unwrap();
        cleanup_expired_logins(&mut logins);
        logins.contains_key(controller_id)
    }
}

fn cleanup_expired_logins(logins: &mut HashMap<String, PendingOpenAiLogin>) {
    logins.retain(|_, pending| !pending.is_expired());
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::auth::SecretBackend;
    use age::secrecy::SecretString;
    use std::ffi::OsStr;
    use std::sync::OnceLock;

    const TEST_AGE_MAX_WORK_FACTOR: u8 = 1;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn set_env<K: AsRef<OsStr>, V: AsRef<OsStr>>(key: K, value: V) {
        unsafe {
            std::env::set_var(key, value);
        }
    }

    fn remove_env<K: AsRef<OsStr>>(key: K) {
        unsafe {
            std::env::remove_var(key);
        }
    }

    fn write_credentials_with_passphrase(config_home: &std::path::Path, passphrase: &str) {
        let kley_config = config_home.join("kley");
        std::fs::create_dir_all(&kley_config).unwrap();
        let backend = crate::auth::AgeFileBackend::with_max_work_factor(
            kley_config.join("credentials.age"),
            SecretString::from(passphrase.to_owned()),
            TEST_AGE_MAX_WORK_FACTOR,
        );
        backend
            .save(&Credentials {
                active_provider: Some("zai".into()),
                openai: None,
                openai_api_key: None,
                zai: Some(ZaiCredentials {
                    api_key: "zai-existing".into(),
                }),
            })
            .unwrap();
    }

    #[test]
    fn live_summary_auto_resets_mismatched_credentials_when_enabled() {
        let _guard = env_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        write_credentials_with_passphrase(temp.path(), "old-pass");

        set_env("XDG_CONFIG_HOME", temp.path());
        set_env("KLEY_PASSPHRASE", "new-pass");
        set_env(
            "KLEY_AGE_MAX_WORK_FACTOR",
            TEST_AGE_MAX_WORK_FACTOR.to_string(),
        );
        set_env(WEB_AUTH_AUTO_RESET_ENV, "1");
        remove_env("VAULT_ADDR");
        remove_env("VAULT_TOKEN");

        let service = LiveWebAuthService;
        let snapshot = service.summary(false);
        assert!(snapshot.storage_available);
        assert!(snapshot.storage_error.is_none());
        assert!(!snapshot.zai_logged_in);
        assert!(snapshot.active_provider.is_none());

        let store = CredentialStore::open_noninteractive().unwrap();
        let credentials = store.load().unwrap();
        assert!(credentials.active_provider.is_none());

        remove_env(WEB_AUTH_AUTO_RESET_ENV);
        remove_env("KLEY_AGE_MAX_WORK_FACTOR");
        remove_env("KLEY_PASSPHRASE");
        remove_env("XDG_CONFIG_HOME");
    }

    #[test]
    fn live_summary_keeps_mismatch_error_when_auto_reset_disabled() {
        let _guard = env_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        write_credentials_with_passphrase(temp.path(), "old-pass");

        set_env("XDG_CONFIG_HOME", temp.path());
        set_env("KLEY_PASSPHRASE", "new-pass");
        set_env(
            "KLEY_AGE_MAX_WORK_FACTOR",
            TEST_AGE_MAX_WORK_FACTOR.to_string(),
        );
        set_env(WEB_AUTH_AUTO_RESET_ENV, "0");
        remove_env("VAULT_ADDR");
        remove_env("VAULT_TOKEN");

        let service = LiveWebAuthService;
        let snapshot = service.summary(false);
        assert!(!snapshot.storage_available);
        assert!(snapshot.storage_error.is_some());

        remove_env(WEB_AUTH_AUTO_RESET_ENV);
        remove_env("KLEY_AGE_MAX_WORK_FACTOR");
        remove_env("KLEY_PASSPHRASE");
        remove_env("XDG_CONFIG_HOME");
    }

    #[test]
    fn local_web_auth_defaults_seed_dev_passphrase_without_vault() {
        let _guard = env_lock().lock().unwrap();

        remove_env("KLEY_PASSPHRASE");
        remove_env("VAULT_ADDR");
        remove_env("VAULT_TOKEN");

        ensure_local_web_auth_defaults();

        assert_eq!(
            std::env::var("KLEY_PASSPHRASE").unwrap(),
            LOCAL_WEB_DEFAULT_PASSPHRASE
        );

        remove_env("KLEY_PASSPHRASE");
    }

    #[test]
    fn local_web_auth_defaults_do_not_override_explicit_auth_env() {
        let _guard = env_lock().lock().unwrap();

        set_env("KLEY_PASSPHRASE", "custom-passphrase");
        remove_env("VAULT_ADDR");
        remove_env("VAULT_TOKEN");

        ensure_local_web_auth_defaults();

        assert_eq!(
            std::env::var("KLEY_PASSPHRASE").unwrap(),
            "custom-passphrase"
        );

        remove_env("KLEY_PASSPHRASE");

        set_env("VAULT_ADDR", "http://vault.example");
        set_env("VAULT_TOKEN", "vault-token");

        ensure_local_web_auth_defaults();

        assert!(std::env::var("KLEY_PASSPHRASE").is_err());

        remove_env("VAULT_ADDR");
        remove_env("VAULT_TOKEN");
    }
}
