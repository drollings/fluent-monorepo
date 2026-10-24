use super::*;
use crate::oscillators::CALIBRATED_BOUNDED_PIVOT_THRESHOLD;
use std::collections::HashMap;
use std::path::PathBuf;

use chrono::TimeZone as _;
use fluent_egui::PivotSide;
use protopyne_chart_model::ChartRow;
use protopyne_kernels::bars::NEUTRAL_BUY_RATIO;
use protopyne_kernels::{
    parse_pine_source, parse_program, resolve_imports, FetchCounters, InputSet, LibraryId,
    LinkedModule, ModuleGraph, ModuleInput, PineEngine, PineRuntimeConfig, PineValue, RunOutcome,
};
use protopyne_types::Bar;
use rust_decimal::prelude::FromPrimitive as _;
use rust_decimal::Decimal;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;
/// Freeze drivers evaluate the length-14 arms on both oscillators.
const DRIVER_IMPORTS: &str =
    "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libMAs/95 as MAS\n";

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn library_id(publisher: &str, name: &str, version: u64) -> LibraryId {
    LibraryId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
        version,
    }
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
                .ts(chrono::Utc
                    .with_ymd_and_hms(2026, 1, 5, 0, 0, 0)
                    .single()
                    .expect("valid time")
                    + chrono::Duration::minutes(i))
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
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    );
    let outcome: RunOutcome = engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("linked run evaluates");
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
        (
            "fast",
            "out = MAS.normalizeOsc(close, UTIL.Oscillators.RSI, 0, 5, 14, UTIL.Smoothings.NA, 0)",
        ),
        (
            "slow",
            "out = MAS.normalizeOsc(close, UTIL.Oscillators.CCI, 0, 5, 14, UTIL.Smoothings.NA, 0)",
        ),
        ("adaptive", "out = MAS.getEIT(close, 0.14)"),
    ]
}

