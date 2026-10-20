//! Credential lifecycle tests: single-flight, expiry, retry taxonomy,
//! persistence/restart, corruption refusal, and redaction.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;

use crate::auth::{
    AuthConfig, AuthDriver, AuthError, AuthFuture, AuthMethod, Credential, CredentialState,
    CredentialStore, FileCredentialStore, MemoryCredentialStore, TokenManager,
};
use crate::command::TradingState;
use crate::reading::{GateReading, GateVerdict};

struct FakeDriver {
    calls: Arc<AtomicU32>,
    outcome: Mutex<Result<(), AuthError>>,
    custom: Mutex<Option<CredentialState>>,
    delay: Duration,
}

impl FakeDriver {
    fn new(outcome: Result<(), AuthError>) -> Arc<Self> {
        Arc::new(Self {
            calls: Arc::new(AtomicU32::new(0)),
            outcome: Mutex::new(outcome),
            custom: Mutex::new(None),
            delay: Duration::from_millis(5),
        })
    }

    fn with_state(state: CredentialState) -> Arc<Self> {
        let driver = Self::new(Ok(()));
        *common_core::sync::lock(&driver.custom) = Some(state);
        driver
    }
}

impl AuthDriver for FakeDriver {
    fn method(&self) -> AuthMethod {
        AuthMethod::SessionLogin
    }

    fn acquire<'a>(
        &'a self,
        _previous: Option<&'a CredentialState>,
    ) -> AuthFuture<'a, Result<CredentialState, AuthError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            if let Some(state) = common_core::sync::lock(&self.custom).clone() {
                return Ok(state);
            }
            match &*common_core::sync::lock(&self.outcome) {
                Ok(()) => Ok(CredentialState::issued(
                    Credential::new("secret-token"),
                    Utc::now(),
                    Some(Duration::from_secs(300)),
                    0,
                )),
                Err(error) => Err(error.clone()),
            }
        })
    }
}

fn fast_config() -> AuthConfig {
    AuthConfig::new()
        .refresh_early_secs(5)
        .max_attempts(3)
        .backoff_base_ms(1)
        .backoff_jitter_pct(0)
        .build()
}

#[tokio::test]
async fn concurrent_leases_refresh_once() {
    let driver = FakeDriver::new(Ok(()));
    let manager =
        Arc::new(TokenManager::new(fast_config(), driver.clone(), None).expect("manager"));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let manager = Arc::clone(&manager);
        tasks.push(tokio::spawn(
            async move { manager.lease().await.map(|_| ()) },
        ));
    }
    for task in tasks {
        task.await.expect("joined").expect("leased");
    }
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        1,
        "one refresh for N leases"
    );
}

#[tokio::test]
async fn expiry_refuses_sync_read_and_gate_entries() {
    let driver = FakeDriver::new(Ok(()));
    let manager = Arc::new(TokenManager::new(fast_config(), driver, None).expect("manager"));
    manager
        .install(CredentialState {
            credential: Credential::new("expired"),
            issued_at: Utc::now() - chrono::Duration::seconds(600),
            expires_at: Some(Utc::now() - chrono::Duration::seconds(1)),
            refresh: None,
            generation: 3,
        })
        .expect("installed");
    assert!(!manager.is_authenticated());
    assert!(manager.current().is_none());
    let handle = crate::auth::AuthHandle::new(Arc::clone(&manager));
    let reading = GateReading::assemble_with_credential(
        TradingState::Enabled,
        None,
        None,
        None,
        Some(&handle),
    );
    assert!(
        !reading.allows_entries(),
        "expired credential disarms entries"
    );
    assert!(reading.credential.allows_exit, "exits still route");
    assert_eq!(
        reading.credential,
        GateVerdict {
            allows_entry: false,
            allows_exit: true,
            reason: reading.credential.reason.clone()
        }
    );
}

