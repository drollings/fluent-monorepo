//! Parquet history ingest: the committed month reads as-is, folds into
//! whole-multiple frames with volume conserved, and the ladder stops on a
//! structurally wrong candidate instead of skipping it.
//!
//! The committed `SPY-2026-03-1M.parquet` is the user-facing artifact: the
//! goldens below pin its bar count, its restatement count, and its first and
//! last bars, so a reader change that silently drops or reorders rows fails
//! here rather than on a chart.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::*;
use crate::backtest::{load_history, HistorySource, LoadedHistory};
use crate::service::{serve, ServerConfig};
use protopyne_store::read_ohlcv_parquet;
use protopyne_types::{validate_new_bar, Bar, Timeframe};
use protopyne_view::TerminalGateway;
use rust_decimal::Decimal;

fn month_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs/historical/SPY-2026-03-1M.parquet")
}

fn dec(text: &str) -> Decimal {
    text.parse().expect("decimal fixture parses")
}

fn load_month() -> Vec<Bar> {
    load_bars_parquet(&month_path()).expect("the committed month loads")
}

fn assert_ascending(bars: &[Bar]) {
    let mut last: Option<&Bar> = None;
    for bar in bars {
        validate_new_bar(last, bar).expect("every frame is a valid committed series");
        last = Some(bar);
    }
}

#[test]
fn the_committed_month_reads_as_is() {
    let read = read_ohlcv_parquet(&month_path(), Some("SPY")).expect("month reads");
    // 20_437 file rows minus 3 restatement pairs = 20_434 committed bars;
    // the file's tail carries three repeated stamps.
    assert_eq!(read.bars.len(), 20_434, "rows merge into committed bars");
    assert_eq!(
        read.restatements_merged, 3,
        "the vendor tail restates 3 stamps"
    );
    assert_eq!(read.rows_filtered, 0, "the file is SPY-only");

    let bars = read.bars;
    assert_eq!(bars[0].ts.to_rfc3339(), "2026-03-02T09:00:00+00:00");
    assert_eq!(bars[0].o, dec("677.66"));
    assert_eq!(bars[0].h, dec("677.66"));
    assert_eq!(bars[0].l, dec("676.54"));
    assert_eq!(bars[0].c, dec("677.00"));
    assert_eq!(bars[0].volume, 8485);
    let last = bars.last().expect("the month has bars");
    assert_eq!(last.ts.to_rfc3339(), "2026-03-31T23:59:00+00:00");
    assert_ascending(&bars);
}

#[test]
fn the_month_tallies_into_frames_with_volume_conserved() {
    let bars = load_month();
    let frames = fold_frames(
        &bars,
        Timeframe::M1,
        &[
            Timeframe::M5,
            Timeframe::M15,
            Timeframe::H1,
            Timeframe::H4,
            Timeframe::D1,
        ],
    )
    .expect("whole-multiple frames fold");

    for (target, frame) in &frames {
        assert_ascending(frame);
        assert!(!frame.is_empty(), "{target:?} frame holds bars");
    }

    // Volume conservation is the tally's core invariant: folding regroups
    // bars, it never drops or duplicates their volume.
    let base_volume: u64 = bars.iter().map(|bar| bar.volume).sum();
    for (target, frame) in &frames {
        let folded: u64 = frame.iter().map(|bar| bar.volume).sum();
        assert_eq!(folded, base_volume, "{target:?} conserves volume");
        assert!(frame.len() <= bars.len(), "{target:?} can only merge bars");
    }

    // Frame counts follow from the bucket grid over the month's stamps.
    let m5 = &frames[0].1;
    assert_eq!(m5.len(), bucket_count(&bars, Timeframe::M5));
    assert_eq!(frames[1].1.len(), bucket_count(&bars, Timeframe::M15));
    assert_eq!(frames[2].1.len(), bucket_count(&bars, Timeframe::H1));
    assert_eq!(frames[3].1.len(), bucket_count(&bars, Timeframe::H4));
    // The month spans 23 UTC calendar dates of data (weekend-only stretches
    // hold a short post-market tail); the daily frame is UTC-anchored.
    assert_eq!(frames[4].1.len(), 23, "one daily bar per covered UTC date");

    // Spot-check the first five-minute bucket against its five minute bars:
    // 09:00 UTC starts a bucket, so the fold is exact at the open.
    assert_eq!(m5[0].o, bars[0].o, "bucket open is the first bar's open");
    assert_eq!(m5[0].c, bars[4].c, "bucket close is the last bar's close");
    let five: u64 = bars[0..5].iter().map(|bar| bar.volume).sum();
    assert_eq!(m5[0].volume, five, "bucket volume sums its children");
}

