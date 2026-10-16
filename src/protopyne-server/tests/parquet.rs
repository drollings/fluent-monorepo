//! Parquet history ingest over a hermetic trimmed fixture.
//!
//! The trimmed `SPY_1M_trim.parquet` holds the committed month's first 120
//! one-minute bars, so the goldens below pin its bar count and its first and
//! last bars without reading the 20k-row operator file. The fixture folds
//! into whole-multiple frames with volume conserved, and the ladder stops on
//! a structurally wrong candidate instead of skipping it.

use std::path::PathBuf;

use super::*;
use crate::backtest::{LoadedHistory, load_history, HistorySource};
use crate::history::{load_bars_candidates, load_bars_file};
use crate::service::{serve, ServerConfig};
use protopyne_store::read_ohlcv_parquet;
use protopyne_types::{validate_new_bar, Bar, FoldEdges, Timeframe};
use protopyne_view::TerminalGateway;
use rust_decimal::Decimal;

fn trim_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("SPY_1M_trim.parquet")
}

fn dec(text: &str) -> Decimal {
    text.parse().expect("decimal fixture parses")
}

fn load_trim() -> Vec<Bar> {
    load_bars_file(&trim_path()).expect("the trimmed fixture loads")
}

fn assert_ascending(bars: &[Bar]) {
    let mut last: Option<&Bar> = None;
    for bar in bars {
        validate_new_bar(last, bar).expect("every frame is a valid committed series");
        last = Some(bar);
    }
}

#[test]
fn the_trimmed_fixture_reads_as_is() {
    let read = read_ohlcv_parquet(&trim_path(), Some("SPY")).expect("trim reads");
    assert_eq!(read.bars.len(), 120, "the trim holds 120 committed bars");
    assert_eq!(read.restatements_merged, 0, "the trim carries no restatements");
    assert_eq!(read.rows_filtered, 0, "the file is SPY-only");

    let bars = read.bars;
    assert_eq!(bars[0].ts.to_rfc3339(), "2026-03-02T09:00:00+00:00");
    assert_eq!(bars[0].o, dec("677.66"));
    assert_eq!(bars[0].h, dec("677.66"));
    assert_eq!(bars[0].l, dec("676.54"));
    assert_eq!(bars[0].c, dec("677.00"));
    assert_eq!(bars[0].volume, 8485);
    let last = bars.last().expect("the trim has bars");
    assert_eq!(last.ts.to_rfc3339(), "2026-03-02T11:00:00+00:00");
    assert_eq!(last.o, dec("679.50"));
    assert_eq!(last.c, dec("679.54"));
    assert_eq!(last.volume, 1426);
    assert_ascending(&bars);
}

#[test]
fn the_trim_tallies_into_frames_with_torn_buckets_omitted() {
    let bars = load_trim();
    let frames = fold_frames(
        &bars,
        Timeframe::M1,
        &[Timeframe::M5, Timeframe::M15, Timeframe::H1],
        FoldEdges::CompleteOnly,
    )
    .expect("whole-multiple frames fold");

    for (target, frame) in &frames {
        assert_ascending(frame);
        assert!(!frame.is_empty(), "{target:?} frame holds bars");
    }

    // The trim misses 10:36, so the buckets spanning it are torn and omitted:
    // 24 five-minute buckets minus the 10:35 one, 8 quarter-hours minus the
    // 10:30 one, and only the whole 09:00 hour. Omitted buckets carry their
    // volume away with them — session data tallies to what is whole, never
    // to a fabricated partial bar.
    assert_eq!(frames[0].1.len(), 23, "the torn 10:35 bucket is omitted");
    assert_eq!(frames[1].1.len(), 7, "the torn 10:30 bucket is omitted");
    assert_eq!(frames[2].1.len(), 1, "the torn 10:00 hour is omitted");
    for (target, frame) in &frames {
        assert!(frame.len() <= bars.len(), "{target:?} can only merge bars");
    }

    // Spot-check the first five-minute bucket against its five minute bars:
    // 09:00 UTC starts a bucket, so the fold is exact at the open.
    let m5 = &frames[0].1;
    assert_eq!(m5[0].o, bars[0].o, "bucket open is the first bar's open");
    assert_eq!(m5[0].c, bars[4].c, "bucket close is the last bar's close");
    let five: u64 = bars[0..5].iter().map(|bar| bar.volume).sum();
    assert_eq!(m5[0].volume, five, "bucket volume sums its children");
}

#[test]
fn fold_frames_refuses_a_non_multiple_target() {
    let bars = load_trim();
    let err = fold_frames(&bars, Timeframe::M15, &[Timeframe::M10], FoldEdges::CompleteOnly)
        .expect_err("10 minutes is not a whole multiple of 15");
    assert!(err.to_string().contains("whole multiple"), "{err}");
}

