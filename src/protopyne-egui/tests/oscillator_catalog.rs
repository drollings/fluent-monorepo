use super::*;

use chrono::TimeZone as _;
use protopyne_kernels::{
    parse_pine_source, parse_program, resolve_imports, InputSet, LibraryId, LinkedModule,
    ModuleGraph, ModuleInput, PineEngine, PineRuntimeConfig, PineValue,
};
use protopyne_types::Bar;
use rust_decimal::prelude::FromPrimitive as _;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::path::PathBuf;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;
/// Drivers import the shipped libraries exactly like real dependents.
const IMPORTS: &str =
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

/// Sixty wobbling-but-rising bars: trending with enough range that even the
/// range-hungry oscillators (SRSI/STOCH) fire.
fn wobble_bars() -> Vec<Bar> {
    (0..60)
        .map(|i| {
            let close = 100.0 + 0.3 * (i as f64) + ((i % 7) as f64);
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

/// Run a driver, returning the `raw` and `out` series.
fn run_two(driver: &str, bars: &[Bar]) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let modules = linked(driver);
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    );
    let outcome = engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("linked run evaluates");
    let read = |name: &str| {
        (0..bars.len())
            .map(|bar| match outcome.value_at(bar, name) {
                Some(Some(PineValue::Float(value))) => Some(value),
                Some(Some(PineValue::Int(value))) => Some(value as f64),
                Some(None) => None,
                other => panic!("expected number or na for {name} on bar {bar}, got {other:?}"),
            })
            .collect()
    };
    (read("raw"), read("out"))
}

/// Driver that exposes the arm's raw `_data` alongside the full
/// `normalizeOsc` output, so the catalog squash can be compared against
/// Pine's squash for the identical raw input.
fn golden_driver(osc: &str) -> String {
    let raw = match osc {
        "RSI" => "raw = ta.rsi(close, 14)".to_owned(),
        "CCI" => "raw = ta.cci(close, 14)".to_owned(),
        "CMO" => "raw = ta.cmo(close, 14)".to_owned(),
        "STOCH" => "raw = ta.stoch(close, high, low, 14)".to_owned(),
        "SRSI" => "r = ta.rsi(close, 14)\nraw = ta.stoch(r, r, r, 14)".to_owned(),
        "TRENDSI" => "raw = ta.correlation(close, bar_index, 14)".to_owned(),
        "VOSS" => {
            "[v, f] = MAS.getVossPredictiveFilter(close, 14, 3, 0.25)\nraw = v - f".to_owned()
        }
        "AROON" => "up = 100 * (ta.highestbars(high, 15) + 14) / 14\n\
             down = 100 * (ta.lowestbars(low, 15) + 14) / 14\n\
             raw = up - down"
            .to_owned(),
        other => panic!("no golden raw arm for {other}"),
    };
    format!(
        "{IMPORTS}{raw}\nout = MAS.normalizeOsc(close, UTIL.Oscillators.{osc}, 0, 5, 14, UTIL.Smoothings.NA, 0)"
    )
}

/// Compare two option series slot-for-slot within tolerance.
fn assert_series_close(got: &[Option<f64>], want: &[Option<f64>], label: &str) {
    assert_eq!(got.len(), want.len(), "{label}: length");
    for (index, (g, w)) in got.iter().zip(want).enumerate() {
        match (g, w) {
            (None, None) => {}
            (Some(g), Some(w)) => {
                assert!(
                    (g - w).abs() <= EPS,
                    "{label} bar {index}: got {g}, want {w}"
                );
            }
            (g, w) => panic!("{label} bar {index}: hole mismatch, got {g:?}, want {w:?}"),
        }
    }
}

#[test]
fn catalog_rows_match_the_pine_normalize_osc_arms() {
    // The Pine `normalizeOsc` arms (offset/k/multiplier) are excerpted here
    // so a Pine edit fails this test loudly before it can drift the catalog.
    let pine: &[(&str, f64, f64, f64)] = &[
        ("AROON", -50.0, 0.005, 800.0),
        ("CCI", -50.0, 0.0035, 500.0),
        ("CMO", 0.0, 0.002, 1000.0),
        ("MFI", -50.0, 0.0084, 740.0),
        ("RSI", -50.0, 0.0084, 740.0),
        ("SRSI", -50.0, 0.0006, 7000.0),
        ("STOCH", -50.0, 0.0005, 7000.0),
        ("TRENDSI", 0.0, 0.016, 9000.0),
        ("VOSS", 0.0, 1.5, 300.0),
    ];
    for (member, offset, k, multiplier) in pine {
        let kind = parse_oscillator_kind(member).expect("pine member parses");
        let params = kind.params().expect("pine arm is evaluable");
        assert_eq!(
            params,
            OscParams {
                offset: *offset,
                k: *k,
                multiplier: *multiplier
            },
            "{member} constants moved from the pine arm"
        );
    }
    // The shipped source must still carry every excerpted arm, so the table
    // cannot silently detach from the reference.
    let text = std::fs::read_to_string(data_root().join("libraries/_libMAs.pine"))
        .expect("shipped library loads");
    let body = text
        .split("export normalizeOsc")
        .nth(1)
        .expect("normalizeOsc present")
        .split("if smoothingLen")
        .next()
        .expect("arm body bounded");
    for (member, _, _, _) in pine {
        assert!(
            body.contains(&format!("UTIL.Oscillators.{member}")),
            "{member} arm missing from shipped normalizeOsc"
        );
    }
}

#[test]
fn pine_oscillator_enum_members_all_map_to_catalog_rows() {
    let text = std::fs::read_to_string(data_root().join("libraries/_libUtil.pine"))
        .expect("shipped util loads");
    let enum_body = text
        .split("export enum Oscillators")
        .nth(1)
        .expect("Oscillators enum present")
        .split("//@enum")
        .next()
        .expect("enum body bounded");
    let mut checked = 0;
    for line in enum_body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(|c: char| c.is_ascii_alphabetic()) {
            let Some((name, value)) = rest.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name == "NA" {
                // Pine `NA` ("-" pass-through) maps to the Rust `Raw` row.
                assert_eq!(parse_oscillator_kind("-"), Some(OscillatorKind::Raw));
                checked += 1;
                continue;
            }
            let value = value
                .split("//")
                .next()
                .unwrap_or(value)
                .trim()
                .trim_matches('"');
            let kind =
                parse_oscillator_kind(value).unwrap_or_else(|| panic!("{value} has a catalog row"));
            assert_eq!(kind.pine_name(), value, "{name} pine spelling");
            checked += 1;
        }
    }
    assert!(
        checked >= 11,
        "every pine Oscillators member mapped, got {checked}"
    );
}

