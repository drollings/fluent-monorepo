//! Directory history roots: ticker discovery, month merging, and the CSV
//! supplement rule.
//!
//! Fixtures are tempdir trees in the shipped layout
//! (`TICKER/YYYY/MM/*.parquet` plus `*.csv` supplements), written with the
//! Arrow/Parquet writer at test time. The last two tests pin the real
//! `data/protopyne-rs/historical` tree the demo ships over: discovery must
//! name `SPY` and `GLD`, and the `SPY` months must merge into a valid
//! ascending series.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::history_root::{load_history_path, HistoryCache, HistoryRoot};
use arrow::array::{Float64Array, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use chrono::{TimeZone, Utc};
use parquet::arrow::ArrowWriter;
use protopyne_types::{validate_new_bar, Bar};
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

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

fn dec(text: &str) -> Decimal {
    text.parse().expect("decimal fixture parses")
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
    let ns: Vec<i64> = bars
        .iter()
        .map(|bar| bar.ts.timestamp_nanos_opt().expect("in range"))
        .collect();
    let ts = TimestampNanosecondArray::from(ns).with_timezone("UTC");
    let prices = |pick: fn(&Bar) -> Decimal| {
        Float64Array::from(
            bars.iter()
                .map(|bar| pick(bar).to_f64().expect("representable"))
                .collect::<Vec<_>>(),
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

/// A two-month `SPY` fixture tree: January and March months with disjoint
/// stamps, so the merge has an order to establish.
fn two_month_tree(root: &Path) -> PathBuf {
    let jan = root.join("SPY/2026/01");
    let mar = root.join("SPY/2026/03");
    std::fs::create_dir_all(&jan).expect("jan creates");
    std::fs::create_dir_all(&mar).expect("mar creates");
    let base = 1_772_442_000_i64; // 2026-03-02T09:00:00Z
                                  // March stamps first on disk on purpose: the loader sorts, never assumes.
    write_vendor_parquet(
        &mar.join("SPY-2026-03-1M.parquet"),
        &[bar_at(base, "102.00", 30), bar_at(base + 60, "103.00", 40)],
        "SPY",
    );
    write_vendor_parquet(
        &jan.join("SPY-2026-01-1M.parquet"),
        &[
            bar_at(base - 5_000_000, "100.00", 10),
            bar_at(base - 5_000_000 + 60, "101.00", 20),
        ],
        "SPY",
    );
    root.to_path_buf()
}

#[test]
fn tickers_lists_ticker_directories_sorted() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("SPY/2026/03")).expect("spy creates");
    std::fs::create_dir_all(dir.path().join("GLD/2026/01")).expect("gld creates");
    // Dot-directories are never tickers, and root-level files are never
    // ticker data: a tick tape and a legacy trim sit beside the tree and
    // load only through an explicit file argument.
    std::fs::create_dir_all(dir.path().join(".cache")).expect("dotdir creates");
    // Root-level files are never ticker data: a tick tape and a legacy trim
    // sit beside the tree and load only through an explicit file argument.
    std::fs::write(dir.path().join("notes.txt"), "not history").expect("note writes");
    std::fs::write(dir.path().join("SPY_ticks.csv"), "time,price,volume\n").expect("tape writes");
    write_csv_file(
        dir.path().join("SPY_15M.csv").as_path(),
        &[bar_at(1_772_442_000, "100.00", 10)],
    );
    let root = HistoryRoot::open(dir.path()).expect("root opens");
    assert_eq!(root.tickers().expect("tickers list"), vec!["GLD", "SPY"]);
}

#[test]
fn opening_a_file_as_a_root_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("SPY_15M.csv");
    write_csv_file(&file, &[bar_at(1_772_442_000, "100.00", 10)]);
    let err = HistoryRoot::open(&file).expect_err("a file is not a root");
    assert!(err.to_string().contains("not a directory"), "{err}");
}

#[test]
fn parquet_months_merge_into_one_ascending_series() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_path = two_month_tree(dir.path());
    let root = HistoryRoot::open(&root_path).expect("root opens");
    let load = root.load_ticker("SPY").expect("months merge");
    let bars = &load.bars;
    assert_eq!(bars.len(), 4, "both months land");
    assert!(load.skipped.is_empty(), "clean months skip nothing");
    assert_eq!(load.superseded, 0);
    let mut last: Option<&Bar> = None;
    for bar in bars {
        validate_new_bar(last, bar).expect("the merged series is clean");
        last = Some(bar);
    }
    assert_eq!(bars.first().expect("first").c, dec("100"));
    assert_eq!(bars.last().expect("last").c, dec("103"));
}

