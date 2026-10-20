//! Credential lifecycle: one authority, single-flight refresh, optional
//! durable persistence, and a fail-closed health reading.
//!
//! A credential is a value plus its validity window. [`TokenManager`] owns
//! that value behind one lock and one refresh gate; callers lease it through
//! [`TokenManager::lease`], which re-checks the state after acquiring the gate
//! so N concurrent callers perform one refresh. Reads that only need the
//! current credential use [`TokenManager::current`] and refuse an expired
//! value rather than serving it.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use common_core::io::write_atomic;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::tastytrade::TransportError;

/// A secret credential value. `Debug` is redacted by construction.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential(String);

impl Credential {
    /// Wrap one secret value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the secret for an authenticated request.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential(<redacted>)")
    }
}

/// One issued credential with its validity window and generation.
#[derive(Clone, Debug)]
pub struct CredentialState {
    /// The bearer/session secret.
    pub credential: Credential,
    /// When this credential was issued.
    pub issued_at: DateTime<Utc>,
    /// When it stops being valid; `None` means no declared expiry.
    pub expires_at: Option<DateTime<Utc>>,
    /// Optional refresh credential (OAuth refresh token).
    pub refresh: Option<Credential>,
    /// Monotonic generation for lease stamping.
    pub generation: u64,
}

impl CredentialState {
    /// Build a state at `issued_at` with `ttl` seconds.
    #[must_use]
    pub fn issued(
        credential: Credential,
        issued_at: DateTime<Utc>,
        ttl: Option<Duration>,
        generation: u64,
    ) -> Self {
        Self {
            credential,
            issued_at,
            expires_at: ttl.map(|ttl| {
                issued_at
                    + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::zero())
            }),
            refresh: None,
            generation,
        }
    }

    /// Whether the credential is expired at `now`.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|expires| now >= expires)
    }

    /// Whether the credential will expire before `margin`.
    #[must_use]
    pub fn expires_within(&self, now: DateTime<Utc>, margin: Duration) -> bool {
        self.expires_at.is_some_and(|expires| {
            expires
                <= now
                    + chrono::Duration::from_std(margin)
                        .unwrap_or_else(|_| chrono::Duration::zero())
        })
    }
}

/// A generation-stamped read of the current credential.
#[derive(Clone, Debug)]
pub struct CredentialLease {
    state: CredentialState,
}

impl CredentialLease {
    /// Build a lease over one state.
    #[must_use]
    pub fn new(state: CredentialState) -> Self {
        Self { state }
    }

    /// The secret for one authenticated call.
    #[must_use]
    pub fn credential(&self) -> &Credential {
        &self.state.credential
    }

    /// Generation this lease was stamped with.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.state.generation
    }

    /// Expiry of this lease's credential.
    #[must_use]
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.state.expires_at
    }

    /// The state behind the lease.
    #[must_use]
    pub fn state(&self) -> &CredentialState {
        &self.state
    }
}

/// How a credential is acquired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    /// Username/password session login.
    SessionLogin,
    /// OAuth refresh-token exchange.
    OAuthRefresh,
    /// Gateway-issued session.
    GatewaySession,
}

/// Credential acquisition failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// The venue refused: retrying cannot authenticate.
    #[error("terminal auth failure: {0}")]
    Terminal(String),
    /// A retryable transport/venue failure.
    #[error("transient auth failure: {0}")]
    Transient(String),
    /// The response or stored state could not be parsed.
    #[error("malformed auth material: {0}")]
    Malformed(String),
    /// No credential has been acquired.
    #[error("credential is missing")]
    Missing,
    /// The credential expired and could not be refreshed.
    #[error("credential expired")]
    Expired,
}

impl AuthError {
    /// Whether another attempt may succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    /// Map a transport failure into the auth taxonomy.
    #[must_use]
    pub fn from_transport(error: &TransportError) -> Self {
        match error {
            TransportError::Rest(reason) | TransportError::Transient(reason) => {
                Self::Transient(reason.clone())
            }
            TransportError::Terminal(reason) => Self::Terminal(reason.clone()),
        }
    }
}

