//! GitHub App device flow and OS keychain storage.
//!
//! Starlet is registered as a GitHub App rather than an OAuth App, so the token
//! it receives is scoped by the App's installation permissions instead of by
//! classic OAuth scopes. The device flow is used because a desktop application
//! cannot keep a client secret and should not embed a browser.
//!
//! The token never touches SQLite, the config file, or a log line. It lives in
//! the OS keychain and in one `String` in memory.

use std::time::Duration;

use serde::Deserialize;

use crate::{Result, SyncError};

/// Keychain service name. Shared with the AI provider keys, which use a
/// different account within the same service.
pub const KEYCHAIN_SERVICE: &str = "dev.starlet.starlet";
const KEYCHAIN_ACCOUNT: &str = "github-token";

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// The GitHub App client id.
///
/// Baked in at build time from `STARLET_GITHUB_CLIENT_ID`, with a runtime
/// environment override so a user can point a self-registered App at their own
/// build without recompiling. See the README for registration steps.
pub fn client_id() -> Option<String> {
    if let Ok(id) = std::env::var("STARLET_GITHUB_CLIENT_ID")
        && !id.trim().is_empty()
    {
        return Some(id);
    }
    option_env!("STARLET_GITHUB_CLIENT_ID")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// What the user has to do to finish signing in.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceGrant {
    /// Opaque handle Starlet polls with. Not shown to the user.
    pub device_code: String,
    /// The short code the user types into GitHub.
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds until `device_code` stops being accepted.
    pub expires_in: u64,
    /// Minimum seconds between polls. GitHub raises this with `slow_down`.
    pub interval: u64,
}

impl DeviceGrant {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.interval.max(1))
    }

    pub fn expires_in(&self) -> Duration {
        Duration::from_secs(self.expires_in)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

/// One poll of the access-token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// The user has not finished authorising yet.
    Pending,
    /// GitHub asked us to back off; the new minimum interval is attached.
    SlowDown(Duration),
    Authorized(String),
}

/// Talks to `github.com` (not the API host) for the two device-flow endpoints.
#[derive(Clone)]
pub struct DeviceFlow {
    http: reqwest::Client,
    client_id: String,
    device_code_url: String,
    access_token_url: String,
}

impl std::fmt::Debug for DeviceFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceFlow")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl DeviceFlow {
    pub fn new(client_id: impl Into<String>) -> Result<Self> {
        Ok(Self {
            http: crate::client::user_agent_client()?,
            client_id: client_id.into(),
            device_code_url: DEVICE_CODE_URL.to_string(),
            access_token_url: ACCESS_TOKEN_URL.to_string(),
        })
    }

    /// Point both endpoints at another origin. Used by the integration tests.
    pub fn with_base_url(mut self, base: &str) -> Self {
        let base = base.trim_end_matches('/');
        self.device_code_url = format!("{base}/login/device/code");
        self.access_token_url = format!("{base}/login/oauth/access_token");
        self
    }

    /// Start a sign-in. The returned code is what the dialog displays.
    pub async fn request_code(&self) -> Result<DeviceGrant> {
        let response = self
            .http
            .post(&self.device_code_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&serde_json::json!({ "client_id": self.client_id }))
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(SyncError::Auth(format!(
                "device code request failed: {status}"
            )));
        }
        serde_json::from_str(&body).map_err(|_| {
            // The body can be a form-encoded error when the client id is wrong.
            SyncError::Auth(format!(
                "unexpected device code response: {}",
                truncate(&body)
            ))
        })
    }

    /// Poll once. The caller owns the sleep so it can also honour cancellation.
    pub async fn poll_once(&self, device_code: &str) -> Result<PollOutcome> {
        let response = self
            .http
            .post(&self.access_token_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&serde_json::json!({
                "client_id": self.client_id,
                "device_code": device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            }))
            .send()
            .await?;

        let body = response.text().await?;
        let parsed: TokenResponse = serde_json::from_str(&body).map_err(|_| {
            SyncError::Auth(format!("unexpected token response: {}", truncate(&body)))
        })?;

        if let Some(token) = parsed.access_token {
            return Ok(PollOutcome::Authorized(token));
        }
        match parsed.error.as_deref() {
            Some("authorization_pending") => Ok(PollOutcome::Pending),
            Some("slow_down") => Ok(PollOutcome::SlowDown(Duration::from_secs(
                parsed.interval.unwrap_or(10).max(1),
            ))),
            Some("expired_token") => Err(SyncError::Auth(
                "the sign-in code expired; start again".to_string(),
            )),
            Some("access_denied") => Err(SyncError::Auth("sign-in was declined".to_string())),
            Some(other) => Err(SyncError::Auth(
                parsed
                    .error_description
                    .unwrap_or_else(|| other.to_string()),
            )),
            None => Err(SyncError::Auth(
                "token response had no token and no error".into(),
            )),
        }
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(200).collect()
}

/// The OS keychain entry holding the GitHub token.
///
/// Every method is blocking: the platform APIs behind `keyring` are
/// synchronous, and callers run them off the UI thread.
pub struct TokenStore;

impl TokenStore {
    fn entry() -> Result<keyring::Entry> {
        keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
            .map_err(|e| SyncError::Keychain(e.to_string()))
    }

    /// The stored token, or `None` when the user has never signed in.
    ///
    /// A keychain that is locked or unavailable also reads as `None`: the app
    /// then behaves exactly as it does when signed out, which is the only
    /// useful response.
    pub fn load() -> Option<String> {
        match Self::entry().and_then(|e| {
            e.get_password()
                .map_err(|err| SyncError::Keychain(err.to_string()))
        }) {
            Ok(token) if !token.is_empty() => Some(token),
            Ok(_) => None,
            Err(err) => {
                tracing::debug!("no github token in keychain: {err}");
                None
            }
        }
    }

    pub fn save(token: &str) -> Result<()> {
        Self::entry()?
            .set_password(token)
            .map_err(|e| SyncError::Keychain(e.to_string()))
    }

    /// Sign out. Succeeds when there was nothing to remove.
    pub fn clear() -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SyncError::Keychain(e.to_string())),
        }
    }
}

/// Keychain storage for BYOK provider keys, one account per provider.
pub struct ProviderKeyStore;

impl ProviderKeyStore {
    fn entry(provider: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYCHAIN_SERVICE, &format!("ai:{provider}"))
            .map_err(|e| SyncError::Keychain(e.to_string()))
    }

    pub fn load(provider: &str) -> Option<String> {
        Self::entry(provider)
            .ok()?
            .get_password()
            .ok()
            .filter(|k| !k.is_empty())
    }

    pub fn save(provider: &str, key: &str) -> Result<()> {
        Self::entry(provider)?
            .set_password(key)
            .map_err(|e| SyncError::Keychain(e.to_string()))
    }

    pub fn clear(provider: &str) -> Result<()> {
        match Self::entry(provider)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SyncError::Keychain(e.to_string())),
        }
    }
}
