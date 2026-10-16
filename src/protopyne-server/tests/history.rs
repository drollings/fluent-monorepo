//! One history-loading surface: the ladder matrix, CSV/Parquet parity, the
//! files arm end to end, and the load-time tally golden.
//!
//! The ladder skips only `Io` (a missing or unreadable candidate) and stops
//! on every parse or structural refusal: a bad header, a bad row, a schema
//! mismatch, a mixed-ticker file, or an unknown extension. A corrupt artifact
//! is investigated, never skipped over.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::backtest::{HistorySource, load_history};
use crate::fold_frames;
use crate::history::{HistoryFormat, format_of, load_bars_candidates, load_bars_file};
use arrow::array::{Float64Array, Int64Array, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use chrono::{TimeZone, Utc};
use parquet::arrow::ArrowWriter;
use protopyne_types::{Bar, FoldEdges, Timeframe};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive as _;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join(name)
}

fn month_parquet() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs/historical/SPY/2026/03/SPY-2026-03-1M.parquet")
}

fn bar_at(ts_secs: i64, price: &str, volume: u64) -> Bar {
    let price: Decimal = price.parse().expect("decimal parses");
    Bar::new()
        .ts(Utc.timestamp_opt(ts_secs, 0).single().expect("in range"))
        .o(price)
        .h(price)
        .l(price)
        .c(price)
        .volume(volume)
        .maybe_adj(None)
        .build()
}

fn eight_bars() -> Vec<Bar> {
    // Eight contiguous M1 bars from a grid line: two whole five-minute
    // buckets plus a partial tail, so the tally golden has both cases.
    let base = 1_772_442_000_i64; // 2026-03-02T09:00:00Z
    (0..8)
        .map(|i| bar_at(base + i * 60, &format!("{}.00", 100 + i), 1_000 + i as u64))
        .collect()
}

fn write_csv_file(path: &Path, bars: &[Bar]) {
    let mut content = String::from("time,open,high,low,close,volume\n");
    for bar in bars {
        use std::fmt::Write as _;
        writeln!(
            content,
            "{},{},{},{},{},{}",
            bar.ts.timestamp(),
            bar.o,
            bar.h,
            bar.l,
            bar.c,
            bar.volume
        )
        .expect("writing to a String never fails");
    }
    std::fs::write(path, content).expect("csv fixture writes");
}

fn vendor_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new(
            "timestamp",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            false,
        ),
        Field::new("open", DataType::Float64, false),
        Field::new("high", DataType::Float64, false),
        Field::new("low", DataType::Float64, false),
        Field::new("close", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false),
        Field::new("ticker", DataType::Utf8, false),
    ]))
}

fn write_vendor_parquet(path: &Path, bars: &[Bar], ticker: &str) {
    let schema = vendor_schema();
    let ns: Vec<i64> =
        bars.iter().map(|bar| bar.ts.timestamp_nanos_opt().expect("in range")).collect();
    let ts = TimestampNanosecondArray::from(ns).with_timezone("UTC");
    let prices = |pick: fn(&Bar) -> Decimal| {
        Float64Array::from(
            bars.iter().map(|bar| pick(bar).to_f64().expect("representable")).collect::<Vec<_>>(),
        )
    };
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(ts),
            Arc::new(prices(|bar| bar.o)),
            Arc::new(prices(|bar| bar.h)),
            Arc::new(prices(|bar| bar.l)),
            Arc::new(prices(|bar| bar.c)),
            Arc::new(Float64Array::from(
                bars.iter().map(|bar| bar.volume as f64).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(vec![ticker; bars.len()])),
        ],
    )
    .expect("vendor batch builds");
    let file = std::fs::File::create(path).expect("parquet fixture creates");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("writer builds");
    writer.write(&batch).expect("parquet fixture writes");
    writer.close().expect("parquet fixture closes");
}

fn write_mixed_ticker_parquet(path: &Path) {
    let schema = vendor_schema();
    let base_ns = 1_772_442_000_000_000_000_i64;
    let ts =
        TimestampNanosecondArray::from(vec![base_ns, base_ns + 60_000_000_000]).with_timezone("UTC");
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(ts),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
            Arc::new(Float64Array::from(vec![10.0, 20.0])),
            Arc::new(StringArray::from(vec!["QQQ", "SPY"])),
        ],
    )
    .expect("mixed batch builds");
    let file = std::fs::File::create(path).expect("fixture creates");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("writer builds");
    writer.write(&batch).expect("writes");
    writer.close().expect("closes");
}

fn write_schema_mismatch_parquet(path: &Path) {
    // A market file without the `close` column: structurally wrong, never
    // skipped over.
    let schema = Arc::new(Schema::new(vec![
        Field::new("timestamp", DataType::Int64, false),
        Field::new("open", DataType::Float64, false),
        Field::new("high", DataType::Float64, false),
        Field::new("low", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![1_772_442_000_000i64])),
            Arc::new(Float64Array::from(vec![1.0])),
            Arc::new(Float64Array::from(vec![1.0])),
            Arc::new(Float64Array::from(vec![1.0])),
            Arc::new(Float64Array::from(vec![5.0])),
        ],
    )
    .expect("batch builds");
    let file = std::fs::File::create(path).expect("fixture creates");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("writer builds");
    writer.write(&batch).expect("writes");
    writer.close().expect("closes");
}