/// Credential tunables. The early-refresh margin is the one correctness
/// threshold here: a credential inside it must be renewed before use.
#[derive(Debug, Clone, bon::Builder, Serialize, Deserialize, FieldAccess, Describable)]
#[builder(start_fn = new)]
pub struct AuthConfig {
    /// Refresh when the credential expires inside this many seconds.
    #[field(desc = "Early-refresh margin in seconds", min = 0, max = 86_400)]
    #[builder(default = 60)]
    #[serde(default = "default_refresh_early")]
    pub refresh_early_secs: u64,
    /// Attempts per refresh (first try plus retries).
    #[field(desc = "Refresh attempts", min = 1, max = 32)]
    #[builder(default = 3)]
    #[serde(default = "default_attempts")]
    pub max_attempts: u32,
    /// Backoff base in milliseconds.
    #[field(desc = "Refresh backoff base in milliseconds", min = 1, max = 60_000)]
    #[builder(default = 200)]
    #[serde(default = "default_backoff")]
    pub backoff_base_ms: u64,
    /// Backoff jitter percent.
    #[field(desc = "Refresh backoff jitter percent", min = 0, max = 100)]
    #[builder(default = 25)]
    #[serde(default = "default_jitter")]
    pub backoff_jitter_pct: u32,
    /// Optional durable credential-store path.
    #[field(desc = "Credential store path (empty disables persistence)")]
    #[serde(default)]
    pub store_path: Option<String>,
    /// Whether a configured store is written and read.
    #[field(desc = "Persist credentials")]
    #[builder(default = true)]
    #[serde(default = "default_persist")]
    pub persist: bool,
    /// Minimum interval between automatic refreshes, in seconds (a cost
    /// bound, never a correctness threshold). A lease inside the interval
    /// fails fast instead of re-dialing; a forced `refresh()` bypasses it.
    #[field(
        desc = "Minimum automatic-refresh interval in seconds",
        min = 0,
        max = 86_400
    )]
    #[builder(default = 5)]
    #[serde(default = "default_min_interval")]
    pub min_refresh_interval_secs: u64,
    /// Fallback credential lifetime in seconds when the venue declares none.
    #[field(
        desc = "Fallback credential lifetime in seconds",
        min = 0,
        max = 604_800
    )]
    #[builder(default = 3_600)]
    #[serde(default = "default_ttl")]
    pub ttl_secs: u64,
}

fn default_refresh_early() -> u64 {
    60
}
fn default_attempts() -> u32 {
    3
}
fn default_backoff() -> u64 {
    200
}
fn default_jitter() -> u32 {
    25
}
fn default_persist() -> bool {
    true
}
fn default_min_interval() -> u64 {
    5
}
fn default_ttl() -> u64 {
    3_600
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl AuthConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_attempts < 1 {
            return Err("max_attempts must be at least 1".to_owned());
        }
        if self.backoff_base_ms < 1 {
            return Err("backoff_base_ms must be at least 1".to_owned());
        }
        if self.backoff_jitter_pct > 100 {
            return Err("backoff_jitter_pct must be at most 100".to_owned());
        }
        if let Some(path) = &self.store_path {
            if self.persist && path.trim().is_empty() {
                return Err("store_path must not be blank when persistence is on".to_owned());
            }
        }
        Ok(())
    }
}

impl protopyne_types::Validate for AuthConfig {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        AuthConfig::validate(self)
    }
}

impl WorkUnit for AuthConfig {
    fn name(&self) -> &str {
        "auth_config"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        protopyne_types::check_for_execute(self, "auth config invalid")?;
        Ok(protopyne_types::valid_output("auth config valid"))
    }
}

impl_component!(AuthConfig);

/// Published credential health: validity and refresh degradation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuthHealth {
    /// Whether the current credential is usable.
    pub valid: bool,
    /// Current generation, if any.
    pub generation: Option<u64>,
    /// Expiry, if declared.
    pub expires_at: Option<DateTime<Utc>>,
    /// Why the last refresh failed, if it did.
    pub degraded_reason: Option<String>,
}

impl AuthHealth {
    /// A healthy reading for one leased state.
    #[must_use]
    pub fn valid(state: &CredentialState) -> Self {
        Self {
            valid: true,
            generation: Some(state.generation),
            expires_at: state.expires_at,
            degraded_reason: None,
        }
    }

