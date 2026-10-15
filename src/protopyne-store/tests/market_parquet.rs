//! Market-data Parquet ingest: the announced vendor schema reads as-is,
//! restatements merge, and every malformed shape refuses with its reason.
//!
//! Fixtures are written with the Arrow/Parquet writer at test time — the
//! vendor schema (nanosecond UTC timestamps, float prices, optional ticker)
//! and the store's own artifact shape (millisecond integers, decimal
//! prices) — so the reader is pinned against both encodings without
//! committing binary fixtures.

use std::path::Path;
use std::sync::Arc;

use crate::market_parquet::read_ohlcv_parquet;
use arrow::array::{
    Decimal128Array, Float64Array, Int64Array, StringArray, TimestampNanosecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use protopyne_types::Bar;
use rust_decimal::Decimal;

/// One vendor row: (epoch nanoseconds, o, h, l, c, volume, ticker).
type VendorRow = (i64, f64, f64, f64, f64, f64, &'static str);

const BASE_NS: i64 = 1_772_442_000_000_000_000; // 2026-03-02T09:00:00Z

fn vendor_schema(open_nullable: bool) -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new(
            "timestamp",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            false,
        ),
        Field::new("open", DataType::Float64, open_nullable),
        Field::new("high", DataType::Float64, false),
        Field::new("low", DataType::Float64, false),
        Field::new("close", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false),
        Field::new("ticker", DataType::Utf8, false),
    ]))
}

fn vendor_batch(rows: &[VendorRow], nullable_open: Option<usize>) -> RecordBatch {
    let schema = vendor_schema(nullable_open.is_some());
    let ts = TimestampNanosecondArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())
        .with_timezone("UTC");
    let opens: Vec<Option<f64>> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if Some(i) == nullable_open {
                None
            } else {
                Some(r.1)
            }
        })
        .collect();
    let open = if nullable_open.is_some() {
        Float64Array::from(opens)
    } else {
        Float64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())
    };
    let high = Float64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>());
    let low = Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>());
    let close = Float64Array::from(rows.iter().map(|r| r.4).collect::<Vec<_>>());
    let volume = Float64Array::from(rows.iter().map(|r| r.5).collect::<Vec<_>>());
    let ticker = StringArray::from(rows.iter().map(|r| r.6).collect::<Vec<_>>());
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(ts),
            Arc::new(open),
            Arc::new(high),
            Arc::new(low),
            Arc::new(close),
            Arc::new(volume),
            Arc::new(ticker),
        ],
    )
    .expect("vendor batch builds")
}

/// Write a vendor-schema file. `nullable_open` marks one row whose open is
/// null, for the null refusal control.
fn write_vendor(path: &Path, rows: &[VendorRow], nullable_open: Option<usize>) {
    let schema = vendor_schema(nullable_open.is_some());
    let batch = vendor_batch(rows, nullable_open);
    let file = std::fs::File::create(path).expect("fixture file creates");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("writer builds");
    writer.write(&batch).expect("fixture writes");
    writer.close().expect("fixture closes");
}

fn dec(text: &str) -> Decimal {
    text.parse().expect("decimal fixture parses")
}

fn three_rows() -> Vec<VendorRow> {
    vec![
        (BASE_NS, 677.66, 677.66, 676.54, 677.0, 8485.0, "SPY"),
        (
            BASE_NS + 60_000_000_000,
            676.91,
            677.45,
            676.74,
            677.45,
            24153.0,
            "SPY",
        ),
        (
            BASE_NS + 120_000_000_000,
            677.38,
            677.38,
            677.11,
            677.25,
            11541.0,
            "SPY",
        ),
    ]
}

#[test]
fn the_announced_vendor_schema_reads_as_is() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("SPY-2026-03-1M.parquet");
    write_vendor(&path, &three_rows(), None);

    let read = read_ohlcv_parquet(&path, Some("SPY")).expect("vendor file reads");
    assert_eq!(read.bars.len(), 3, "every row lands");
    assert_eq!(
        read.restatements_merged, 0,
        "no equal stamps in the fixture"
    );
    assert_eq!(read.rows_filtered, 0, "the requested ticker is every row");

    let first = &read.bars[0];
    assert_eq!(first.ts.timestamp_millis(), BASE_NS / 1_000_000);
    assert_eq!(first.o, dec("677.66"), "float prices cross exactly once");
    assert_eq!(first.h, dec("677.66"));
    assert_eq!(first.l, dec("676.54"));
    assert_eq!(first.c, dec("677.00"));
    assert_eq!(first.volume, 8485);
    assert_eq!(read.bars[2].c, dec("677.25"));
}