#[test]
fn csv_supplement_supersedes_parquet_at_shared_stamps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_path = two_month_tree(dir.path());
    let base = 1_772_442_000_i64;
    // The supplement restates both March bars and appends one of its own.
    write_csv_file(
        &root_path.join("SPY/2026/03/SPY-2026-03-1M-supplement.csv"),
        &[
            bar_at(base, "202.00", 300),
            bar_at(base + 60, "203.00", 400),
            bar_at(base + 120, "204.00", 500),
        ],
    );
    let root = HistoryRoot::open(&root_path).expect("root opens");
    let load = root.load_ticker("SPY").expect("supplement merges");
    let bars = &load.bars;
    assert_eq!(
        bars.len(),
        5,
        "two restated plus one appended plus two January"
    );
    assert_eq!(load.superseded, 2, "two shared stamps replaced");
    let closes: Vec<Decimal> = bars.iter().map(|bar| bar.c).collect();
    assert_eq!(
        closes,
        vec![dec("100"), dec("101"), dec("202"), dec("203"), dec("204")]
    );
    let restated = bars
        .iter()
        .find(|bar| bar.ts.timestamp() == base)
        .expect("restated");
    assert_eq!(restated.volume, 300, "the CSV row replaces wholesale");
}

#[test]
fn subtree_root_csvs_supplement_the_same_ticker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_path = two_month_tree(dir.path());
    let base = 1_772_442_000_i64;
    // A supplement at the subtree root (not a month dir) still rides the
    // same recursive discovery.
    write_csv_file(
        &root_path.join("SPY/SPY_latest.csv"),
        &[bar_at(base + 120, "300.00", 700)],
    );
    let root = HistoryRoot::open(&root_path).expect("root opens");
    let load = root.load_ticker("SPY").expect("subtree supplement merges");
    assert_eq!(load.bars.len(), 5);
    assert_eq!(load.bars.last().expect("last").c, dec("300"));
    assert_eq!(load.superseded, 0, "the appended stamp supersedes nothing");
}

#[test]
fn csv_only_ticker_serves_without_parquet() {
    let dir = tempfile::tempdir().expect("tempdir");
    let subtree = dir.path().join("QQQ/2026/03");
    std::fs::create_dir_all(&subtree).expect("subtree creates");
    write_csv_file(
        &subtree.join("QQQ-2026-03-1M.csv"),
        &[
            bar_at(1_772_442_000, "500.00", 60),
            bar_at(1_772_442_060, "501.00", 70),
        ],
    );
    let root = HistoryRoot::open(dir.path()).expect("root opens");
    assert_eq!(root.tickers().expect("tickers"), vec!["QQQ"]);
    let load = root.load_ticker("QQQ").expect("csv-only serves");
    assert_eq!(load.bars.len(), 2);
    assert!(load.skipped.is_empty());
}

#[test]
fn a_refusing_month_is_excluded_and_named() {
    use arrow::array::Int64Array;
    let dir = tempfile::tempdir().expect("tempdir");
    let good = dir.path().join("SPY/2026/01");
    let bad = dir.path().join("SPY/2026/02");
    std::fs::create_dir_all(&good).expect("good creates");
    std::fs::create_dir_all(&bad).expect("bad creates");
    let base = 1_772_442_000_i64;
    write_vendor_parquet(
        &good.join("SPY-2026-01-1M.parquet"),
        &[bar_at(base, "100.00", 10)],
        "SPY",
    );
    // A market file without the `close` column: structurally wrong, excluded
    // loudly instead of stranding the ticker.
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
    let bad_path = bad.join("SPY-2026-02-1M.parquet");
    let file = std::fs::File::create(&bad_path).expect("fixture creates");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("writer builds");
    writer.write(&batch).expect("writes");
    writer.close().expect("closes");

    let root = HistoryRoot::open(dir.path()).expect("root opens");
    let load = root
        .load_ticker("SPY")
        .expect("the good month still serves");
    assert_eq!(load.bars.len(), 1);
    assert_eq!(load.skipped.len(), 1, "the bad month is named, not silent");
    assert_eq!(load.skipped_rows, 0, "structural skips carry no row count");
    assert_eq!(load.skipped[0].path, bad_path);
    assert!(
        load.skipped[0].reason.contains("close"),
        "the reason names the fault: {}",
        load.skipped[0].reason
    );
}

