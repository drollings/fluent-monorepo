//! Native `Bars` execution through the engine: under an explicit opt-in,
//! the per-tick `bars.fetch()` hot path runs the Rust ring while every
//! other method, field read, snapshot, and dump behaves exactly as the
//! interpreted run. No opt-in runs Pine byte-identically — the control
//! that keeps every existing suite's meaning.
//!
//! Scope v1: `fetch` only (zero-arg, no fetch-reference injection). Any
//! other method, any argument shape, any registered lower-tf injection, or
//! any untranslatable field state runs the Pine body or refuses with a
//! reason — never silently half-native. Free `libBars` functions stay
//! interpreted (stateless surface, not the hot path).
#![allow(clippy::float_cmp)]

const EPS: f64 = 1e-9;

use chrono::TimeZone as _;
use crate::pine::test_fixture as fixture;
use crate::{
    BarRing, InputSet, LibBarsProvider, LinkedModule, MethodCtx, NativeOptions, NativeProvider,
    PineEngine, PineRuntimeConfig, PineValue, RunOutcome, TickLegs, UserInstance,
};
use protopyne_types::Bar;

fn native_on() -> NativeOptions {
    NativeOptions {
        enabled: true,
        only_symbols: None,
    }
}

fn linked_fetch(driver_tail: &str) -> Vec<LinkedModule> {
    fixture::linked_modules(vec![
        fixture::shipped_module(
            "libraries/_libUtil.pine",
            "showmethegrail",
            "libUtil",
            12,
        ),
        fixture::shipped_module(
            "libraries/_libBars.pine",
            "showmethegrail",
            "libBars",
            12,
        ),
        fixture::driver_module(driver_tail),
    ])
}

fn test_engine() -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("1".to_owned())
            .build(),
    )
}

fn ms_bar(open: &str, high: &str, low: &str, close: &str, volume: u64, ts_millis: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::Utc
                .timestamp_opt(
                    ts_millis.div_euclid(1000),
                    ts_millis.rem_euclid(1000) as u32 * 1_000_000,
                )
                .single()
                .expect("valid test timestamp"),
        )
        .o(open.parse().expect("decimal"))
        .h(high.parse().expect("decimal"))
        .l(low.parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

fn fmt2(price: f64) -> String {
    format!("{price:.2}")
}

/// Synthetic one-bar tape: `(Bar legs, tick stamp)` with bar-so-far OHLC
/// and rounded cumulative volume legs over a constant bar-open time.
fn one_bar_legs(bar_open_ms: i64, prices: &[f64], volumes: &[u64], stamps: &[i64]) -> (Vec<Bar>, Vec<i64>) {
    assert_eq!(prices.len(), volumes.len(), "price per tick");
    assert_eq!(prices.len(), stamps.len(), "stamp per tick");
    let mut bars = Vec::with_capacity(prices.len());
    let mut ticks = Vec::with_capacity(prices.len());
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let mut cum = 0u64;
    for (((price, volume), stamp), index) in prices
        .iter()
        .zip(volumes.iter())
        .zip(stamps.iter())
        .zip(0usize..)
    {
        if index == 0 {
            high = *price;
            low = *price;
        } else {
            high = high.max(*price);
            low = low.min(*price);
        }
        cum += *volume;
        bars.push(ms_bar(
            &fmt2(prices[0]),
            &fmt2(high),
            &fmt2(low),
            &fmt2(*price),
            cum,
            bar_open_ms,
        ));
        ticks.push(*stamp);
    }
    (bars, ticks)
}

const T0: i64 = 1_767_344_400_000;

/// Seeding-loss budget: the first two ticks of a bar never enter a delta,
/// so their volume must stay under the 4% drift gate or the confirm pulls
/// (and refuses without an injected lower-tf pair). Quiet legs keep the
/// seeding pair tiny; the pull legs below spend it deliberately.
fn quiet_stamps() -> Vec<i64> {
    [5_000, 12_000, 20_000, 33_000, 40_000, 47_000, 52_000, 58_000]
        .iter()
        .map(|dt| T0 + dt)
        .collect::<Vec<_>>()
}

fn quiet_prices() -> Vec<f64> {
    vec![100.00, 100.05, 100.03, 100.10, 100.08, 100.12, 100.09, 100.15]
}

/// Per-tick volumes with a 3-unit seeding loss on a ~600-unit bar
/// (drift ~0.5%, remainder path, no pull).
fn quiet_volumes() -> Vec<u64> {
    vec![1, 2, 100, 100, 100, 100, 100, 100]
}

/// Per-tick volumes with a 25-unit seeding loss on a 130-unit bar
/// (drift ~19%: the confirm pulls, which refuses without injection).
fn pull_volumes() -> Vec<u64> {
    vec![10, 15, 5, 20, 10, 25, 15, 30]
}

fn fetch_driver() -> String {
    "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libBars/12 as BARS\nvar bars = BARS.Bars.new()\nif barstate.isfirst\n    bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\nbars.fetch()\n"
        .to_owned()
}

fn run_pine(modules: &[LinkedModule], bars: &[Bar], stamps: &[i64]) -> RunOutcome {
    let mut engine = test_engine();
    engine
        .run_linked_ticks(modules, bars, stamps, InputSet::new())
        .expect("pine run evaluates")
}

fn run_native(modules: &[LinkedModule], bars: &[Bar], stamps: &[i64]) -> RunOutcome {
    let mut engine = test_engine();
    engine
        .set_native_options(native_on())
        .expect("native opt-in applies");
    engine
        .run_linked_ticks(modules, bars, stamps, InputSet::new())
        .expect("native run evaluates")
}

fn bars_at(outcome: &RunOutcome, bar: usize) -> UserInstance {
    match outcome.value_at(bar, "bars") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected Bars object for bars on tick {bar}, got {other:?}"),
    }
}

