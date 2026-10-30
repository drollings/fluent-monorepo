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

#[tokio::test]
async fn recorded_history_symbol_refuses_wrong_market_reads() {
    // SPY tape + SPY boot history with the history symbol recorded: asking
    // for GLD must refuse naming GLD, never serve SPY bars under the GLD
    // label. The miss still counts as UnknownSymbol; the matching symbol
    // keeps its fallback.
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
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_symbol("SPY".to_owned())
        .build();
    let handle = serve_with_tape(config, bars.clone(), Some(reader), None)
        .await
        .expect("server boots with a tape");
    let gateway = handle.gateway();

    let err = gateway
        .chart_bars_for("GLD", "15M")
        .expect_err("a wrong-market read refuses");
    assert!(
        err.to_string().contains("GLD"),
        "the refusal names the requested symbol: {err}"
    );
    let overlay_err = gateway
        .chart_overlays_for("GLD", "15M", "bb", 20)
        .expect_err("a wrong-market overlay refuses");
    assert!(
        overlay_err.to_string().contains("GLD"),
        "the overlay refusal names the requested symbol: {overlay_err}"
    );
    let served = gateway
        .chart_bars_for("SPY", "15M")
        .expect("the recorded market still serves");
    assert_eq!(served.rows.len(), bars.len());
    let stats = handle.tape_serve_stats();
    assert_eq!(stats.served, 1, "one tape serve");
    assert_eq!(stats.unknown_symbol, 1, "the wrong-market miss counts");
    handle.shutdown().await;
}

#[tokio::test]
async fn tapeless_boot_refuses_unrecorded_symbols() {
    // CSV-only boot with the history symbol recorded: the single boot
    // market serves its own symbol, and any other symbol refuses instead
    // of painting the boot bars under a foreign label.
    let bars = boot_history();
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_symbol("SPY".to_owned())
        .build();
    let handle = serve_with_tape(config, bars.clone(), None, None)
        .await
        .expect("server boots without a tape");
    let gateway = handle.gateway();
    let fallback = gateway
        .chart_bars_for("SPY", "15M")
        .expect("the recorded market falls back");
    assert_eq!(fallback.rows.len(), bars.len());
    let err = gateway
        .chart_bars_for("GLD", "15M")
        .expect_err("a foreign symbol refuses without a tape");
    assert!(
        err.to_string().contains("GLD"),
        "the refusal names the requested symbol: {err}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn history_root_serves_tickers_the_tape_does_not_hold() {
    // Dir boot with SPY boot history and the recorded symbol, no tape: GLD
    // months under the root serve GLD reads (its own closes from the 1M
    // months), while a ticker with no directory still refuses naming
    // itself. Months parse once per ticker per boot (cached), never per
    // read.
    let dir = tempfile::tempdir().expect("tempdir");
    let month = dir.path().join("GLD/2026/01");
    std::fs::create_dir_all(&month).expect("month creates");
    let base = 1_772_442_000i64;
    let mut csv = String::from("time,open,high,low,close,volume\n");
    for step in 0..5 {
        use std::fmt::Write as _;
        writeln!(csv, "{},55.00,55.00,55.00,55.00,10", base + step * 60)
            .expect("csv fixture writes");
    }
    std::fs::write(month.join("GLD-2026-01-1M.csv"), csv).expect("month writes");
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_path(dir.path().to_string_lossy().into_owned())
        .history_symbol("SPY".to_owned())
        .build();
    let handle = serve_with_tape(config, boot_history(), None, None)
        .await
        .expect("server boots over the root");
    let gateway = handle.gateway();
    let gld = gateway
        .chart_bars_for("GLD", "1M")
        .expect("the root serves GLD");
    assert_eq!(gld.rows.len(), 5, "the GLD months serve whole");
    assert!(
        gld.rows.iter().all(|row| (row.close - 55.0).abs() < 1e-6),
        "GLD closes serve, never the SPY boot bars"
    );
    // Second read hits the per-boot cache and agrees byte-for-byte.
    let again = gateway
        .chart_bars_for("GLD", "1M")
        .expect("the cached months serve again");
    assert_eq!(again, gld, "cached months agree");
    let err = gateway
        .chart_bars_for("NOPE", "1M")
        .expect_err("a ticker with no months refuses");
    assert!(
        err.to_string().contains("NOPE"),
        "the refusal names the requested ticker: {err}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn shipped_root_serves_gld_months_by_symbol() {
    // End-to-end over the shipped data root: GLD parquet months serve GLD
    // reads with GLD closes (never the SPY boot bars), proving a picked
    // ticker loads instead of substituting or refusing.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs/historical");
    if !root.join("GLD").is_dir() {
        eprintln!("shipped root has no GLD tree; skipping");
        return;
    }
    let boot = boot_history();
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_path(root.to_string_lossy().into_owned())
        .history_symbol("SPY".to_owned())
        .build();
    let handle = serve_with_tape(config, boot.clone(), None, None)
        .await
        .expect("server boots over the shipped root");
    let gateway = handle.gateway();
    let gld = gateway
        .chart_bars_for("GLD", "1M")
        .expect("the shipped root serves GLD");
    assert!(!gld.rows.is_empty(), "GLD months serve bars");
    let spy_close = boot.last().map(|bar| bar.c);
    let gld_close = gld.rows.last().map(|row| row.close);
    assert_ne!(
        gld_close, spy_close.map(|c| c.to_string().parse().unwrap_or(f64::NAN)),
        "GLD serves its own closes: {gld_close:?} vs SPY {spy_close:?}"
    );
    handle.shutdown().await;
}
