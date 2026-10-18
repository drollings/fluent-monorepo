//! The workspace plans symbol/timeframe switches through a store-backed
//! gateway, and still seeds and paints headlessly without one.
//!
//! Switching a symbol pulls its history through the gateway; with a durable
//! tape behind the gateway the pull plans and reads the store, and the pane
//! cache recomputes from the same rows. The control group: a gateway-less
//! workspace seeded with rows paints unchanged, so the store path is
//! additive and never required.

use std::sync::Arc;

use super::*;
use chrono::DateTime;
use protopyne_chart_model::ChartRow;
use protopyne_server::{serve_with_tape, ServerConfig, TapeReader, DEFAULT_TAPE_LIMIT};
use protopyne_store::{
    AssetClass, CandleStore as _, MarketTape, PriceScale, StoreStorage, TapeKey, WritePolicy,
};
use protopyne_types::{Bar, Timeframe};

const VENUE: &str = "test-venue";
const BASE_MS: i64 = 1_767_974_400_000;

/// A flat 15-minute bar series on the fixed stamp grid: contiguous, whole,
/// and two-decimal, so the tape stores every row. `count` bars at `price`,
/// so boot history and tape can hold visibly different series.
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

/// The boot history the server is handed: twelve flat bars at 100.
fn boot_bars() -> Vec<Bar> {
    flat_bars(12, "100")
}

/// The tape series: twenty-four flat bars at 200. Deliberately unlike the
/// boot history, so a read's provenance (tape vs. fallback) is observable.
fn tape_bars() -> Vec<Bar> {
    flat_bars(24, "200")
}

/// The capability set an ephemeral tape is gated on. One spelling, shared by
/// the seeding step and the reader the gateway holds.
fn caps() -> fluent_wvr::CapabilitySet {
    fluent_wvr::CapabilitySet::new().with(StoreStorage::new(":memory:".into()))
}

fn seed_tape(bars: &[Bar]) -> MarketTape {
    let tape = MarketTape::open_in_memory().expect("ephemeral tape opens");
    fluent_wvr::CURRENT_CAPS.sync_scope(caps(), || {
        let symbol_id = tape
            .register(
                "SPY",
                VENUE,
                AssetClass::Stock,
                PriceScale::of(2).expect("valid scale"),
                "UTC",
                Timeframe::M15,
            )
            .expect("registration succeeds");
        let key = TapeKey {
            symbol_id,
            timeframe: Timeframe::M15,
        };
        tape.append(key, bars, WritePolicy::InsertIfAbsent)
            .expect("seed append succeeds");
    });
    tape
}

fn sample_rows() -> Vec<ChartRow> {
    (0..8i64)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: 1_700_000_000 + i * 900,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 1_000 + i as u64,
            }
        })
        .collect()
}

#[tokio::test]
async fn a_store_backed_switch_replans_and_paints_headlessly() {
    let boot = boot_bars();
    let tape = tape_bars();
    let reader = TapeReader {
        store: Arc::new(seed_tape(&tape)),
        caps: caps(),
        permit: StoreStorage::new(":memory:".into()),
        venue: VENUE.to_owned(),
        limit: DEFAULT_TAPE_LIMIT,
    };
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve_with_tape(config, boot, Some(reader))
        .await
        .expect("server boots with a tape");
    let gateway = handle.gateway();

    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    // The store and boot history are both 15-minute; select that key so the
    // store-backed read is served natively (the five-minute default cannot be
    // synthesized from a 15-minute base).
    workspace
        .switch_timeframe(TimeframeKey::M15, &gateway)
        .expect("the store serves M15");
    workspace
        .switch_symbol("SPY", &gateway)
        .expect("a store-backed symbol switch pulls history");
    assert_eq!(workspace.active_symbol(), "SPY", "the symbol switched");
    // Provenance: the rows are the tape's 24 bars at 200, not the boot
    // history's 12 at 100. A silent fallback would serve the wrong series.
    assert_eq!(
        workspace.rows().len(),
        tape.len(),
        "the tape's series served the switch"
    );
    assert!(
        workspace
            .rows()
            .iter()
            .all(|row| (row.close - 200.0).abs() < 1e-6),
        "the store served the switch, not the boot history"
    );
    assert!(workspace.cached_series() > 0, "the pane cache recomputed");

    // A full frame renders headlessly over the store-backed rows.
    egui::__run_test_ui(|ui| {
        paint_workspace(ui, &mut workspace, Some(&gateway)).expect("the workspace paints");
    });
    handle.shutdown().await;
}

#[test]
fn a_gateway_less_workspace_seeds_and_paints_unchanged() {
    let rows = sample_rows();
    let mut workspace = ChartWorkspace::with_rows(rows.clone()).expect("the workspace seeds");
    assert_eq!(
        workspace.rows().len(),
        rows.len(),
        "the seeded rows are the workspace's"
    );
    egui::__run_test_ui(|ui| {
        paint_workspace(ui, &mut workspace, None).expect("the workspace paints without a gateway");
    });
}