fn assert_value_eq(pine: Option<&PineValue>, native: Option<&PineValue>, field: &str) {
    match (pine, native) {
        (None, None) => {}
        (Some(PineValue::Bool(a)), Some(PineValue::Bool(b))) => {
            assert_eq!(a, b, "bool field {field}");
        }
        (Some(PineValue::Int(a)), Some(PineValue::Int(b))) => {
            assert_eq!(a, b, "int field {field}");
        }
        (Some(PineValue::Float(a)), Some(PineValue::Float(b))) => {
            assert!(
                (a.is_nan() && b.is_nan()) || (a - b).abs() <= EPS,
                "float field {field}: pine {a} vs native {b}"
            );
        }
        (Some(PineValue::Int(a)), Some(PineValue::Float(b))) => {
            assert!(
                (*a as f64 - b).abs() <= EPS,
                "int/float field {field}: pine {a} vs native {b}"
            );
        }
        (Some(PineValue::Float(a)), Some(PineValue::Int(b))) => {
            assert!(
                (a - *b as f64).abs() <= EPS,
                "float/int field {field}: pine {a} vs native {b}"
            );
        }
        (Some(PineValue::Str(a)), Some(PineValue::Str(b))) => {
            assert_eq!(a, b, "str field {field}");
        }
        (Some(PineValue::EnumHandle(a)), Some(PineValue::EnumHandle(b))) => {
            assert_eq!(a.member, b.member, "enum field {field}");
            assert_eq!(a.enum_name, b.enum_name, "enum type {field}");
        }
        (Some(PineValue::Array(a)), Some(PineValue::Array(b))) => {
            assert_eq!(a.len(), b.len(), "array field {field} length");
            for (index, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                assert_value_eq(x.as_ref(), y.as_ref(), &format!("{field}[{index}]"));
            }
        }
        (None, Some(_)) | (Some(_), None) => {
            panic!("na-ness mismatch on field {field}: pine {pine:?} vs native {native:?}");
        }
        _ => panic!("type mismatch on field {field}: pine {pine:?} vs native {native:?}"),
    }
}

fn assert_instances_eq(pine: &UserInstance, native: &UserInstance) {
    assert_eq!(pine.type_name, native.type_name, "instance type");
    assert_eq!(
        pine.fields.len(),
        native.fields.len(),
        "field count: pine {:?} vs native {:?}",
        pine.fields.keys().collect::<Vec<_>>(),
        native.fields.keys().collect::<Vec<_>>()
    );
    for (name, pine_value) in &pine.fields {
        let native_value = native
            .fields
            .get(name)
            .unwrap_or_else(|| panic!("native instance drops field {name}"));
        assert_value_eq(pine_value.as_ref(), native_value.as_ref(), name);
    }
}