#[test]
fn freeze_pins_native_against_pine() {
    let bars = ramp_bars();
    let native = native_lite_series(&ramp_rows(), 0.14).expect("native series evaluate");
    let frozen = LiteFrozen::from_native(&native);
    let mut pine = LiteFrozen {
        fast: Vec::new(),
        slow: Vec::new(),
        adaptive: Vec::new(),
    };
    let mut fallbacks = 0_u64;
    for (which, expr) in freeze_drivers() {
        let (series, counters) = run_driver(&format!("{DRIVER_IMPORTS}{expr}"), &bars);
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
    for (name, pair) in [
        ("fast", report.fast),
        ("slow", report.slow),
        ("adaptive", report.adaptive),
    ] {
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
    let view =
        integration_lite_view_calibrated(&ramp_rows(), 0.14, 0.5).expect("calibrated view builds");
    assert!(
        (view.threshold - CALIBRATED_PIVOT_THRESHOLD).abs() < 1e-12,
        "view carries the frozen threshold"
    );
    assert_eq!(view.fast.len(), 60);
    assert_eq!(view.slow.len(), 60);
    assert_eq!(view.ribbon.upper.len(), 60);
    // Monotonic ramp pins the oscillator high: no threshold crossings.
    assert!(
        view.pivots.is_empty(),
        "steady ramp fires no pivots: {:?}",
        view.pivots
    );
    for (index, (up, lo)) in view
        .ribbon
        .upper
        .iter()
        .zip(view.ribbon.lower.iter())
        .enumerate()
    {
        if let (Some(a), Some(b)) = (up, lo) {
            assert!(a >= b, "ribbon orders at {index}");
        }
    }
}

#[test]
fn bad_alphas_and_thresholds_refuse() {
    let rows = ramp_rows();
    assert!(
        native_lite_series(&rows, 0.0).is_err(),
        "zero EIT alpha refuses"
    );
    assert!(
        native_lite_series(&rows, 1.5).is_err(),
        "divergent EIT alpha refuses"
    );
    assert!(native_lite_series(&rows, f64::NAN).is_err());
    assert!(
        integration_lite_view(&rows, 0.14, 2.0, 100.0).is_err(),
        "fill alpha refuses"
    );
    assert!(
        integration_lite_view(&rows, 0.14, 0.5, 0.0).is_err(),
        "zero threshold refuses"
    );
}

/// Forty-bar up-then-down reversal: climbs 100 → 138 over twenty bars, then
/// descends symmetrically. Fires bounded pivots at the frozen threshold, so
/// dial tests can compare mark sets across thresholds on one live window.
fn reversal_rows() -> Vec<ChartRow> {
    (0..40_i64)
        .map(|i| {
            let close = if i < 20 {
                100.0 + 2.0 * i as f64
            } else {
                138.0 - 2.0 * (i - 20) as f64
            };
            ChartRow {
                time: i,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume: 1_000 + i as u64,
            }
        })
        .collect()
}

fn flat_fast(bars: usize, level: f64) -> Vec<Option<f64>> {
    vec![Some(level); bars]
}

fn chop_fast(bars: usize) -> Vec<Option<f64>> {
    (0..bars)
        .map(|i| Some(if i % 2 == 0 { 60.0 } else { -60.0 }))
        .collect()
}

fn rise_reversal() -> (Vec<Option<f64>>, Vec<PivotMark>) {
    let fast = [
        60.0, 80.0, 100.0, 120.0, 150.0, 170.0, 150.0, 90.0, 70.0, 60.0,
    ]
    .map(Some)
    .to_vec();
    (
        fast,
        vec![PivotMark {
            index: 7,
            side: PivotSide::High,
        }],
    )
}

fn fall_recovery() -> (Vec<Option<f64>>, Vec<PivotMark>) {
    let fast = [40.0, 10.0, -30.0, -80.0, -120.0, -90.0, -40.0, 0.0]
        .map(Some)
        .to_vec();
    (
        fast,
        vec![PivotMark {
            index: 5,
            side: PivotSide::Low,
        }],
    )
}

#[test]
fn controls_stay_quiet_at_the_frozen_threshold() {
    for (name, fast) in [("flat", flat_fast(40, 5.0)), ("chop", chop_fast(40))] {
        let marks = pivot_marks(&fast, CALIBRATED_PIVOT_THRESHOLD).expect("valid threshold");
        assert!(marks.is_empty(), "{name} control must not fire: {marks:?}");
    }
}

#[test]
fn known_pivots_score_full_precision_and_recall() {
    for (name, (fast, expected)) in [
        ("rise_reversal", rise_reversal()),
        ("fall_recovery", fall_recovery()),
    ] {
        let (precision, recall) = precision_recall(&fast, &expected, CALIBRATED_PIVOT_THRESHOLD)
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
    let (_, missed_recall) =
        precision_recall(&flat_fast(10, 5.0), &expected, CALIBRATED_PIVOT_THRESHOLD)
            .expect("valid");
    assert!(missed_recall < 1e-12, "missed pivots recall zero");
}

/// Calibration pin (task value: alert fitness): the checked-in report is
/// load-bearing — the recomputed sweep must match it number-for-number.
/// Floors pin only the frozen threshold; every other recorded number is
/// evidence the suite re-verifies rather than re-prints.
#[test]
fn calibration_report_matches_fixture() {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/pivot_calibration.json"),
    )
    .expect("calibration report loads");
    let report: serde_json::Value = serde_json::from_str(&text).expect("report parses");
    for threshold in [50.0, 100.0, 150.0, 200.0] {
        let key = format!("{threshold}");
        for (name, fast) in [("flat_40", flat_fast(40, 5.0)), ("chop_40", chop_fast(40))] {
            let marks = pivot_marks(&fast, threshold).expect("valid threshold");
            let recorded = report["controls"][name]["marks"][key.as_str()]
                .as_u64()
                .expect("control marks recorded");
            assert_eq!(marks.len() as u64, recorded, "{name} at {threshold}");
        }
        for (name, (fast, expected)) in [
            ("rise_reversal", rise_reversal()),
            ("fall_recovery", fall_recovery()),
        ] {
            let marks = pivot_marks(&fast, threshold).expect("valid threshold");
            let (precision, recall) =
                precision_recall(&fast, &expected, threshold).expect("valid threshold");
            let window = &report["windows"][name];
            let recorded_marks = window["marks"][key.as_str()]
                .as_u64()
                .expect("window marks recorded");
            let recorded_precision = window["precision"][key.as_str()]
                .as_f64()
                .expect("precision recorded");
            let recorded_recall = window["recall"][key.as_str()]
                .as_f64()
                .expect("recall recorded");
            assert_eq!(
                marks.len() as u64,
                recorded_marks,
                "{name} marks at {threshold}"
            );
            assert!(
                (precision - recorded_precision).abs() < 1e-12
                    && (recall - recorded_recall).abs() < 1e-12,
                "{name} at {threshold}: got {precision}/{recall}"
            );
        }
    }
}

#[test]
fn calibration_sweep_is_reported() {
    // Candidate sweep each window fires through; the checked-in report
    // records these numbers (floors pin only the frozen threshold above).
    let mut table = serde_json::Map::new();
    for (name, (fast, expected)) in [
        ("rise_reversal", rise_reversal()),
        ("fall_recovery", fall_recovery()),
    ] {
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
            row.insert(
                format!("{threshold}"),
                serde_json::json!({ "marks": marks.len() }),
            );
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
    let rows = vec![
        row(100.0, 101.0),
        row(100.0, 99.0),
        row(100.0, 99.0),
        row(100.0, 101.0),
    ];
    let keys = candle_momentum_keys(&rows, &[true, true, false, false]).expect("aligned inputs");
    assert_eq!(keys, vec!["bull", "bull_down", "bear", "bear_up"]);
    assert!(
        candle_momentum_keys(&rows, &[true]).is_err(),
        "key mismatch refuses"
    );
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
    assert!(
        (VOLUME_TRANSPARENCY - 60.0).abs() < 1e-12,
        "reference volume color input"
    );
}

fn test_palette() -> MomentumPalette {
    MomentumPalette {
        bull_up: [0x20, 0xB4, 0x6C],
        bear_down: [0xB4, 0x20, 0x20],
        bull_down: [0xFF, 0x98, 0x00],
        bear_up: [0x21, 0x96, 0xF3],
    }
}

#[test]
fn momentum_tint_matches_hand_computed_gradient() {
    let palette = test_palette();
    // Bull bar, mid conviction: fast_tanh(1.5) = 0.6 -> na 54, t 46;
    // leg 50 * 54 / 100 = 27 -> pos 0.635 over relief-blue -> bull-green.
    let tint = momentum_tint(100.0, 101.0, 50.0, 0.5, 1.0, &palette).expect("finite inputs tint");
    assert_eq!(
        tint,
        MomentumTint {
            red: 32,
            green: 169,
            blue: 157,
            transparency: 46.0
        }
    );
    // Bear bar, softer conviction: fast_tanh(1.0) = 0.5 -> na 45, t 55;
    // leg -40 * 45 / 100 = -18 -> pos 0.41 over bear-red -> retrace-orange.
    let tint = momentum_tint(101.0, 100.0, -40.0, 0.5, 0.5, &palette).expect("finite inputs tint");
    assert_eq!(
        tint,
        MomentumTint {
            red: 211,
            green: 81,
            blue: 19,
            transparency: 55.0
        }
    );
    // Flat reads bearish through the `else`: leg 0 -> midpoint at t 88.
    let tint = momentum_tint(100.0, 100.0, 0.0, 0.14, 0.0, &palette).expect("flat tints");
    assert_eq!(
        tint,
        MomentumTint {
            red: 218,
            green: 92,
            blue: 16,
            transparency: 88.0
        }
    );
    // Momentum clamps to +/-100 before the map.
    let over = momentum_tint(100.0, 101.0, 300.0, 0.5, 1.0, &palette).expect("clamped tints");
    let at = momentum_tint(100.0, 101.0, 100.0, 0.5, 1.0, &palette).expect("clamped tints");
    assert_eq!(over, at, "over-range momentum reads at the rail");
    assert!(
        momentum_tint(100.0, 101.0, f64::NAN, 0.5, 1.0, &palette).is_none(),
        "na propagates"
    );
    assert!(
        momentum_tint(100.0, 101.0, 50.0, f64::NAN, 1.0, &palette).is_none(),
        "na propagates"
    );
}

#[test]
fn momentum_batch_preserves_holes_and_refuses() {
    let palette = test_palette();
    let rows = ramp_rows();
    let window = &rows[..8];
    let momentum = CandleMomentum {
        health: vec![
            Some(20.0),
            None,
            Some(-30.0),
            Some(0.0),
            None,
            Some(90.0),
            Some(10.0),
            Some(-10.0),
        ],
        vol_ratio: vec![1.0; 8],
    };
    let tints = momentum_tints(window, &momentum, 0.14, &palette).expect("aligned inputs tint");
    assert_eq!(tints.len(), window.len());
    assert!(tints[0].is_some());
    assert!(
        tints[1].is_none(),
        "warming bars fall back to the discrete key"
    );
    assert!(tints[4].is_none());
    let short = CandleMomentum {
        health: vec![Some(1.0)],
        vol_ratio: vec![1.0],
    };
    assert!(
        momentum_tints(window, &short, 0.14, &palette).is_err(),
        "length mismatch refuses"
    );
    assert!(
        momentum_tints(window, &momentum, 0.0, &palette).is_err(),
        "zero alpha refuses"
    );
    assert!(
        momentum_tints(window, &momentum, f64::NAN, &palette).is_err(),
        "na alpha refuses"
    );
}

#[test]
fn momentum_series_builds_from_kernels_over_the_ramp() {
    let rows = ramp_rows();
    let momentum = candle_momentum_series(&rows, 0.14).expect("ramp builds momentum");
    assert_eq!(momentum.health.len(), rows.len());
    assert_eq!(momentum.vol_ratio.len(), rows.len());
    assert!(
        momentum.vol_ratio[..19].iter().all(|ratio| *ratio == 0.0),
        "volume SMA warmup reads zero conviction, never a partial ratio"
    );
    assert!(
        momentum.vol_ratio.iter().all(|ratio| *ratio >= 0.0),
        "conviction never negative"
    );
    assert!(
        momentum.health.iter().any(Option::is_some),
        "warmed legs score health on a 60-bar ramp"
    );
    assert!(
        candle_momentum_series(&rows, 0.0).is_err(),
        "zero EIT alpha refuses"
    );
}

#[test]
fn momentum_palette_resolves_the_pine_input_hues() {
    let palette = MomentumPalette::from_theme(&fluent_egui::ThemeConfig::default())
        .expect("default theme resolves");
    assert_eq!(
        palette,
        test_palette(),
        "theme defaults carry the barcolor inputs"
    );
}

#[test]
fn sub_pane_builds_four_bounded_oscillators_and_centered_delta_volume_histogram() {
    let rows = ramp_rows();
    let config = IntegrationLiteConfig::new()
        .eit_alpha(0.14)
        .fill_alpha(0.5)
        .ribbon_health_k(RIBBON_HEALTH_K)
        .ma_health_k(MA_HEALTH_K)
        .delta_unit(BOUNDED_DELTA_UNIT)
        .show_ribbon(true)
        .build();
    let pane: IntegrationLiteSubPane =
        integration_lite_sub_pane(&rows, &config).expect("sub pane builds");
    assert_eq!(pane.fast_bounded.len(), rows.len());
    assert_eq!(pane.slow_bounded.len(), rows.len());
    assert_eq!(pane.ribbon_health_bounded.len(), rows.len());
    assert_eq!(pane.ma_health_bounded.len(), rows.len());
    let band = crate::oscillators::BOUNDED_OSC_SCALE;
    for series in [
        &pane.fast_bounded,
        &pane.slow_bounded,
        &pane.ribbon_health_bounded,
        &pane.ma_health_bounded,
    ] {
        for value in series.iter().flatten() {
            assert!(value.abs() < band, "every readout stays bounded: {value}");
        }
    }
    assert_eq!(pane.delta_volume.buy_top.len(), rows.len());
    assert_eq!(pane.delta_volume.sell_bottom.len(), rows.len());
    for (top, bottom) in pane
        .delta_volume
        .buy_top
        .iter()
        .zip(pane.delta_volume.sell_bottom.iter())
    {
        if let Some(value) = top {
            assert!(
                *value >= 0.0 && *value <= BOUNDED_DELTA_UNIT,
                "buy top {value} in [0, unit]"
            );
        }
        if let Some(value) = bottom {
            assert!(
                *value >= 0.0 && *value <= BOUNDED_DELTA_UNIT,
                "sell bottom {value} in [0, unit]"
            );
        }
    }
    assert_eq!(
        pane.delta_confidence,
        DeltaConfidence::Low,
        "shape-estimated splits read low confidence"
    );
    assert_eq!(
        pane.pivots.len(),
        0,
        "a steady ramp fires no pivots: {:?}",
        pane.pivots
    );
    assert_eq!(
        pane.ribbon.upper.len(),
        rows.len(),
        "the ribbon leg aligns with the rows"
    );
    assert_eq!(
        pane.ribbon.bullish.len(),
        rows.len(),
        "trend keys align with the rows"
    );
    assert!(pane.show_ribbon, "the trend ribbon is enabled by config");
}

#[test]
fn integration_lite_config_derives_component_schema_and_validates_fail_closed() {
    let config = IntegrationLiteConfig::new().build();
    let names = config.field_names();
    for expected in [
        "eit_alpha",
        "fill_alpha",
        "ribbon_health_k",
        "ma_health_k",
        "delta_unit",
        "pivot_threshold",
        "show_ribbon",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} is in the schema: {names:?}"
        );
    }
    let fields = fluent_egui::schema_fields(&config.describe());
    assert_eq!(
        fields.len(),
        names.len(),
        "the schema describes every field"
    );
    let (mut inputs, mut style, mut visibility) = (0_u32, 0_u32, 0_u32);
    for field in &fields {
        match fluent_egui::classify_schema_field(field) {
            fluent_egui::SettingsTab::Inputs => inputs += 1,
            fluent_egui::SettingsTab::Style => style += 1,
            fluent_egui::SettingsTab::Visibility => visibility += 1,
        }
    }
    assert!(
        inputs > 0 && style > 0 && visibility > 0,
        "fields partition across Inputs/Style/Visibility: {inputs}/{style}/{visibility}"
    );
    assert!(
        IntegrationLiteConfig::new()
            .eit_alpha(0.0)
            .build()
            .validate()
            .is_err(),
        "zero adaptive smoothing refuses"
    );
    assert!(
        IntegrationLiteConfig::new()
            .eit_alpha(1.5)
            .build()
            .validate()
            .is_err(),
        "divergent adaptive smoothing refuses"
    );
    assert!(
        IntegrationLiteConfig::new()
            .fill_alpha(2.0)
            .build()
            .validate()
            .is_err(),
        "fill strength above one refuses"
    );
    assert!(
        IntegrationLiteConfig::new()
            .ribbon_health_k(0.0)
            .build()
            .validate()
            .is_err(),
        "zero ribbon health gain refuses"
    );
    assert!(
        IntegrationLiteConfig::new()
            .ma_health_k(0.0)
            .build()
            .validate()
            .is_err(),
        "zero MA health gain refuses"
    );
    assert!(
        IntegrationLiteConfig::new()
            .delta_unit(0.0)
            .build()
            .validate()
            .is_err(),
        "zero delta unit refuses"
    );
    let rows = ramp_rows();
    assert!(
        integration_lite_sub_pane(&rows, &IntegrationLiteConfig::new().delta_unit(0.0).build())
            .is_err(),
        "the pane refuses a config its own validation refuses"
    );
}

/// Inverse of [`crate::oscillators::squash_to_100`]: the unbounded reading
/// whose squash equals `bounded`. Used to prove the bounded pivot threshold
/// is bijective with the frozen Pine-scale threshold.
fn unsquash_100(bounded: f64) -> f64 {
    bounded / (1.0 - (bounded / crate::oscillators::BOUNDED_OSC_SCALE).abs())
}

/// Calibration (task value: alert fitness on the `[-100, +100]` band): the
/// bounded pivot threshold must not fire on the flat/chop controls, must score
/// full precision/recall on the known-reversal windows, and must be
/// algebraically bijective with the frozen `±CALIBRATED_PIVOT_THRESHOLD` under
/// the strictly monotonic `squash_to_100`.
#[test]
fn bounded_tanh_pivot_threshold_calibration_matches_unbounded_bijection() {
    // Control group (must NOT fire): squashed flat and chop fast legs stay
    // clear of the bounded threshold.
    for (name, fast) in [("flat_40", flat_fast(40, 5.0)), ("chop_40", chop_fast(40))] {
        let bounded = squash_series_to_100(&fast);
        let marks = pivot_marks(&bounded, CALIBRATED_BOUNDED_PIVOT_THRESHOLD).expect("valid");
        assert!(
            marks.is_empty(),
            "{name} control must not fire on the bounded band: {marks:?}"
        );
    }
    for (name, (fast, expected)) in [
        ("rise_reversal", rise_reversal()),
        ("fall_recovery", fall_recovery()),
    ] {
        let bounded = squash_series_to_100(&fast);
        // Target window: the bounded band scores full precision and recall.
        let (precision, recall) =
            precision_recall(&bounded, &expected, CALIBRATED_BOUNDED_PIVOT_THRESHOLD)
                .expect("valid threshold");
        assert!(
            (precision - 1.0).abs() < 1e-12 && (recall - 1.0).abs() < 1e-12,
            "{name} must score fully on the bounded band: {precision}/{recall}"
        );
        // Frozen threshold: crossing `±CALIBRATED_BOUNDED_PIVOT_THRESHOLD` on
        // the bounded band is identical to crossing `±CALIBRATED_PIVOT_THRESHOLD`
        // on the Pine band.
        assert_eq!(
            pivot_marks(&bounded, CALIBRATED_BOUNDED_PIVOT_THRESHOLD).expect("valid"),
            pivot_marks(&fast, CALIBRATED_PIVOT_THRESHOLD).expect("valid"),
            "{name}: the bounded threshold is bijective with the frozen Pine threshold"
        );
        // Candidate sweep: every bounded threshold marks exactly what its
        // unbounded preimage marks, so the bijection holds across the band.
        for candidate in [25.0, 50.0, 60.0, 66.666_666_666_666_67] {
            let unbounded = unsquash_100(candidate);
            assert_eq!(
                pivot_marks(&bounded, candidate).expect("valid"),
                pivot_marks(&fast, unbounded).expect("valid"),
                "{name}: bounded {candidate} is bijective with unbounded {unbounded}"
            );
        }
    }
}

/// A fully flat bar: no body and no range, so its shape carries no direction
/// and the delta split reads the declared neutral prior.
fn flat_row(time: i64, price: f64, volume: u64) -> ChartRow {
    ChartRow {
        time,
        open: price,
        high: price,
        low: price,
        close: price,
        volume,
    }
}

/// Calibration (task value: histogram scaling; producer-confidence vs.
/// task-value separation): zero-volume bars split to exactly zero (never
/// `NaN`), fully flat bars split neutrally at [`NEUTRAL_BUY_RATIO`], the
/// histogram never exceeds [`BOUNDED_DELTA_UNIT`], and the price-derived
/// pivot marks stay invariant to the volume-split confidence.
#[test]
fn delta_volume_histogram_controls_and_confidence_axis_separation() {
    let unit = BOUNDED_DELTA_UNIT;
    // Control: zero-volume bars. The split is exactly zero (no division by the
    // absent volume), and the histogram reads zero height without NaN.
    let zero_rows: Vec<ChartRow> = (0..8)
        .map(|i| ChartRow {
            time: i,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 0,
        })
        .collect();
    let (buy, sell, confidence) = delta_splits(&zero_rows);
    assert_eq!(
        confidence,
        DeltaConfidence::Low,
        "shape splits read producer-low"
    );
    assert!(
        buy.iter().all(|slot| *slot == Some(0.0)),
        "zero-volume buy split is zero: {buy:?}"
    );
    assert!(
        sell.iter().all(|slot| *slot == Some(0.0)),
        "zero-volume sell split is zero: {sell:?}"
    );
    let zero_view = delta_candle_view(&zero_rows, &buy, &sell, unit).expect("histogram builds");
    for (top, bottom) in zero_view.buy_top.iter().zip(zero_view.sell_bottom.iter()) {
        assert_eq!(
            *top,
            Some(0.0),
            "zero-volume buy top is exactly zero, never NaN"
        );
        assert_eq!(
            *bottom,
            Some(0.0),
            "zero-volume sell bottom is exactly zero, never NaN"
        );
    }
    // Control: fully flat doji bars split 50/50 at the neutral prior, and the
    // two legs are exact complements. (A doji with `open == close` but a
    // non-degenerate range still carries a close-position bias, so the neutral
    // control uses fully flat bars.)
    let flat_rows: Vec<ChartRow> = (0..8).map(|i| flat_row(i, 100.0, 1_000)).collect();
    let (flat_buy, flat_sell, _) = delta_splits(&flat_rows);
    for slot in &flat_buy {
        let ratio = slot.expect("finite split") / 1_000.0;
        assert!(
            (ratio - NEUTRAL_BUY_RATIO).abs() < 1e-12,
            "flat bars read the neutral prior: {ratio}"
        );
    }
    for (b, s) in flat_buy.iter().zip(flat_sell.iter()) {
        assert_eq!(*b, *s, "a neutral split is its own complement");
    }
    // Boundedness: an arbitrary volume spike never pushes a bar past the unit,
    // so the histogram stays strictly inside the oscillator viewport.
    let spiked: Vec<ChartRow> = (0..40)
        .map(|i| {
            let volume = if i == 20 { 5_000_000 } else { 100 + i as u64 };
            ChartRow {
                time: i,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                volume,
            }
        })
        .collect();
    let (spike_buy, spike_sell, _) = delta_splits(&spiked);
    let spike_view =
        delta_candle_view(&spiked, &spike_buy, &spike_sell, unit).expect("histogram builds");
    for (top, bottom) in spike_view.buy_top.iter().zip(spike_view.sell_bottom.iter()) {
        for value in [top, bottom].into_iter().flatten() {
            assert!(
                *value >= 0.0 && *value <= unit,
                "histogram stays inside the unit: {value}"
            );
        }
    }
    // Axis separation: the pivots are price-derived task-value marks, so
    // scaling every volume (a producer-confidence input) leaves them identical
    // while the split confidence stays producer-low.
    let closes: Vec<f64> = (0..40)
        .map(|i| {
            if i < 20 {
                100.0 + 2.0 * f64::from(i)
            } else {
                138.0 - 2.0 * f64::from(i - 20)
            }
        })
        .collect();
    let make_rows = |scale: u64| -> Vec<ChartRow> {
        closes
            .iter()
            .enumerate()
            .map(|(i, close)| ChartRow {
                time: i as i64,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 0.5,
                close: *close,
                volume: 1_000 * scale + i as u64,
            })
            .collect()
    };
    let config = IntegrationLiteConfig::new().delta_unit(unit).build();
    let pane_a = integration_lite_sub_pane(&make_rows(1), &config).expect("pane a builds");
    let pane_b = integration_lite_sub_pane(&make_rows(7), &config).expect("pane b builds");
    assert!(
        !pane_a.pivots.is_empty(),
        "the reversal window fires bounded pivots: {:?}",
        pane_a.pivots
    );
    assert_eq!(
        pane_a.pivots, pane_b.pivots,
        "pivots are invariant to the volume split"
    );
    assert_eq!(pane_a.delta_confidence, DeltaConfidence::Low);
    assert_eq!(pane_b.delta_confidence, DeltaConfidence::Low);
}

/// Calibration gate (producer confidence): over randomized history windows —
/// trends, gaps, dojis, spikes, flats — the shape split never reports above
/// `Low`, and rescaling every volume leaves every pivot mark identical.
/// History carries no tick tape; a claim above `Low` needs a labeled-tape
/// precision measurement first, and no consumer may branch a decision on
/// this field until that measurement lands.
#[test]
fn pivot_threshold_renders_with_the_frozen_default_and_the_reference_floor() {
    let config = IntegrationLiteConfig::new().build();
    assert!(
        (config.pivot_threshold - CALIBRATED_PIVOT_THRESHOLD).abs() < 1e-12,
        "the dial defaults to the frozen Pine-scale threshold, got {}",
        config.pivot_threshold
    );
    assert_eq!(
        config.get_field("pivot_threshold").expect("reads"),
        "100",
        "the default round-trips through field access"
    );
    let lifted = fluent_egui::schema_fields(&config.describe());
    let field = lifted
        .iter()
        .find(|field| field.name == "pivot_threshold")
        .expect("pivot_threshold lifts");
    assert_eq!(
        field.minimum,
        Some(20.0),
        "the reference floor lifts into the schema"
    );
    assert_eq!(
        field.maximum, None,
        "the reference sets no ceiling, so the schema carries none"
    );
    assert_eq!(
        fluent_egui::classify_schema_field(field),
        fluent_egui::SettingsTab::Style,
        "thresholds render on the Style tab"
    );
}

#[test]
fn pivot_threshold_refusals_name_the_field() {
    for value in [0.0, -100.0, 19.9, f64::NAN, f64::INFINITY] {
        let candidate = IntegrationLiteConfig::new().pivot_threshold(value).build();
        let error = format!("{}", candidate.validate().expect_err("refuses"));
        assert!(
            error.contains("pivot_threshold"),
            "threshold {value} names the field: {error}"
        );
    }
    for value in [20.0, 100.0, 200.0, 1000.0] {
        assert!(
            IntegrationLiteConfig::new()
                .pivot_threshold(value)
                .build()
                .validate()
                .is_ok(),
            "threshold {value} validates"
        );
    }
    let rows = ramp_rows();
    assert!(
        integration_lite_sub_pane(
            &rows,
            &IntegrationLiteConfig::new().pivot_threshold(0.0).build()
        )
        .is_err(),
        "the pane refuses a config its own validation refuses"
    );
}

#[test]
fn default_pivot_threshold_reproduces_the_frozen_marks() {
    let rows = reversal_rows();
    let pane = integration_lite_sub_pane(&rows, &IntegrationLiteConfig::new().build())
        .expect("default pane builds");
    assert!(
        !pane.pivots.is_empty(),
        "the reversal window fires at the default: {:?}",
        pane.pivots
    );
    let frozen = fluent_egui::pivot_marks(&pane.fast_bounded, CALIBRATED_BOUNDED_PIVOT_THRESHOLD)
        .expect("frozen threshold is valid");
    assert_eq!(
        pane.pivots, frozen,
        "the default dial reproduces the frozen-constant marks bar-for-bar"
    );
}

#[test]
fn explicit_pivot_thresholds_reshape_the_marks_without_touching_the_legs() {
    let rows = reversal_rows();
    let default_pane = integration_lite_sub_pane(&rows, &IntegrationLiteConfig::new().build())
        .expect("default pane builds");
    for threshold in [60.0, 150.0, 200.0] {
        let config = IntegrationLiteConfig::new()
            .pivot_threshold(threshold)
            .build();
        let pane = integration_lite_sub_pane(&rows, &config).expect("builds");
        for (name, leg, frozen_leg) in [
            ("fast", &pane.fast_bounded, &default_pane.fast_bounded),
            ("slow", &pane.slow_bounded, &default_pane.slow_bounded),
            (
                "ribbon_health",
                &pane.ribbon_health_bounded,
                &default_pane.ribbon_health_bounded,
            ),
            (
                "ma_health",
                &pane.ma_health_bounded,
                &default_pane.ma_health_bounded,
            ),
        ] {
            assert_eq!(
                leg, frozen_leg,
                "threshold {threshold} steers marks, never the {name} leg"
            );
        }
        let converted = crate::oscillators::squash_to_100(threshold);
        assert_eq!(
            pane.pivots,
            fluent_egui::pivot_marks(&pane.fast_bounded, converted).expect("valid"),
            "marks at {threshold} follow the converted dial"
        );
    }
    let quiet = integration_lite_sub_pane(
        &rows,
        &IntegrationLiteConfig::new().pivot_threshold(150.0).build(),
    )
    .expect("builds");
    assert_ne!(
        quiet.pivots, default_pane.pivots,
        "an explicit threshold visibly reshapes the marks"
    );
}

/// The script input defaults to 200 on the gained leg while the port freezes
/// the task value at 100: at 200 the frozen calibration windows go quiet, so
/// the port keeps the frozen default and records the script value as a
/// reachable dial stop instead of its default.
#[test]
fn reference_default_silences_the_calibration_windows() {
    use crate::oscillators::squash_to_100;
    for (name, (fast, _)) in [
        ("rise_reversal", rise_reversal()),
        ("fall_recovery", fall_recovery()),
    ] {
        let bounded = squash_series_to_100(&fast);
        let script_default =
            fluent_egui::pivot_marks(&bounded, squash_to_100(200.0)).expect("valid");
        assert!(
            script_default.is_empty(),
            "{name}: the script default would miss the known pivot"
        );
        let frozen =
            fluent_egui::pivot_marks(&bounded, CALIBRATED_BOUNDED_PIVOT_THRESHOLD).expect("valid");
        assert!(
            !frozen.is_empty(),
            "{name}: the frozen threshold catches the known pivot"
        );
    }
}

#[test]
fn pivot_threshold_joins_the_cache_fingerprint() {
    use crate::{ChartIndicator, IntegrationLiteIndicator};
    let plugin = IntegrationLiteIndicator::new();
    let base: Box<dyn fluent_wvr::Component> = Box::new(IntegrationLiteConfig::new().build());
    let moved: Box<dyn fluent_wvr::Component> =
        Box::new(IntegrationLiteConfig::new().pivot_threshold(150.0).build());
    assert_ne!(
        plugin.config_key(&*base).expect("keys"),
        plugin.config_key(&*moved).expect("keys"),
        "the dial moves the cache key"
    );
    let same: Box<dyn fluent_wvr::Component> = Box::new(IntegrationLiteConfig::new().build());
    assert_eq!(
        plugin.config_key(&*base).expect("keys"),
        plugin.config_key(&*same).expect("keys"),
        "the default fingerprints stably"
    );
}

#[test]
fn history_confidence_never_exceeds_low_and_never_moves_pivots() {
    let config = IntegrationLiteConfig::new().build();
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as f64 / f64::from(u32::MAX)
    };
    for seed in 0..25 {
        let mut close = 100.0 + next() * 50.0;
        let rows: Vec<ChartRow> = (0..40)
            .map(|i| {
                close += (next() - 0.48) * 4.0;
                if seed % 5 == 4 && i == 20 {
                    close += 15.0; // gap bar
                }
                let spread = 0.2 + next() * 0.8;
                ChartRow {
                    time: i,
                    open: close - spread,
                    high: close + spread,
                    low: close - spread * 2.0,
                    close,
                    volume: 100 + (next() * 5_000.0) as u64,
                }
            })
            .collect();
        let pane = integration_lite_sub_pane(&rows, &config).expect("pane builds");
        assert_eq!(
            pane.delta_confidence,
            DeltaConfidence::Low,
            "seed {seed}: history never exceeds Low"
        );
        let scaled: Vec<ChartRow> = rows
            .iter()
            .map(|row| ChartRow {
                volume: row.volume.saturating_mul(9).max(1),
                ..*row
            })
            .collect();
        let rescaled = integration_lite_sub_pane(&scaled, &config).expect("rescaled builds");
        assert_eq!(
            pane.pivots, rescaled.pivots,
            "seed {seed}: volume rescale never moves pivots"
        );
    }
}

