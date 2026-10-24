use super::*;
use crate::oscillators::{
    normalize_series, OscillatorKind, WhichLeg, DISPLAY_EXTREME_BAND, DISPLAY_RULE_BAND,
};
use fluent_egui::band_squash;

fn ramp_rows(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: i as i64,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

fn evaluated_display(
    workspace: &ChartWorkspace,
    slot: &str,
) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let evaluation = workspace.evaluations.get(slot).expect("slot evaluates");
    let series = evaluation.series.first().expect("one series");
    (series.values.clone(), series.display_values.clone())
}

#[test]
fn mapped_oscillators_paint_normalized_display() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    for (slot, kind) in [("rsi", OscillatorKind::Rsi), ("cci", OscillatorKind::Cci)] {
        workspace.slot_indicator(slot).expect("kernel slots");
        workspace.recompute().expect("evaluations build");
        let (raw, display) = evaluated_display(&workspace, slot);
        let catalog = normalize_series(&raw, kind).expect("kind evaluates");
        let expected: Vec<Option<f64>> = catalog
            .iter()
            .map(|slot| {
                slot.map(|value| band_squash(value, DISPLAY_RULE_BAND, DISPLAY_EXTREME_BAND))
            })
            .collect();
        assert_eq!(
            display, expected,
            "{slot} display equals the catalog squash constrained into the extremes band"
        );
        assert_ne!(raw, display, "{slot} display differs from raw on a ramp");
        workspace.remove_indicator(slot).expect("slot removes");
        workspace.recompute().expect("evaluations build");
    }
}

#[test]
fn mapped_series_publish_raw_under_unchanged_keys() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");
    let evaluation = workspace.evaluations.get("rsi").expect("rsi evaluates");
    let series = evaluation.series.first().expect("one series");
    let node = series.node.clone();
    let key = series.key;
    let catalog_values = workspace
        .series_catalog
        .resolve(&node)
        .expect("node publishes")
        .to_vec();
    assert_eq!(
        catalog_values, series.values,
        "the catalog carries raw values under the unchanged node"
    );
    assert_eq!(
        workspace.series_catalog.series_key(&node),
        Some(key),
        "the catalog key matches the stored series key"
    );
    // A consumer reading series:<node> sees raw, never display.
    let consumer = workspace
        .series_catalog
        .resolve(&node)
        .expect("consumer resolves");
    assert_eq!(
        consumer.to_vec(),
        series.values,
        "a series:<node> consumer resolves raw"
    );
}

#[test]
fn extremes_band_pins_fixed_points_and_asymptotes() {
    for fixed in [0.0, 100.0, -100.0] {
        let squashed = band_squash(fixed, DISPLAY_RULE_BAND, DISPLAY_EXTREME_BAND);
        assert!(
            (squashed - fixed).abs() <= 1e-9,
            "the rules sit exactly on the display: {fixed} -> {squashed}"
        );
    }
    for extreme in [500.0, 740.0, 5000.0, -500.0, -740.0, -5000.0] {
        let squashed = band_squash(extreme, DISPLAY_RULE_BAND, DISPLAY_EXTREME_BAND);
        assert!(
            squashed.abs() < DISPLAY_EXTREME_BAND,
            "overshoot compresses inside the extremes: {extreme} -> {squashed}"
        );
        assert!(
            squashed.abs() > DISPLAY_RULE_BAND,
            "extremes stay past the rules: {extreme} -> {squashed}"
        );
    }
    let mut previous = f64::NEG_INFINITY;
    let mut value = -5000.0;
    while value <= 5000.0 {
        let squashed = band_squash(value, DISPLAY_RULE_BAND, DISPLAY_EXTREME_BAND);
        assert!(
            squashed >= previous,
            "the band squash never decreases: {value} -> {squashed}"
        );
        previous = squashed;
        value += 7.0;
    }
}

#[test]
fn warmup_holes_stay_holes_through_the_mapping() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(30)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");
    let evaluation = workspace.evaluations.get("rsi").expect("rsi evaluates");
    let series = evaluation.series.first().expect("one series");
    assert!(
        series.values.first().is_some_and(Option::is_none),
        "raw warms up with a hole"
    );
    for (raw, display) in series.values.iter().zip(series.display_values.iter()) {
        if raw.is_none() {
            assert!(
                display.is_none(),
                "warmup holes stay holes through the mapping"
            );
        }
    }
    assert_eq!(
        series.values.len(),
        series.display_values.len(),
        "display preserves length"
    );
    let _ = WhichLeg::Slow;
}

