//! Ring-lease contract: the engine may serve `fetch` from a cached resident
//! ring (zero hydrate) while a Pine-side write to the same object slot
//! invalidates the lease, never serves stale state. Four proofs:
//!
//! - Pine-side `init` rewrites invalidate the lease: a driver re-running
//!   `init` on every bar open agrees Pine-vs-native on every tick over a
//!   two-bar tape (a stale lease would continue the old accumulation and
//!   diverge — same-args `init` still resets trackers, so it catches a
//!   missing bump).
//! - Snapshots agree under the lease: every captured variable on every bar
//!   matches Pine-vs-native (the lease never starves snapshot reads).
//! - The confirmed close agrees through the lease, including cursor
//!   accounting.
//! - Capacity overflow declines through the lease seam: a full ring that
//!   would append past `max_candles` declines (Pine body runs next) with
//!   fields and cursor untouched — pinning the behavior M12 must prove.
//!
//! Float compares use EPS-bounded diffs; integer quantities compare
//! exactly (`#[allow(clippy::float_cmp)]` covers the count literals).
#![allow(clippy::float_cmp)]

use std::collections::HashMap;

use chrono::TimeZone as _;

use crate::indicator::BarTick;
use crate::pine::test_fixture as fixture;
use crate::{
    BarRing, BarsConfig, FetchSignal, InputSet, LinkedModule, LtfRow, NativeOptions, PineEngine,
    PineRuntimeConfig, PineValue, RingCursor, RunOutcome, UserInstance,
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

fn reinit_driver() -> String {
    // Re-runs `init` on every bar open (including the first, where both
    // branches fire): each rewrite is a Pine-side write that must
    // invalidate any cached ring for `bars`.
    "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libBars/12 as BARS\nvar bars = BARS.Bars.new()\nif barstate.isfirst\n    bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\nif barstate.isnew\n    bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\nbars.fetch()\n"
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

/// Two-bar tape: eight quiet ticks on bar one, four on bar two. Legs track
/// bar-so-far extremes per bar; stamps stay inside each bar-open time so
/// `isnew` fires only on bar opens.
fn two_bar_legs() -> (Vec<Bar>, Vec<i64>) {
    let open1 = 100.00;
    let prices1 = [100.00, 100.05, 100.03, 100.10, 100.08, 100.12, 100.09, 100.15];
    let vols1 = [1u64, 2, 100, 100, 100, 100, 100, 100];
    let dts1 = [5_000i64, 12_000, 20_000, 33_000, 40_000, 47_000, 52_000, 58_000];
    let open2 = 100.20;
    let prices2 = [100.20, 100.22, 100.18, 100.25];
    // Seed absorbs the first two prints (cum 10); the rest must stay within
    // the drift gate at confirm (250/260 ≈ 3.8% < 4%) so the close takes the
    // remainder path instead of the pull refusal.
    let vols2 = [5u64, 5, 100, 150];
    let dts2 = [5_000i64, 12_000, 20_000, 33_000];
    let bar2 = T0 + 60_000;
    let mut bars = Vec::new();
    let mut stamps = Vec::new();
    let mut push_bar = |open: f64, prices: &[f64], vols: &[u64], dts: &[i64], bar_open: i64| {
        let mut high = f64::NEG_INFINITY;
        let mut low = f64::INFINITY;
        let mut cum = 0u64;
        for (index, ((price, volume), dt)) in
            prices.iter().zip(vols.iter()).zip(dts.iter()).enumerate()
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
                &fmt2(open),
                &fmt2(high),
                &fmt2(low),
                &fmt2(*price),
                cum,
                bar_open,
            ));
            stamps.push(bar_open + dt);
        }
    };
    push_bar(open1, &prices1, &vols1, &dts1, T0);
    push_bar(open2, &prices2, &vols2, &dts2, bar2);
    (bars, stamps)
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
        (Some(PineValue::ColorHandle(a)), Some(PineValue::ColorHandle(b))) => {
            assert_eq!(a, b, "color field {field}");
        }
        (Some(PineValue::Drawing(a)), Some(PineValue::Drawing(b))) => {
            assert_eq!(a, b, "drawing field {field}");
        }
        (Some(PineValue::Matrix(a)), Some(PineValue::Matrix(b))) => {
            assert_eq!(a.rows, b.rows, "matrix rows {field}");
            assert_eq!(a.cols, b.cols, "matrix cols {field}");
            assert_eq!(a.cells.len(), b.cells.len(), "matrix cells {field}");
            for (index, (x, y)) in a.cells.iter().zip(b.cells.iter()).enumerate() {
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

/// Pine-side `init` rewrites invalidate the lease: the re-init driver
/// agrees Pine-vs-native on every tick of both bars, including the second
/// bar open (fresh `init` after a full bar of cached serves) and the
/// confirmed close.
#[test]
fn pine_init_rewrites_invalidate_lease() {
    let modules = linked(&reinit_driver());
    let (bars, stamps) = two_bar_legs();
    let pine = run_pine(&modules, &bars, &stamps);
    let native = run_native(&modules, &bars, &stamps);
    assert_eq!(pine.evaluated, native.evaluated, "same tick count");
    for tick in 0..bars.len() {
        assert_instances_eq(&bars_at(&pine, tick), &bars_at(&native, tick));
    }
}

/// Snapshots agree under the lease: every captured variable on every tick
/// matches Pine-vs-native, so cached serves never starve snapshot reads.
#[test]
fn snapshots_agree_under_lease() {
    let modules = linked(&fetch_driver());
    let (bars, stamps) = two_bar_legs();
    let pine = run_pine(&modules, &bars, &stamps);
    let native = run_native(&modules, &bars, &stamps);
    assert_eq!(
        pine.snapshots.len(),
        native.snapshots.len(),
        "same snapshot count"
    );
    for (tick, (pine_snap, native_snap)) in pine
        .snapshots
        .iter()
        .zip(native.snapshots.iter())
        .enumerate()
    {
        assert_eq!(pine_snap.bar, native_snap.bar, "snapshot bar {tick}");
        assert_eq!(
            pine_snap.values.len(),
            native_snap.values.len(),
            "snapshot width on tick {tick}"
        );
        for (name, pine_value) in &pine_snap.values {
            let native_value = native_snap
                .values
                .get(name)
                .unwrap_or_else(|| panic!("native drops snapshot {name} on tick {tick}"));
            match (pine_value.as_ref(), native_value.as_ref()) {
                (Some(PineValue::Object(pine_obj)), Some(PineValue::Object(native_obj))) => {
                    assert_instances_eq(pine_obj, native_obj);
                }
                (pine, native) => {
                    assert_value_eq(pine, native, &format!("{name}@{tick}"));
                }
            }
        }
    }
}

/// Capacity overflow declines through the lease seam: a full ring that
/// would append past `max_candles` reports `FallbackCapacity`, and the seam
/// maps it to a decline with fields and cursor untouched (the Pine body
/// runs next — the overflow sizing proof itself belongs to M12).
#[test]
fn overflow_declines_with_fields_untouched() {
    // Tiny chart-over-LTF ratio plus padding: capacity 6.
    let config = BarsConfig::new(60_000, 300_000, 60_000, 86_400_000);
    assert_eq!(config.max_candles(), 6);
    let rows: Vec<LtfRow> = (0..6)
        .map(|bucket| LtfRow {
            ts_ms: T0 - 6 * 60_000 + bucket * 60_000,
            o: 99.0 + bucket as f64,
            h: 100.0 + bucket as f64,
            l: 99.0 + bucket as f64,
            c: 99.5 + bucket as f64,
            volume: 50.0,
        })
        .collect();
    // Fill the ring to capacity without touching the accumulator clock.
    let full_ring = || {
        let mut ring = BarRing::new(&config).expect("config validates");
        ring.on_bar_open(100.0, 100.5, 100.0, 100.3, T0);
        let legs = BarTick {
            open: 100.0,
            high: 100.5,
            low: 100.0,
            close: 100.3,
            volume: 200.0,
        };
        ring.ingest_tick(legs, T0 + 20_000);
        ring.ingest_tick(
            BarTick {
                volume: 350.0,
                ..legs
            },
            T0 + 33_000,
        );
        let outcome = ring.reconcile(&rows, 300.0, 99.5);
        assert_eq!(outcome.rebuilt, 6, "ring filled to capacity");
        assert_eq!(ring.len(), 6);
        ring
    };

    // The confirmed close on a full ring reports `FallbackCapacity`: the
    // rebuild evicts instead of growing, so the confirm branch refuses to
    // miscount the cursor. The confirm reference matches the post-finalize
    // total (zero drift) so the drift gate stays out of this pin.
    let tick = crate::NativeFetchTick {
        legs: BarTick {
            open: 100.0,
            high: 101.0,
            low: 100.0,
            close: 101.0,
            volume: 400.0,
        },
        tick_ms: T0 + 120_000,
        bar_open_ms: T0,
        is_new: false,
        is_confirmed: true,
        confirm: Some(crate::ConfirmedBar {
            bar_open_ms: T0,
            o: 100.0,
            h: 101.0,
            l: 100.0,
            c: 101.0,
            volume: 450.0,
        }),
    };
    let mut ring = full_ring();
    let out = ring
        .fetch_tick_native(&tick)
        .expect("overflow reports, never refuses");
    assert_eq!(out.signal, FetchSignal::FallbackCapacity);

    // The seam maps the signal to a decline with table and cursor
    // untouched (fresh ring: single deterministic pass).
    let mut ring = full_ring();
    let mut fields = HashMap::new();
    fields.insert("max_candles".to_owned(), Some(PineValue::Int(6)));
    fields.insert("head".to_owned(), Some(PineValue::Int(0)));
    fields.insert("count".to_owned(), Some(PineValue::Int(6)));
    ring.write_pine_fields(&mut fields, 0, 6, 0)
        .expect("filled ring snapshots onto the template");
    let before = fields.clone();
    let cursor = RingCursor { head: 0, count: 6 };
    let (outcome, next) = ring
        .serve_fetch_tick(&mut fields, cursor, "Bars", &tick)
        .expect("seam runs");
    assert!(
        matches!(outcome, crate::NativeCall::Declined),
        "overflow declines, got {outcome:?}"
    );
    assert_eq!(next, cursor, "cursor untouched on decline");
    assert_eq!(fields, before, "fields untouched on decline");
}

// Commit-vs-fallback telemetry is observed, never asserted: the counters
// print for the acceptance record (steady-state lease behavior), and no
// assertion pins their values — pinning them would turn a telemetry change
// into a build break, the opposite of the tick-baseline discipline.
#[test]
fn fetch_counters_observed_not_asserted() {
    let modules = linked(&fetch_driver());
    let (bars, stamps) = two_bar_legs();
    let mut engine = test_engine();
    engine
        .set_native_options(native_on())
        .expect("native opt-in applies");
    engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("native run evaluates");
    let counters = engine.fetch_counters();
    println!(
        "fetch counters over the two-bar tape: commits={} fallbacks={}",
        counters.commits(),
        counters.fallbacks()
    );
}