#[test]
fn restatements_merge_under_the_shared_fold_rule_and_are_counted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("restated.parquet");
    let rows = vec![
        (BASE_NS, 10.0, 11.0, 9.0, 10.5, 100.0, "SPY"),
        (BASE_NS, 10.2, 12.0, 8.5, 11.9, 130.0, "SPY"),
        (
            BASE_NS + 60_000_000_000,
            11.9,
            12.1,
            11.8,
            12.0,
            40.0,
            "SPY",
        ),
    ];
    write_vendor(&path, &rows, None);

    let read = read_ohlcv_parquet(&path, None).expect("file reads");
    assert_eq!(read.bars.len(), 2, "the restatement folds into its stamp");
    assert_eq!(read.restatements_merged, 1);
    let merged = &read.bars[0];
    assert_eq!(merged.o, dec("10.0"), "open stays the first reading's");
    assert_eq!(merged.c, dec("11.9"), "close is the latest reading's");
    assert_eq!(merged.h, dec("12.0"), "the range only widens");
    assert_eq!(merged.l, dec("8.5"));
    assert_eq!(
        merged.volume, 130,
        "the restated volume is the later reading's"
    );
}

#[test]
fn shuffled_batches_read_back_ascending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("shuffled.parquet");
    let mut rows = three_rows();
    rows.reverse();
    write_vendor(&path, &rows, None);

    let read = read_ohlcv_parquet(&path, None).expect("shuffled file reads");
    let stamps: Vec<i64> = read.bars.iter().map(|b| b.ts.timestamp_millis()).collect();
    let mut sorted = stamps.clone();
    sorted.sort_unstable();
    assert_eq!(
        stamps, sorted,
        "the reader establishes order, never assumes it"
    );
}

#[test]
fn a_ticker_filter_skips_and_counts_other_symbols() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mixed.parquet");
    let rows = vec![
        (BASE_NS, 1.0, 1.0, 1.0, 1.0, 10.0, "QQQ"),
        (BASE_NS + 60_000_000_000, 2.0, 2.0, 2.0, 2.0, 20.0, "SPY"),
        (BASE_NS + 120_000_000_000, 3.0, 3.0, 3.0, 3.0, 30.0, "QQQ"),
    ];
    write_vendor(&path, &rows, None);

    let read = read_ohlcv_parquet(&path, Some("SPY")).expect("filtered read");
    assert_eq!(read.bars.len(), 1);
    assert_eq!(read.rows_filtered, 2);
    assert_eq!(read.bars[0].volume, 20);
}

#[test]
fn mixed_tickers_refuse_without_a_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mixed.parquet");
    let rows = vec![
        (BASE_NS, 1.0, 1.0, 1.0, 1.0, 10.0, "QQQ"),
        (BASE_NS + 60_000_000_000, 2.0, 2.0, 2.0, 2.0, 20.0, "SPY"),
    ];
    write_vendor(&path, &rows, None);

    let err = read_ohlcv_parquet(&path, None).expect_err("mixed tickers refuse");
    let text = err.to_string();
    assert!(
        text.contains("name one ticker"),
        "the refusal names the fix: {text}"
    );
}

#[test]
fn a_missing_column_refuses_naming_what_the_file_carries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("no_close.parquet");
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
    let file = std::fs::File::create(&path).expect("file creates");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("writer builds");
    writer.write(&batch).expect("writes");
    writer.close().expect("closes");

    let err = read_ohlcv_parquet(&path, None).expect_err("missing close refuses");
    let text = err.to_string();
    assert!(
        text.contains("close"),
        "the refusal names the column: {text}"
    );
    assert!(
        text.contains("timestamp, open, high, low, volume"),
        "the refusal lists what the file carries: {text}"
    );
}

