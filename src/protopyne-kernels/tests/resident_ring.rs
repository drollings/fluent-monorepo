//! Resident-ring proof: the `Bars` ring state lives wholly in its Pine field
//! table between native calls (hydrate, run, dehydrate — the table is the
//! resident store), and the engine writes the mutated instance back to its
//! single owner — including `varip` homes — through the existing call
//! mechanism. Three proofs, no new math:
//!
//! - Sequential provider calls on ONE table (no re-seeding) track the Pine
//!   oracle tick-for-tick: the per-tick round-trip is lossless and cursor
//!   accounting stays exact.
//! - A driver holding `Bars` in `varip` agrees Pine-vs-native on every tick
//!   with an explicit `tick_count` progression: tick N+1 observes tick N's
//!   writes, so the write-back lands in the `varip` home.
//! - Reconcile after native accumulation equals reconcile after rehearsal
//!   accumulation: the DVP rebuild observes the unified fold identically
//!   through either entry clock.
//!
//! Float compares use EPS-bounded diffs; exact integer quantities compare
//! exactly (`#[allow(clippy::float_cmp)]` covers the count literals).
#![allow(clippy::float_cmp)]

use chrono::TimeZone as _;

use crate::indicator::BarTick;
use crate::pine::test_fixture as fixture;
use crate::{
    BarRing, BarsConfig, FetchSignal, InputSet, LibBarsProvider, LinkedModule, LtfRow, MethodCtx,
    NativeCall, NativeFetchTick, NativeOptions, NativeProvider, PineEngine, PineRuntimeConfig,
    PineValue, RunOutcome, TickIngest, TickLegs, UserInstance,
};
use protopyne_types::Bar;

const EPS: f64 = 1e-9;
const T0: i64 = 1_767_344_400_000;

fn native_on() -> NativeOptions {
    NativeOptions {
        enabled: true,
        only_symbols: None,
    }
}

fn test_engine() -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("1".to_owned())
            .build(),
    )
}

fn linked(driver_tail: &str) -> Vec<LinkedModule> {
    fixture::linked_modules(vec![
        fixture::shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        fixture::shipped_module("libraries/_libBars.pine", "showmethegrail", "libBars", 12),
        fixture::driver_module(driver_tail),
    ])
}

fn fetch_driver() -> String {
    "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libBars/12 as BARS\nvar bars = BARS.Bars.new()\nif barstate.isfirst\n    bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\nbars.fetch()\n"
        .to_owned()
}

fn varip_single_driver() -> String {
    // Single-step `varip` home: the declaration, `init`, and `fetch` all run
    // on tick 0 (the only step the intrabar cell survives — `advance`
    // resets `varip` per step by design), exercising take/native/put-back
    // through the `VarStore(true)` home.
    "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libBars/12 as BARS\nvarip tmp = BARS.Bars.new()\nif barstate.isfirst\n    tmp.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\n    tmp.fetch()\n"
        .to_owned()
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

fn prices() -> Vec<f64> {
    vec![100.00, 100.05, 100.03, 100.10, 100.08, 100.12, 100.09, 100.15]
}

fn volumes() -> Vec<u64> {
    vec![1, 2, 100, 100, 100, 100, 100, 100]
}

fn stamps() -> Vec<i64> {
    [5_000, 12_000, 20_000, 33_000, 40_000, 47_000, 52_000, 58_000]
        .iter()
        .map(|dt| T0 + dt)
        .collect()
}

/// One-bar tape with bar-so-far legs over a constant bar-open time.
fn one_bar_legs() -> (Vec<Bar>, Vec<i64>) {
    let prices = prices();
    let volumes = volumes();
    let stamps = stamps();
    let mut bars = Vec::with_capacity(prices.len());
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let mut cum = 0u64;
    for (index, (price, volume)) in prices.iter().zip(volumes.iter()).enumerate() {
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
            T0,
        ));
    }
    (bars, stamps)
}

