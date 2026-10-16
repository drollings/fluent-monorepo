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
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
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
        observed: true,
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

fn shared_with(store_health: StoreHealth) -> Shared {
    std::sync::Arc::new(AppState {
        limiter: Limiter::new(32),
        history: history(),
        history_timeframe: Timeframe::M15,
        inbox: Mutex::new(Vec::new()),
        armed: Mutex::new(None),
        portfolio: Mutex::new(PortfolioView::default()),
        feed: tokio::sync::broadcast::channel(16).0,
        trading_state: StateHandle::new(TradingState::Enabled),
        last_command: Mutex::new(None),
        store_health: Mutex::new(store_health),
        tape: None,
        tape_serve_stats: Arc::new(TapeServeStats::default()),
        fallback_latch: Mutex::new(FallbackLatch::default()),
    })
}

#[test]
fn health_view_reports_no_reading_as_no_fields() {
    let view = health_view(&shared_with(StoreHealth::default()));
    assert_eq!(view.store_reachable, None);
    assert_eq!(view.store_watermark_age_secs, None);
    assert_eq!(view.store_open_gaps, None);
}

#[test]
fn health_view_reports_an_observed_reading_even_at_default_values() {
    // A genuine reading that happens to equal the old default is
    // distinguishable from never having observed the store: the view
    // reports every field instead of none.
    let observed = StoreHealth {
        observed: true,
        ..StoreHealth::default()
    };
    let view = health_view(&shared_with(observed));
    assert_eq!(view.store_reachable, Some(true));
    assert_eq!(view.store_open_gaps, Some(0));
    assert_eq!(view.store_watermark_age_secs, None);
}

#[tokio::test]
async fn tape_counters_surface_on_the_route_and_the_gateway() {
    use crate::common_tape as tape;
    use protopyne_types::Timeframe;
    use protopyne_view::HealthView;

    // A file-backed tape holding one bar the gateway can serve: after one
    // symbol-scoped read, /health carries the serve count and the gateway
    // reports the identical view — one truth for the terminal.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let bars = crate::csv::load_bars_csv(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("data")
            .join("SPY_15M.csv"),
    )
    .expect("vendored history loads");
    tape::seed_file(&root, &bars, "SPY", Timeframe::M15, tape::VENUE);
    let reader = crate::service::open_tape_reader(&root).expect("boot opens the store");
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, bars, Some(reader))
        .await
        .expect("server boots with a tape");
    handle
        .gateway()
        .chart_bars_for("SPY", "15M")
        .expect("store-backed gateway read");

    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let body = get_body(&client, &base).await;
    let routed: HealthView = serde_json::from_str(&body).expect("health is JSON");
    assert_eq!(routed.tape_served, Some(1), "the serve is counted on /health");
    assert_eq!(routed.tape_no_tape, Some(0));
    assert_eq!(routed.tape_unknown_symbol, Some(0));
    assert_eq!(routed.tape_unsupported_timeframe, Some(0));
    assert_eq!(routed.tape_denied, Some(0));
    assert_eq!(routed.tape_store_error, Some(0));
    assert_eq!(routed.tape_last_miss, None, "no miss on record");
    // Gateway-route parity: the terminal sees one truth.
    let via_gateway = handle.gateway().health().expect("gateway health reads");
    assert_eq!(via_gateway, routed);
    handle.shutdown().await;
}

#[tokio::test]
async fn unobserved_tape_stats_report_no_fields() {
    // A tape-backed server that served nothing yet keeps the pre-tape
    // payload byte-identical: observation, not configuration, decides.
    use crate::common_tape as tape;
    use protopyne_types::Timeframe;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let bars = history();
    tape::seed_file(&root, &bars, "SPY", Timeframe::M15, tape::VENUE);
    let reader = crate::service::open_tape_reader(&root).expect("boot opens the store");
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, bars, Some(reader))
        .await
        .expect("server boots with a tape");
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let body = get_body(&client, &base).await;
    assert!(!body.contains("tape_"), "no tape key before any tape read");
    let expected =
        std::fs::read_to_string(golden_path("health.json")).expect("golden checked in");
    assert_eq!(body, expected, "unobserved tape stats must not move the payload");
    handle.shutdown().await;
}