#[test]
fn golden_normalize_series_matches_pine_normalize_osc() {
    let bars = wobble_bars();
    let cases: &[(&str, OscillatorKind)] = &[
        ("RSI", OscillatorKind::Rsi),
        ("CCI", OscillatorKind::Cci),
        ("CMO", OscillatorKind::Cmo),
        ("STOCH", OscillatorKind::Stoch),
        ("SRSI", OscillatorKind::Srsi),
        ("TRENDSI", OscillatorKind::TrendSi),
        ("VOSS", OscillatorKind::Voss),
        ("AROON", OscillatorKind::Aroon),
    ];
    for (osc, kind) in cases {
        let (raw, pine_out) = run_two(&golden_driver(osc), &bars);
        // The run must be substantive: warmup holes aside, Pine has to emit
        // real readings or the comparison below would pass vacuously.
        assert!(
            pine_out.iter().flatten().count() >= bars.len() / 2,
            "{osc}: pine emitted too few readings ({})",
            pine_out.iter().flatten().count()
        );
        let catalog = normalize_series(&raw, *kind).expect("evaluable kind");
        assert_series_close(&catalog, &pine_out, osc);
    }
}

/// Driver for one `which` leg of a multi-leg family: the raw leg plus the
/// full `normalizeOsc` at that `which`, each using the leg's own window
/// (`which = 1` reads `fastLen`, every other leg reads `slowLen`).
fn leg_driver(osc: &str, which: i64) -> String {
    let len = if which == 1 { 5 } else { 14 };
    let raw = match (osc, which) {
        ("VOSS", _) => {
            let leg = match which {
                1 => "v",
                -1 => "f",
                _ => "v - f",
            };
            format!("[v, f] = MAS.getVossPredictiveFilter(close, {len}, 3, 0.25)\nraw = {leg}")
        }
        ("AROON", _) => {
            let leg = if which == 0 { "up - down" } else { "up" };
            format!(
                "up = 100 * (ta.highestbars(high, {window}) + {len}) / {len}\n\
                 down = 100 * (ta.lowestbars(low, {window}) + {len}) / {len}\n\
                 raw = {leg}",
                window = len + 1,
            )
        }
        other => panic!("no leg arm for {other:?}"),
    };
    format!(
        "{IMPORTS}{raw}\nout = MAS.normalizeOsc(close, UTIL.Oscillators.{osc}, {which}, 5, 14, UTIL.Smoothings.NA, 0)"
    )
}

#[test]
fn each_which_leg_matches_pine_for_the_multi_leg_families() {
    let bars = wobble_bars();
    for (osc, kind) in [
        ("VOSS", OscillatorKind::Voss),
        ("AROON", OscillatorKind::Aroon),
    ] {
        let mut distinct: Vec<Vec<Option<f64>>> = Vec::new();
        for leg in kind.which_legs() {
            let which = leg.which();
            let (raw, pine_out) = run_two(&leg_driver(osc, which), &bars);
            assert!(
                pine_out.iter().flatten().count() >= bars.len() / 2,
                "{osc} leg {which}: pine emitted too few readings"
            );
            let catalog = normalize_series(&raw, kind).expect("evaluable kind");
            assert_series_close(&catalog, &pine_out, &format!("{osc} which={which}"));
            distinct.push(pine_out);
        }
        // The legs are content-distinct wherever the reference distinguishes
        // them: fast/slow use different windows, and the spread leg adds the
        // second series the family produces.
        for (i, left) in distinct.iter().enumerate() {
            for right in &distinct[i + 1..] {
                let differs = left.iter().zip(right).any(|(a, b)| match (a, b) {
                    (Some(a), Some(b)) => (a - b).abs() > 1e-9,
                    (None, None) => false,
                    _ => true,
                });
                assert!(
                    differs,
                    "{osc}: legs {i} and the next produce identical series"
                );
            }
        }
    }
}

