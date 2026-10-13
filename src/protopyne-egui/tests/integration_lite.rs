use super::*;
use std::collections::HashMap;
use std::path::PathBuf;

use chrono::TimeZone as _;
use fluent_egui::PivotSide;
use protopyne_chart_model::ChartRow;
use protopyne_kernels::{
    FetchCounters, InputSet, LibraryId, LinkedModule, ModuleGraph, ModuleInput, PineEngine,
    PineRuntimeConfig, PineValue, RunOutcome, parse_pine_source, parse_program, resolve_imports,
};
use protopyne_types::Bar;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive as _;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;
/// Freeze drivers evaluate the length-14 arms on both oscillators.
const DRIVER_IMPORTS: &str =
    "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libMAs/95 as MAS\n";

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn library_id(publisher: &str, name: &str, version: u64) -> LibraryId {
    LibraryId { publisher: publisher.to_owned(), name: name.to_owned(), version }
}

fn shipped_module(file: &str, publisher: &str, name: &str, version: u64) -> ModuleInput {
    let text = std::fs::read_to_string(data_root().join(file)).expect("shipped library loads");
    let source = parse_pine_source(file, &text);
    let program = parse_program(&text).expect("shipped library parses");
    ModuleInput::new(library_id(publisher, name, version), source, program)
}

fn driver_module(text: &str) -> ModuleInput {
    let source = parse_pine_source("local/Driver", text);
    let program = parse_program(text).expect("driver parses");
    ModuleInput::new(library_id("local", "Driver", 1), source, program)
}

/// Map an ordered module graph to the alias tables linked execution runs
/// (same loader/parser/linker mapping the server harness uses).
fn linked_from_graph(graph: &ModuleGraph) -> Vec<LinkedModule> {
    graph
        .order()
        .iter()
        .map(|id| {
            let entry = graph.get(id).expect("ordered module resolves");
            let aliases = graph
                .alias_table(id)
                .unwrap_or_default()
                .into_iter()
                .map(|(alias, target)| (alias, target.to_string()))
                .collect::<HashMap<String, String>>();
            LinkedModule {
                key: id.to_string(),
                program: entry.program.clone(),
                aliases,
            }
        })
        .collect()
}

fn linked(driver: &str) -> Vec<LinkedModule> {
    linked_from_graph(
        &resolve_imports(vec![
            shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
            shipped_module("libraries/_libMAs.pine", "showmethegrail", "libMAs", 95),
            driver_module(driver),
        ])
        .expect("modules resolve"),
    )
}

/// Sixty rising bars with exact decimal legs (integral closes, so the
/// display-grade legs are bit-identical on both engines).
fn ramp_bars() -> Vec<Bar> {
    (0..60)
        .map(|i| {
            let close = 100.0 + i as f64;
            let px = |v: f64| Decimal::from_f64(v).expect("exact test decimal");
            Bar::new()
                .ts(
                    chrono::Utc
                        .with_ymd_and_hms(2026, 1, 5, 0, 0, 0)
                        .single()
                        .expect("valid time")
                        + chrono::Duration::minutes(i),
                )
                .o(px(close - 0.25))
                .h(px(close + 0.5))
                .l(px(close - 0.5))
                .c(px(close))
                .volume(1_000 + 10 * i as u64)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn ramp_rows() -> Vec<ChartRow> {
    (0..60)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: i,
                open: close - 0.25,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume: 1_000 + 10 * i as u64,
            }
        })
        .collect()
}

/// Run one freeze driver, returning the `out` series plus the engine's
/// fetch counters (confidence signal, reported alongside — never a
/// substitute for the max-diff pin).
fn run_driver(driver: &str, bars: &[Bar]) -> (Vec<Option<f64>>, FetchCounters) {
    let modules = linked(driver);
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new().chart_symbol("SPY".to_owned()).chart_period("5".to_owned()).build(),
    );
    let outcome: RunOutcome =
        engine.run_linked(&modules, bars, InputSet::new()).expect("linked run evaluates");
    let series = (0..bars.len())
        .map(|bar| match outcome.value_at(bar, "out") {
            Some(Some(PineValue::Float(value))) => Some(value),
            Some(Some(PineValue::Int(value))) => Some(value as f64),
            Some(None) => None,
            other => panic!("expected number or na for out on bar {bar}, got {other:?}"),
        })
        .collect();
    (series, engine.fetch_counters())
}

fn freeze_drivers() -> [(&'static str, &'static str); 3] {
    [
        ("fast", "out = MAS.normalizeOsc(close, UTIL.Oscillators.RSI, 0, 5, 14, UTIL.Smoothings.NA, 0)"),
        ("slow", "out = MAS.normalizeOsc(close, UTIL.Oscillators.CCI, 0, 5, 14, UTIL.Smoothings.NA, 0)"),
        ("adaptive", "out = MAS.getEIT(close, 0.14)"),
    ]
}