#[test]
fn format_of_dispatches_on_extension() {
    assert_eq!(
        format_of(Path::new("history.csv")).expect("csv dispatches"),
        HistoryFormat::Csv
    );
    assert_eq!(
        format_of(Path::new("history.parquet")).expect("parquet dispatches"),
        HistoryFormat::Parquet
    );
}

#[test]
fn format_of_refuses_unknown_and_missing_extensions_by_name() {
    let err = format_of(Path::new("history.json")).expect_err("json refuses");
    assert!(err.to_string().contains("json"), "the refusal names the extension: {err}");
    let err = format_of(Path::new("history")).expect_err("bare refuses");
    let text = err.to_string();
    assert!(
        text.contains(".csv") && text.contains(".parquet"),
        "the refusal names what it accepts: {text}"
    );
}

#[test]
fn a_missing_candidate_skips_to_the_next() {
    let missing = data("does-not-exist.csv");
    let bars =
        load_bars_candidates(&[missing, data("SPY_15M.csv")]).expect("a missing file skips");
    assert_eq!(bars.len(), 11, "falls through to the usable file");
}

#[test]
fn an_unreadable_candidate_skips_to_the_next() {
    // A directory where a file should be: the read fails as `Io`, which is
    // the one skippable refusal.
    let dir = tempfile::tempdir().expect("tempdir");
    let unreadable = dir.path().join("history.csv");
    std::fs::create_dir(&unreadable).expect("a directory named like a file");
    let bars = load_bars_candidates(&[unreadable, data("SPY_15M.csv")])
        .expect("an unreadable file skips");
    assert_eq!(bars.len(), 11, "falls through to the usable file");
}

#[test]
fn a_bad_header_stops_the_walk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("bad.csv");
    std::fs::write(&bad, "time,open,high\n1,2,3").expect("fixture writes");
    let err = load_bars_candidates(&[bad, data("SPY_15M.csv")])
        .expect_err("a bad header stops the walk");
    assert!(
        matches!(err, ServerError::BadHeader(_)),
        "a bad header stops in the header shape, got: {err:?}"
    );
}

#[test]
fn a_bad_row_stops_the_walk() {
    // A corrupt artifact is investigated, not skipped over: the ladder stops
    // on the row refusal instead of serving the next candidate.
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("bad.csv");
    std::fs::write(
        &bad,
        "time,open,high,low,close,volume\n1767344400,686.03,683.82,686.62,686.56,19063",
    )
    .expect("fixture writes");
    assert!(matches!(
        load_bars_file(&bad),
        Err(ServerError::BadRow { .. })
    ));
    let err = load_bars_candidates(&[bad, data("SPY_15M.csv")])
        .expect_err("a bad row stops the walk");
    assert!(
        matches!(err, ServerError::BadRow { .. }),
        "a bad row stops in the row shape, got: {err:?}"
    );
}

#[test]
fn a_schema_mismatch_stops_the_walk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("no_close.parquet");
    write_schema_mismatch_parquet(&bad);
    let err = load_bars_candidates(&[bad.clone(), month_parquet()])
        .expect_err("a schema mismatch stops the walk");
    match err {
        ServerError::Parquet { path, .. } => {
            assert_eq!(path, bad.display().to_string(), "the refusal names the candidate");
        }
        other => panic!("must stop in the Parquet shape, got: {other:?}"),
    }
}

#[test]
fn a_mixed_ticker_file_stops_the_walk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mixed = dir.path().join("mixed.parquet");
    write_mixed_ticker_parquet(&mixed);
    let err = load_bars_candidates(&[mixed.clone(), month_parquet()])
        .expect_err("a mixed-ticker file stops the walk");
    match err {
        ServerError::Parquet { path, reason } => {
            assert_eq!(path, mixed.display().to_string(), "the refusal names the candidate");
            assert!(reason.contains("ticker"), "the refusal names the fix: {reason}");
        }
        other => panic!("must stop in the Parquet shape, got: {other:?}"),
    }
}

#[test]
fn an_unknown_extension_stops_the_walk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let json = dir.path().join("history.json");
    std::fs::write(&json, "{}").expect("fixture writes");
    let err = load_bars_candidates(&[json, data("SPY_15M.csv")])
        .expect_err("an unknown extension stops the walk");
    assert!(
        err.to_string().contains("json"),
        "the refusal names the extension: {err}"
    );
}

#[test]
fn a_missing_parquet_candidate_skips_to_the_next() {
    let missing = month_parquet().with_file_name("does-not-exist.parquet");
    let bars =
        load_bars_candidates(&[missing, month_parquet()]).expect("a missing parquet skips");
    assert_eq!(bars.len(), 20434, "falls through to the usable file");
}