#[tokio::test]
async fn terminal_failure_is_not_retried_and_transient_is_bounded() {
    let terminal = FakeDriver::new(Err(AuthError::Terminal("bad login".to_owned())));
    let manager = TokenManager::new(fast_config(), terminal.clone(), None).expect("manager");
    assert!(manager.refresh().await.is_err());
    assert_eq!(terminal.calls.load(Ordering::SeqCst), 1, "terminal stops");

    let transient = FakeDriver::new(Err(AuthError::Transient("down".to_owned())));
    let manager = TokenManager::new(fast_config(), transient.clone(), None).expect("manager");
    assert!(manager.refresh().await.is_err());
    assert_eq!(
        transient.calls.load(Ordering::SeqCst),
        3,
        "transient attempts are bounded by config"
    );
}

#[tokio::test]
async fn file_store_survives_restart_and_corruption_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("credential.json");
    let store: Arc<dyn CredentialStore> = Arc::new(FileCredentialStore::new(path.clone()));

    let driver = FakeDriver::new(Ok(()));
    let manager = TokenManager::new(
        AuthConfig::new()
            .store_path(path.display().to_string())
            .build(),
        driver,
        Some(store),
    )
    .expect("manager");
    manager.refresh().await.expect("refreshed");
    drop(manager);

    let driver = FakeDriver::new(Err(AuthError::Terminal("must not run".to_owned())));
    let restarted = TokenManager::new(
        AuthConfig::new()
            .store_path(path.display().to_string())
            .build(),
        driver.clone(),
        Some(Arc::new(FileCredentialStore::new(path.clone()))),
    )
    .expect("restart");
    assert!(restarted.is_authenticated(), "credential survives restart");
    assert_eq!(driver.calls.load(Ordering::SeqCst), 0, "no refresh needed");

    std::fs::write(&path, b"not json").expect("corrupt");
    let refused = TokenManager::new(
        AuthConfig::new()
            .store_path(path.display().to_string())
            .build(),
        FakeDriver::new(Ok(())),
        Some(Arc::new(FileCredentialStore::new(path))),
    );
    assert!(refused.is_err(), "corrupt store refuses");
}

#[tokio::test]
async fn memory_store_and_redaction() {
    let store = Arc::new(MemoryCredentialStore::new());
    let state = CredentialState::issued(
        Credential::new("super-secret"),
        Utc::now(),
        Some(Duration::from_secs(60)),
        7,
    );
    store.save(&state).expect("saved");
    assert_eq!(store.load().expect("loaded").expect("some").generation, 7);
    let rendered = format!("{state:?}");
    assert!(!rendered.contains("super-secret"), "Debug must redact");
    assert!(rendered.contains("<redacted>"));
}

/// Deterministic refresh timing over paused tokio time: every sleep the
/// manager or driver takes auto-advances, so attempt counts, single-flight
/// coalescing, and abort behavior assert exactly with no wall-clock flake.
fn timing_config() -> AuthConfig {
    AuthConfig::new()
        .refresh_early_secs(60)
        .max_attempts(3)
        .backoff_base_ms(50)
        .backoff_jitter_pct(0)
        .min_refresh_interval_secs(5)
        .build()
}

fn fresh_state(generation: u64) -> CredentialState {
    CredentialState::issued(
        Credential::new("fresh"),
        Utc::now(),
        Some(Duration::from_secs(3_600)),
        generation,
    )
}

fn due_state(generation: u64) -> CredentialState {
    CredentialState::issued(
        Credential::new("due"),
        Utc::now(),
        Some(Duration::from_secs(30)),
        generation,
    )
}

#[tokio::test(start_paused = true)]
async fn paused_time_a_fresh_credential_leases_without_dialing() {
    let driver = FakeDriver::new(Ok(()));
    let manager = TokenManager::new(timing_config(), driver.clone(), None).expect("manager");
    manager.install(fresh_state(7)).expect("installed");
    let lease = manager.lease().await.expect("leased");
    assert_eq!(lease.generation(), 7, "no refresh means no new generation");
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        0,
        "a credential outside the early window never dials"
    );
}

