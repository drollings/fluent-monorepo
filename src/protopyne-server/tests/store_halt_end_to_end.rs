//! A store outage halts entries end to end, and never strands an exit.
//!
//! With a store outage injected at boot, `/health` reports
//! `allows_entries == false` and `allows_exits == true` (exits stay
//! routable), and a preview request refuses with the failing gate named —
//! the same pure verdict `/health` reports, so the two cannot disagree. The
//! control group runs the identical route over a healthy default reading.

use std::path::PathBuf;

use super::*;
use protopyne_store::StoreHealth;
use protopyne_view::TerminalGateway;

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

async fn boot(reading: StoreHealth) -> ServerHandle {
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .store_health(reading)
        .build();
    serve(config, history()).await.expect("server boots")
}

#[tokio::test]
async fn a_store_outage_halts_entries_and_refuses_previews() {
    let handle = boot(StoreHealth {
        reachable: false,
        observed: true,
        ..StoreHealth::default()
    })
    .await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    let health: HealthView = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("health reachable")
        .json()
        .await
        .expect("health decodes");
    assert!(!health.allows_entries, "a store outage halts entries");
    assert!(health.allows_exits, "exits stay routable through a store halt");
    assert_eq!(health.store_reachable, Some(false), "the route names the reading");

    let response = client
        .post(format!("{base}/proposals/preview"))
        .json(&serde_json::json!({ "seed": 0 }))
        .send()
        .await
        .expect("preview reachable");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "a store outage refuses previews"
    );
    let body: ErrorBody = response.json().await.expect("error body decodes");
    assert!(
        body.error.contains("store"),
        "the refusal must name the failing gate: {}",
        body.error
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn a_healthy_default_still_trades_and_previews() {
    // Control group: the default (no reading) is inert, so entries and
    // previews behave exactly as they did before the store gate existed.
    let handle = boot(StoreHealth::default()).await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    let view = handle.gateway().health().expect("gateway health reads");
    assert!(view.allows_entries, "a healthy default allows entries");
    assert!(view.allows_exits, "and exits");

    let response = client
        .post(format!("{base}/proposals/preview"))
        .json(&serde_json::json!({ "seed": 0 }))
        .send()
        .await
        .expect("preview reachable");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "a healthy store previews normally"
    );
    handle.shutdown().await;
}
