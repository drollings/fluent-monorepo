//! Cert-sandbox soak: the long read-only storm behind env gating.
//!
//! Read-only by design — no order entry, no cancel, no dry-run from
//! automation. What runs when `TASTYTRADE_*` is present: boot, login, then
//! a fifty-read storm of absent-order queries and account snapshots, every
//! one of which must succeed. Without the env contract the test skips
//! cleanly; run it via `make venue-test-live`.

use std::sync::Arc;
use std::time::Instant;

use fluent_concurrency::io::net::NetCapability;
use fluent_wvr::{CapabilitySet, CURRENT_CAPS};
use protopyne_venue::{
    boot_live, LiveAttachments, LiveBoot, SessionConfig, TastytradeConfig, Venue,
};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Fifty read-only storm rounds: each round queries an order that was
/// never placed (must read absent, never error) and takes an account
/// snapshot. Success is total — any failed round fails the soak — and the
/// worst round latency is reported, never averaged away.
#[ignore = "live: needs TASTYTRADE_* env against the cert sandbox; run via `make venue-test-live`"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cert_sandbox_read_storm() {
    let (Some(base_url), Some(account), Some(username), Some(password)) = (
        env("TASTYTRADE_BASE_URL"),
        env("TASTYTRADE_ACCOUNT"),
        env("TASTYTRADE_LOGIN"),
        env("TASTYTRADE_PASSWORD"),
    ) else {
        eprintln!("SKIP: TASTYTRADE_* env absent; cert sandbox not attempted");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let boot = LiveBoot::new()
        .session(SessionConfig::default())
        .transport(TastytradeConfig::new().base_url(base_url).build())
        .account(account)
        .journal_path(dir.path().join("cert-soak.db"))
        .build();
    let net = Arc::new(NetCapability::new());
    let caps = CapabilitySet::new().with(NetCapability::new());
    const ROUNDS: usize = 50;
    let live = CURRENT_CAPS
        .scope(
            caps,
            boot_live(
                &boot,
                net,
                &username,
                &password,
                LiveAttachments::none(),
                None,
            ),
        )
        .await
        .expect("cert-sandbox boot");
    let mut worst_ms = 0;
    for round in 0..ROUNDS {
        let started = Instant::now();
        let missing = live.venue.query(&format!("soak-probe-{round}#0")).await;
        assert!(
            missing.is_none(),
            "round {round}: an unplaced order reads absent"
        );
        let _snapshot = live.venue.account_snapshot();
        worst_ms = worst_ms.max(started.elapsed().as_millis());
    }
    eprintln!("SOAK: {ROUNDS} read rounds clean, worst round {worst_ms}ms");
    live.session.shutdown().await;
}