#[tokio::test(start_paused = true)]
async fn paused_time_a_due_credential_refreshes_exactly_once_for_many_leases() {
    let driver = FakeDriver::new(Ok(()));
    let manager =
        Arc::new(TokenManager::new(timing_config(), driver.clone(), None).expect("manager"));
    manager.install(due_state(7)).expect("installed");
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let manager = Arc::clone(&manager);
        tasks.push(tokio::spawn(async move {
            manager.lease().await.map(|lease| lease.generation())
        }));
    }
    let mut generations = Vec::new();
    for task in tasks {
        generations.push(task.await.expect("joined").expect("leased"));
    }
    assert!(
        generations.iter().all(|generation| *generation == 8),
        "every lease observes the single refresh, got {generations:?}"
    );
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        1,
        "one refresh for N concurrent leases inside the window"
    );
}

#[tokio::test(start_paused = true)]
async fn paused_time_terminal_failure_dials_once_and_disarms_entries() {
    let driver = FakeDriver::new(Err(AuthError::Terminal("bad login".to_owned())));
    let manager = TokenManager::new(timing_config(), driver.clone(), None).expect("manager");
    let error = manager.lease().await.expect_err("terminal refuses");
    assert_eq!(error, AuthError::Terminal("bad login".to_owned()));
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        1,
        "a terminal failure never retries"
    );
    assert!(manager.current().is_none(), "nothing bankable is banked");
}

#[tokio::test(start_paused = true)]
async fn paused_time_transient_failure_is_bounded_and_leaves_state_untouched() {
    let driver = FakeDriver::new(Err(AuthError::Transient("down".to_owned())));
    let manager = TokenManager::new(timing_config(), driver.clone(), None).expect("manager");
    manager.install(fresh_state(7)).expect("installed");
    manager.clear().expect("cleared");
    let error = manager.lease().await.expect_err("transient exhausts");
    assert_eq!(error, AuthError::Transient("down".to_owned()));
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        3,
        "attempts are bounded by config"
    );
    assert!(
        manager.current().is_none(),
        "a failed refresh banks nothing"
    );
    let health = manager.health();
    assert!(
        health.degraded_reason.is_some(),
        "the failure is published, got {health:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn paused_time_an_already_expired_acquisition_never_succeeds() {
    let expired = CredentialState::issued(
        Credential::new("stale"),
        Utc::now() - chrono::Duration::seconds(600),
        Some(Duration::from_secs(300)),
        99,
    );
    let driver = FakeDriver::with_state(expired);
    let manager = TokenManager::new(timing_config(), driver.clone(), None).expect("manager");
    manager.install(fresh_state(7)).expect("installed");
    manager.clear().expect("cleared");
    let error = manager
        .lease()
        .await
        .expect_err("expired acquisition refuses");
    assert_eq!(error, AuthError::Expired);
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        1,
        "the attempt happens; the success does not"
    );
    assert!(manager.current().is_none(), "an expired win is not banked");
    let health = manager.health();
    assert!(
        health
            .degraded_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("expired")),
        "the refusal is published, got {health:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn paused_time_automatic_refreshes_obey_the_minimum_interval() {
    let driver = FakeDriver::new(Err(AuthError::Transient("down".to_owned())));
    let manager = TokenManager::new(timing_config(), driver.clone(), None).expect("manager");
    let error = manager.lease().await.expect_err("first cycle exhausts");
    assert_eq!(error, AuthError::Transient("down".to_owned()));
    assert_eq!(driver.calls.load(Ordering::SeqCst), 3);
    // Inside the minimum interval the next lease fails fast without dialing:
    // a down venue is not hammered once per caller.
    let error = manager
        .lease()
        .await
        .expect_err("rate-limited lease refuses");
    assert_eq!(error, AuthError::Expired);
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        3,
        "no new attempt inside the minimum interval"
    );
    // A forced refresh bypasses the interval: the operator's explicit retry
    // always dials.
    let error = manager.refresh().await.expect_err("forced cycle exhausts");
    assert_eq!(error, AuthError::Transient("down".to_owned()));
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        6,
        "a forced refresh always dials"
    );
}

