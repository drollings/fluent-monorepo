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
use crate::service::{serve_with_tape, ServerConfig, TapeReader};
use chrono::DateTime;
use protopyne_store::StoreStorage;
use protopyne_types::Timeframe;
use protopyne_view::{ChartBars, TerminalGateway};

fn boot_history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

/// `count` flat 15-minute bars at `price` on a fixed stamp grid: a series
/// distinct from the vendored boot history, so a read's provenance is
/// observable in the served closes.
fn flat_tape(count: i64, price: &str) -> Vec<Bar> {
    let base = 1_767_974_400_000i64;
    (0..count)
        .map(|step| {
            let price = price.parse().expect("decimal");
            Bar::new()
                .ts(DateTime::from_timestamp_millis(base + step * 900_000).expect("in range"))
                .o(price)
                .h(price)
                .l(price)
                .c(price)
                .volume(10)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

#[tokio::test]
async fn store_backed_read_matches_the_route_over_the_same_bars() {
    let bars = boot_history();
    let caps = tape::caps();
    let (store, _key, _request) = tape::seed(&bars, "SPY", Timeframe::M15, &caps);
    let reader = TapeReader {
        store: Arc::new(store),
        caps,
        permit: StoreStorage::new(":memory:".into()),
        venue: tape::VENUE.to_owned(),
        limit: crate::DEFAULT_TAPE_LIMIT,
    };
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, bars.clone(), Some(reader), None)
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
    // boot history exactly as it did before the store existed, and the miss
    // is counted as `NoTape` (which never warns).
    let bars = boot_history();
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, bars.clone(), None, None)
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
    let stats = handle.tape_serve_stats();
    assert_eq!(stats.served, 0, "no tape served the read");
    assert_eq!(stats.no_tape, 1, "the miss is counted as NoTape");
    assert_eq!(stats.last_miss, Some(TapeMissKind::NoTape));
    handle.shutdown().await;
}

/// Provenance: when the tape holds a series unlike the boot history, the
/// symbol-scoped read serves the tape's series, never a silent fallback.
#[tokio::test]
async fn a_divergent_tape_serves_the_tape_series() {
    let boot = boot_history();
    let tape_bars = flat_tape(5, "777");
    let caps = tape::caps();
    let (store, _key, _request) = tape::seed(&tape_bars, "SPY", Timeframe::M15, &caps);
    let reader = TapeReader {
        store: Arc::new(store),
        caps,
        permit: StoreStorage::new(":memory:".into()),
        venue: tape::VENUE.to_owned(),
        limit: crate::DEFAULT_TAPE_LIMIT,
    };
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, boot.clone(), Some(reader), None)
        .await
        .expect("server boots with a tape");

    let served = handle
        .gateway()
        .chart_bars_for("SPY", "15M")
        .expect("store-backed gateway read");
    assert_eq!(
        served.rows.len(),
        tape_bars.len(),
        "the tape's series served, not the boot history"
    );
    assert!(
        served
            .rows
            .iter()
            .all(|row| (row.close - 777.0).abs() < 1e-6),
        "the tape's closes served"
    );
    let stats = handle.tape_serve_stats();
    assert_eq!(stats.served, 1);
    assert_eq!(stats.last_miss, None);
    handle.shutdown().await;
}

/// A capability set without the store permit denies the read; the gateway
/// falls back to boot history and counts the denial, so a mis-wired permit
/// is observable rather than silent.
#[tokio::test]
async fn a_denied_permit_falls_back_and_counts_denied() {
    let boot = boot_history();
    let caps = tape::caps();
    let (store, _key, _request) = tape::seed(&boot, "SPY", Timeframe::M15, &caps);
    let reader = TapeReader {
        store: Arc::new(store),
        // Deliberately no store permit: the read must be denied.
        caps: fluent_wvr::CapabilitySet::new(),
        permit: StoreStorage::new(":memory:".into()),
        venue: tape::VENUE.to_owned(),
        limit: crate::DEFAULT_TAPE_LIMIT,
    };
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, boot.clone(), Some(reader), None)
        .await
        .expect("server boots with a tape");

    let served = handle
        .gateway()
        .chart_bars_for("SPY", "15M")
        .expect("fallback read");
    assert_eq!(
        served.rows.len(),
        boot.len(),
        "the denied read falls back to the boot history"
    );
    let stats = handle.tape_serve_stats();
    assert_eq!(stats.served, 0, "the tape did not serve");
    assert_eq!(stats.denied, 1, "the denial is counted");
    assert_eq!(stats.last_miss, Some(TapeMissKind::Denied));
    handle.shutdown().await;
}

/// A miss/serve/miss sequence tallies each counter exactly: the latch edges
/// (warn once per entry, one log per recovery) are pinned by the latch unit
/// tests, and this pins the counts the same reads record — including that a
/// served read leaves the last miss standing, so the most recent fault stays
/// visible until the next one.
#[tokio::test]
async fn miss_serve_miss_tallies_each_counter() {
    let bars = boot_history();
    let caps = tape::caps();
    let (store, _key, _request) = tape::seed(&bars, "SPY", Timeframe::M15, &caps);
    let reader = TapeReader {
        store: Arc::new(store),
        caps,
        permit: StoreStorage::new(":memory:".into()),
        venue: tape::VENUE.to_owned(),
        limit: crate::DEFAULT_TAPE_LIMIT,
    };
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, bars.clone(), Some(reader), None)
        .await
        .expect("server boots with a tape");
    let gateway = handle.gateway();

    // Unknown symbol: the tape cannot serve it, so the boot history serves
    // the read and the miss is counted.
    let fallback = gateway
        .chart_bars_for("NOPE", "15M")
        .expect("fallback read");
    assert_eq!(
        fallback.rows.len(),
        bars.len(),
        "the fallback serves the boot history"
    );
    // Unknown label: the tape records the miss, then the fallback itself
    // refuses — the counter still tallies the attempt.
    assert!(
        gateway.chart_bars_for("SPY", "9M").is_err(),
        "no ladder serves 9M"
    );
    // Served: the tape's series, counted once.
    let served = gateway
        .chart_bars_for("SPY", "15M")
        .expect("store-backed read");
    assert_eq!(served.rows.len(), bars.len());
    // A second miss after the serve: the counters keep every outcome.
    gateway
        .chart_bars_for("NOPE", "15M")
        .expect("fallback read");

    let stats = handle.tape_serve_stats();
    assert_eq!(stats.served, 1, "one tape serve");
    assert_eq!(stats.unknown_symbol, 2, "two unknown-symbol misses");
    assert_eq!(stats.unsupported_timeframe, 1, "one unsupported-label miss");
    assert_eq!(stats.no_tape, 0);
    assert_eq!(stats.denied, 0);
    assert_eq!(stats.store_error, 0);
    assert_eq!(stats.last_miss, Some(TapeMissKind::UnknownSymbol));
    handle.shutdown().await;
}