/// Leg kinds render as dropdowns defaulting to the Pine legs; the display
/// legs stay blank to follow the panel.
#[test]
fn leg_kinds_render_as_dropdowns_with_the_pine_legs() {
    use fluent_egui::FieldKind;
    let config = IntegrationLiteConfig::new().build();
    assert_eq!(config.get_field("fast_kind").expect("reads"), "rsi");
    assert_eq!(config.get_field("slow_kind").expect("reads"), "cci");
    for name in ["fast_period", "fast_smooth_period", "fast_k"] {
        assert_eq!(
            config.get_field(name).expect("reads"),
            "",
            "{name} defaults to following the panel"
        );
    }
    let lifted = fluent_egui::schema_fields(&config.describe());
    for name in ["fast_kind", "slow_kind"] {
        let field = lifted
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"));
        assert_eq!(field.kind, FieldKind::Select, "{name} is a dropdown");
        assert_eq!(
            field.options,
            vec!["rsi", "cci", "cmo", "mfi", "stoch"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            "only normalized kernel-backed kinds are offered"
        );
    }
    for name in [
        "fast_period",
        "slow_period",
        "fast_smooth_period",
        "slow_smooth_period",
    ] {
        let field = lifted
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"));
        assert_eq!((field.minimum, field.maximum), (Some(2.0), Some(200.0)));
    }
}

/// Unknown or unmapped leg kinds refuse, and every leg bound names its
/// field.
#[test]
fn leg_kind_and_bound_refusals_name_the_field() {
    for kind in ["not-an-oscillator", "aroon", "adx", ""] {
        for slot in ["fast_kind", "slow_kind"] {
            let mut candidate = IntegrationLiteConfig::new().build();
            candidate.set_field(slot, kind).expect("writes");
            let error = format!("{}", candidate.validate().expect_err("refuses"));
            assert!(
                error.contains(slot),
                "{slot}={kind:?} names the field: {error}"
            );
        }
    }
    for (slot, value) in [
        ("fast_period", "1"),
        ("fast_period", "201"),
        ("slow_smooth_period", "1"),
        ("fast_k", "0"),
        ("slow_k", "-1"),
    ] {
        let mut candidate = IntegrationLiteConfig::new().build();
        candidate.set_field(slot, value).expect("writes");
        let error = format!("{}", candidate.validate().expect_err("refuses"));
        assert!(
            error.contains(slot),
            "{slot}={value} names the field: {error}"
        );
    }
    for smooth in ["not-a-smoothing", "eit"] {
        let mut candidate = IntegrationLiteConfig::new().build();
        candidate
            .set_field("fast_smooth_type", smooth)
            .expect("writes");
        let error = format!("{}", candidate.validate().expect_err("refuses"));
        assert!(
            error.contains("fast_smooth_type"),
            "{smooth} names the field: {error}"
        );
    }
}

/// Defaults reproduce today's legs bar-for-bar: RSI/CCI at 14, no
/// smoothing, unit gain.
#[test]
fn default_legs_reproduce_the_frozen_arms() {
    use crate::oscillators::{normalize_series_bounded, OscillatorKind};
    let rows = ramp_rows();
    let config = IntegrationLiteConfig::new().build();
    let pane = integration_lite_sub_pane(&rows, &config).expect("pane builds");
    let native = native_lite_series(&rows, 0.14).expect("native builds");
    assert_eq!(
        pane.fast_bounded,
        normalize_series_bounded(&native.fast_raw, OscillatorKind::Rsi).expect("evaluable"),
        "the default fast leg is the frozen RSI arm"
    );
    assert_eq!(
        pane.slow_bounded,
        normalize_series_bounded(&native.slow_raw, OscillatorKind::Cci).expect("evaluable"),
        "the default slow leg is the frozen CCI arm"
    );
}
