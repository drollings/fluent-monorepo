//! The store-backed gateway read and the route-backed read agree.
//!
//! C63 already pins gateway-route parity; this extends it to the tape arm:
//! with the store holding the same bars the server booted with, the
//! symbol-scoped `chart_bars_for` (which plans through the durable tape) and
//! the slot-agnostic `/chart/bars` route return the same `ChartBars`. The
//! store is additive — when it cannot serve, the read falls back to the boot
//! history, which is the same series here.

use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::common_tape as tape;
use crate::service::{ServerConfig, TapeReader, serve_with_tape};
use protopyne_types::Timeframe;
use protopyne_view::{ChartBars, TerminalGateway};

fn boot_history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::load_bars_csv(&path).expect("vendored history loads")
}

#[tokio::test]
async fn store_backed_read_matches_the_route_over_the_same_bars() {
    let bars = boot_history();
    let caps = tape::caps();
    let (store, _key, _request) = tape::seed(&bars, "SPY", Timeframe::M15, &caps);
    let reader = TapeReader {
        store: Arc::new(store),
        caps,
        venue: tape::VENUE.to_owned(),
        limit: crate::DEFAULT_TAPE_LIMIT,
    };
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, bars.clone(), Some(reader))
        .await
        .expect("server boots with a tape");

    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    let body = client
        .get(format!("{base}/chart/bars"))
        .send()
        .await
        .expect("route reachable")
        .text()
        .await
        .expect("body reads");
    let routed: ChartBars = serde_json::from_str(&body).expect("chart bars decode");

    let via_gateway = handle
        .gateway()
        .chart_bars_for("SPY", "15M")
        .expect("store-backed gateway read");
    assert_eq!(
        via_gateway, routed,
        "the store-backed read and the route must serve the same bars"
    );
    assert!(!routed.rows.is_empty(), "the read served the seeded bars");
    handle.shutdown().await;
}

#[tokio::test]
async fn a_store_less_gateway_serves_the_boot_history() {
    // Control: with no tape booted, the symbol-scoped read falls back to the
    // boot history exactly as it did before the store existed.
    let bars = boot_history();
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, bars.clone(), None)
        .await
        .expect("server boots without a tape");
    let via_gateway = handle
        .gateway()
        .chart_bars_for("SPY", "15M")
        .expect("fallback read");
    assert_eq!(
        via_gateway.rows.len(),
        bars.len(),
        "the fallback serves the boot history"
    );
    handle.shutdown().await;
}