/// A month with dirty rows still serves its clean bars: per-row value
/// faults are excluded and counted, never file-fatal.
#[test]
fn dirty_rows_are_excluded_and_counted_not_file_fatal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let month = dir.path().join("SPY/2026/02");
    std::fs::create_dir_all(&month).expect("month creates");
    let base_ns = 1_772_442_000_000_000_000_i64;
    let schema = vendor_schema();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(
                TimestampNanosecondArray::from(vec![
                    base_ns,
                    base_ns + 60_000_000_000,
                    base_ns + 120_000_000_000,
                ])
                .with_timezone("UTC"),
            ),
            Arc::new(Float64Array::from(vec![100.0, 101.0, 102.0])),
            Arc::new(Float64Array::from(vec![100.0, 101.0, 102.0])),
            Arc::new(Float64Array::from(vec![100.0, 101.0, 102.0])),
            // The middle row carries a NaN close: a row fault, not a file fault.
            Arc::new(Float64Array::from(vec![100.0, f64::NAN, 102.0])),
            Arc::new(Float64Array::from(vec![10.0, 20.0, 30.0])),
            Arc::new(StringArray::from(vec!["SPY", "SPY", "SPY"])),
        ],
    )
    .expect("batch builds");
    let file = std::fs::File::create(month.join("SPY-2026-02-1M.parquet")).expect("creates");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("writer builds");
    writer.write(&batch).expect("writes");
    writer.close().expect("closes");

    let root = HistoryRoot::open(dir.path()).expect("root opens");
    let load = root.load_ticker("SPY").expect("dirty month serves its clean bars");
    assert_eq!(load.bars.len(), 2, "the good rows land");
    assert!(load.skipped.is_empty(), "nothing is excluded whole");
    assert_eq!(load.skipped_rows, 1, "the dirty row is counted");
    assert_eq!(load.bars[0].volume, 10);
    assert_eq!(load.bars[1].volume, 30);
}

#[test]
fn unknown_and_malformed_tickers_refuse() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = HistoryRoot::open(dir.path()).expect("root opens");
    let err = root
        .load_ticker("NOPE")
        .expect_err("unknown ticker refuses");
    assert!(err.to_string().contains("no history for ticker"), "{err}");
    for bad in ["", "..", "SPY/Y"] {
        let err = root.load_ticker(bad).expect_err("bad spelling refuses");
        assert!(err.to_string().contains("invalid ticker"), "{err}");
    }
}

#[test]
fn cache_loads_once_and_serves_clones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_path = two_month_tree(dir.path());
    let mut cache = HistoryCache::new(HistoryRoot::open(&root_path).expect("root opens"));
    let first = cache.bars_for("SPY").expect("first load reads");
    let second = cache.bars_for("SPY").expect("second load clones");
    assert_eq!(first, second);
    let err = cache
        .bars_for("NOPE")
        .expect_err("unknown ticker refuses through the cache");
    assert!(err.to_string().contains("no history for ticker"), "{err}");
}

#[test]
fn load_history_path_serves_files_and_directories() {
    // A file keeps the legacy single-file ladder.
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/SPY_15M.csv");
    let bars = load_history_path(&file, None).expect("file loads");
    assert!(!bars.is_empty(), "the vendored CSV serves");
    // A directory serves the preferred ticker, defaulting to SPY.
    let dir = tempfile::tempdir().expect("tempdir");
    let root_path = two_month_tree(dir.path());
    let bars = load_history_path(&root_path, None).expect("directory loads SPY by default");
    assert_eq!(bars.len(), 4);
    let bars = load_history_path(&root_path, Some("SPY")).expect("preferred ticker loads");
    assert_eq!(bars.len(), 4);
    let err = load_history_path(&root_path, Some("NOPE")).expect_err("unknown preferred refuses");
    assert!(err.to_string().contains("no history for ticker"), "{err}");
}

fn shipped_history_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical")
}

#[test]
fn shipped_layout_discovers_spy_and_gld() {
    let root = HistoryRoot::open(&shipped_history_dir()).expect("shipped tree opens");
    let tickers = root.tickers().expect("shipped tickers list");
    assert!(
        tickers.contains(&"SPY".to_owned()),
        "SPY is a ticker: {tickers:?}"
    );
    assert!(
        tickers.contains(&"GLD".to_owned()),
        "GLD is a ticker: {tickers:?}"
    );
}

/// Make `tickers` appear as discovered directories under `root` (month
/// subdirectories included: discovery reads directories, and the tree must
/// read as real ticker data, not empty shells).
fn ticker_tree(root: &Path, tickers: &[&str]) {
    for ticker in tickers {
        std::fs::create_dir_all(root.join(ticker).join("2026/01"))
            .expect("ticker tree creates");
    }
}

