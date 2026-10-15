//! Store facts on the health route: populated, additive, and byte-stable.
//!
//! A server that never observed the store serves the pre-store payload
//! byte-identically (pinned against the checked-in golden, not a string in
//! this file, so the two can never drift apart). A server booted with a
//! store reading surfaces all three facts on the route, and the gateway
//! reports the identical view — one truth for the terminal, whichever side
//! it reads.

use super::*;
use std::path::PathBuf;

use protopyne_store::StoreHealth;
use protopyne_view::TerminalGateway;

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::load_bars_csv(&path).expect("vendored history loads")
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("views")
        .join(name)
}

async fn boot_with(store_health: StoreHealth) -> ServerHandle {
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .store_health(store_health)
        .build();
    serve(config, history()).await.expect("test server boots")
}

async fn get_body(client: &reqwest::Client, base: &str) -> String {
    client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("route reachable")
        .text()
        .await
        .expect("body reads")
}

#[tokio::test]
async fn default_reading_serves_the_pre_store_bytes() {
    let handle = boot_with(StoreHealth::default()).await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let body = get_body(&client, &base).await;
    let expected =
        std::fs::read_to_string(golden_path("health.json")).expect("golden checked in");
    assert_eq!(body, expected, "default store reading must not move the payload");
    assert!(!body.contains("store_"), "no store key without a reading");
    handle.shutdown().await;
}

#[tokio::test]
async fn recorded_reading_surfaces_on_the_route_and_the_gateway() {
    let reading = StoreHealth {
        reachable: false,
        watermark_age_secs: Some(120),
        open_gaps: 2,
    };
    let handle = boot_with(reading).await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let body = get_body(&client, &base).await;
    let routed: HealthView = serde_json::from_str(&body).expect("health is JSON");
    assert_eq!(routed.store_reachable, Some(false));
    assert_eq!(routed.store_watermark_age_secs, Some(120));
    assert_eq!(routed.store_open_gaps, Some(2));
    // Gateway-route parity: the terminal sees one truth.
    let via_gateway = handle.gateway().health().expect("gateway health reads");
    assert_eq!(via_gateway, routed);
    handle.shutdown().await;
}

#[test]
fn health_view_reports_no_reading_as_no_fields() {
    let state: Shared = std::sync::Arc::new(AppState {
        limiter: Limiter::new(32),
        history: history(),
        history_timeframe: Timeframe::M15,
        inbox: Mutex::new(Vec::new()),
        armed: Mutex::new(None),
        portfolio: Mutex::new(PortfolioView::default()),
        feed: tokio::sync::broadcast::channel(16).0,
        trading_state: StateHandle::new(TradingState::Enabled),
        last_command: Mutex::new(None),
        store_health: Mutex::new(StoreHealth::default()),
        tape: None,
    });
    let view = health_view(&state);
    assert_eq!(view.store_reachable, None);
    assert_eq!(view.store_watermark_age_secs, None);
    assert_eq!(view.store_open_gaps, None);
}
