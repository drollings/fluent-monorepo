//! Cert-sandbox bounded soak: the same deterministic schedule as the
//! hermetic twin, plus a read-only storm against the certification sandbox.
//!
//! Read-only by design — no order entry, no cancel, no dry-run from
//! automation. What runs when `TASTYTRADE_*` is present: the hermetic fault
//! schedule re-proves inside the `live-ai` build, then boot, login, and a
//! time-boxed storm of absent-order queries and account snapshots, every one
//! of which must succeed. Without the env contract the test skips cleanly;
//! run it via `make venue-test-live` with `SOAK_MINUTES` bounding the storm.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fluent_concurrency::io::net::NetCapability;
use fluent_wvr::{CapabilitySet, CURRENT_CAPS};
use protopyne_venue::{
    boot_live, cert_soak, Alert, Journal, LiveAttachments, LiveBoot, SessionConfig,
    TastytradeConfig, Venue, VenueStorage, ALERTS_FILE, BREAKER_FILE, JOURNAL_FILE, RECONCILE_FILE,
};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Minutes of read storm, bounded and defaulted: the soak never runs open
/// ended. `SOAK_MINUTES` must be a positive integer; anything else (or an
/// absent variable) reads as one minute.
fn soak_minutes() -> u64 {
    env("SOAK_MINUTES")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|minutes| *minutes > 0)
        .unwrap_or(1)
        .min(60)
}

#[ignore = "live-ai: needs TASTYTRADE_* env against the cert sandbox; run via `make venue-test-live` with SOAK_MINUTES"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cert_sandbox_bounded_soak() {
    // The schedule first: the live-ai build proves the same numbers the
    // hermetic gate pins, so a live-only divergence would fail here rather
    // than hiding behind credentials.
    let schedule_dir = tempfile::tempdir().expect("tempdir");
    let proven = cert_soak::run_paper_soak(schedule_dir.path())
        .await
        .expect("the schedule runs in the live build");
    assert_eq!(proven.unjournaled_intents, 0);
    assert_eq!(proven.unknown_positions, 0);
    assert!(proven.breaker_tripped);
    assert_eq!(proven.alerts_delivered, 2);
    assert_eq!(proven.repoll_delivered, 0);

    let (Some(base_url), Some(account), Some(username), Some(password)) = (
        env("TASTYTRADE_BASE_URL"),
        env("TASTYTRADE_ACCOUNT"),
        env("TASTYTRADE_LOGIN"),
        env("TASTYTRADE_PASSWORD"),
    ) else {
        eprintln!("SKIP: TASTYTRADE_* env absent; cert sandbox not attempted");
        return;
    };
    let minutes = soak_minutes();
    let dir = tempfile::tempdir().expect("tempdir");
    let boot = LiveBoot::new()
        .session(SessionConfig::default())
        .transport(TastytradeConfig::new().base_url(base_url).build())
        .account(account)
        .journal_path(dir.path().join(JOURNAL_FILE))
        .alerts_path(dir.path().join(ALERTS_FILE))
        .build();
    let net = Arc::new(NetCapability::new());
    let caps = CapabilitySet::new().with(NetCapability::new());
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
    assert!(live.venue.is_authenticated(), "logged in");

    // Time-boxed read storm: absent queries plus snapshots until the minute
    // budget ends. Success is total — any failed round fails the soak — and
    // the worst round latency is reported, never averaged away.
    let deadline = Instant::now() + Duration::from_secs(minutes * 60);
    let mut rounds = 0_usize;
    let mut worst_ms = 0_u128;
    while Instant::now() < deadline && rounds < 10_000 {
        let started = Instant::now();
        let missing = live
            .venue
            .query(&format!("live-soak-probe#{}#0", rounds))
            .await;
        assert!(
            missing.is_none(),
            "round {rounds}: an unplaced order reads absent"
        );
        let _snapshot = live.venue.account_snapshot();
        worst_ms = worst_ms.max(started.elapsed().as_millis());
        rounds += 1;
    }
    assert!(rounds > 0, "the storm ran at least one round");
    eprintln!("SOAK: {rounds} read rounds clean, worst round {worst_ms}ms");

    // Live-run artifacts under the same four names: the journal replays
    // (read-only, so it holds no rows), the reconcile and breaker lines
    // parse, and the alert file parses to the pages the run delivered.
    let journal_path = dir.path().join(JOURNAL_FILE);
    let permit = VenueStorage::new(journal_path.clone());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps, async {
            Journal::open(&journal_path)
                .expect("live journal parses")
                .replay(&permit)
                .expect("live journal replays");
        })
        .await;
    for name in [RECONCILE_FILE, BREAKER_FILE] {
        let path = dir.path().join(name);
        if path.is_file() {
            let text = std::fs::read_to_string(&path).expect("artifact reads");
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                let _: serde_json::Value = serde_json::from_str(line).expect("line parses");
            }
        }
    }
    let alerts_path = dir.path().join(ALERTS_FILE);
    if alerts_path.is_file() {
        let text = std::fs::read_to_string(&alerts_path).expect("alerts read");
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let _: Alert = serde_json::from_str(line).expect("alert parses");
        }
    }
    live.session.shutdown().await;
}