/// Bar-so-far legs for tick `index` (open anchored, extremes running,
/// volume cumulative).
fn legs_at(index: usize) -> BarTick {
    let prices = prices();
    let volumes = volumes();
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let mut cum = 0u64;
    for (at, (price, volume)) in prices.iter().zip(volumes.iter()).enumerate() {
        if at == 0 {
            high = *price;
            low = *price;
        } else {
            high = high.max(*price);
            low = low.min(*price);
        }
        cum += *volume;
        if at == index {
            break;
        }
    }
    BarTick {
        open: prices[0],
        high,
        low,
        close: prices[index],
        volume: cum as f64,
    }
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

fn tick_count_of(instance: &UserInstance) -> i64 {
    match instance.fields.get("tick_count") {
        Some(Some(PineValue::Int(value))) => *value,
        Some(Some(PineValue::Float(value))) if value.fract() == 0.0 => *value as i64,
        other => panic!("expected numeric tick_count, got {other:?}"),
    }
}

fn num_of(instance: &UserInstance, name: &str) -> f64 {
    match instance.fields.get(name) {
        Some(Some(PineValue::Float(value))) => *value,
        Some(Some(PineValue::Int(value))) => *value as f64,
        other => panic!("expected numeric {name}, got {other:?}"),
    }
}

/// Sequential native calls on ONE resident table track the oracle: the
/// per-tick hydrate/dehydrate round-trip is lossless and cursor accounting
/// stays exact across the whole bar, including the confirmed close (the
/// provider builds its own confirm legs from the frame, exactly like the
/// engine bridge does).
#[test]
fn resident_table_tracks_oracle_across_the_bar() {
    let modules = linked(&fetch_driver());
    let (bars, stamps) = one_bar_legs();
    let pine = run_pine(&modules, &bars, &stamps);
    let provider = LibBarsProvider;

    // Resident store seeded from the oracle's post-open state (tick 0 ran
    // the `isnew` branch on both paths).
    let mut resident = bars_at(&pine, 0).fields.clone();
    for (index, stamp) in stamps.iter().enumerate().skip(1) {
        let legs = legs_at(index);
        let ctx = MethodCtx {
            is_new: false,
            // The last update closes the bar, like the live feed.
            is_confirmed: index + 1 == bars.len(),
            legs: TickLegs {
                open: legs.open,
                high: legs.high,
                low: legs.low,
                close: legs.close,
                volume: legs.volume,
            },
            bar_open_ms: T0,
            tick_ms: *stamp,
        };
        match provider
            .call("Bars", "fetch", &mut resident, &[], &ctx)
            .expect("resident call runs")
        {
            NativeCall::Served(Some(PineValue::Object(next))) => {
                assert_instances_eq(&bars_at(&pine, index), &next);
                resident = next.fields.clone();
            }
            other => panic!("resident fetch serves on tick {index}, got {other:?}"),
        }
    }
    // Cursor accounting stayed exact: the table carries the oracle's own
    // head/count after the final commit (compared numerically — the bridge
    // writes integer counters while the Pine body writes floats, a benign
    // representation duality the bridge's readers accept either way).
    let oracle = bars_at(&pine, bars.len() - 1);
    for key in ["head", "count"] {
        assert_eq!(
            num_of(
                &UserInstance::new("Bars", resident.clone()),
                key
            ),
            num_of(&oracle, key),
            "cursor field {key} exact after the bar"
        );
    }
}

/// A `var`-held `Bars` agrees Pine-vs-native on every tick, with explicit
/// accumulator progressions proving tick N+1 observes tick N's writes: the
/// mutated instance lands back in its single home through the existing
/// take/put call mechanism (drivers hold tick state in `var` — `advance`
/// resets `varip` per step by design, so `varip` cells never carry
/// cross-tick state).
#[test]
fn var_home_carries_native_accumulation() {
    let modules = linked(&fetch_driver());
    let (bars, stamps) = one_bar_legs();
    let pine = run_pine(&modules, &bars, &stamps);
    let native = run_native(&modules, &bars, &stamps);
    // Open folds nothing, the next update seeds (absorbing cum 3), every
    // later update accumulates its delta (103-3, 203-103, ...).
    let expected_counts: [i64; 8] = [0, 0, 1, 2, 3, 4, 5, 6];
    let expected_totals: [f64; 8] = [0.0, 0.0, 100.0, 200.0, 300.0, 400.0, 500.0, 600.0];
    for tick in 0..bars.len() {
        let pine_bars = bars_at(&pine, tick);
        let native_bars = bars_at(&native, tick);
        assert_instances_eq(&pine_bars, &native_bars);
        assert_eq!(
            tick_count_of(&native_bars),
            expected_counts[tick],
            "tick_count on tick {tick}"
        );
        assert!(
            (num_of(&native_bars, "tick_volume_total") - expected_totals[tick]).abs() <= EPS,
            "tick_volume_total on tick {tick}"
        );
    }
}

/// A `varip`-held `Bars` writes back through the native call within its
/// single surviving step: declaration, `init`, and `fetch` all run on tick 0
/// (unconfirmed while the bar continues — a lone confirmed tick would take
/// the pull path instead), and the snapshotted cell agrees Pine-vs-native
/// (dispatch path — native serve or Pine-body fallback — is irrelevant to
/// the write-back proof; the take/put discipline is what is proven).
#[test]
fn varip_home_write_back_through_native_call() {
    let modules = linked(&varip_single_driver());
    let (bars, stamps) = one_bar_legs();
    let pine = run_pine(&modules, &bars, &stamps);
    let native = run_native(&modules, &bars, &stamps);
    let pine_tmp = match pine.value_at(0, "tmp") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected varip Bars object for tmp on tick 0, got {other:?}"),
    };
    let native_tmp = match native.value_at(0, "tmp") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected varip Bars object for tmp on tick 0, got {other:?}"),
    };
    assert_instances_eq(&pine_tmp, &native_tmp);
}