/// Aroon parity (bridge lemma): the native 0..1 fractions rescaled by
/// `(P * r - 1) / (P - 1)` and squashed through the frozen catalog row
/// reproduce Pine's `normalizeOsc` Aroon arm bar-for-bar at `P = L + 1`.
/// Both engines break ties to the most recent bar, so the shared extremum
/// carries the same window on both sides. The down leg goes through an
/// inlined mirror of the shipped up-arm line: `normalizeOsc` exposes no
/// down arm (its `which = ±1` branches both read the highs).
#[test]
fn aroon_bridge_matches_the_pine_arms() {
    use crate::topology::{kernel_preview_frame, kernel_series};
    use fluent_egui::affine_squash;
    let rows = aroon_probe_rows();
    let bars = aroon_probe_bars(&rows);
    for pine_len in [8_usize, 14] {
        let native_period = pine_len + 1;
        let up = run_aroon_driver(&bars, true, pine_len);
        let down = run_aroon_driver(&bars, false, pine_len);
        assert_eq!(up.fallbacks, 0, "the up driver runs without fallback");
        assert_eq!(down.fallbacks, 0, "the down driver runs without fallback");
        let frame = kernel_preview_frame(&rows);
        let params =
            protopyne_kernels::indicator::ParamSet::new().with_int("length", native_period as i64);
        let native = kernel_series("aroon", &params, &frame).expect("aroon evaluates");
        let native_down: Vec<Option<f64>> = protopyne_kernels::indicator::shared_registry()
            .expect("registry builds")
            .get("aroon")
            .expect("aroon registers")
            .batch(&frame.views(), &params)
            .expect("batch evaluates")
            .secondary("down")
            .expect("down leg")
            .to_vec();
        let row = crate::oscillators::OscillatorKind::Aroon
            .params()
            .expect("aroon row");
        let mut compared = 0usize;
        for ((native, pine), (native_d, pine_d)) in native
            .iter()
            .zip(up.series.iter())
            .zip(native_down.iter().zip(down.series.iter()))
        {
            match (native, pine, native_d, pine_d) {
                (Some(a), Some(b), Some(c), Some(d))
                    if a.is_finite() && b.is_finite() && c.is_finite() && d.is_finite() =>
                {
                    let bridged = (native_period as f64 * a - 1.0) / (native_period as f64 - 1.0);
                    let bridged_down =
                        (native_period as f64 * c - 1.0) / (native_period as f64 - 1.0);
                    assert!(
                        ((affine_squash(bridged * 100.0, row.offset, row.k, row.multiplier) - b)
                            .abs()
                            < 1e-9),
                        "up leg matches at pine length {pine_len}"
                    );
                    // The down driver returns the raw arm (the shipped
                    // function exposes no down leg), so the comparison
                    // happens pre-squash; the squash itself is shared math
                    // pinned by the up leg.
                    assert!(
                        ((bridged_down * 100.0 - d).abs() < 1e-9),
                        "down leg matches at pine length {pine_len}"
                    );
                    compared += 1;
                }
                (None, None, None, None) => {}
                _ => panic!("presence mismatch at pine length {pine_len}"),
            }
        }
        assert!(compared >= 60, "the windows co-read: {compared}");
    }
}

/// Aroon slots paint rescaled display: the evaluated legs equal the Pine
/// arms constrained into the extremes band, and both legs map.
#[test]
fn aroon_slots_paint_rescaled_display() {
    use crate::indicators::{KernelIndicator, KernelIndicatorConfig};
    use crate::oscillators::OscillatorKind;
    let rows = aroon_probe_rows();
    let bars = aroon_probe_bars(&rows);
    let pine = run_aroon_driver(&bars, true, 8);
    let plugin = KernelIndicator::new("aroon").expect("builds");
    let mut config = KernelIndicatorConfig::defaults_for("aroon");
    config.params.insert("length".to_owned(), "9".to_owned());
    let resolved = config.resolve_panel_defaults(
        crate::oscillator_panel::OscillatorPanelState::new()
            .build()
            .entry("aroon")
            .expect("aroon row"),
    );
    assert_eq!(
        crate::oscillators::oscillator_mapping_for_output("aroon", "value"),
        Some((OscillatorKind::Aroon, crate::oscillators::WhichLeg::Slow)),
        "the up leg maps"
    );
    assert_eq!(
        crate::oscillators::oscillator_mapping_for_output("aroon", "down"),
        Some((OscillatorKind::Aroon, crate::oscillators::WhichLeg::Slow)),
        "the down leg maps"
    );
    assert!(
        crate::indicators::kernel_normalizes("aroon"),
        "aroon normalizes once every leg maps"
    );
    let boxed: Box<dyn fluent_wvr::Component> = Box::new(resolved);
    let evaluation = plugin
        .evaluate(&rows, &*boxed, &crate::topology::SeriesCatalog::empty())
        .expect("evaluates");
    assert_eq!(evaluation.series.len(), 2, "both legs publish");
    assert_eq!(
        evaluation.series[1].display_mapping,
        Some((OscillatorKind::Aroon, crate::oscillators::WhichLeg::Slow)),
        "the down leg maps"
    );
    assert!(
        evaluation.series[1]
            .display_values
            .iter()
            .any(Option::is_some),
        "the down leg paints past warmup"
    );
    let display = &evaluation.series[0].display_values;
    let mut compared = 0usize;
    for (got, want) in display.iter().zip(pine.series.iter()) {
        match (got, want) {
            (Some(a), Some(b)) if a.is_finite() && b.is_finite() => {
                let expected = band_squash(*b, DISPLAY_RULE_BAND, DISPLAY_EXTREME_BAND);
                if (a - expected).abs() >= 1e-9 {
                    eprintln!("MISMATCH compared={compared} slot={a} expected={expected} pine={b}");
                }
                assert!(
                    (a - expected).abs() < 1e-9,
                    "the slot paints the Pine arm through the band"
                );
                compared += 1;
            }
            (None, None) => {}
            _ => panic!("presence mismatch in slot display"),
        }
    }
    assert!(compared >= 60, "the slot leg co-reads: {compared}");
}