#[test]
fn freeze_pins_native_against_pine() {
    let bars = ramp_bars();
    let native =
        native_lite_series(&ramp_rows(), 0.14).expect("native series evaluate");
    let frozen = LiteFrozen::from_native(&native);
    let mut pine = LiteFrozen { fast: Vec::new(), slow: Vec::new(), adaptive: Vec::new() };
    let mut fallbacks = 0_u64;
    for (which, expr) in freeze_drivers() {
        let (series, counters) =
            run_driver(&format!("{DRIVER_IMPORTS}{expr}"), &bars);
        fallbacks = fallbacks.max(counters.fallbacks());
        match which {
            "fast" => pine.fast = series,
            "slow" => pine.slow = series,
            _ => pine.adaptive = series,
        }
    }
    let report = compare_freeze(&frozen, &pine);
    println!(
        "FREEZE_REPORT=fast(diff={}, compared={}) slow(diff={}, compared={}) adaptive(diff={}, compared={}) fallbacks={fallbacks}",
        report.fast.max_abs_diff,
        report.fast.compared,
        report.slow.max_abs_diff,
        report.slow.compared,
        report.adaptive.max_abs_diff,
        report.adaptive.compared,
    );
    for (name, pair) in
        [("fast", report.fast), ("slow", report.slow), ("adaptive", report.adaptive)]
    {
        assert!(
            pair.compared >= 30,
            "{name}: only {} bars co-read after warmup — fixture too short",
            pair.compared
        );
        assert!(
            pair.max_abs_diff <= EPS,
            "{name} drifted past the freeze (max diff {})",
            pair.max_abs_diff
        );
    }
    assert_eq!(fallbacks, 0, "freeze drivers perform no seam fetches");
}

#[test]
fn calibrated_view_holds_shape_over_the_ramp() {
    let view = integration_lite_view_calibrated(&ramp_rows(), 0.14, 0.5)
        .expect("calibrated view builds");
    assert!(
        (view.threshold - CALIBRATED_PIVOT_THRESHOLD).abs() < 1e-12,
        "view carries the frozen threshold"
    );
    assert_eq!(view.fast.len(), 60);
    assert_eq!(view.slow.len(), 60);
    assert_eq!(view.ribbon.upper.len(), 60);
    // Monotonic ramp pins the oscillator high: no threshold crossings.
    assert!(view.pivots.is_empty(), "steady ramp fires no pivots: {:?}", view.pivots);
    for (index, (up, lo)) in
        view.ribbon.upper.iter().zip(view.ribbon.lower.iter()).enumerate()
    {
        if let (Some(a), Some(b)) = (up, lo) {
            assert!(a >= b, "ribbon orders at {index}");
        }
    }
}

#[test]
fn bad_alphas_and_thresholds_refuse() {
    let rows = ramp_rows();
    assert!(native_lite_series(&rows, 0.0).is_err(), "zero EIT alpha refuses");
    assert!(native_lite_series(&rows, 1.5).is_err(), "divergent EIT alpha refuses");
    assert!(native_lite_series(&rows, f64::NAN).is_err());
    assert!(integration_lite_view(&rows, 0.14, 2.0, 100.0).is_err(), "fill alpha refuses");
    assert!(integration_lite_view(&rows, 0.14, 0.5, 0.0).is_err(), "zero threshold refuses");
}

fn flat_fast(bars: usize, level: f64) -> Vec<Option<f64>> {
    vec![Some(level); bars]
}

fn chop_fast(bars: usize) -> Vec<Option<f64>> {
    (0..bars).map(|i| Some(if i % 2 == 0 { 60.0 } else { -60.0 })).collect()
}

fn rise_reversal() -> (Vec<Option<f64>>, Vec<PivotMark>) {
    let fast = [60.0, 80.0, 100.0, 120.0, 150.0, 170.0, 150.0, 90.0, 70.0, 60.0]
        .map(Some)
        .to_vec();
    (fast, vec![PivotMark { index: 7, side: PivotSide::High }])
}

fn fall_recovery() -> (Vec<Option<f64>>, Vec<PivotMark>) {
    let fast = [40.0, 10.0, -30.0, -80.0, -120.0, -90.0, -40.0, 0.0].map(Some).to_vec();
    (fast, vec![PivotMark { index: 5, side: PivotSide::Low }])
}

#[test]
fn controls_stay_quiet_at_the_frozen_threshold() {
    for (name, fast) in [("flat", flat_fast(40, 5.0)), ("chop", chop_fast(40))] {
        let marks =
            pivot_marks(&fast, CALIBRATED_PIVOT_THRESHOLD).expect("valid threshold");
        assert!(marks.is_empty(), "{name} control must not fire: {marks:?}");
    }
}