#[test]
fn default_engine_runs_pine_without_opt_in() {
    let modules = linked_fetch(&fetch_driver());
    let (bars, stamps) = one_bar_legs(T0, &quiet_prices(), &quiet_volumes(), &quiet_stamps());
    let plain = run_pine(&modules, &bars, &stamps);
    let mut explicit = test_engine();
    explicit
        .set_native_options(NativeOptions {
            enabled: false,
            only_symbols: None,
        })
        .expect("explicit pine opt-out applies");
    let opted_out = explicit
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("opt-out run evaluates");
    assert_instances_eq(
        &bars_at(&plain, bars.len() - 1),
        &bars_at(&opted_out, bars.len() - 1),
    );
}

#[test]
fn bridge_fetch_matches_pine_wholesale_on_one_bar() {
    let modules = linked_fetch(&fetch_driver());
    let (bars, stamps) = one_bar_legs(T0, &quiet_prices(), &quiet_volumes(), &quiet_stamps());
    let pine = run_pine(&modules, &bars, &stamps);
    let native = run_native(&modules, &bars, &stamps);
    for tick in 0..bars.len() {
        assert_instances_eq(&bars_at(&pine, tick), &bars_at(&native, tick));
    }
}

#[test]
fn bridge_pull_refusal_matches_pine_word_for_word() {
    // Drift over the gate with no injected lower-tf pair: Pine refuses
    // naming the live fetch. The native path must refuse identically —
    // same outcome, same message — never invent pull rows.
    let modules = linked_fetch(&fetch_driver());
    let (bars, stamps) = one_bar_legs(T0, &quiet_prices(), &pull_volumes(), &quiet_stamps());
    let mut pine_engine = test_engine();
    let pine_err = pine_engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect_err("pine pull refuses without injection");
    let mut native_engine = test_engine();
    native_engine
        .set_native_options(native_on())
        .expect("native opt-in applies");
    let native_err = native_engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect_err("native pull refuses without injection");
    assert_eq!(
        format!("{pine_err}"),
        format!("{native_err}"),
        "pull refusal identical under both positions"
    );
    assert!(
        format!("{native_err}").contains("security_lower_tf"),
        "refusal names the live fetch: {native_err}"
    );
}

#[test]
fn bridge_fetch_matches_pine_across_bar_boundary() {
    let modules = linked_fetch(&fetch_driver());
    let (mut bars, mut stamps) = one_bar_legs(
        T0,
        &[100.00, 100.05, 100.03],
        &[1, 2, 100],
        &[5_000, 12_000, 20_000]
            .iter()
            .map(|dt| T0 + dt)
            .collect::<Vec<_>>(),
    );
    let (tail_bars, tail_stamps) = one_bar_legs(
        T0 + 60_000,
        &[100.10, 100.08, 100.12],
        &[1, 2, 100],
        &[65_000, 72_000, 80_000]
            .iter()
            .map(|dt| T0 + dt)
            .collect::<Vec<_>>(),
    );
    bars.extend(tail_bars);
    stamps.extend(tail_stamps);
    let pine = run_pine(&modules, &bars, &stamps);
    let native = run_native(&modules, &bars, &stamps);
    for tick in 0..bars.len() {
        assert_instances_eq(&bars_at(&pine, tick), &bars_at(&native, tick));
    }
}

#[test]
fn unserved_method_runs_pine_over_native_state() {
    let driver = "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libBars/12 as BARS\nvar bars = BARS.Bars.new()\nif barstate.isfirst\n    bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\nbars.fetch()\nvol_imb = bars.getVolumeImbalance()\n";
    let modules = linked_fetch(driver);
    let (bars, stamps) = one_bar_legs(
        T0,
        &[100.00, 100.05, 100.03, 100.10],
        &[1, 2, 100, 100],
        &[5_000, 12_000, 20_000, 33_000]
            .iter()
            .map(|dt| T0 + dt)
            .collect::<Vec<_>>(),
    );
    let pine = run_pine(&modules, &bars, &stamps);
    let native = run_native(&modules, &bars, &stamps);
    for tick in 0..bars.len() {
        assert_instances_eq(&bars_at(&pine, tick), &bars_at(&native, tick));
        let pine_imb = pine
            .value_at(tick, "vol_imb")
            .unwrap_or_else(|| panic!("pine vol_imb missing on tick {tick}"));
        let native_imb = native
            .value_at(tick, "vol_imb")
            .unwrap_or_else(|| panic!("native vol_imb missing on tick {tick}"));
        assert_value_eq(pine_imb.as_ref(), native_imb.as_ref(), &format!("vol_imb@{tick}"));
    }
}

