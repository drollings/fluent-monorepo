//! Cert-sandbox readiness: production boot against real venue REST.
//!
//! Read-only by design — no order entry, no cancel, no dry-run against the
//! sandbox from automation. What runs: env-gated boot, login, an unknown
//! order query resolving absent, and truth-mirror reads. Anything beyond
//! that is the manual soak (reconnect storm, kill-mid-submit,
//! reconcile mismatch) with committed numbers.

use std::sync::Arc;

use fluent_concurrency::io::net::NetCapability;
use fluent_wvr::{CapabilitySet, CURRENT_CAPS};
use protopyne_venue::Venue;
use protopyne_venue::{boot_live, LiveAttachments, LiveBoot, SessionConfig, TastytradeConfig};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

#[ignore = "live: needs TASTYTRADE_* env against the cert sandbox; run via `make venue-test-live`"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cert_sandbox_boot_login_and_absent_query() {
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
        .journal_path(dir.path().join("cert-sandbox.db"))
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
    assert!(
        live.venue.query("cert-probe#0").await.is_none(),
        "an unplaced order reads absent"
    );
    let _snapshot = live.venue.account_snapshot();
    live.session.shutdown().await;
}