#[test]
fn known_pivots_score_full_precision_and_recall() {
    for (name, (fast, expected)) in
        [("rise_reversal", rise_reversal()), ("fall_recovery", fall_recovery())]
    {
        let (precision, recall) =
            precision_recall(&fast, &expected, CALIBRATED_PIVOT_THRESHOLD)
                .expect("valid threshold");
        assert!(
            (precision - 1.0).abs() < 1e-12 && (recall - 1.0).abs() < 1e-12,
            "{name} must score fully: {precision}/{recall}"
        );
    }
    // Empty against empty is perfect; empty against labeled is a total miss.
    let (empty_precision, empty_recall) =
        precision_recall(&flat_fast(10, 5.0), &[], CALIBRATED_PIVOT_THRESHOLD).expect("valid");
    assert!(
        (empty_precision - 1.0).abs() < 1e-12 && (empty_recall - 1.0).abs() < 1e-12,
        "empty scores perfectly only against empty"
    );
    let (_, expected) = rise_reversal();
    let (_, missed_recall) = precision_recall(&flat_fast(10, 5.0), &expected, CALIBRATED_PIVOT_THRESHOLD)
        .expect("valid");
    assert!(missed_recall < 1e-12, "missed pivots recall zero");
}

#[test]
fn calibration_sweep_is_reported() {
    // Candidate sweep each window fires through; the checked-in report
    // records these numbers (floors pin only the frozen threshold above).
    let mut table = serde_json::Map::new();
    for (name, (fast, expected)) in
        [("rise_reversal", rise_reversal()), ("fall_recovery", fall_recovery())]
    {
        let mut row = serde_json::Map::new();
        for threshold in [50.0, 100.0, 150.0, 200.0] {
            let marks = pivot_marks(&fast, threshold).expect("valid threshold");
            let (precision, recall) =
                precision_recall(&fast, &expected, threshold).expect("valid threshold");
            row.insert(
                format!("{threshold}"),
                serde_json::json!({
                    "marks": marks.len(),
                    "precision": precision,
                    "recall": recall,
                }),
            );
        }
        table.insert(name.to_owned(), serde_json::Value::Object(row));
    }
    for (name, fast) in [("flat", flat_fast(40, 5.0)), ("chop", chop_fast(40))] {
        let mut row = serde_json::Map::new();
        for threshold in [50.0, 100.0, 150.0, 200.0] {
            let marks = pivot_marks(&fast, threshold).expect("valid threshold");
            row.insert(format!("{threshold}"), serde_json::json!({ "marks": marks.len() }));
        }
        table.insert(name.to_owned(), serde_json::Value::Object(row));
    }
    println!("CALIBRATION_SWEEP={}", serde_json::Value::Object(table));
}

#[test]
fn momentum_keys_cross_direction_with_trend() {
    let row = |open: f64, close: f64| ChartRow {
        time: 0,
        open,
        high: open.max(close),
        low: open.min(close),
        close,
        volume: 10,
    };
    let rows = vec![row(100.0, 101.0), row(100.0, 99.0), row(100.0, 99.0), row(100.0, 101.0)];
    let keys = candle_momentum_keys(&rows, &[true, true, false, false]).expect("aligned inputs");
    assert_eq!(keys, vec!["bull", "bull_down", "bear", "bear_up"]);
    assert!(candle_momentum_keys(&rows, &[true]).is_err(), "key mismatch refuses");
}

#[test]
fn specified_transparencies_follow_the_reference_inputs() {
    for (strength, expected) in [(0.5, 65.0), (0.14, 86.6), (0.8, 47.0), (2.0, 0.0)] {
        let got = ribbon_fill_transparency(strength);
        assert!(
            (got - expected).abs() < 1e-9,
            "95 - 60 * alpha clamped: strength {strength} -> {got}, want {expected}"
        );
    }
    let garbage = ribbon_fill_transparency(f64::NAN);
    assert!(
        (garbage - 100.0).abs() < 1e-9,
        "garbage strength paints nothing, got {garbage}"
    );
    assert_eq!(OSCILLATOR_BRIGHTNESS, 30, "reference brightness input");
    for (brightness, expected) in [(30_u32, 60.0), (0, 90.0), (200, 0.0)] {
        let got = oscillator_transparency(brightness);
        assert!(
            (got - expected).abs() < 1e-9,
            "90 - brightness: {brightness} -> {got}, want {expected}"
        );
    }
    assert!((VOLUME_TRANSPARENCY - 60.0).abs() < 1e-12, "reference volume color input");
}