#[test]
fn fold_frames_refuses_a_non_multiple_target() {
    let bars = load_month();
    let err = fold_frames(&bars, Timeframe::M15, &[Timeframe::M10])
        .expect_err("10 minutes is not a whole multiple of 15");
    assert!(err.to_string().contains("whole multiple"), "{err}");
}

#[test]
fn the_ladder_skips_a_missing_candidate_and_stops_on_a_wrong_one() {
    use std::io::Write as _;
    let missing = month_path().with_file_name("does-not-exist.parquet");
    let bars = load_bars_parquet_with_fallback(&[missing.clone(), month_path()])
        .expect("a missing candidate skips to the usable one");
    assert_eq!(bars.len(), 20_434);

    let mut corrupt = tempfile::NamedTempFile::new().expect("temp file");
    writeln!(corrupt, "this is not parquet").expect("write");
    let corrupt_path = corrupt.path().to_path_buf();
    let err = load_bars_parquet_with_fallback(&[corrupt_path.clone(), month_path()])
        .expect_err("a structurally wrong candidate stops the walk");
    match err {
        ServerError::Parquet { path, .. } => {
            assert_eq!(path, corrupt_path.display().to_string());
        }
        other => panic!("must stop in the Parquet shape, got: {other:?}"),
    }
}

#[test]
fn a_parquet_history_source_loads_through_the_single_entry_point() {
    let loaded: LoadedHistory = load_history(&HistorySource::Parquet(vec![month_path()]), None)
        .expect("the parquet arm loads");
    assert_eq!(loaded.bars.len(), 20_434);
    assert!(
        loaded.provenance.is_empty(),
        "a file arm names no planner provenance"
    );
}

#[tokio::test]
async fn a_one_minute_boot_history_resamples_from_its_actual_base() {
    let bars = load_month();
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

    // Complete-only edges over the committed month: whole buckets emit,
    // short and torn buckets are omitted. The month opens on a grid line
    // (Monday 09:00 UTC), so the first bucket is whole on every grid.
    let five = gateway.chart_bars_for("SPY", "5M").expect("5M read");
    assert_eq!(
        five.rows.len(),
        3_754,
        "5M serves the whole five-minute buckets, not the trailing count"
    );
    let fifteen = gateway.chart_bars_for("SPY", "15M").expect("15M read");
    assert_eq!(fifteen.rows.len(), 1_129, "15M serves the whole quarter-hours");
    let hour = gateway.chart_bars_for("SPY", "1H").expect("1H read");
    assert_eq!(
        hour.rows.len(),
        242,
        "1H serves the whole hours: the M15-base bug would serve 4-minute hours"
    );
    let day = gateway.chart_bars_for("SPY", "1D").expect("1D read");
    assert!(
        day.rows.is_empty(),
        "no UTC date holds 1,440 minute children, so the daily read is empty rather than partial"
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
    assert_eq!(
        hour.rows[0].time,
        bars[0].ts.timestamp(),
        "every grid stamps the bucket start"
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

fn bucket_count(bars: &[Bar], tf: Timeframe) -> usize {
    let step = tf.duration_ms();
    let buckets: BTreeSet<i64> = bars
        .iter()
        .map(|bar| bar.ts.timestamp_millis().div_euclid(step))
        .collect();
    buckets.len()
}

fn to_f64(value: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive as _;
    value.to_f64().expect("display conversion")
}

fn write_csv(path: &std::path::Path) {
    std::fs::write(
        path,
        "time,open,high,low,close,Volume\n\
         1772442000,677.66,677.66,676.54,677.00,8485\n\
         1772442060,676.91,677.45,676.74,677.45,24153\n",
    )
    .expect("csv fixture writes");
}

#[test]
fn load_bars_any_dispatches_csv_by_extension() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("two.csv");
    write_csv(&path);

    let bars = load_bars_any(&path).expect("csv dispatches");
    assert_eq!(bars.len(), 2);
    assert_eq!(bars[0].volume, 8485);
}

#[test]
fn load_bars_any_dispatches_parquet_by_extension() {
    let bars = load_bars_any(&month_path()).expect("parquet dispatches");
    assert_eq!(bars, load_month(), "dispatch reads what the parquet loader reads");
}

#[test]
fn load_bars_any_refuses_unknown_and_missing_extensions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let json = dir.path().join("history.json");
    std::fs::write(&json, "{}").expect("json fixture writes");
    let err = load_bars_any(&json).expect_err("unknown extension refuses");
    assert!(
        err.to_string().contains("json"),
        "the refusal names the extension: {err}"
    );

    let bare = dir.path().join("history");
    std::fs::write(&bare, "{}").expect("bare fixture writes");
    let err = load_bars_any(&bare).expect_err("missing extension refuses");
    assert!(
        err.to_string().contains("csv") && err.to_string().contains("parquet"),
        "the refusal names what it accepts: {err}"
    );
}
