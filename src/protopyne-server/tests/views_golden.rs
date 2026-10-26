//! Route-payload goldens: every read route plus the arm submit, byte-exact.
//!
//! The files under `tests/data/views/` were generated BEFORE the
//! `protopyne-view` swap with the old local view structs; the swap must
//! reproduce them byte-for-byte (enum string tags and key order
//! preserved). Regenerate only by running with `OVERWRITE_GOLDENS=1` on
//! reviewed code — the overwrite path writes and passes, the default path
//! compares and fails loudly on any drift.

use super::*;
use crate::common_fixtures::spy_history;
use protopyne_strategy::fixtures::grant_with;
use std::path::PathBuf;

fn test_grant(id: &str, max_trades: u32) -> Grant {
    // The shared fixture's shape; only the id and the trade budget differ.
    grant_with(|grant| {
        grant.id = id.to_owned();
        grant.budgets.max_trades = max_trades;
    })
}

async fn boot() -> ServerHandle {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    serve(config, spy_history())
        .await
        .expect("test server boots")
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("views")
        .join(name)
}

/// Byte-exact compare against the checked-in golden; with
/// `OVERWRITE_GOLDENS=1`, rewrite the file instead (reviewed code only).
fn check_golden(name: &str, body: &str) {
    let path = golden_path(name);
    if std::env::var("OVERWRITE_GOLDENS").is_ok() {
        std::fs::create_dir_all(path.parent().expect("golden dir")).expect("golden dir creates");
        std::fs::write(&path, body).expect("golden writes");
        return;
    }
    let expected = std::fs::read_to_string(&path).expect("golden checked in");
    assert_eq!(body, expected, "{name} route bytes drifted from the golden");
}

async fn get(client: &reqwest::Client, base: &str, route: &str) -> String {
    client
        .get(format!("{base}{route}"))
        .send()
        .await
        .expect("route reachable")
        .text()
        .await
        .expect("body reads")
}

#[tokio::test]
async fn route_payloads_match_checked_in_goldens() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    check_golden("health.json", &get(&client, &base, "/health").await);
    check_golden(
        "portfolio_fresh.json",
        &get(&client, &base, "/portfolio").await,
    );

    let preview = client
        .post(format!("{base}/proposals/preview"))
        .json(&serde_json::json!({"seed": 11}))
        .send()
        .await
        .expect("preview reachable")
        .text()
        .await
        .expect("preview reads");
    check_golden("preview.json", &preview);
    // The rehearsal inbox the preview published, under its own name.
    check_golden(
        "proposals.json",
        &get(&client, &base, "/proposals/rehearsal").await,
    );
    // The authoritative inbox: the journaled fold (empty until a proposal is
    // journaled for approval).
    check_golden(
        "proposals_journaled.json",
        &get(&client, &base, "/proposals").await,
    );
    let booked = get(&client, &base, "/portfolio").await;
    check_golden("portfolio.json", &booked);
    check_golden(
        "portfolio_booked.json",
        &get(&client, &base, "/portfolio/booked").await,
    );
    check_golden(
        "portfolio_rehearsal.json",
        &get(&client, &base, "/portfolio/rehearsal").await,
    );
    check_golden("chart_bars.json", &get(&client, &base, "/chart/bars").await);
    // The journaled event log: empty on a fresh boot, but its envelope bytes
    // are part of the characterized surface.
    check_golden("journal.json", &get(&client, &base, "/journal").await);
    check_golden(
        "overlay_sma_14.json",
        &get(&client, &base, "/chart/overlays?kind=sma&period=14").await,
    );
    check_golden("replay.json", &get(&client, &base, "/replay").await);

    let arm = client
        .post(format!("{base}/grants/arm"))
        .json(&test_grant("golden-grant", 5))
        .send()
        .await
        .expect("arm reachable")
        .text()
        .await
        .expect("arm reads");
    check_golden("arm.json", &arm);

    handle.shutdown().await;
}