#[test]
fn the_ladder_skips_a_missing_candidate_and_stops_on_a_wrong_one() {
    let missing = trim_path().with_file_name("does-not-exist.parquet");
    let bars = load_bars_candidates(&[missing.clone(), trim_path()])
        .expect("a missing candidate skips to the usable one");
    assert_eq!(bars.len(), 120);

    let dir = tempfile::tempdir().expect("tempdir");
    let corrupt_path = dir.path().join("corrupt.parquet");
    std::fs::write(&corrupt_path, "this is not parquet").expect("write");
    let err = load_bars_candidates(&[corrupt_path.clone(), trim_path()])
        .expect_err("a structurally wrong candidate stops the walk");
    match err {
        ServerError::Parquet { path, .. } => {
            assert_eq!(path, corrupt_path.display().to_string());
        }
        other => panic!("must stop in the Parquet shape, got: {other:?}"),
    }
}

#[test]
fn a_files_history_source_loads_through_the_single_entry_point() {
    let loaded: LoadedHistory = load_history(&HistorySource::Files(vec![trim_path()]), None)
        .expect("the files arm loads");
    assert_eq!(loaded.bars.len(), 120);
    assert!(
        loaded.provenance.is_empty(),
        "a file arm names no planner provenance"
    );
}

#[tokio::test]
async fn a_one_minute_boot_history_resamples_from_its_actual_base() {
    let bars = load_trim();
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_timeframe("1M".to_owned())
        .build();
    let handle = serve(config, bars.clone()).await.expect("server boots");
    let gateway = handle.gateway();

    let base = gateway.chart_bars_for("SPY", "1M").expect("1M read");
    assert_eq!(
        base.rows.len(),
        bars.len(),
        "the base serves unchanged at its own spacing"
    );

    // Complete-only edges over the trim: whole buckets emit, short and torn
    // buckets are omitted. The trim opens on a grid line (09:00 UTC), so the
    // first bucket is whole on every grid.
    let five = gateway.chart_bars_for("SPY", "5M").expect("5M read");
    assert!(!five.rows.is_empty(), "5M serves the whole five-minute buckets");
    assert!(five.rows.len() <= bars.len());
    let fifteen = gateway.chart_bars_for("SPY", "15M").expect("15M read");
    assert!(!fifteen.rows.is_empty(), "15M serves the whole quarter-hours");
    let hour = gateway.chart_bars_for("SPY", "1H").expect("1H read");
    assert!(!hour.rows.is_empty(), "1H serves the whole hours");
    let day = gateway.chart_bars_for("SPY", "1D").expect("1D read");
    assert_eq!(
        day.rows.len(),
        bars.len(),
        "a history shorter than one daily group serves the base unchanged rather than an empty window"
    );

    let expected_open = to_f64(bars[0].o);
    assert!(
        (five.rows[0].open - expected_open).abs() < 1e-9,
        "the first folded bar opens at the first whole bucket's open"
    );
    assert_eq!(
        five.rows[0].time,
        bars[0].ts.timestamp(),
        "a folded row stamps its bucket start, never the last child"
    );

    let overlay = gateway
        .chart_overlays_for("SPY", "5M", "ema", 10)
        .expect("5M overlay read");
    assert_eq!(
        overlay.values.len(),
        five.rows.len(),
        "overlay agrees with bars"
    );

    handle.shutdown().await;
}

fn to_f64(value: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive as _;
    value.to_f64().expect("display conversion")
}

fn write_csv(path: &std::path::Path) {
    std::fs::write(
        path,
        "time,open,high,low,close,volume\n\
         1772442000,677.66,677.66,676.54,677.00,8485\n\
         1772442060,676.91,677.45,676.74,677.45,24153\n",
    )
    .expect("csv fixture writes");
}

#[test]
fn load_bars_file_dispatches_csv_by_extension() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("two.csv");
    write_csv(&path);

    let bars = load_bars_file(&path).expect("csv dispatches");
    assert_eq!(bars.len(), 2);
    assert_eq!(bars[0].volume, 8485);
}

#[test]
fn load_bars_file_dispatches_parquet_by_extension() {
    let bars = load_bars_file(&trim_path()).expect("parquet dispatches");
    assert_eq!(bars, load_trim(), "dispatch reads what the parquet loader reads");
}

#[test]
fn load_bars_file_refuses_unknown_and_missing_extensions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let json = dir.path().join("history.json");
    std::fs::write(&json, "{}").expect("json fixture writes");
    let err = load_bars_file(&json).expect_err("unknown extension refuses");
    assert!(
        err.to_string().contains("json"),
        "the refusal names the extension: {err}"
    );

    let bare = dir.path().join("history");
    std::fs::write(&bare, "{}").expect("bare fixture writes");
    let err = load_bars_file(&bare).expect_err("missing extension refuses");
    assert!(
        err.to_string().contains("csv") && err.to_string().contains("parquet"),
        "the refusal names what it accepts: {err}"
    );
}