#[test]
fn symbol_rows_discover_exactly_what_the_root_holds() {
    let dir = tempfile::tempdir().expect("tempdir");
    ticker_tree(dir.path(), &["AAA", "ZZZ"]);
    std::fs::create_dir_all(dir.path().join(".hidden")).expect("dot dir creates");
    let root = HistoryRoot::open(dir.path()).expect("root opens");
    let rows = root.symbol_rows().expect("rows read");
    let ids: Vec<&str> = rows.iter().map(|row| row.symbol.as_str()).collect();
    assert_eq!(ids, ["AAA", "ZZZ"], "no SPY/GLD unless present");
    for row in &rows {
        assert_eq!(
            row.name, row.symbol,
            "an undeclared ticker displays as itself"
        );
        assert_eq!(
            row.asset_class, "Unknown",
            "an unmapped ticker is a real Unknown class, never a guessed one"
        );
    }
}

#[test]
fn symbol_rows_of_a_dot_only_root_stay_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".cache")).expect("dot dir creates");
    let root = HistoryRoot::open(dir.path()).expect("root opens");
    let rows = root
        .symbol_rows()
        .expect("empty discovery answers empty, it is not an error");
    assert!(rows.is_empty(), "empty, not a fallback leak: {rows:?}");
}

#[test]
fn symbol_rows_of_a_shipped_root_are_the_shipped_wire() {
    // Backward-compat pin: a root holding exactly the shipped tickers serves
    // the identical wire the shipped catalog serves, byte for byte.
    let dir = tempfile::tempdir().expect("tempdir");
    ticker_tree(dir.path(), &["GLD", "SPY", "QQQ", "BTC-USD", "ES"]);
    let root = HistoryRoot::open(dir.path()).expect("root opens");
    let rows = root.symbol_rows().expect("rows read");
    let shipped = protopyne_view::symbol_catalog();
    assert_eq!(rows, shipped);
    assert_eq!(
        serde_json::to_string(&rows).expect("rows serialize"),
        serde_json::to_string(&shipped).expect("shipped rows serialize"),
        "byte-identical wire shape, not just equal fields"
    );
}

#[test]
fn symbol_rows_read_ticker_sidecar_declarations() {
    let dir = tempfile::tempdir().expect("tempdir");
    ticker_tree(dir.path(), &["AAA", "BBB", "CCC"]);
    std::fs::write(
        dir.path().join("AAA").join(".coral-instrument"),
        r#"{"name":"Acme Rentals","class":"Futures"}"#,
    )
    .expect("sidecar writes");
    // A class label that does not parse: the name still applies, the class
    // lands on Unknown rather than being guessed.
    std::fs::write(
        dir.path().join("BBB").join(".coral-instrument"),
        r#"{"name":"Bee Freight","class":"Equities"}"#,
    )
    .expect("sidecar writes");
    // An unreadable sidecar declares nothing at all.
    std::fs::write(dir.path().join("CCC").join(".coral-instrument"), "not json")
        .expect("sidecar writes");
    let root = HistoryRoot::open(dir.path()).expect("root opens");
    let rows = root.symbol_rows().expect("rows read");
    let row = |ticker: &str| {
        rows
            .iter()
            .find(|row| row.symbol == ticker)
            .unwrap_or_else(|| panic!("{ticker} is listed"))
    };
    assert_eq!(row("AAA").name, "Acme Rentals");
    assert_eq!(row("AAA").asset_class, "Futures");
    assert_eq!(row("BBB").name, "Bee Freight");
    assert_eq!(row("BBB").asset_class, "Unknown");
    assert_eq!(row("CCC").name, "CCC");
    assert_eq!(row("CCC").asset_class, "Unknown");
}

#[test]
fn shipped_spy_months_merge_into_a_valid_series() {
    let root = HistoryRoot::open(&shipped_history_dir()).expect("shipped tree opens");
    let load = root.load_ticker("SPY").expect("shipped SPY months merge");
    let bars = &load.bars;
    assert!(!bars.is_empty(), "the shipped months hold bars");
    // The February 2026 vendor month carries fractional volumes, which round
    // to the nearest share: nothing is excluded, nothing is counted.
    assert!(
        load.skipped.is_empty(),
        "no month is excluded whole: {:?}",
        load.skipped.iter().map(|skip| &skip.path).collect::<Vec<_>>()
    );
    assert_eq!(load.skipped_rows, 0, "rounding leaves no row behind");
    let mut last: Option<&Bar> = None;
    for bar in bars {
        validate_new_bar(last, bar).expect("the shipped series is clean");
        last = Some(bar);
    }
}