#[tokio::test]
async fn timing_constants_are_frozen() {
    let config = AuthConfig::default();
    assert_eq!(config.refresh_early_secs, 60, "early margin is frozen");
    assert_eq!(config.max_attempts, 3, "attempt bound is frozen");
    assert_eq!(config.backoff_base_ms, 200, "backoff base is frozen");
    assert_eq!(config.backoff_jitter_pct, 25, "backoff jitter is frozen");
    assert_eq!(
        config.min_refresh_interval_secs, 5,
        "minimum refresh interval is frozen"
    );
    assert_eq!(config.ttl_secs, 3_600, "fallback lifetime is frozen");
}

/// Abort mid-sleep ends the refresher with no task leak and no later
/// publication: the generation and the attempt count freeze at the abort.
#[ignore = "soak: refresher abort/leak audit; the per-cycle behavior is pinned by the paused-time tests every run"]
#[tokio::test(start_paused = true)]
async fn soak_abort_during_sleep_freezes_publication() {
    use fluent_concurrency::scope::Scope;
    use fluent_concurrency::stream::StreamAbort;

    let driver = FakeDriver::new(Err(AuthError::Transient("down".to_owned())));
    let manager = Arc::new(
        TokenManager::new(
            AuthConfig::new()
                .refresh_early_secs(60)
                .max_attempts(2)
                .backoff_base_ms(10)
                .backoff_jitter_pct(0)
                .min_refresh_interval_secs(5)
                .build(),
            driver.clone(),
            None,
        )
        .expect("manager"),
    );
    let mut scope = Scope::new();
    let abort = StreamAbort::new();
    let handle = manager.spawn_refresher(&mut scope, abort.clone(), Duration::from_secs(60));
    // Let one cadence elapse: the first cycle attempts, then the refresher
    // sleeps until the next cadence. Paused time needs both halves of the
    // pump: `advance` moves the clock past the sleeps, `yield_now` lets the
    // spawned task poll between advances.
    let mut spins = 0;
    while driver.calls.load(Ordering::SeqCst) == 0 && spins < 1_000 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        spins += 1;
    }
    let calls_before_abort = driver.calls.load(Ordering::SeqCst);
    assert!(
        calls_before_abort > 0,
        "the refresher attempted before the abort"
    );
    let generation_before_abort = manager.current_generation();
    abort.cancel();
    // The abort may land mid-cycle: the in-flight lease drains first, then
    // the loop observes the abort. Pump advances until the task lands,
    // bounded so a leak fails loudly.
    let mut spins = 0;
    while !handle.is_finished() && spins < 1_000 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        spins += 1;
    }
    assert!(handle.is_finished(), "no task leaks past the abort");
    // Far-future cadences must not publish: attempts and generation freeze.
    tokio::time::advance(Duration::from_secs(3_600)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        calls_before_abort,
        "no attempt fires after the abort"
    );
    assert_eq!(
        manager.current_generation(),
        generation_before_abort,
        "no publication lands after the abort"
    );
    scope.close().await;

    // Abort during the cadence sleep: a refresher that never woke attempts
    // nothing and publishes nothing.
    let idle_driver = FakeDriver::new(Ok(()));
    let idle =
        Arc::new(TokenManager::new(timing_config(), idle_driver.clone(), None).expect("manager"));
    let mut idle_scope = Scope::new();
    let idle_abort = StreamAbort::new();
    let idle_handle = idle.spawn_refresher(
        &mut idle_scope,
        idle_abort.clone(),
        Duration::from_secs(3_600),
    );
    idle_abort.cancel();
    let mut spins = 0;
    while !idle_handle.is_finished() && spins < 1_000 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        spins += 1;
    }
    assert!(idle_handle.is_finished(), "no task leaks past the abort");
    assert_eq!(
        idle_driver.calls.load(Ordering::SeqCst),
        0,
        "an abort before the first cadence attempts nothing"
    );
    idle_scope.close().await;
}