#[test]
fn a_null_price_refuses_naming_its_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("null_open.parquet");
    write_vendor(&path, &three_rows(), Some(1));

    let err = read_ohlcv_parquet(&path, None).expect_err("a null price refuses");
    let text = err.to_string();
    assert!(text.contains("row 1"), "the refusal names the row: {text}");
    assert!(text.contains("price is null"), "and the fault: {text}");
}

#[test]
fn a_fractional_volume_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fractional.parquet");
    let mut rows = three_rows();
    rows[1].5 = 24153.5;
    write_vendor(&path, &rows, None);

    let err = read_ohlcv_parquet(&path, None).expect_err("fractional volume refuses");
    assert!(err.to_string().contains("not a whole share count"), "{err}");
}

#[test]
fn a_non_finite_price_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nan.parquet");
    let mut rows = three_rows();
    rows[2].4 = f64::NAN;
    write_vendor(&path, &rows, None);

    let err = read_ohlcv_parquet(&path, None).expect_err("NaN refuses");
    assert!(err.to_string().contains("not finite"), "{err}");
}

#[test]
fn an_ohc_inversion_refuses_naming_its_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("inverted.parquet");
    let mut rows = three_rows();
    rows[1].2 = 1.0; // high below open/close
    write_vendor(&path, &rows, None);

    let err = read_ohlcv_parquet(&path, None).expect_err("inverted OHLC refuses");
    let text = err.to_string();
    assert!(text.contains("row 1"), "{text}");
    assert!(text.contains("high"), "{text}");
}

#[test]
fn the_store_s_own_artifact_shape_reads_too() {
    // ts_ms Int64 + Decimal128 prices at scale 2 + Int64 volume: the shape
    // SnapshotWriter writes. One reader, both encodings.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sealed.parquet");
    let schema = Arc::new(Schema::new(vec![
        Field::new("ts_ms", DataType::Int64, false),
        Field::new("open", DataType::Decimal128(38, 2), false),
        Field::new("high", DataType::Decimal128(38, 2), false),
        Field::new("low", DataType::Decimal128(38, 2), false),
        Field::new("close", DataType::Decimal128(38, 2), false),
        Field::new("volume", DataType::Int64, false),
        Field::new("adj", DataType::Utf8, true),
    ]));
    let decimal = |values: Vec<i128>| {
        Decimal128Array::from(values)
            .with_precision_and_scale(38, 2)
            .expect("scale applies")
    };
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![
                1_772_442_000_000i64,
                1_772_442_060_000,
            ])),
            Arc::new(decimal(vec![6_776_600, 6_769_100])),
            Arc::new(decimal(vec![6_776_600, 6_774_500])),
            Arc::new(decimal(vec![6_765_400, 6_767_400])),
            Arc::new(decimal(vec![6_770_000, 6_774_500])),
            Arc::new(Int64Array::from(vec![8_485i64, 24_153])),
            Arc::new(StringArray::from(vec![None::<&str>, None])),
        ],
    )
    .expect("batch builds");
    let file = std::fs::File::create(&path).expect("file creates");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("writer builds");
    writer.write(&batch).expect("writes");
    writer.close().expect("closes");

    let read = read_ohlcv_parquet(&path, None).expect("artifact shape reads");
    assert_eq!(read.bars.len(), 2);
    assert_eq!(read.bars[0].o, dec("67766.00"));
    assert_eq!(read.bars[1].volume, 24_153);
}

#[test]
fn a_schema_only_file_reads_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.parquet");
    write_vendor(&path, &[], None);

    let read = read_ohlcv_parquet(&path, None).expect("an empty file answers empty");
    assert!(read.bars.is_empty());
    assert_eq!(read.restatements_merged, 0);
}

#[test]
fn bars_are_usable_as_committed_history() {
    // The boundary contract: whatever the reader answers, every adjacent
    // pair passes the shared validator the decision path relies on.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("valid.parquet");
    write_vendor(&path, &three_rows(), None);
    let read = read_ohlcv_parquet(&path, None).expect("file reads");
    let mut last: Option<&Bar> = None;
    for bar in &read.bars {
        protopyne_types::validate_new_bar(last, bar).expect("the reader's series is clean");
        last = Some(bar);
    }
}
