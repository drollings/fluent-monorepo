//! Real-file closure fixtures: the indicator plus its seven libraries load
//! with warn-only version findings, resolve to the pinned module set, and
//! refuse unknown files with a reason. Shipped CSV histories pin their
//! header shape and data-row counts here (lightweight header/count reads;
//! full bar parsing stays in the server ingest suite).
//!
//! Test path note: this file rides the `pine::closure` unit module, so the
//! runnable filter is `pine::closure`, not the file stem.

use super::*;
use crate::{PineSource, SourceKind, load_pine_source, parse_pine_source};
use protopyne_types::Validate;
use std::path::PathBuf;

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs")
}

fn load(entry: &ClosureEntry) -> PineSource {
    let path = data_root().join(entry.relpath);
    load_pine_source(&path).expect("real file loads")
}

#[test]
fn seven_libraries_load_with_identities() {
    assert_eq!(REAL_LIBRARIES.len(), 7, "five in-scope plus drawing/candles");
    for entry in REAL_LIBRARIES {
        let source = load(entry);
        assert_eq!(source.version, Some(6), "{} declares v6", entry.relpath);
        assert!(
            source.warnings.is_empty(),
            "{} warns nothing on a clean v6 tree: {:?}",
            entry.relpath,
            source.warnings
        );
        match &source.kind {
            SourceKind::Library { name } => assert_eq!(name, entry.name),
            other => panic!("{} is a library, got {other:?}", entry.relpath),
        }
        assert_eq!(source.imports.len(), entry.imports.len());
        for (got, want) in source.imports.iter().zip(entry.imports.iter()) {
            assert_eq!(got.library.name, want.0);
            assert_eq!(got.alias, want.1);
        }
    }
}

#[test]
fn indicator_header_pins_seven_aliases() {
    let path = data_root().join(INDICATOR_PATH);
    let source = load_pine_source(&path).expect("indicator loads");
    assert_eq!(source.version, Some(6));
    assert!(source.warnings.is_empty());
    assert_eq!(source.kind, SourceKind::Indicator);
    let aliases: Vec<(&str, &str)> = source
        .imports
        .iter()
        .map(|decl| (decl.library.name.as_str(), decl.alias.as_str()))
        .collect();
    assert_eq!(
        aliases,
        vec![
            ("libUtil", "UTIL"),
            ("libBars", "BARS"),
            ("libBuffers", "libBuffers"),
            ("libDrawing", "libDrawing"),
            ("libCandles", "libCandles"),
            ("libMAs", "libMAs"),
            ("libApex", "libApex"),
        ]
    );
}

#[test]
fn closure_module_set_is_pinned() {
    // Identity set from the fixture entries (load-time only, no execution).
    // Dependency-first ordering over the parsed programs is exercised by
    // the import-graph suite once every file parses.
    let mut ids: Vec<String> = REAL_LIBRARIES
        .iter()
        .map(|entry| entry.module_id().to_string())
        .collect();
    ids.push("local/IntegrationLite".to_owned());
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "local/IntegrationLite",
            "showmethegrail/libApex",
            "showmethegrail/libBars",
            "showmethegrail/libBuffers",
            "showmethegrail/libCandles",
            "showmethegrail/libDrawing",
            "showmethegrail/libMAs",
            "showmethegrail/libUtil",
        ]
    );
}

#[test]
fn unknown_file_refuses_with_reason() {
    let missing = data_root().join("libraries/_no_such_library.pine");
    let err = load_pine_source(&missing).expect_err("missing file refuses");
    let text = err.to_string();
    assert!(
        text.contains("_no_such_library.pine"),
        "refusal names the path: {text}"
    );
}

#[test]
fn version_skew_warns_and_still_loads() {
    let skewed = parse_pine_source("skewed.pine", "//@version=5\nlibrary('libSkew')\n");
    assert_eq!(skewed.version, Some(5));
    assert_eq!(skewed.warnings.len(), 1, "skew warns exactly once");
    assert!(
        skewed.warnings[0].message.contains("5"),
        "warning names the declared version: {}",
        skewed.warnings[0].message
    );
    skewed.validate().expect("skewed source still validates");
}

fn header_of(relative: &str) -> (Vec<String>, usize) {
    let path = data_root().join(relative);
    let text = std::fs::read_to_string(&path).expect("csv reads");
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(text.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .expect("header reads")
        .iter()
        .map(str::to_owned)
        .collect();
    let rows = reader.records().count();
    (headers, rows)
}

#[test]
fn bar_histories_pin_header_and_counts() {
    let (header, rows) = header_of("historical/SPY_1M.csv");
    assert_eq!(header, vec!["time", "open", "high", "low", "close", "Volume"]);
    assert_eq!(rows, 5_311, "1M data rows after the header");

    let (header, rows) = header_of("historical/SPY_5M.csv");
    assert_eq!(header, vec!["time", "open", "high", "low", "close", "Volume"]);
    assert_eq!(rows, 1_149, "5M data rows after the header");

    let (header, rows) = header_of("historical/SPY_15M.csv");
    assert_eq!(header, vec!["time", "open", "high", "low", "close", "Volume"]);
    assert_eq!(rows, 384, "15M data rows after the header");
}

#[test]
fn tick_harness_pins_columns_and_count() {
    let (header, rows) = header_of("historical/SPY_ticks.csv");
    assert_eq!(
        header,
        vec![
            "timestamp_ms",
            "price",
            "volume",
            "is_out_of_order",
            "original_timestamp_ms"
        ]
    );
    assert_eq!(rows, 115_839, "tick data rows after the header");
}