#[test]
fn provider_call_reports_handled_with_matching_fields() {
    // Direct provider proof (independent of the engine consult): the call
    // reports handled and its fields equal the Pine post-tick state.
    let modules = linked_fetch(&fetch_driver());
    let prices = vec![100.00, 100.05, 100.03, 100.10];
    let volumes = vec![1u64, 2, 100, 100];
    let stamps: Vec<i64> = [5_000, 12_000, 20_000, 33_000]
        .iter()
        .map(|dt| T0 + dt)
        .collect();
    let (bars, _) = one_bar_legs(T0, &prices, &volumes, &stamps);
    let pine = run_pine(&modules, &bars, &stamps);
    let fields = bars_at(&pine, 2).fields.clone();
    let cum: u64 = volumes.iter().sum();
    let ctx = MethodCtx {
        is_new: false,
        is_confirmed: true,
        legs: TickLegs {
            open: prices[0],
            high: prices.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            low: prices.iter().copied().fold(f64::INFINITY, f64::min),
            close: prices[3],
            volume: cum as f64,
        },
        bar_open_ms: T0,
        tick_ms: stamps[3],
    };
    let provider = LibBarsProvider;
    let mut next = fields;
    let value = provider
        .call("Bars", "fetch", &mut next, &[], &ctx)
        .expect("provider call runs")
        .expect("provider reports handled");
    assert!(
        matches!(value, Some(PineValue::Object(_))),
        "fetch yields the receiver object"
    );
    assert_instances_eq(&bars_at(&pine, 3), &UserInstance::new("Bars", next));
}

#[test]
fn corrupt_state_refuses_through_the_engine() {
    // Dispatch-engagement probe with no scaffolding: shrinking a ring
    // array from the driver makes hydration fail, and the run must carry
    // the bridge's refusal marker — Pine alone could never emit it.
    let driver = "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libBars/12 as BARS\nvar bars = BARS.Bars.new()\nif barstate.isfirst\n    bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\n    bars.ch := [1.0]\nbars.fetch()\n";
    let modules = linked_fetch(driver);
    let (bars, stamps) = one_bar_legs(T0, &quiet_prices(), &quiet_volumes(), &quiet_stamps());
    let mut engine = test_engine();
    engine
        .set_native_options(native_on())
        .expect("native opt-in applies");
    let err = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect_err("ragged ring refuses through the engine");
    assert!(
        format!("{err}").contains("native Bars bridge"),
        "refusal carries the bridge marker: {err}"
    );
}

#[test]
fn bridge_refuses_ragged_ring_arrays() {
    let modules = linked_fetch(&fetch_driver());
    let (bars, stamps) = one_bar_legs(
        T0,
        &[100.00, 100.05, 100.03, 100.10],
        &[1, 2, 100, 100],
        &[5_000, 12_000, 20_000, 33_000]
            .iter()
            .map(|dt| T0 + dt)
            .collect::<Vec<_>>(),
    );
    let pine = run_pine(&modules, &bars, &stamps);
    let mut fields = bars_at(&pine, bars.len() - 1).fields.clone();
    let ch_len = match fields.get("ch") {
        Some(Some(PineValue::Array(items))) => items.len(),
        other => panic!("expected ch array, got {other:?}"),
    };
    fields.insert(
        "ct".to_owned(),
        Some(PineValue::Array(vec![Some(PineValue::Int(1)); ch_len + 1])),
    );
    let err = BarRing::from_pine_fields(&fields).expect_err("ragged arrays refuse");
    assert!(
        err.to_string().contains("ct"),
        "refusal names the ragged array: {err}"
    );
}

#[test]
fn bridge_rerun_is_deterministic() {
    let modules = linked_fetch(&fetch_driver());
    let (bars, stamps) = one_bar_legs(T0, &quiet_prices(), &quiet_volumes(), &quiet_stamps());
    let first = run_native(&modules, &bars, &stamps);
    let second = run_native(&modules, &bars, &stamps);
    for tick in 0..bars.len() {
        assert_instances_eq(&bars_at(&first, tick), &bars_at(&second, tick));
    }
}

#[test]
fn subset_options_refuse_at_set_time() {
    let mut engine = test_engine();
    let err = engine
        .set_native_options(NativeOptions {
            enabled: true,
            only_symbols: Some(vec!["Bars".to_owned()]),
        })
        .expect_err("symbol subset refuses");
    assert!(
        err.to_string().contains("whole-library"),
        "refusal names the whole-library rule: {err}"
    );
}