    /// A degraded reading after a failed refresh.
    #[must_use]
    pub fn degraded(reason: impl Into<String>, generation: Option<u64>) -> Self {
        Self {
            valid: false,
            generation,
            expires_at: None,
            degraded_reason: Some(reason.into()),
        }
    }
}

/// Boxed future for object-safe credential acquisition.
pub type AuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Acquires one credential. Implementations map transport failures into
/// [`AuthError`]; the manager owns retry and publication.
pub trait AuthDriver: Send + Sync {
    /// How this driver acquires.
    fn method(&self) -> AuthMethod;
    /// Acquire a fresh credential, optionally using `previous` refresh data.
    fn acquire<'a>(
        &'a self,
        previous: Option<&'a CredentialState>,
    ) -> AuthFuture<'a, Result<CredentialState, AuthError>>;
}

/// Tastytrade username/password session driver. Credentials are injected
/// through the shared login slot so the venue can construct the manager
/// before the operator logs in.
pub struct TastytradeSessionDriver {
    base_url: String,
    net: Arc<fluent_concurrency::io::net::NetCapability>,
    credentials: Arc<Mutex<Option<(String, String)>>>,
    ttl_secs: u64,
}

impl TastytradeSessionDriver {
    /// Build a driver over the venue base URL and login slot.
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        net: Arc<fluent_concurrency::io::net::NetCapability>,
        credentials: Arc<Mutex<Option<(String, String)>>>,
        ttl_secs: u64,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            net,
            credentials,
            ttl_secs,
        }
    }

    /// Install the credentials the next acquisition will use.
    pub fn set_credentials(&self, username: impl Into<String>, password: impl Into<String>) {
        *common_core::sync::lock(&self.credentials) = Some((username.into(), password.into()));
    }

    /// Drop the credentials and the acquisition source.
    pub fn clear_credentials(&self) {
        *common_core::sync::lock(&self.credentials) = None;
    }
}

impl AuthDriver for TastytradeSessionDriver {
    fn method(&self) -> AuthMethod {
        AuthMethod::SessionLogin
    }

    fn acquire<'a>(
        &'a self,
        _previous: Option<&'a CredentialState>,
    ) -> AuthFuture<'a, Result<CredentialState, AuthError>> {
        Box::pin(async move {
            if self.base_url.is_empty() {
                return Err(AuthError::Terminal(
                    "tastytrade base URL unconfigured: refusing to dial".to_owned(),
                ));
            }
            let credentials = common_core::sync::lock(&self.credentials).clone();
            let Some((username, password)) = credentials else {
                return Err(AuthError::Missing);
            };
            if username.is_empty() || password.is_empty() {
                return Err(AuthError::Terminal(
                    "tastytrade credentials are empty".to_owned(),
                ));
            }
            let url = format!("{}/sessions", self.base_url.trim_end_matches('/'));
            let body = serde_json::json!({ "login": username, "password": password }).to_string();
            let raw = self
                .net
                .http_post(&url, &body)
                .await
                .map_err(|error| AuthError::Transient(error.to_string()))?;
            let token = parse_session_token(&raw)?;
            let issued_at = Utc::now();
            let ttl = (self.ttl_secs > 0).then(|| Duration::from_secs(self.ttl_secs));
            Ok(CredentialState::issued(
                Credential::new(token),
                issued_at,
                ttl,
                0,
            ))
        })
    }
}