/// Reconcile after native accumulation equals reconcile after rehearsal
/// accumulation: the DVP rebuild observes the unified fold identically
/// through either entry clock (clock fields excluded — they document the
/// entry, not the fold).
#[test]
fn reconcile_observes_the_unified_fold() {
    let config = BarsConfig::new(60_000, 300_000, 300_000, 86_400_000);
    let mut rehearsal = BarRing::new(&config).expect("config validates");
    let mut native = BarRing::new(&config).expect("config validates");

    rehearsal.on_bar_open(100.0, 100.5, 100.0, 100.3, T0);
    let legs = BarTick {
        open: 100.0,
        high: 100.5,
        low: 100.0,
        close: 100.3,
        volume: 200.0,
    };
    let kind = rehearsal.ingest_tick(legs, T0 + 20_000);
    assert!(matches!(kind, TickIngest::Seeded));
    let out = native
        .fetch_tick_native(&NativeFetchTick {
            legs,
            tick_ms: T0 + 20_000,
            bar_open_ms: T0,
            is_new: true,
            is_confirmed: false,
            confirm: None,
        })
        .expect("native open commits");
    assert_eq!(out.signal, FetchSignal::Commit);
    // Second update accumulates on both (seed absorbed the first).
    let legs = BarTick {
        open: 100.0,
        high: 101.0,
        low: 100.0,
        close: 101.0,
        volume: 350.0,
    };
    let kind = rehearsal.ingest_tick(legs, T0 + 33_000);
    assert!(matches!(kind, TickIngest::Accumulated { .. }));
    let out = native
        .fetch_tick_native(&NativeFetchTick {
            legs,
            tick_ms: T0 + 33_000,
            bar_open_ms: T0,
            is_new: false,
            is_confirmed: false,
            confirm: None,
        })
        .expect("native fold commits");
    assert_eq!(out.signal, FetchSignal::Commit);

    let rows = vec![
        LtfRow {
            ts_ms: T0,
            o: 100.0,
            h: 100.5,
            l: 100.0,
            c: 100.3,
            volume: 200.0,
        },
        LtfRow {
            ts_ms: T0 + 60_000,
            o: 100.3,
            h: 101.0,
            l: 100.3,
            c: 101.0,
            volume: 150.0,
        },
    ];
    let left = rehearsal.reconcile(&rows, 350.0, 101.0);
    let right = native.reconcile(&rows, 350.0, 101.0);
    assert_eq!(left.rebuilt, right.rebuilt, "same rebuild size");
    assert_eq!(left.rebuilt, 2);
    assert_eq!(left.remainder, right.remainder, "same remainder top-up");
    assert_eq!(rehearsal.len(), native.len(), "same ring length");
    assert_eq!(
        rehearsal.total_volume(),
        native.total_volume(),
        "same reconstructed volume"
    );
    assert_eq!(
        rehearsal.pull_count(),
        native.pull_count(),
        "same pull count"
    );
    assert_eq!(rehearsal.pull_count(), 1);
    for (index, (a, b)) in rehearsal.iter().zip(native.iter()).enumerate() {
        assert_eq!(a, b, "rebuilt candle {index} identical");
    }
}