#[test]
fn squash_to_100_is_bijective_with_the_pivot_thresholds() {
    // A reading fires the unbounded threshold iff its bounded image fires
    // the bounded threshold; `squash_to_100(±100) = ±50` pins the rails.
    for value in [
        -1.0e6, -300.0, -100.0, -99.9, -0.0, 0.0, 99.9, 100.0, 300.0, 1.0e6,
    ] {
        let bounded = squash_to_100(value);
        let fires_unbounded = value.abs() >= crate::integration_lite::CALIBRATED_PIVOT_THRESHOLD;
        let fires_bounded = bounded.abs() >= CALIBRATED_BOUNDED_PIVOT_THRESHOLD;
        assert_eq!(
            fires_unbounded, fires_bounded,
            "{value} fires both thresholds together (bounded {bounded})"
        );
    }
    assert!(
        (squash_to_100(100.0) - 50.0).abs() < 1e-12,
        "the +100 rail maps to +50"
    );
    assert!(
        (squash_to_100(-100.0) + 50.0).abs() < 1e-12,
        "the -100 rail maps to -50"
    );
}

#[test]
fn cdv_refuses_and_raw_passes_through() {
    // `Cdv` parses (it is a listed Pine oscillator) but refuses evaluation.
    let cdv = parse_oscillator_kind("cdv").expect("cdv parses");
    assert_eq!(cdv, OscillatorKind::Cdv);
    assert!(!cdv.is_evaluable(), "cdv refuses, never substitutes");
    assert!(cdv.refusal_reason().is_some(), "cdv names its refusal");
    assert_eq!(cdv.kernel_id(), None, "cdv has no kernel");
    assert!(
        cdv.params().is_none(),
        "cdv never falls back to default constants"
    );
    let refused = normalize_series(&[Some(50.0)], cdv).expect_err("cdv refuses");
    assert!(
        refused.to_string().contains("cdv has no kernel"),
        "refusal names its reason, got {refused}"
    );
    assert!(
        normalize_series_bounded(&[Some(50.0)], cdv).is_err(),
        "bounded path also refuses"
    );
    // `Raw` is the pass-through: default constants, no kernel.
    let raw = parse_oscillator_kind("raw").expect("raw parses");
    assert!(raw.is_evaluable(), "raw evaluates as pass-through");
    assert_eq!(raw.kernel_id(), None, "raw has no kernel");
    assert_eq!(
        raw.params(),
        OscillatorKind::Rsi.params(),
        "raw shares the default constants"
    );
}

#[test]
fn which_legs_match_the_reference_dispatch() {
    // Aroon and Voss handle all three `which` legs; the rest only the
    // window-length axis.
    for kind in [OscillatorKind::Aroon, OscillatorKind::Voss] {
        assert_eq!(
            kind.which_legs(),
            &[WhichLeg::Fast, WhichLeg::Slow, WhichLeg::Spread],
            "{kind:?} handles every which leg"
        );
    }
    for kind in [
        OscillatorKind::Cci,
        OscillatorKind::Cmo,
        OscillatorKind::Mfi,
        OscillatorKind::Rsi,
        OscillatorKind::Srsi,
        OscillatorKind::Stoch,
        OscillatorKind::TrendSi,
        OscillatorKind::Raw,
    ] {
        assert_eq!(
            kind.which_legs(),
            &[WhichLeg::Fast, WhichLeg::Slow],
            "{kind:?} length axis"
        );
    }
    assert_eq!(WhichLeg::from_which(1), Some(WhichLeg::Fast));
    assert_eq!(WhichLeg::from_which(-1), Some(WhichLeg::Slow));
    assert_eq!(WhichLeg::from_which(0), Some(WhichLeg::Spread));
    assert_eq!(WhichLeg::from_which(2), None);
    assert_eq!(WhichLeg::Fast.which(), 1);
    // Every family trailing-smooths when armed (Pine gates uniformly).
    for kind in OscillatorKind::all() {
        assert!(kind.smoothing_eligible(), "{kind:?} smoothing-eligible");
    }
}

#[test]
fn catalog_ids_and_pine_names_round_trip() {
    for kind in OscillatorKind::all() {
        assert_eq!(
            parse_oscillator_kind(kind.id()),
            Some(kind),
            "{} parses",
            kind.id()
        );
        assert_eq!(
            parse_oscillator_kind(kind.pine_name()),
            Some(kind),
            "{} parses",
            kind.pine_name()
        );
        assert_eq!(
            parse_oscillator_kind(&kind.id().to_ascii_uppercase()),
            Some(kind),
            "ids parse case-insensitively"
        );
    }
    for bad in ["", "sma", "ema", "wave", "rsi "] {
        assert_eq!(parse_oscillator_kind(bad), None, "{bad:?} refuses");
    }
}