fn parse_session_token(raw: &str) -> Result<String, AuthError> {
    let wire: serde_json::Value = serde_json::from_str(raw)
        .map_err(|error| AuthError::Transient(format!("response is not JSON: {error}")))?;
    if let Some(failure) = crate::tastytrade::envelope_error(&wire) {
        return Err(AuthError::from_transport(&failure));
    }
    wire.get("token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AuthError::Transient("session wire carries no token".to_owned()))
}

/// Object-safe durable credential storage.
pub trait CredentialStore: Send + Sync {
    /// Load the last persisted state.
    fn load(&self) -> Result<Option<CredentialState>, AuthError>;
    /// Persist one state atomically.
    fn save(&self, state: &CredentialState) -> Result<(), AuthError>;
    /// Remove the persisted state.
    fn clear(&self) -> Result<(), AuthError>;
}

/// In-memory credential store.
#[derive(Debug, Default)]
pub struct MemoryCredentialStore {
    state: Mutex<Option<CredentialState>>,
}

impl MemoryCredentialStore {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn load(&self) -> Result<Option<CredentialState>, AuthError> {
        Ok(common_core::sync::lock(&self.state).clone())
    }
    fn save(&self, state: &CredentialState) -> Result<(), AuthError> {
        *common_core::sync::lock(&self.state) = Some(state.clone());
        Ok(())
    }
    fn clear(&self) -> Result<(), AuthError> {
        *common_core::sync::lock(&self.state) = None;
        Ok(())
    }
}

/// Opt-in file store: one redacted-at-rest? No — the file necessarily holds
/// the secret; it is written atomically, owner-only where supported, and
/// never debug-printed.
#[derive(Debug, Clone)]
pub struct FileCredentialStore {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct PersistedCredential {
    credential: String,
    issued_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    refresh: Option<String>,
    generation: u64,
}

impl FileCredentialStore {
    /// Bind a store to `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    /// The file this store owns.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl CredentialStore for FileCredentialStore {
    fn load(&self) -> Result<Option<CredentialState>, AuthError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AuthError::Transient(error.to_string())),
        };
        let raw: PersistedCredential = serde_json::from_str(&text).map_err(|error| {
            AuthError::Malformed(format!("credential store is not valid: {error}"))
        })?;
        if raw.credential.is_empty() {
            return Err(AuthError::Malformed(
                "credential store holds an empty credential".to_owned(),
            ));
        }
        Ok(Some(CredentialState {
            credential: Credential::new(raw.credential),
            issued_at: raw.issued_at,
            expires_at: raw.expires_at,
            refresh: raw.refresh.map(Credential::new),
            generation: raw.generation,
        }))
    }

    fn save(&self, state: &CredentialState) -> Result<(), AuthError> {
        let raw = PersistedCredential {
            credential: state.credential.expose().to_owned(),
            issued_at: state.issued_at,
            expires_at: state.expires_at,
            refresh: state.refresh.as_ref().map(|c| c.expose().to_owned()),
            generation: state.generation,
        };
        let bytes = serde_json::to_vec(&raw)
            .map_err(|error| AuthError::Malformed(format!("credential encode failed: {error}")))?;
        write_atomic(&self.path, &bytes).map_err(|error| {
            AuthError::Transient(format!("credential store write failed: {error}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn clear(&self) -> Result<(), AuthError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AuthError::Transient(error.to_string())),
        }
    }
}

/// One credential authority: single-flight refresh and generation-stamped
/// leases.
pub struct TokenManager {
    config: AuthConfig,
    driver: Arc<dyn AuthDriver>,
    store: Option<Arc<dyn CredentialStore>>,
    state: RwLock<Option<CredentialState>>,
    gate: tokio::sync::Mutex<()>,
    health: RwLock<AuthHealth>,
    published: Notify,
    /// Last automatic refresh attempt, wall-clock. Bounds how often the
    /// lease path dials a failing venue; forced `refresh()` bypasses it.
    last_attempt: Mutex<Option<DateTime<Utc>>>,
}

impl fmt::Debug for TokenManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenManager")
            .field("method", &self.driver.method())
            .field("generation", &self.current_generation())
            .field("health", &self.health())
            .finish_non_exhaustive()
    }
}

impl TokenManager {
    /// Build a manager. A persisted state is loaded when a store is supplied;
    /// an unreadable store refuses rather than silently starting empty.
    pub fn new(
        config: AuthConfig,
        driver: Arc<dyn AuthDriver>,
        store: Option<Arc<dyn CredentialStore>>,
    ) -> Result<Self, AuthError> {
        config.validate().map_err(AuthError::Malformed)?;
        let loaded = match &store {
            Some(store) if config.persist => store.load()?,
            _ => None,
        };
        let health = loaded
            .as_ref()
            .filter(|state| !state.is_expired(Utc::now()))
            .map_or_else(
                || AuthHealth::degraded("credential is missing", None),
                AuthHealth::valid,
            );
        Ok(Self {
            config,
            driver,
            store,
            state: RwLock::new(loaded),
            gate: tokio::sync::Mutex::new(()),
            health: RwLock::new(health),
            published: Notify::new(),
            last_attempt: Mutex::new(None),
        })
    }

