//! Tape boot: a file-backed store behind the serving gateway.
//!
//! The boot opens the store at its root, asserts the permit binding, and
//! serves the tape's series with observable provenance: when the tape holds
//! a series unlike the boot history, the symbol-scoped read serves the
//! tape's closes and counts one serve. A permit bound to another root is
//! refused (fallback plus the denied count), and an empty catalog refuses
//! boot loudly instead of serving every read as a silent fallback.

use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::common_tape as tape;
use crate::service::open_tape_reader;
use chrono::DateTime;
use protopyne_store::StoreStorage;
use protopyne_types::Timeframe;
use protopyne_view::TerminalGateway;

const BASE_MS: i64 = 1_767_974_400_000;

fn boot_history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

/// `count` flat 15-minute bars on a fixed stamp grid: contiguous, whole, and
/// two-decimal, so the tape stores every row. The price makes provenance
/// observable — a read serves either the tape's closes or the boot
/// history's, never a mixture.
fn flat_bars(count: i64, price: &str) -> Vec<Bar> {
    (0..count)
        .map(|step| {
            let price = price.parse().expect("decimal");
            Bar::new()
                .ts(DateTime::from_timestamp_millis(BASE_MS + step * 900_000).expect("in range"))
                .o(price)
                .h(price)
                .l(price)
                .c(price)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

#[tokio::test]
async fn boot_with_tape_serves_the_tape_series() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let tape_bars = flat_bars(24, "200");
    tape::seed_file(&root, &tape_bars, "SPY", Timeframe::M15, tape::VENUE);
    let reader = open_tape_reader(&root).expect("boot opens the seeded store");
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, boot_history(), Some(reader), None)
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
            .all(|row| (row.close - 200.0).abs() < 1e-6),
        "the tape's closes served"
    );
    let stats = handle.tape_serve_stats();
    assert_eq!(stats.served, 1, "the tape serve is counted");
    assert_eq!(stats.last_miss, None, "no miss on record");
    handle.shutdown().await;
}

#[tokio::test]
async fn wrong_root_permit_falls_back_and_counts_denied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let other = dir.path().join("other");
    let tape_bars = flat_bars(24, "200");
    let store = tape::seed_file(&root, &tape_bars, "SPY", Timeframe::M15, tape::VENUE);
    // A permit bound to another root, installed as the reader's capability
    // set: presence is satisfied, but the tape's own permit is absent, so
    // the read is denied rather than served.
    let foreign = StoreStorage::new(other);
    let reader = TapeReader {
        store: Arc::new(store),
        caps: fluent_wvr::CapabilitySet::new().with(foreign.clone()),
        permit: foreign,
        venue: tape::VENUE.to_owned(),
        limit: crate::DEFAULT_TAPE_LIMIT,
    };
    let boot = boot_history();
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

#[test]
fn empty_catalog_refuses_boot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    protopyne_store::MarketTape::open(&root).expect("empty store opens");
    let Err(err) = open_tape_reader(&root) else {
        panic!("an empty catalog refuses boot")
    };
    assert!(
        err.to_string().contains("no markets"),
        "the refusal names the empty catalog: {err}"
    );
}
