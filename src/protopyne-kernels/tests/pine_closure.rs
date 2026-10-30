//! Fixture closure files: the fixture indicator plus its seven libraries load
//! with warn-only version findings, resolve to the pinned module set, and
//! refuse unknown files with a reason. Shipped CSV histories pin their
//! header shape and data-row counts here (lightweight header/count reads;
//! full bar parsing stays in the server ingest suite).
//!
//! All Pine file reads run over the checked-in fixtures
//! (server `tests/data/pine_closure`), never the volatile live root.
//!
//! Test path note: this file rides the `pine::closure` unit module, so the
//! runnable filter is `pine::closure`, not the file stem.

use crate::{load_pine_source, parse_pine_source, PineSource, SourceKind};
use fluent_wvr::validate::Validate;
use std::path::PathBuf;

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../protopyne-server/tests/data/pine_closure")
}

/// The volatile live root, used only for the shipped CSV histories (out
/// of scope for the pin-free migration: histories are not Pine files).
fn history_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn load(relpath: &str) -> PineSource {
    let path = data_root().join(relpath);
    load_pine_source(&path).expect("fixture file loads")
}

#[test]
#[allow(clippy::type_complexity)]
fn seven_libraries_load_with_identities() {
    // Fixture bytes are checked in: exact identities and import pairs pin
    // here, never over the volatile live root.
    let cases: &[(&str, &str, &[(&str, &str)])] = &[
        ("libraries/_libUtil.pine", "libUtil", &[]),
        ("libraries/_libBars.pine", "libBars", &[("libUtil", "UTIL")]),
        ("libraries/_libBuffers.pine", "libBuffers", &[]),
        (
            "libraries/_libDrawing.pine",
            "libDrawing",
            &[("libUtil", "UTIL")],
        ),
        (
            "libraries/_libCandles.pine",
            "libCandles",
            &[
                ("libUtil", "UTIL"),
                ("libDrawing", "libDrawing"),
                ("libApex", "libApex"),
            ],
        ),
        ("libraries/_libMAs.pine", "libMAs", &[("libUtil", "UTIL")]),
        (
            "libraries/_libApex.pine",
            "libApex",
            &[
                ("libUtil", "UTIL"),
                ("libBuffers", "libBuffers"),
                ("libBars", "BARS"),
                ("libDrawing", "libDrawing"),
            ],
        ),
    ];
    assert_eq!(cases.len(), 7, "five in-scope plus drawing/candles");
    for (relpath, name, imports) in cases {
        let source = load(relpath);
        assert_eq!(source.version, Some(6), "{relpath} declares v6");
        assert!(
            source.warnings.is_empty(),
            "{relpath} warns nothing on a clean v6 tree: {:?}",
            source.warnings
        );
        match &source.kind {
            SourceKind::Library { name: kind } => assert_eq!(kind.as_str(), *name),
            other => panic!("{relpath} is a library, got {other:?}"),
        }
        let got: Vec<(&str, &str)> = source
            .imports
            .iter()
            .map(|decl| (decl.library.name.as_str(), decl.alias.as_str()))
            .collect();
        assert_eq!(got, *imports, "{relpath} import pairs");
    }
}

#[test]
fn indicator_header_pins_seven_aliases() {
    let path = data_root().join("indicators/_IntegrationLite.pine");
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
    // Identity set from discovery over the fixtures (load-time only, no
    // execution). Dependency-first ordering over the parsed programs is
    // exercised by the import-graph suite once every file parses.
    let (indicators, libraries) =
        crate::manifest_files::load_pine_sources(&data_root()).expect("fixtures load");
    let closure = crate::discover_closures(&indicators, &libraries)
        .into_iter()
        .find_map(|outcome| match outcome {
            crate::ClosureOutcome::Linked(closure) if closure.stem == "_IntegrationLite" => {
                Some(closure)
            }
            _ => None,
        })
        .expect("fixture closure discovered");
    let mut ids: Vec<String> = closure.sources.iter().map(|(key, _)| key.clone()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "local/_IntegrationLite",
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
    let path = history_root().join(relative);
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
    assert_eq!(
        header,
        vec!["time", "open", "high", "low", "close", "Volume"]
    );
    assert_eq!(rows, 436, "1M data rows after the header");

    let (header, rows) = header_of("historical/SPY_5M.csv");
    assert_eq!(
        header,
        vec!["time", "open", "high", "low", "close", "Volume"]
    );
    assert_eq!(rows, 346, "5M data rows after the header");

    let (header, rows) = header_of("historical/SPY_15M.csv");
    assert_eq!(
        header,
        vec!["time", "open", "high", "low", "close", "Volume"]
    );
    assert_eq!(rows, 116, "15M data rows after the header");
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
    assert_eq!(rows, 10_503, "tick data rows after the header");
}