    /// Whether a refresh is due: no usable credential is held, or the held
    /// one expires inside the early-refresh window. The background refresher
    /// polls this; the lease path answers it inline.
    #[must_use]
    pub fn refresh_due(&self) -> bool {
        self.lease_if_fresh().is_none()
    }

    /// The configured policy.
    #[must_use]
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// Current generation, if any.
    #[must_use]
    pub fn current_generation(&self) -> Option<u64> {
        common_core::sync::lock_read(&self.state)
            .as_ref()
            .map(|state| state.generation)
    }

    /// The last published health reading.
    #[must_use]
    pub fn health(&self) -> AuthHealth {
        common_core::sync::lock_read(&self.health).clone()
    }

    /// Wait for a publication after the given generation. Used by refresh
    /// workers that only observe; never by the trade path.
    pub async fn changed_after(&self, generation: Option<u64>) {
        loop {
            if self.current_generation() != generation {
                return;
            }
            self.published.notified().await;
        }
    }

    /// Current unexpired credential, if one is held.
    #[must_use]
    pub fn current(&self) -> Option<CredentialLease> {
        let state = common_core::sync::lock_read(&self.state);
        let now = Utc::now();
        state
            .as_ref()
            .filter(|state| !state.is_expired(now))
            .map(|state| CredentialLease::new(state.clone()))
    }

    /// Whether a usable credential is held.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.current().is_some()
    }

    /// Lease a usable credential, refreshing once across concurrent callers
    /// when it is missing, expired, or inside the early-refresh window.
    /// Automatic refreshes obey the minimum interval: a lease inside it
    /// fails fast with the held state (or `Expired`) instead of dialing a
    /// venue that just refused.
    pub async fn lease(&self) -> Result<CredentialLease, AuthError> {
        if let Some(lease) = self.lease_if_fresh() {
            return Ok(lease);
        }
        let _guard = self.gate.lock().await;
        if let Some(lease) = self.lease_if_fresh() {
            return Ok(lease);
        }
        self.refresh_locked(true).await
    }

    /// Force one refresh under the single-flight gate. Forced means forced:
    /// the minimum-interval bound applies to automatic (`lease`) refreshes
    /// only, so an operator's explicit retry always dials.
    pub async fn refresh(&self) -> Result<CredentialLease, AuthError> {
        let _guard = self.gate.lock().await;
        self.refresh_locked(false).await
    }

    /// Install a credential directly (tests and gateway flows). The next
    /// refresh increments the generation.
    pub fn install(&self, state: CredentialState) -> Result<(), AuthError> {
        if state.credential.is_empty() {
            return Err(AuthError::Missing);
        }
        if let Some(store) = &self.store {
            if self.config.persist {
                store.save(&state)?;
            }
        }
        let health = if state.is_expired(Utc::now()) {
            AuthHealth::degraded("credential expired", Some(state.generation))
        } else {
            AuthHealth::valid(&state)
        };
        *common_core::sync::lock_write(&self.state) = Some(state);
        self.publish(health);
        Ok(())
    }

    /// Clear the credential and its persisted copy.
    pub fn clear(&self) -> Result<(), AuthError> {
        if let Some(store) = &self.store {
            if self.config.persist {
                store.clear()?;
            }
        }
        *common_core::sync::lock_write(&self.state) = None;
        self.publish(AuthHealth::degraded("credential is missing", None));
        Ok(())
    }

    fn lease_if_fresh(&self) -> Option<CredentialLease> {
        let state = common_core::sync::lock_read(&self.state);
        let now = Utc::now();
        let state = state.as_ref()?;
        let margin = Duration::from_secs(self.config.refresh_early_secs);
        if state.is_expired(now) || state.expires_within(now, margin) {
            return None;
        }
        Some(CredentialLease::new(state.clone()))
    }

    async fn refresh_locked(&self, rate_limit: bool) -> Result<CredentialLease, AuthError> {
        if rate_limit {
            let interval = chrono::Duration::from_std(Duration::from_secs(
                self.config.min_refresh_interval_secs,
            ))
            .unwrap_or_else(|_| chrono::Duration::zero());
            let now = Utc::now();
            let mut last = common_core::sync::lock(&self.last_attempt);
            if let Some(previous) = *last {
                if now - previous < interval {
                    return self.current().ok_or(AuthError::Expired);
                }
            }
            *last = Some(now);
        }
        let previous = common_core::sync::lock_read(&self.state).clone();
        let attempts = self.config.max_attempts.max(1);
        let result = common_core::retry::retry_async(
            attempts,
            self.config.backoff_base_ms,
            self.config.backoff_jitter_pct,
            AuthError::is_retryable,
            || self.driver.acquire(previous.as_ref()),
        )
        .await;
        match result {
            Ok(mut state) => {
                if state.is_expired(Utc::now()) {
                    let health = AuthHealth::degraded(
                        "refreshed credential already expired",
                        self.current_generation(),
                    );
                    self.publish(health);
                    return Err(AuthError::Expired);
                }
                let generation = previous
                    .as_ref()
                    .map_or(1, |previous| previous.generation.saturating_add(1));
                state.generation = generation;
                if state.credential.is_empty() {
                    return Err(AuthError::Malformed(
                        "auth driver returned an empty credential".to_owned(),
                    ));
                }
                if let Some(store) = &self.store {
                    if self.config.persist {
                        store.save(&state)?;
                    }
                }
                let health = AuthHealth::valid(&state);
                *common_core::sync::lock_write(&self.state) = Some(state);
                self.publish(health);
                let lease = self.current().ok_or(AuthError::Expired)?;
                Ok(lease)
            }
            Err(error) => {
                let health = AuthHealth::degraded(error.to_string(), self.current_generation());
                self.publish(health);
                Err(error)
            }
        }
    }

    fn publish(&self, health: AuthHealth) {
        *common_core::sync::lock_write(&self.health) = health;
        self.published.notify_waiters();
    }

    /// Spawn the periodic refresher under `scope`, abortable through `abort`.
    /// It only refreshes credentials that are due; a failure is published and
    /// retried on the next cadence, never spun. The abort preempts both the
    /// cadence sleep and an in-flight refresh: dropping the lease future drops
    /// its gate guard, so nothing publishes after the abort.
    pub fn spawn_refresher(
        self: &Arc<Self>,
        scope: &mut fluent_concurrency::scope::Scope,
        abort: StreamAbort,
        cadence: Duration,
    ) -> tokio::task::AbortHandle {
        let manager = Arc::clone(self);
        scope.spawn(async move {
            loop {
                tokio::select! {
                    () = abort.cancelled() => return,
                    () = tokio::time::sleep(cadence) => {}
                }
                if !manager.refresh_due() {
                    continue;
                }
                tokio::select! {
                    () = abort.cancelled() => return,
                    _ = manager.lease() => {}
                }
            }
        })
    }
}

