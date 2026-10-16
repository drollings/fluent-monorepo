//! The tape arm is a substrate, not a second decision path.
//!
//! A store-backed run and a file-backed run over the **same bars** must reach
//! the same verdict on every bar: the store supplies the series, nothing
//! more. The decision trace is compared byte for byte, and the manifests are
//! compared with the additive tape fields cleared — a CSV run pins no tape
//! data, while a store run pins the read's digest and spans. Neither
//! difference touches a single decision.

use std::path::PathBuf;

use crate::backtest::HistorySource;
use crate::common_tape as tape;
use crate::report::RunManifest;
use crate::runner::run_backtest_from_source;
use protopyne_types::{Bar, Timeframe};

fn csv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv")
}

fn csv_bars() -> Vec<Bar> {
    crate::csv::load_bars_csv(&csv_path()).expect("vendored history loads")
}

/// The manifest's pre-tape projection: every additive field cleared, so the
/// shared pin (data, code, config, range, seed) can be compared across arms.
fn untaped(mut manifest: RunManifest) -> RunManifest {
    manifest.data_digest = None;
    manifest.data_first_ts = None;
    manifest.data_last_ts = None;
    manifest.data_bar_count = None;
    manifest.data_provenance = None;
    manifest
}

#[tokio::test]
async fn tape_and_csv_runs_over_the_same_bars_decide_identically() {
    let bars = csv_bars();
    let caps = tape::caps();
    let (store, key, request) = tape::seed(&bars, "SPY", Timeframe::M15, &caps);

    let strategy = tape::strategy();
    let risk = tape::risk();
    let grant = tape::grant("SPY");
    let venue = tape::venue_config("SPY", "spy-15m-v1");
    let runner = tape::runner_config("SPY");

    let csv_source = HistorySource::Files(vec![csv_path()]);
    let csv_run = run_backtest_from_source(
        &strategy, &risk, &grant, &venue, &runner, &csv_source, None, 7,
    )
    .await
    .expect("files run");

    let tape_source = HistorySource::Tape { key, request };
    let tape_run = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            run_backtest_from_source(
                &strategy,
                &risk,
                &grant,
                &venue,
                &runner,
                &tape_source,
                Some(&store),
                7,
            )
            .await
        })
        .await
        .expect("tape run");

    // The decision path is identical, bar for bar: the store is a substrate.
    assert_eq!(
        tape_run.decisions, csv_run.decisions,
        "a store-backed run must not change a single verdict"
    );
    // The shared pin is identical once the additive tape fields are cleared.
    assert_eq!(
        untaped(tape_run.manifest.clone()),
        untaped(csv_run.manifest.clone()),
        "the store changes provenance, never the reproducibility pin"
    );
    // Each arm pins exactly what it can: no digest for CSV, a digest and
    // provenance for the store.
    assert!(csv_run.manifest.data_digest.is_none(), "files pin no tape digest");
    assert!(tape_run.manifest.data_digest.is_some(), "store pins a digest");
    assert_eq!(tape_run.manifest.data_bar_count, Some(bars.len() as u64));
}

#[tokio::test]
async fn a_tape_read_without_its_store_refuses_loudly() {
    let bars = csv_bars();
    let caps = tape::caps();
    let (_, key, request) = tape::seed(&bars, "SPY", Timeframe::M15, &caps);
    let source = HistorySource::Tape { key, request };
    let error = crate::load_history(&source, None).expect_err("a tape read names its store");
    let message = error.to_string();
    assert!(
        message.contains("store"),
        "the refusal must name the missing store: {message}"
    );
}