#[test]
fn a_corrupt_parquet_candidate_stops_the_walk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let corrupt = dir.path().join("corrupt.parquet");
    std::fs::write(&corrupt, "this is not parquet").expect("fixture writes");
    let err = load_bars_candidates(&[corrupt.clone(), month_parquet()])
        .expect_err("a corrupt parquet stops the walk");
    match err {
        ServerError::Parquet { path, .. } => {
            assert_eq!(path, corrupt.display().to_string());
        }
        other => panic!("must stop in the Parquet shape, got: {other:?}"),
    }
}

#[test]
fn exhaustion_without_a_usable_file_refuses() {
    let err =
        load_bars_candidates(&[data("does-not-exist.csv")]).expect_err("exhaustion refuses");
    assert!(err.to_string().contains("no candidate"), "the refusal says so: {err}");
    assert!(load_bars_candidates(&[]).is_err(), "an empty candidate set refuses");
}

#[test]
fn a_csv_rendering_and_a_parquet_rendering_of_one_series_load_identically() {
    let bars = eight_bars();
    let dir = tempfile::tempdir().expect("tempdir");
    let csv_path = dir.path().join("same.csv");
    let parquet_path = dir.path().join("same.parquet");
    write_csv_file(&csv_path, &bars);
    write_vendor_parquet(&parquet_path, &bars, "SPY");

    let via_csv = load_bars_file(&csv_path).expect("csv loads");
    let via_parquet = load_bars_file(&parquet_path).expect("parquet loads");
    assert_eq!(via_csv, bars, "the csv rendering reads back verbatim");
    assert_eq!(via_parquet, via_csv, "both encodings serve the same series");
}

#[test]
fn a_files_source_loads_end_to_end_and_refuses_empty() {
    let loaded = load_history(&HistorySource::Files(vec![data("SPY_15M.csv")]), None)
        .expect("the files arm loads");
    assert_eq!(loaded.bars.len(), 11);
    assert!(loaded.provenance.is_empty(), "a file arm names no planner span");

    let loaded = load_history(&HistorySource::Files(vec![month_parquet()]), None)
        .expect("a parquet candidate loads through the same arm");
    assert_eq!(loaded.bars.len(), 20434);

    assert!(
        load_history(&HistorySource::Files(Vec::new()), None).is_err(),
        "an empty candidate set refuses like an empty tape"
    );
    assert!(
        load_history(
            &HistorySource::Files(vec![data("does-not-exist.csv")]),
            None
        )
        .is_err(),
        "an unusable candidate set refuses"
    );
}

#[test]
fn the_load_time_tally_conserves_volume_over_whole_buckets() {
    // Ten contiguous minute bars: two whole five-minute buckets and one whole
    // ten-minute bucket, so conservation is exact and countable.
    let base = 1_772_442_000_i64;
    let bars: Vec<Bar> =
        (0..10).map(|i| bar_at(base + i * 60, &format!("{}.00", 100 + i), 1_000)).collect();
    let frames = fold_frames(
        &bars,
        Timeframe::M1,
        &[Timeframe::M5, Timeframe::M10],
        FoldEdges::CompleteOnly,
    )
    .expect("whole-multiple frames fold");
    assert_eq!(frames[0].1.len(), 2, "ten minutes fill two whole five-minute buckets");
    assert_eq!(frames[1].1.len(), 1, "ten minutes fill one whole ten-minute bucket");
    let base_volume: u64 = bars.iter().map(|bar| bar.volume).sum();
    for (target, frame) in &frames {
        let folded: u64 = frame.iter().map(|bar| bar.volume).sum();
        assert_eq!(folded, base_volume, "{target:?} conserves volume");
        let mut last: Option<&Bar> = None;
        for bar in frame {
            protopyne_types::validate_new_bar(last, bar).expect("every frame is clean");
            last = Some(bar);
        }
    }
    assert_eq!(frames[0].1[0].o, bars[0].o, "the first bucket opens at the first bar");
    assert_eq!(frames[0].1[1].c, bars[9].c, "the last bucket closes at the last bar");
}

#[test]
fn the_tally_omits_the_month_s_torn_buckets() {
    // Overnight gaps tear every bucket they touch: the month folds to 3754
    // whole five-minute buckets, 1129 whole quarter-hours, and 242 whole
    // hours (pinned exactly in `parquet.rs`). Complete-only tally omits the
    // torn ones; this test pins the omission rule (emitted < base) rather
    // than restating those counts.
    let bars = load_bars_file(&month_parquet()).expect("the month loads");
    assert_eq!(bars.len(), 20434);
    let frames = fold_frames(
        &bars,
        Timeframe::M1,
        &[Timeframe::M5, Timeframe::M15, Timeframe::H1],
        FoldEdges::CompleteOnly,
    )
    .expect("whole-multiple frames fold");
    assert_eq!(frames[0].1.len(), 3754, "only whole five-minute buckets emit");
    assert_eq!(frames[1].1.len(), 1129, "only whole quarter-hours emit");
    assert_eq!(frames[2].1.len(), 242, "only whole hours emit");
    let base_volume: u64 = bars.iter().map(|bar| bar.volume).sum();
    let emitted: u64 = frames[0].1.iter().map(|bar| bar.volume).sum();
    assert!(
        emitted < base_volume,
        "omitted buckets carry volume away with them: {emitted} < {base_volume}"
    );
}