/// Publication handle for gate readers: a manager plus its last reading.
#[derive(Clone, Debug)]
pub struct AuthHandle {
    manager: Arc<TokenManager>,
}

impl AuthHandle {
    /// Wrap one manager.
    #[must_use]
    pub fn new(manager: Arc<TokenManager>) -> Self {
        Self { manager }
    }

    /// The underlying manager.
    #[must_use]
    pub fn manager(&self) -> &Arc<TokenManager> {
        &self.manager
    }

    /// Current producer-confidence reading.
    #[must_use]
    pub fn health(&self) -> AuthHealth {
        self.manager.health()
    }

    /// Whether entries may flow: an unexpired credential exists.
    #[must_use]
    pub fn allows_entries(&self) -> bool {
        self.manager.current().is_some()
    }

    /// Refusal reason when entries are disarmed.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        if self.allows_entries() {
            None
        } else {
            let health = self.health();
            Some(
                health
                    .degraded_reason
                    .unwrap_or_else(|| "credential unavailable: entries disarmed".to_owned()),
            )
        }
    }
}

/// One credential authority published for gate readers.
pub trait CredentialSource: Send + Sync {
    /// The current publication handle.
    fn auth_handle(&self) -> AuthHandle;
}

impl CredentialSource for Arc<TokenManager> {
    fn auth_handle(&self) -> AuthHandle {
        AuthHandle::new(Arc::clone(self))
    }
}

#[cfg(test)]
#[path = "../tests/auth.rs"]
mod tests;