fn aroon_probe_rows() -> Vec<ChartRow> {
    let closes: Vec<f64> = (0..40)
        .map(|i| 100.0 + f64::from(i))
        .chain(std::iter::repeat_n(140.0, 20))
        .chain((0..30).map(|i| 140.0 - f64::from(i)))
        .collect();
    closes
        .iter()
        .enumerate()
        .map(|(i, close)| {
            let spike = if i == 75 { 6.0 } else { 0.0 };
            ChartRow {
                time: i as i64,
                open: close - 0.5,
                high: close + 0.5 + spike,
                low: close - 0.5,
                close: *close,
                volume: 10,
            }
        })
        .collect()
}

fn aroon_probe_bars(rows: &[ChartRow]) -> Vec<protopyne_types::Bar> {
    use chrono::TimeZone as _;
    use rust_decimal::prelude::FromPrimitive as _;
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let px = |v: f64| rust_decimal::Decimal::from_f64(v).expect("exact test decimal");
            protopyne_types::Bar::new()
                .ts(chrono::Utc
                    .with_ymd_and_hms(2026, 1, 5, 0, 0, 0)
                    .single()
                    .expect("valid time")
                    + chrono::Duration::minutes(i as i64))
                .o(px(row.open))
                .h(px(row.high))
                .l(px(row.low))
                .c(px(row.close))
                .volume(row.volume)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

struct AroonPineArm {
    series: Vec<Option<f64>>,
    fallbacks: u64,
}

fn aroon_linked(driver: &str) -> Vec<protopyne_kernels::LinkedModule> {
    use std::collections::HashMap;
    fn shipped(
        file: &str,
        publisher: &str,
        name: &str,
        version: u64,
    ) -> protopyne_kernels::ModuleInput {
        let text = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../data/protopyne-rs")
                .join(file),
        )
        .expect("shipped library loads");
        let source = protopyne_kernels::parse_pine_source(file, &text);
        let program = protopyne_kernels::parse_program(&text).expect("shipped library parses");
        protopyne_kernels::ModuleInput::new(
            protopyne_kernels::LibraryId {
                publisher: publisher.to_owned(),
                name: name.to_owned(),
                version,
            },
            source,
            program,
        )
    }
    let source = protopyne_kernels::parse_pine_source("local/Driver", driver);
    let program = protopyne_kernels::parse_program(driver).expect("driver parses");
    let driver = protopyne_kernels::ModuleInput::new(
        protopyne_kernels::LibraryId {
            publisher: "local".to_owned(),
            name: "Driver".to_owned(),
            version: 1,
        },
        source,
        program,
    );
    let graph = protopyne_kernels::resolve_imports(vec![
        shipped("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        shipped("libraries/_libMAs.pine", "showmethegrail", "libMAs", 95),
        driver,
    ])
    .expect("modules resolve");
    graph
        .order()
        .iter()
        .map(|id| {
            let entry = graph.get(id).expect("ordered module resolves");
            protopyne_kernels::LinkedModule {
                key: id.to_string(),
                program: entry.program.clone(),
                aliases: graph
                    .alias_table(id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(alias, target)| (alias, target.to_string()))
                    .collect::<HashMap<String, String>>(),
            }
        })
        .collect()
}

fn run_aroon_driver(bars: &[protopyne_types::Bar], up: bool, len: usize) -> AroonPineArm {
    // The up leg goes through the shipped arm (which=1 reads the highs);
    // the shipped function exposes no down arm (its `which = -1` branch
    // also reads the highs), so the down leg goes through an inlined
    // mirror of the same line over the lows.
    let driver = if up {
        format!("import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libMAs/95 as MAS\nout = MAS.normalizeOsc(close, UTIL.Oscillators.AROON, 1, {len}, {len}, UTIL.Smoothings.NA, 0)")
    } else {
        let window = len + 1;
        format!("out = 100 * (ta.lowestbars(low, {window}) + {len}) / {len}")
    };
    let modules = aroon_linked(&driver);
    let mut engine = protopyne_kernels::PineEngine::new(
        protopyne_kernels::PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    );
    let outcome: protopyne_kernels::RunOutcome = engine
        .run_linked(&modules, bars, protopyne_kernels::InputSet::new())
        .expect("linked run evaluates");
    let series = (0..bars.len())
        .map(|bar| match outcome.value_at(bar, "out") {
            Some(Some(protopyne_kernels::PineValue::Float(value))) => Some(value),
            Some(Some(protopyne_kernels::PineValue::Int(value))) => Some(value as f64),
            Some(None) => None,
            other => panic!("expected number or na for out on bar {bar}, got {other:?}"),
        })
        .collect();
    AroonPineArm {
        series,
        fallbacks: engine.fetch_counters().fallbacks(),
    }
}
