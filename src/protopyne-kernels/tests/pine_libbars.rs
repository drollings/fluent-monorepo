//! Real tick-engine library goldens: the shipped `libBars` source linked
//! with small driver scripts, pinning each export against hand-computed
//! expectations.
//!
//! Drivers call through the `BARS` alias exactly like the real dependents
//! do, so cross-module dispatch, ring-buffer `array.*` writes, and the
//! `fetch`/`onConfirmLTF`/`onConfirm` tick path run the same code the
//! indicator uses. Two harnesses cover the two time domains:
//!
//! - Closed-bar runs (seconds `time` leg): unit-style goldens for the pure
//!   helpers, the ring buffer, `_processLTFArrays`, `get`/`isSpike`/
//!   `getVolumeImbalance`/`getAlpha`, `clear`, `onConfirm` idempotency, and
//!   the `request.security_lower_tf` injection contract. Drivers observe
//!   state through methods and snapshots: instance fields are not
//!   driver-addressable, so counters (`pull_count`, `gap_count`, ...) are
//!   read from the snapshotted `Bars` object.
//! - Tick runs (`run_linked_ticks`, millisecond `time` leg, `timenow` per
//!   tick, realtime cursor flags): the `fetch` accumulation path over
//!   synthetic tick streams, with per-tick legs tracking bar-so-far OHLC,
//!   cumulative volume, and the constant bar-open time.
//!
//! `libBuffers` boundary: `libBars` imports `libUtil` only — no
//! `libBuffers` item is linked, called, or snapshotted anywhere in this
//! suite, and no Rust mirror of `BufferFloat`/`BufferInt` internals exists
//! (per the task's explicit out-of-scope rule). Where `libBars` methods
//! take shape from buffers they own their own arrays outright; the suites
//! observe those arrays through `Bars` methods and snapshots only.
//!
//! Float goldens below are hand-computed decimal expectations with a tight
//! tolerance, never measured quantities.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::{parse_program, InputSet, LtfDataset, ModuleInput, PineRuntimeConfig, UserInstance};
use chrono::TimeZone as _;
use protopyne_types::Bar;
use std::collections::HashMap;
use std::path::PathBuf;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;

/// Minute-aligned base for tick fixtures: `T0 / 60000 = 28333333` exactly,
/// so LTF bucket indices are hand-checkable integers.
const T0: i64 = 1_699_999_980_000;

const IMPORTS: &str =
    "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libBars/12 as BARS\n";

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

use crate::pine::test_fixture as fixture;

/// One shipped library as a graph input (shared fixture constructor).
fn shipped_module(file: &str, publisher: &str, name: &str, version: u64) -> ModuleInput {
    fixture::shipped_module(file, publisher, name, version)
}

/// One synthetic driver script as a graph input (shared fixture constructor).
fn driver_module(text: &str) -> ModuleInput {
    fixture::driver_module(text)
}

/// Link both shipped libraries under graph order with one driver root
/// calling through the `UTIL` and `BARS` aliases.
fn linked(driver: &str) -> Vec<LinkedModule> {
    fixture::linked_modules(vec![
        shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        shipped_module("libraries/_libBars.pine", "showmethegrail", "libBars", 12),
        driver_module(driver),
    ])
}

fn test_engine() -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    )
}

/// One bar from decimal legs and a millisecond timestamp (exact below 2^53).
fn ms_bar(open: &str, high: &str, low: &str, close: &str, volume: u64, ts_millis: i64) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .timestamp_opt(
                ts_millis.div_euclid(1000),
                ts_millis.rem_euclid(1000) as u32 * 1_000_000,
            )
            .single()
            .expect("valid test timestamp"))
        .o(open.parse().expect("decimal"))
        .h(high.parse().expect("decimal"))
        .l(low.parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

fn run_linked(driver: &str, bars: &[Bar]) -> RunOutcome {
    let modules = linked(driver);
    let mut engine = test_engine();
    engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("linked run evaluates")
}

/// One tick update: legs track bar-so-far OHLC plus cumulative volume over
/// the constant bar-open time; the tick stamp rides the side channel.
fn tick_update(
    bar_open_ms: i64,
    tick_ms: i64,
    open: f64,
    high: f64,
    low: f64,
    price: f64,
    cumvol: u64,
) -> (Bar, i64) {
    let fmt = |v: f64| format!("{v:.2}");
    (
        ms_bar(
            &fmt(open),
            &fmt(high),
            &fmt(low),
            &fmt(price),
            cumvol,
            bar_open_ms,
        ),
        tick_ms,
    )
}

/// Standard driver prologue: one held instance, initialized once on the
/// first update with M1 LTF/fetch under the 5M chart and D1 validation.
const BARS_PROLOGUE: &str = "var bars = BARS.Bars.new()\nif barstate.isfirst\n    bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\n";

/// The snapshotted `Bars` instance held by the driver.
fn bars_at(outcome: &RunOutcome, bar: usize) -> UserInstance {
    match outcome.value_at(bar, "bars") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected Bars object for bars on bar {bar}, got {other:?}"),
    }
}

fn field_num(instance: &UserInstance, name: &str) -> f64 {
    match instance.fields.get(name) {
        Some(Some(PineValue::Float(value))) => *value,
        Some(Some(PineValue::Int(value))) => *value as f64,
        other => panic!("expected number field {name}, got {other:?}"),
    }
}

/// One imbalance window: candle (open, high, low, close, volume),
/// expected imbalance, label.
type ImbalanceWindow = ((f64, f64, f64, f64, f64), f64, &'static str);

fn field_bool(instance: &UserInstance, name: &str) -> bool {
    match instance.fields.get(name) {
        Some(Some(PineValue::Bool(value))) => *value,
        other => panic!("expected bool field {name}, got {other:?}"),
    }
}

fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> f64 {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Float(value))) => value,
        Some(Some(PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn boolean(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Bool(value))) => value,
        other => panic!("expected bool for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn is_absent(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    matches!(outcome.value_at(at_bar, name), Some(None))
}

#[test]
fn helper_edge_table() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}\
            drift = BARS.calculateDriftPct(110.0, 100.0)\n\
            drift_full = BARS.calculateDriftPct(0.0, 100.0)\n\
            drift_zero_ref = BARS.calculateDriftPct(100.0, 0.0)\n\
            drift_neg_ref = BARS.calculateDriftPct(100.0, -5.0)\n\
            drift_na = BARS.calculateDriftPct(na, 100.0)\n\
            buy_up = BARS.estimateBuyVolume(100.0, 5.0)\n\
            buy_down = BARS.estimateBuyVolume(100.0, -5.0)\n\
            buy_flat = BARS.estimateBuyVolume(100.0, 0.0)\n\
            buy_na = BARS.estimateBuyVolume(100.0, na)\n\
            valid = BARS.isValidVolume(0.5)\n\
            valid_eps = BARS.isValidVolume(0.0001)\n\
            valid_above = BARS.isValidVolume(0.00011)\n\
            valid_zero = BARS.isValidVolume(0.0)\n\
            valid_neg = BARS.isValidVolume(-1.0)\n\
            valid_na = BARS.isValidVolume(na)\n\
            idx = BARS.getCandleIndex(1700000050000, 60000)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 0, "drift"), 10.0, "10% drift");
    assert_eq!(
        num(&outcome, 0, "drift_full"),
        100.0,
        "zero actual is full drift"
    );
    assert_eq!(
        num(&outcome, 0, "drift_zero_ref"),
        0.0,
        "zero reference guards to zero"
    );
    assert_eq!(
        num(&outcome, 0, "drift_neg_ref"),
        0.0,
        "negative reference guards to zero"
    );
    assert!(
        is_absent(&outcome, 0, "drift_na"),
        "absent actual propagates"
    );
    assert_eq!(num(&outcome, 0, "buy_up"), 100.0, "up move takes all");
    assert_eq!(num(&outcome, 0, "buy_down"), 0.0, "down move takes none");
    assert_eq!(
        num(&outcome, 0, "buy_flat"),
        50.0,
        "flat splits at the neutral ratio"
    );
    assert_eq!(
        num(&outcome, 0, "buy_na"),
        50.0,
        "absent delta reads false on both guards, so the neutral split applies (comparisons are two-valued)"
    );
    assert!(boolean(&outcome, 0, "valid"));
    assert!(
        !boolean(&outcome, 0, "valid_eps"),
        "epsilon itself is dust (strict)"
    );
    assert!(boolean(&outcome, 0, "valid_above"));
    assert!(!boolean(&outcome, 0, "valid_zero"));
    assert!(!boolean(&outcome, 0, "valid_neg"));
    assert!(!boolean(&outcome, 0, "valid_na"));
    // 1700000050000 / 60000 = 28333334.16... floors to 28333334.
    assert_eq!(num(&outcome, 0, "idx"), 28_333_334.0, "candle index floors");
}

#[test]
fn ltf_boundary_math() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}\
            edge = bars.getLTFBoundary(1700000050000)\n\
            aligned = bars.getLTFBoundary(1700000040000)"
        ),
        &bars,
    );
    // floor(1700000050000 / 60000) = 28333334; 28333334 * 60000 = 1700000040000.
    assert_eq!(
        num(&outcome, 0, "edge"),
        1_700_000_040_000.0,
        "boundary floors to the bucket open"
    );
    assert_eq!(
        num(&outcome, 0, "aligned"),
        1_700_000_040_000.0,
        "aligned stamp is its own boundary"
    );
}

#[test]
fn array_set_golden() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        "a = array.new<float>(3, 0.0)\n\
        array.set(a, 1, 5.0)\n\
        hit = array.get(a, 1)\n\
        miss_low = array.get(a, 0)\n\
        na_set = array.set(na, 0, 1.0)\n\
        na_idx = array.set(a, na, 1.0)",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "hit"), 5.0, "set slot reads back");
    assert_eq!(
        num(&outcome, 0, "miss_low"),
        0.0,
        "untouched slot keeps its fill"
    );
    assert!(
        is_absent(&outcome, 0, "na_set"),
        "absent array stays absent"
    );
    assert!(
        is_absent(&outcome, 0, "na_idx"),
        "absent index stays absent"
    );

    for (source, refusal) in [
        (
            "a = array.new<float>(3, 0.0)\narray.set(a, 3, 1.0)",
            "array.set index 3 is past size 3",
        ),
        (
            "a = array.new<float>(3, 0.0)\narray.set(a, -1, 1.0)",
            "array.set takes an index in 0..size, got -1",
        ),
    ] {
        let err = PineEngine::new(PineRuntimeConfig::new().build())
            .run(
                &parse_program(source).expect("probes parse"),
                &bars,
                InputSet::new(),
            )
            .expect_err("out-of-range set refuses");
        assert_eq!(
            err.to_string(),
            format!("pine evaluation refused: {refusal}"),
            "exact refusal pins the fail-closed bound"
        );
    }
}

#[test]
fn break_golden() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        "x = 0\n\
        for i = 0 to 10\n    \
        if i > 3\n        \
        break\n    \
        x := i\n\
        y = 0\n\
        j = 0\n\
        while true\n    \
        j := j + 1\n    \
        if j >= 3\n        \
        break\n    \
        y := j\n\
        outer = 0\n\
        for a = 0 to 2\n    \
        for b = 0 to 5\n        \
        if b > 1\n            \
        break\n        \
        outer := outer + 1",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "x"), 3.0, "for stops at the break");
    assert_eq!(
        num(&outcome, 0, "j"),
        3.0,
        "while condition ran three guards"
    );
    assert_eq!(
        num(&outcome, 0, "y"),
        2.0,
        "while body stopped before the third yield"
    );
    assert_eq!(
        num(&outcome, 0, "outer"),
        6.0,
        "inner breaks do not unwind the outer loop (3 x 2)"
    );

    // A loop-less `break` refuses instead of unwinding the run.
    let err = PineEngine::new(PineRuntimeConfig::new().build())
        .run(
            &parse_program("break").expect("probe parses"),
            &bars,
            InputSet::new(),
        )
        .expect_err("top-level break refuses");
    assert_eq!(
        err.to_string(),
        "pine evaluation refused: 'break' outside a 'for'/'while' body refuses instead of unwinding",
        "exact refusal pins the lexical bound"
    );

    // A `break` inside a called body still refuses: calls reset the depth.
    let err = PineEngine::new(PineRuntimeConfig::new().build())
        .run(
            &parse_program("f() =>\n    break\n    1\nx = 0\nfor i = 0 to 3\n    x := f()")
                .expect("probe parses"),
            &bars,
            InputSet::new(),
        )
        .expect_err("call-boundary break refuses");
    assert_eq!(
        err.to_string(),
        "pine evaluation refused: 'break' outside a 'for'/'while' body refuses instead of unwinding",
        "calls bind break lexically, never dynamically"
    );

    // Display round-trips the new statement: parse, display, reparse equal.
    let first = parse_program("for i = 0 to 5\n    if i > 1\n        break\n    x := i")
        .expect("break snippet parses");
    let shown = crate::display_program(&first);
    assert!(
        shown.contains("break"),
        "displayed source keeps the statement"
    );
    let second = parse_program(&shown).expect("displayed source reparses");
    assert_eq!(first, second, "break survives the display round-trip");
}

#[test]
fn init_allocates_buffers() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(&format!("{IMPORTS}{BARS_PROLOGUE}sz = bars.size()"), &bars);
    let state = bars_at(&outcome, 0);
    // CTF 5M over M1 LTF/fetch: ratios 5/1/1440, five spare slots.
    assert_eq!(field_num(&state, "ltf_ms"), 60_000.0);
    assert_eq!(field_num(&state, "fetch_ms"), 60_000.0);
    assert_eq!(field_num(&state, "ctf_ms"), 300_000.0);
    assert_eq!(field_num(&state, "ratio_ctf_ltf"), 5.0);
    assert_eq!(field_num(&state, "ratio_fetch_ltf"), 1.0);
    assert_eq!(field_num(&state, "ratio_htf_ltf"), 1440.0);
    assert_eq!(field_num(&state, "max_candles"), 10.0);
    assert_eq!(
        field_num(&state, "count"),
        0.0,
        "fresh buffers hold no candles"
    );
    assert_eq!(num(&outcome, 0, "sz"), 0.0);
    assert_eq!(field_num(&state, "spike_threshold"), 2.0);
    assert_eq!(field_num(&state, "drift_threshold"), 4.0);
    assert!(
        !field_bool(&state, "fill_gaps"),
        "sparse representation is the default"
    );
    assert_eq!(field_num(&state, "pull_count"), 0.0);
    assert_eq!(field_num(&state, "fetch_check_count"), 0.0);
}

#[test]
fn clear_resets_bar_state() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(&format!("{IMPORTS}{BARS_PROLOGUE}bars.clear()"), &bars);
    let state = bars_at(&outcome, 0);
    assert_eq!(field_num(&state, "count"), 0.0);
    assert_eq!(field_num(&state, "total_vol"), 0.0);
    assert_eq!(field_num(&state, "total_buy"), 0.0);
    assert_eq!(field_num(&state, "boundary_count"), 0.0);
    assert_eq!(field_num(&state, "gap_count"), 0.0);
    assert_eq!(
        field_num(&state, "candle_open"),
        100.0,
        "open leg seeds the next candle chain"
    );
    assert!(
        !field_bool(&state, "ready_ltf_boundary"),
        "flags rest pulsed-off"
    );
}

#[test]
fn ring_wrap_preserves_newest() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}\
            for i = 1 to 12\n    \
            bars._writeCandle(110.0 + i, 100.0 + i, 105.0 + i, 10.0, 5.0, i)\n\
            [o0, h0, l0, c0, b0, s0] = bars.get(0)\n\
            [o9, h9, l9, c9, b9, s9] = bars.get(9)\n\
            phys0 = bars._getPhysicalIndex(0)\n\
            last = bars._getLastIndex()"
        ),
        &bars,
    );
    let state = bars_at(&outcome, 0);
    // Twelve writes into ten slots: the count saturates, the head wraps to
    // slot 2, and reads serve the newest ten in logical order.
    assert_eq!(field_num(&state, "count"), 10.0);
    assert_eq!(
        num(&outcome, 0, "phys0"),
        2.0,
        "logical zero sits at the wrapped head"
    );
    assert_eq!(
        num(&outcome, 0, "last"),
        1.0,
        "last written slot trails the head"
    );
    // Third write (i=3) is the oldest held: h=113, l=103, c=108.
    assert_eq!(num(&outcome, 0, "h0"), 113.0);
    assert_eq!(num(&outcome, 0, "l0"), 103.0);
    assert_eq!(num(&outcome, 0, "c0"), 108.0);
    assert_eq!(num(&outcome, 0, "b0"), 5.0);
    assert_eq!(num(&outcome, 0, "s0"), 5.0, "sell is total minus buy");
    // Twelfth write (i=12) is the newest: h=122, l=112, c=117.
    assert_eq!(num(&outcome, 0, "h9"), 122.0);
    assert_eq!(num(&outcome, 0, "l9"), 112.0);
    assert_eq!(num(&outcome, 0, "c9"), 117.0);
    // Running totals accumulate every write, including evicted candles.
    assert_eq!(
        field_num(&state, "total_vol"),
        120.0,
        "evicted candles keep their totals"
    );
    assert_eq!(field_num(&state, "total_buy"), 60.0);
}

#[test]
fn get_tuple_shape() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}\
            times = array.from(1000, 2000)\n\
            opens = array.from(100.0, 101.0)\n\
            highs = array.from(101.0, 102.0)\n\
            lows = array.from(99.0, 100.0)\n\
            closes = array.from(100.5, 101.5)\n\
            vols = array.from(60.0, 40.0)\n\
            bars._processLTFArrays(times, opens, highs, lows, closes, vols)\n\
            [o0, h0, l0, c0, b0, s0] = bars.get(0)\n\
            [o1, h1, l1, c1, b1, s1] = bars.get(1)\n\
            [x0, x1, x2, x3, x4, x5] = bars.get(7)"
        ),
        &bars,
    );
    let state = bars_at(&outcome, 0);
    assert_eq!(field_num(&state, "count"), 2.0);
    assert_eq!(field_num(&state, "total_vol"), 100.0);
    // First open chains from the stored candle open (first LTF open).
    assert_eq!(num(&outcome, 0, "o0"), 100.0);
    assert_eq!(num(&outcome, 0, "h0"), 101.0);
    assert_eq!(num(&outcome, 0, "c0"), 100.5);
    // Later opens chain from the previous close.
    assert_eq!(num(&outcome, 0, "o1"), 100.5);
    assert_eq!(num(&outcome, 0, "c1"), 101.5);
    assert_eq!(
        num(&outcome, 0, "b0") + num(&outcome, 0, "s0"),
        60.0,
        "buy and sell split the candle"
    );
    assert_eq!(num(&outcome, 0, "b1") + num(&outcome, 0, "s1"), 40.0);
    assert!(
        is_absent(&outcome, 0, "x0"),
        "out-of-range reads are absent, never invented"
    );
}

#[test]
fn process_arrays_truncates_and_ignores_empty() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}\
            times = array.from(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12)\n\
            opens = array.from(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0)\n\
            highs = array.from(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0)\n\
            lows = array.from(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0)\n\
            closes = array.from(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0)\n\
            vols = array.from(10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0)\n\
            bars._processLTFArrays(times, opens, highs, lows, closes, vols)\n\
            [_, h9, _, _, _, _] = bars.get(9)"
        ),
        &bars,
    );
    let state = bars_at(&outcome, 0);
    // Twelve rows into ten slots: the buffer-safety break keeps the first
    // ten instead of aborting the run.
    assert_eq!(field_num(&state, "count"), 10.0);
    assert_eq!(num(&outcome, 0, "h9"), 10.0, "tenth row is the newest held");
    assert_eq!(
        field_num(&state, "candle_open"),
        1.0,
        "first LTF open chains the reads"
    );

    // Empty and absent arrays are no-ops, never refusals.
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}\
            bars._processLTFArrays(array.new<int>(0), array.new<float>(0), array.new<float>(0), array.new<float>(0), array.new<float>(0), array.new<float>(0))\n\
            bars._processLTFArrays(na, na, na, na, na, na)"
        ),
        &bars,
    );
    assert_eq!(field_num(&bars_at(&outcome, 0), "count"), 0.0);
}

#[test]
fn dust_volumes_create_no_candles() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}\
            times = array.from(1000, 2000)\n\
            opens = array.from(100.0, 100.0)\n\
            highs = array.from(100.0, 100.0)\n\
            lows = array.from(100.0, 100.0)\n\
            closes = array.from(100.0, 100.0)\n\
            vols = array.from(0.00001, 0.00002)\n\
            bars._processLTFArrays(times, opens, highs, lows, closes, vols)"
        ),
        &bars,
    );
    assert_eq!(
        field_num(&bars_at(&outcome, 0), "count"),
        0.0,
        "epsilon dust never becomes a candle"
    );
}

/// Tick driver: the standard prologue plus `fetch` every update.
fn fetch_driver() -> String {
    format!("{IMPORTS}{BARS_PROLOGUE}bars.fetch()\n")
}

/// The ten-tick clean fixture: two CTF minutes are out of scope (one 5M
/// bar), LTF M1 buckets 0/1/3/4 carry ticks, bucket 2 is a gap. Ticks 1-2
/// seed the tracker (their 20 units never enter a delta); every later
/// tick accumulates. Reference volume equals the tick sum, so the bar
/// reconciles through the remainder path with no pull.
fn clean_ticks() -> Vec<(Bar, i64)> {
    let ticks: Vec<(i64, f64, u64)> = vec![
        (5_000, 100.0, 10),
        (10_000, 100.2, 10),
        (20_000, 101.0, 200),
        (30_000, 102.0, 200),
        (70_000, 101.5, 150),
        (80_000, 101.0, 150),
        (190_000, 99.0, 100),
        (200_000, 99.5, 100),
        (250_000, 100.0, 50),
        (290_000, 100.5, 30),
    ];
    let mut cumvol = 0;
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    ticks
        .iter()
        .map(|(offset, price, vol)| {
            cumvol += vol;
            high = high.max(*price);
            low = low.min(*price);
            tick_update(T0, T0 + offset, 100.0, high, low, *price, cumvol)
        })
        .collect()
}

#[test]
fn tick_flags_follow_bar_time() {
    // Two ticks in bar A, one tick in bar B: `isnew` fires on first ticks,
    // `isconfirmed` on last ticks, and the run reports realtime.
    let program = parse_program(
        "n = barstate.isnew\nc = barstate.isconfirmed\nh = barstate.ishistory\nr = barstate.isrealtime\nt = timenow\nbt = time",
    )
    .expect("driver parses");
    let modules = vec![LinkedModule::new("local", program, HashMap::new())];
    let bars = vec![
        ms_bar("100", "101", "99", "100", 1000, T0),
        ms_bar("100", "101", "99", "100", 1000, T0),
        ms_bar("100", "101", "99", "100", 1000, T0 + 60_000),
    ];
    let stamps = vec![T0 + 100, T0 + 200, T0 + 60_100];
    let mut engine = test_engine();
    let outcome = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("tick run evaluates");
    let flag = |bar: usize, name: &str| match outcome.value_at(bar, name) {
        Some(Some(PineValue::Bool(value))) => value,
        other => panic!("expected bool for {name} on bar {bar}, got {other:?}"),
    };
    assert!(
        flag(0, "n") && !flag(0, "c"),
        "first tick opens unconfirmed"
    );
    assert!(!flag(1, "n") && flag(1, "c"), "middle tick closes the bar");
    assert!(
        flag(2, "n") && flag(2, "c"),
        "single-tick bar both opens and confirms"
    );
    for (bar, stamp) in stamps.iter().enumerate() {
        assert!(
            !flag(bar, "h") && flag(bar, "r"),
            "tick runs emulate realtime"
        );
        assert_eq!(
            num(&outcome, bar, "t"),
            *stamp as f64,
            "timenow reads the tick stamp"
        );
    }
    assert_eq!(
        num(&outcome, 0, "bt"),
        T0 as f64,
        "time leg runs in milliseconds here"
    );

    // Length mismatch refuses fail-closed.
    let program = parse_program("x = 1").expect("probe parses");
    let mut engine = test_engine();
    let err = engine
        .run_linked_ticks(
            &[LinkedModule::new("local", program, HashMap::new())],
            &bars,
            &[1],
            InputSet::new(),
        )
        .expect_err("tick/bar mismatch refuses");
    assert_eq!(
        err.to_string(),
        "pine evaluation refused: tick run needs one timestamp per bar, got 1 stamps for 3 bars"
    );
}

#[test]
fn tick_reconstruction_clean() {
    let mut updates = clean_ticks();
    // Reference legs match the tick tape exactly: the bar reconciles.
    if let Some((last, _)) = updates.last_mut() {
        *last = ms_bar("100", "102", "99", "100.5", 1000, T0);
    }
    let bars: Vec<Bar> = updates.iter().map(|(bar, _)| bar.clone()).collect();
    let stamps: Vec<i64> = updates.iter().map(|(_, stamp)| *stamp).collect();
    let modules = linked(&format!(
        "{}\
        [o0, h0, l0, c0, b0, s0] = bars.get(0)\n\
        imb = bars.getVolumeImbalance()",
        fetch_driver()
    ));
    let mut engine = test_engine();
    let outcome = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("clean tick run evaluates");
    let last = 9;
    let state = bars_at(&outcome, last);
    // Four distributed candles plus the remainder top-up.
    assert_eq!(field_num(&state, "count"), 5.0);
    assert_eq!(
        field_num(&state, "total_vol"),
        1000.0,
        "remainder tops up to the reference"
    );
    // Remainder buy hand roll: range 3, body 0.5, direction +1 gives
    // 0.5 + 0.5/3/2 = 0.5833...; 20 * 0.5833... = 35/3.
    assert_eq!(field_num(&state, "total_buy"), 580.0 + 35.0 / 3.0);
    assert_eq!(
        field_num(&state, "gap_count"),
        1.0,
        "bucket 2 had no trades"
    );
    assert_eq!(
        field_num(&state, "boundary_count"),
        4.0,
        "crossings at ticks 5, 7 (+2), 9"
    );
    assert_eq!(
        field_num(&state, "pull_count"),
        0.0,
        "reconciled bars trigger no pull"
    );
    assert_eq!(field_num(&state, "fetch_check_count"), 0.0);
    assert_eq!(
        field_num(&state, "tick_count"),
        8.0,
        "ticks 3-10 accumulate"
    );
    assert_eq!(
        field_num(&state, "tick_volume_total"),
        980.0,
        "first two ticks seed the tracker"
    );
    assert_eq!(field_num(&state, "tick_accuracy"), 0.98);
    assert_eq!(field_num(&state, "volume_drift_pct"), 2.0);
    assert_eq!(field_num(&state, "last_bar_drift"), 2.0);
    assert!(
        field_bool(&state, "last_bar_healthy"),
        "2% drift is under the 4% threshold"
    );
    // First distributed candle: ticks 3-5 bleed into it (102/100/101.5,
    // 550 total, 400 buy, stamped at the accumulator start).
    assert_eq!(
        num(&outcome, last, "o0"),
        100.0,
        "reads chain from the bar open"
    );
    assert_eq!(num(&outcome, last, "h0"), 102.0);
    assert_eq!(num(&outcome, last, "l0"), 100.0);
    assert_eq!(num(&outcome, last, "c0"), 101.5);
    assert_eq!(num(&outcome, last, "b0"), 400.0);
    assert_eq!(num(&outcome, last, "s0"), 150.0);
    // Imbalance hand roll: (580 + 35/3) - 500 = 275/3.
    assert!(
        (num(&outcome, last, "imb") - 275.0 / 3.0).abs() < EPS,
        "buy pressure reads positive"
    );
    // Boundary flags pulse on crossing ticks only.
    for bar in [4, 6, 8] {
        assert!(
            field_bool(&bars_at(&outcome, bar), "ready_ltf_boundary"),
            "pulse on tick {bar}"
        );
    }
    assert!(
        !field_bool(&state, "ready_ltf_boundary"),
        "quiet on the confirming tick"
    );
}

/// Injected LTF rows with hand-split buys: row 1 buys at 0.70
/// (mid-body), rows 2-5 at 0.825 (strong close), scaled by `row_vol`.
fn pull_dataset_scaled(row_vol: f64) -> LtfDataset {
    // Row times sit at or before the chart bar-open `T0`: tick emulation
    // serves the confirmed prefix only, so synthetic future rows would
    // serve empty. Values, order, and widths are untouched, so every
    // hand-split golden below still holds.
    let times = [
        T0 - 2_400_000,
        T0 - 1_800_000,
        T0 - 1_200_000,
        T0 - 600_000,
        T0,
    ];
    LtfDataset {
        times: times.iter().copied().map(EpochMs::from).collect(),
        // Each row stands for one 600 s bucket.
        closes_at: times
            .iter()
            .map(|open| Some(EpochMs::from(open + 600_000)))
            .collect(),
        opens: vec![100.0, 100.5, 101.5, 102.5, 103.5],
        highs: vec![101.0, 102.0, 103.0, 104.0, 105.0],
        lows: vec![99.0, 100.0, 101.0, 102.0, 103.0],
        closes: vec![100.5, 101.5, 102.5, 103.5, 104.5],
        volumes: vec![row_vol; 5],
    }
}

/// Five injected LTF rows summing to 2000, hand-split per candle:
/// row 1 buys at 0.70 (mid-body), rows 2-5 at 0.825 (strong close).
fn pull_dataset() -> LtfDataset {
    pull_dataset_scaled(400.0)
}

#[test]
fn tick_pull_reconciles_drifted_bar() {
    // The first two ticks carry 600 of the 1000-unit tape, but seeding
    // absorbs them before any delta accumulates: the 60% confirm drift
    // trips the pull, which the injected 1000-unit window reconciles.
    let tape = high_drift_tape();
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let updates: Vec<(Bar, i64)> = tape
        .iter()
        .map(|(offset, price, cumvol)| {
            high = high.max(*price);
            low = low.min(*price);
            tick_update(T0, T0 + offset, 100.0, high, low, *price, *cumvol)
        })
        .collect();
    let bars: Vec<Bar> = updates.iter().map(|(bar, _)| bar.clone()).collect();
    let stamps: Vec<i64> = updates.iter().map(|(_, stamp)| *stamp).collect();
    let modules = linked(&format!(
        "{}\
        [o0, h0, l0, c0, b0, s0] = bars.get(0)",
        fetch_driver()
    ));
    let mut engine = test_engine();
    engine
        .inject_lower_tf("SPY", "1", pull_dataset_scaled(200.0))
        .expect("pull dataset validates");
    let outcome = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("drifted tick run evaluates");
    let last = 9;
    let state = bars_at(&outcome, last);
    // The pull replaces the tick-built candles with the injected rows
    // (200 each): 140 + 4 x 165 hand splits.
    assert_eq!(field_num(&state, "count"), 5.0);
    assert_eq!(
        field_num(&state, "total_vol"),
        1000.0,
        "injected rows sum to the reference"
    );
    assert_eq!(
        field_num(&state, "total_buy"),
        800.0,
        "140 + 4 x 165 hand splits"
    );
    assert_eq!(field_num(&state, "pull_count"), 1.0, "realtime pull counts");
    assert_eq!(
        field_num(&state, "fetch_check_count"),
        1.0,
        "confirm-time checks count too"
    );
    assert_eq!(field_num(&state, "tick_accuracy"), 0.4);
    assert_eq!(field_num(&state, "volume_drift_pct"), 60.0);
    assert_eq!(field_num(&state, "last_bar_drift"), 60.0);
    assert!(
        !field_bool(&state, "last_bar_healthy"),
        "60% drift trips the 4% threshold"
    );
    assert_eq!(
        field_num(&state, "candle_open"),
        100.0,
        "first injected open chains reads"
    );
    assert_eq!(num(&outcome, last, "o0"), 100.0);
    assert_eq!(num(&outcome, last, "h0"), 101.0);
    assert_eq!(num(&outcome, last, "c0"), 100.5);
    assert_eq!(num(&outcome, last, "b0"), 140.0);
    assert_eq!(num(&outcome, last, "s0"), 60.0);
}

#[test]
fn tick_rollback_resets_accumulator() {
    // Third tick arrives with lower cumulative volume (out-of-order tape):
    // the bar rolls back, then keeps accumulating from the reset point.
    // Early ticks stay tiny so the seeding-plus-rollback loss (1 unit)
    // reconciles without a pull.
    let tape: Vec<(i64, f64, u64)> = vec![
        (5_000, 100.0, 1),
        (10_000, 101.0, 2),
        (20_000, 100.5, 1),
        (30_000, 100.5, 100),
    ];
    let updates: Vec<(Bar, i64)> = tape
        .iter()
        .map(|(offset, price, cumvol)| {
            tick_update(T0, T0 + offset, 100.0, 101.0, 100.0, *price, *cumvol)
        })
        .collect();
    let bars: Vec<Bar> = updates.iter().map(|(bar, _)| bar.clone()).collect();
    let stamps: Vec<i64> = updates.iter().map(|(_, stamp)| *stamp).collect();
    let modules = linked(&format!(
        "{}\
        [o0, h0, l0, c0, b0, s0] = bars.get(0)",
        fetch_driver()
    ));
    let mut engine = test_engine();
    let outcome = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("rollback run evaluates");
    let state = bars_at(&outcome, 3);
    assert_eq!(
        field_num(&state, "rollback_count"),
        1.0,
        "volume decrease detected once"
    );
    // Post-reset delta (100 - 1) is the only accumulated volume; the 1%
    // confirm drift reconciles through the remainder path with no pull.
    assert_eq!(field_num(&state, "tick_volume_total"), 99.0);
    assert_eq!(field_num(&state, "total_vol"), 100.0);
    assert_eq!(
        field_num(&state, "count"),
        2.0,
        "finalized candle plus remainder top-up"
    );
    assert_eq!(field_num(&state, "pull_count"), 0.0);
    assert_eq!(field_num(&state, "tick_accuracy"), 0.99);
    assert!(field_bool(&state, "last_bar_healthy"));
    assert_eq!(
        num(&outcome, 3, "h0"),
        101.0,
        "reset accumulator tracks bar legs"
    );
    assert_eq!(num(&outcome, 3, "c0"), 100.5);
    assert_eq!(num(&outcome, 3, "b0"), 49.5, "flat delta splits neutral");
}

#[test]
fn flat_tape_splits_neutral() {
    // Flat price: every delta splits at the neutral ratio, so the
    // imbalance reads exactly zero both mid-bar and after confirm. The
    // seeding pair stays tiny so the 4-unit loss reconciles quietly.
    let tape: Vec<(i64, f64, u64)> = vec![
        (5_000, 100.0, 2),
        (10_000, 100.0, 4),
        (20_000, 100.0, 104),
        (30_000, 100.0, 204),
        (40_000, 100.0, 304),
    ];
    let updates: Vec<(Bar, i64)> = tape
        .iter()
        .map(|(offset, price, cumvol)| {
            tick_update(T0, T0 + offset, 100.0, 100.0, 100.0, *price, *cumvol)
        })
        .collect();
    let bars: Vec<Bar> = updates.iter().map(|(bar, _)| bar.clone()).collect();
    let stamps: Vec<i64> = updates.iter().map(|(_, stamp)| *stamp).collect();
    let modules = linked(&format!(
        "{}imb = bars.getVolumeImbalance()",
        fetch_driver()
    ));
    let mut engine = test_engine();
    let outcome = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("flat run evaluates");
    // Ticks 3-5 accumulate 300 with 150 buy; the 304 reference reconciles
    // through a 4-unit doji remainder (buy 2), keeping both splits exact.
    assert_eq!(
        num(&outcome, 2, "imb"),
        0.0,
        "accumulator split is neutral mid-bar"
    );
    assert_eq!(
        num(&outcome, 4, "imb"),
        0.0,
        "ring split is neutral after confirm"
    );
    assert_eq!(field_num(&bars_at(&outcome, 4), "pull_count"), 0.0);
    assert!(field_bool(&bars_at(&outcome, 4), "last_bar_healthy"));
}

#[test]
fn spike_precision_recall() {
    // Five labeled windows against an HTF SMA of 144000 (expected 100.0
    // at the M1/D1 ratio): spike iff the ratio tops 1 + 2.0 = 3.0.
    // Window 2 sits exactly on the boundary and must NOT fire.
    let vols = [400.0, 300.0, 301.0, 100.0, 50.0];
    let labels = [true, false, true, false, false];
    let mut driver = format!("{IMPORTS}{BARS_PROLOGUE}");
    for (slot, vol) in vols.iter().enumerate() {
        let chunk = format!(
            "w{slot} = BARS.Bars.new()\n\
            w{slot}.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\n\
            w{slot}._processLTFArrays(array.from(1000), array.from(100.0), array.from(101.0), array.from(99.0), array.from(100.5), array.from({vol}))\n\
            spike{slot} = w{slot}.isSpike(144000.0)\n"
        );
        driver.push_str(&chunk);
    }
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(&driver, &bars);
    let mut true_positives = 0;
    let mut false_positives = 0;
    let mut false_negatives = 0;
    for (slot, label) in labels.iter().enumerate() {
        let fired = boolean(&outcome, 0, &format!("spike{slot}"));
        assert_eq!(
            fired, *label,
            "window {slot} (vol {}) fires {label}",
            vols[slot]
        );
        match (fired, label) {
            (true, true) => true_positives += 1,
            (true, false) => false_positives += 1,
            (false, true) => false_negatives += 1,
            (false, false) => {}
        }
    }
    let precision = f64::from(true_positives) / f64::from(true_positives + false_positives);
    let recall = f64::from(true_positives) / f64::from(true_positives + false_negatives);
    assert_eq!(precision, 1.0, "no flat window fires");
    assert_eq!(recall, 1.0, "every spike window fires");
}

#[test]
fn spike_degenerates_quiet() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}\
            empty = bars.isSpike(144000.0)\n\
            zero_sma = bars.isSpike(0.0)\n\
            neg_sma = bars.isSpike(-5.0)\n\
            times = array.from(1000)\n\
            opens = array.from(100.0)\n\
            highs = array.from(101.0)\n\
            lows = array.from(99.0)\n\
            closes = array.from(100.5)\n\
            vols = array.from(400.0)\n\
            bars._processLTFArrays(times, opens, highs, lows, closes, vols)\n\
            zero_sma_full = bars.isSpike(0.0)\n\
            na_sma = bars.isSpike(na)"
        ),
        &bars,
    );
    assert!(!boolean(&outcome, 0, "empty"), "empty buffers never spike");
    assert!(!boolean(&outcome, 0, "zero_sma"));
    assert!(!boolean(&outcome, 0, "neg_sma"));
    assert!(
        !boolean(&outcome, 0, "zero_sma_full"),
        "400x volume still quiet without a baseline"
    );
    assert!(
        !boolean(&outcome, 0, "na_sma"),
        "absent baseline reads false (comparisons are two-valued, never na)"
    );
}

#[test]
fn imbalance_sign_follows_pressure() {
    // One-candle windows: up buys 420 of 600, down buys 70 of 400, doji
    // splits exactly half.
    let windows: Vec<ImbalanceWindow> = vec![
        ((100.0, 102.0, 98.0, 101.0, 600.0), 120.0, "up"),
        ((101.0, 101.5, 99.5, 100.0, 400.0), -130.0, "down"),
        ((100.0, 101.0, 99.0, 100.0, 200.0), 0.0, "doji"),
    ];
    for (slot, ((o, h, l, c, v), expected, label)) in windows.iter().enumerate() {
        let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
        let outcome = run_linked(
            &format!(
                "{IMPORTS}{BARS_PROLOGUE}\
                bars._processLTFArrays(array.from(1000), array.from({o}), array.from({h}), array.from({l}), array.from({c}), array.from({v}))\n\
                imb = bars.getVolumeImbalance()"
            ),
            &bars,
        );
        assert_eq!(
            num(&outcome, 0, "imb"),
            *expected,
            "{label} window imbalances {expected}"
        );
        let _ = slot;
    }
}

#[test]
fn alpha_bounds_and_defaults() {
    // Ring: row A buys 420 of 600, row B buys 70 of 400.
    let setup = "times = array.from(1000, 2000)\n\
        opens = array.from(100.0, 101.0)\n\
        highs = array.from(102.0, 101.5)\n\
        lows = array.from(98.0, 99.5)\n\
        closes = array.from(101.0, 100.0)\n\
        vols = array.from(600.0, 400.0)\n\
        bars._processLTFArrays(times, opens, highs, lows, closes, vols)\n";
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}{setup}\
            live = bars.getAlpha(1000.0, 1000.0, 0.5, 1.0, 2.0, 0.5)\n\
            expected = UTIL.fast_tanh(0.16084032653061225)\n\
            zero_sma = bars.getAlpha(1000.0, 0.0, 0.5, 1.0, 2.0, 0.5)\n\
            neg_sma = bars.getAlpha(1000.0, -1.0, 0.5, 1.0, 2.0, 0.5)\n\
            na_sma = bars.getAlpha(1000.0, na, 0.5, 1.0, 2.0, 0.5)\n\
            one_sma = bars.getAlpha(1000.0, 1.0, 0.5, 1.0, 2.0, 0.5)"
        ),
        &bars,
    );
    // Hand roll: buy 0.49 / sell 0.51, bar ratio 0.2, delta corrected to
    // 0.255/0.49 by the positive ATR distance, vol_alpha 1.0002,
    // volatility 0.5, squeeze 0.25 -> x = 0.16084032653061225.
    assert!(
        (num(&outcome, 0, "live") - num(&outcome, 0, "expected")).abs() < EPS,
        "alpha composes the hand-derived fast_tanh argument"
    );
    assert!(
        (0.01..=0.90).contains(&num(&outcome, 0, "live")),
        "live alpha stays in range"
    );
    assert_eq!(
        num(&outcome, 0, "zero_sma"),
        0.08,
        "degenerate baseline takes the default"
    );
    assert_eq!(num(&outcome, 0, "neg_sma"), 0.08);
    assert_eq!(
        num(&outcome, 0, "na_sma"),
        num(&outcome, 0, "one_sma"),
        "absent baseline rides its nz default exactly"
    );

    // Empty buffers take the default without touching a kernel.
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}empty = bars.getAlpha(1000.0, 1000.0, 0.5, 1.0, 2.0, 0.5)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 0, "empty"), 0.08);
}

#[test]
fn onconfirm_is_idempotent() {
    // Two confirms on one CTF time reconcile once; the next bar's time
    // reconfirms without duplicating the remainder.
    let bars = vec![
        ms_bar("100", "101", "99", "100", 1000, T0),
        ms_bar("100", "101", "99", "100", 1000, T0 + 60_000),
    ];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}{BARS_PROLOGUE}    times = array.from(1000, 2000)\n    opens = array.from(100.0, 100.5)\n    highs = array.from(101.0, 102.0)\n    lows = array.from(99.0, 100.0)\n    closes = array.from(100.5, 101.5)\n    vols = array.from(500.0, 500.0)\n    bars._processLTFArrays(times, opens, highs, lows, closes, vols)\n\
            bars.onConfirm(1000.0, 100.0, 101.0, 99.0, 100.5)\n\
            bars.onConfirm(1000.0, 100.0, 101.0, 99.0, 100.5)"
        ),
        &bars,
    );
    for bar in 0..2 {
        let state = bars_at(&outcome, bar);
        assert_eq!(
            field_num(&state, "total_vol"),
            1000.0,
            "one reconciliation per bar {bar}"
        );
        assert_eq!(
            field_num(&state, "count"),
            2.0,
            "no duplicate remainder on bar {bar}"
        );
        assert!(
            field_bool(&state, "last_bar_healthy"),
            "zero drift stays healthy"
        );
    }
    assert_eq!(
        field_num(&bars_at(&outcome, 0), "tick_accuracy"),
        0.0,
        "no ticks tracked"
    );
}

#[test]
fn fetch_boundary_pull_reports_accounting() {
    // Explicit fetch-boundary check with a drifted reference: the pull
    // executes through the injection, the check counts, and the history
    // gate holds the pull counter at zero on closed bars.
    let bars = vec![
        ms_bar("100", "101", "99", "100", 1000, T0),
        ms_bar("100", "101", "99", "100", 1000, T0 + 300_000),
    ];
    let modules = linked(&format!(
        "{IMPORTS}{BARS_PROLOGUE}    times = array.from(1000, 2000)\n    opens = array.from(100.0, 100.5)\n    highs = array.from(101.0, 102.0)\n    lows = array.from(99.0, 100.0)\n    closes = array.from(100.5, 101.5)\n    vols = array.from(500.0, 500.0)\n    bars._processLTFArrays(times, opens, highs, lows, closes, vols)\n    bars.clear()\n\
        bars.onConfirmLTF(1700300000, 2000.0)"
    ));
    // NOTE: this run uses M1 init from the shared prologue, so the
    // timeframe pair below is ("SPY", "1"): register the pull rows there.
    let mut engine = test_engine();
    engine
        .inject_lower_tf("SPY", "1", pull_dataset())
        .expect("pull dataset validates");
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("fetch-boundary run evaluates");
    let state = bars_at(&outcome, 0);
    // 1700300000 crosses the M1 fetch boundary past the cleared 1699999980;
    // |0 - 2000| / 2000 = 100% drift trips the pull, which serves the five
    // injected rows summing to the 2000 reference with zero remainder.
    assert_eq!(field_num(&state, "fetch_check_count"), 1.0);
    assert_eq!(field_num(&state, "total_vol"), 2000.0);
    assert_eq!(field_num(&state, "count"), 5.0);
    assert_eq!(
        field_num(&state, "boundary_count"),
        5.0,
        "five LTF buckets crossed"
    );
    assert_eq!(
        field_num(&state, "gap_count"),
        4.0,
        "sparse gaps counted, not filled"
    );
    assert_eq!(
        field_num(&state, "pull_count"),
        0.0,
        "closed-bar history never counts pulls"
    );
    let steady = bars_at(&outcome, 1);
    assert_eq!(
        field_num(&steady, "fetch_check_count"),
        1.0,
        "second bar crosses nothing new"
    );
    assert_eq!(
        field_num(&steady, "total_vol"),
        2000.0,
        "one-shot setup persists across bars"
    );
    assert_eq!(field_num(&steady, "count"), 5.0);
    assert_eq!(field_num(&steady, "boundary_count"), 5.0);
}

#[test]
fn pull_precision_recall() {
    // Three labeled windows: a reconciled tape must NOT pull, a drifted
    // tape MUST pull through the injection, and an explicit drifted fetch
    // check MUST pull. All three correct reports precision/recall at 1.0 —
    // outcome correctness, never component confidence.
    let updates = clean_ticks();
    let bars_of = |updates: &[(Bar, i64)], ref_vol: u64| {
        let mut owned: Vec<(Bar, i64)> = updates.to_vec();
        if let Some((last, _)) = owned.last_mut() {
            *last = ms_bar("100", "102", "99", "100.5", ref_vol, T0);
        }
        (
            owned
                .iter()
                .map(|(bar, _)| bar.clone())
                .collect::<Vec<Bar>>(),
            owned.iter().map(|(_, stamp)| *stamp).collect::<Vec<i64>>(),
        )
    };

    // Window 1: reconciled (reference matches the tape).
    let (bars, stamps) = bars_of(&updates, 1000);
    let modules = linked(&fetch_driver());
    let mut engine = test_engine();
    let pulled_clean = {
        let outcome = engine
            .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
            .expect("clean window evaluates");
        field_num(&bars_at(&outcome, 9), "pull_count") > 0.0
    };

    // Window 2: drifted (heavy seeding pair starves the deltas, 60%
    // drift) with 1000-unit rows injected.
    let drifted: Vec<(i64, f64, u64)> = vec![
        (5_000, 100.0, 300),
        (10_000, 100.2, 600),
        (20_000, 101.0, 700),
        (30_000, 102.0, 800),
        (70_000, 101.5, 850),
        (80_000, 101.0, 900),
        (190_000, 99.0, 940),
        (200_000, 99.5, 970),
        (250_000, 100.0, 990),
        (290_000, 100.5, 1000),
    ];
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let updates: Vec<(Bar, i64)> = drifted
        .iter()
        .map(|(offset, price, cumvol)| {
            high = high.max(*price);
            low = low.min(*price);
            tick_update(T0, T0 + offset, 100.0, high, low, *price, *cumvol)
        })
        .collect();
    let bars: Vec<Bar> = updates.iter().map(|(bar, _)| bar.clone()).collect();
    let stamps: Vec<i64> = updates.iter().map(|(_, stamp)| *stamp).collect();
    let modules = linked(&fetch_driver());
    let mut engine = test_engine();
    engine
        .inject_lower_tf("SPY", "1", pull_dataset_scaled(200.0))
        .expect("pull dataset validates");
    let pulled_drifted = {
        let outcome = engine
            .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
            .expect("drifted window evaluates");
        field_num(&bars_at(&outcome, 9), "pull_count") > 0.0
    };

    // Window 3: explicit drifted fetch-boundary check (closed bars).
    let bars = vec![
        ms_bar("100", "101", "99", "100", 1000, T0),
        ms_bar("100", "101", "99", "100", 1000, T0 + 300_000),
    ];
    let modules = linked(&format!(
        "{IMPORTS}{BARS_PROLOGUE}    times = array.from(1000, 2000)\n    opens = array.from(100.0, 100.5)\n    highs = array.from(101.0, 102.0)\n    lows = array.from(99.0, 100.0)\n    closes = array.from(100.5, 101.5)\n    vols = array.from(500.0, 500.0)\n    bars._processLTFArrays(times, opens, highs, lows, closes, vols)\n    bars.clear()\n\
        bars.onConfirmLTF(1700300000, 2000.0)"
    ));
    let mut engine = test_engine();
    engine
        .inject_lower_tf("SPY", "1", pull_dataset())
        .expect("pull dataset validates");
    let pulled_explicit = {
        let outcome = engine
            .run_linked(&modules, &bars, InputSet::new())
            .expect("explicit window evaluates");
        field_num(&bars_at(&outcome, 0), "fetch_check_count") > 0.0
            && field_num(&bars_at(&outcome, 0), "total_vol") == 2000.0
    };

    let fired = [pulled_clean, pulled_drifted, pulled_explicit];
    let labels = [false, true, true];
    let mut true_positives = 0;
    let mut false_positives = 0;
    let mut false_negatives = 0;
    for (slot, (got, want)) in fired.iter().zip(labels.iter()).enumerate() {
        assert_eq!(*got, *want, "pull window {slot} triggers {want}");
        match (got, want) {
            (true, true) => true_positives += 1,
            (true, false) => false_positives += 1,
            (false, true) => false_negatives += 1,
            (false, false) => {}
        }
    }
    let precision = f64::from(true_positives) / f64::from(true_positives + false_positives);
    let recall = f64::from(true_positives) / f64::from(true_positives + false_negatives);
    assert_eq!(precision, 1.0, "clean windows never pull");
    assert_eq!(recall, 1.0, "drifted windows always pull");
}

#[test]
fn ltf_injection_contract() {
    let bars = vec![ms_bar("100", "101", "99", "100", 1000, T0)];
    let dataset = LtfDataset {
        times: vec![1000, 2000, 3000]
            .into_iter()
            .map(EpochMs::from)
            .collect(),
        closes_at: vec![2000, 3000, 4000]
            .into_iter()
            .map(|close| Some(EpochMs::from(close)))
            .collect(),
        opens: vec![10.0, 11.0, 12.0],
        highs: vec![10.5, 11.5, 12.5],
        lows: vec![9.5, 10.5, 11.5],
        closes: vec![10.2, 11.2, 12.2],
        volumes: vec![100.0, 200.0, 300.0],
    };

    // Served verbatim, then tail-taken by calc_bars_count.
    let driver = "[t, o, h, l, c, v] = request.security_lower_tf(\"SPY\", \"1\", [time, open, high, low, close, volume], calc_bars_count = 300)\n\
        n = array.size(t)\nfirst = array.get(c, 0)\nlast = array.get(v, 2)";
    let modules = vec![LinkedModule::new(
        "local",
        parse_program(driver).expect("driver parses"),
        HashMap::new(),
    )];
    let mut engine = test_engine();
    engine
        .inject_lower_tf("SPY", "1", dataset.clone())
        .expect("dataset validates");
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("injected call evaluates");
    assert_eq!(num(&outcome, 0, "n"), 3.0);
    assert_eq!(num(&outcome, 0, "first"), 10.2);
    assert_eq!(num(&outcome, 0, "last"), 300.0);

    let driver = "[t, o, h, l, c, v] = request.security_lower_tf(\"SPY\", \"1\", [time, open, high, low, close, volume], calc_bars_count = 2)\n\
        n = array.size(t)\nfirst = array.get(c, 0)";
    let modules = vec![LinkedModule::new(
        "local",
        parse_program(driver).expect("driver parses"),
        HashMap::new(),
    )];
    let mut engine = test_engine();
    engine
        .inject_lower_tf("SPY", "1", dataset)
        .expect("dataset validates");
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("counted call evaluates");
    assert_eq!(
        num(&outcome, 0, "n"),
        2.0,
        "calc_bars_count keeps the last rows"
    );
    assert_eq!(
        num(&outcome, 0, "first"),
        11.2,
        "window starts at the second row"
    );

    // Unregistered pairs and mismatched symbols refuse with the documented
    // live-fetch reason, never chart data.
    for probe in [
        "[t, o, h, l, c, v] = request.security_lower_tf(\"SPY\", \"5\", [time, open, high, low, close, volume])",
        "[t, o, h, l, c, v] = request.security_lower_tf(\"AAPL\", \"1\", [time, open, high, low, close, volume])",
    ] {
        let modules = vec![LinkedModule::new(
            "local",
            parse_program(probe).expect("probe parses"),
            HashMap::new(),
        )];
        let mut engine = test_engine();
        engine
            .inject_lower_tf(
                "SPY",
                "1",
                LtfDataset {
                    times: vec![EpochMs::from(1000)],
                    closes_at: vec![Some(EpochMs::from(2000))],
                    opens: vec![10.0],
                    highs: vec![10.5],
                    lows: vec![9.5],
                    closes: vec![10.2],
                    volumes: vec![100.0],
                },
            )
            .expect("dataset validates");
        let err = engine
            .run_linked(&modules, &bars, InputSet::new())
            .expect_err("unregistered pair refuses");
        assert_eq!(
            err.to_string(),
            "pine evaluation refused: live request.security_lower_tf is refused headlessly; inject multi-timeframe bars and tick data through the run instead of a network fetch",
            "exact refusal pins the no-silent-substitution rule"
        );
    }

    // Non-leg expressions, unknown named arguments, and non-positive
    // counts refuse instead of serving plausible-looking data.
    for (probe, refusal) in [
        (
            "[t, o, h, l, c, v] = request.security_lower_tf(\"SPY\", \"1\", [time, open + 1, high, low, close, volume])",
            "injected lower-tf serves bare chart legs only, never computed expressions",
        ),
        (
            "[t, o, h, l, c, v] = request.security_lower_tf(\"SPY\", \"1\", [time, open, high, low, close, volume], foo = 1)",
            "request.security_lower_tf takes no argument 'foo'",
        ),
        (
            "[t, o, h, l, c, v] = request.security_lower_tf(\"SPY\", \"1\", [time, open, high, low, close, volume], calc_bars_count = 0)",
            "request.security_lower_tf takes a positive calc_bars_count, got 0",
        ),
        (
            "[t, o, h, l, c, v] = request.security_lower_tf(\"SPY\", \"1\", [time, open, high, low, close, volume], calc_bars_count = 2.5)",
            "request.security_lower_tf takes an int calc_bars_count, got Some(Float(2.5))",
        ),
    ] {
        let modules = vec![LinkedModule::new(
            "local",
            parse_program(probe).expect("probe parses"),
            HashMap::new(),
        )];
        let mut engine = test_engine();
        engine
            .inject_lower_tf(
                "SPY",
                "1",
                LtfDataset {
                    times: vec![EpochMs::from(1000)],
                    closes_at: vec![Some(EpochMs::from(2000))],
                    opens: vec![10.0],
                    highs: vec![10.5],
                    lows: vec![9.5],
                    closes: vec![10.2],
                    volumes: vec![100.0],
                },
            )
            .expect("dataset validates");
        let err = engine
            .run_linked(&modules, &bars, InputSet::new())
            .expect_err("malformed pull refuses");
        assert_eq!(
            err.to_string(),
            format!("pine evaluation refused: {refusal}"),
            "exact refusal pins the fail-closed bound"
        );
    }

    // Ragged columns refuse at registration, never mid-run.
    let mut engine = test_engine();
    let err = engine
        .inject_lower_tf(
            "SPY",
            "1",
            LtfDataset {
                times: vec![EpochMs::from(1000), EpochMs::from(2000)],
                closes_at: vec![Some(EpochMs::from(2000)), Some(EpochMs::from(3000))],
                opens: vec![10.0],
                highs: vec![10.5, 11.5],
                lows: vec![9.5, 10.5],
                closes: vec![10.2, 11.2],
                volumes: vec![100.0, 200.0],
            },
        )
        .expect_err("ragged dataset refuses");
    assert_eq!(
        err.to_string(),
        "pine evaluation refused: injected lower-tf dataset for SPY/1 is ragged: opens has 1 rows for 2 timestamps"
    );
}

/// A hundred one-minute buckets from `start_ms`: opens `100..=199`, volumes
/// `1..=100`, each bucket closing exactly one minute after it opens.
fn hundred_minutes(start_ms: i64) -> LtfDataset {
    let rows = || 0..100_i64;
    LtfDataset {
        times: rows()
            .map(|i| EpochMs::from(start_ms + i * 60_000))
            .collect(),
        closes_at: rows()
            .map(|i| Some(EpochMs::from(start_ms + (i + 1) * 60_000)))
            .collect(),
        opens: rows().map(|i| 100.0 + i as f64).collect(),
        highs: rows().map(|i| 100.5 + i as f64).collect(),
        lows: rows().map(|i| 99.5 + i as f64).collect(),
        closes: rows().map(|i| 100.2 + i as f64).collect(),
        volumes: rows().map(|i| i as f64 + 1.0).collect(),
    }
}

/// The lower-timeframe window driver: row count plus the oldest and newest
/// served opens, which together pin *which* window was served.
const LTF_WINDOW_DRIVER: &str = "[t, o, h, l, c, v] = request.security_lower_tf(\"SPY\", \"1\", [time, open, high, low, close, volume], calc_bars_count = 300)\n\
    n = array.size(t)\nfirst = array.get(o, 0)\nlast = array.get(o, n - 1)";

/// The closed-bar window is the confirmed prefix, not the whole tape: with a
/// hundred injected minute buckets, the call at bar *k* (the bucket grid's own
/// open) sees exactly the *k* buckets that had closed by then. Serving the
/// whole dataset would return 100 rows on every bar.
#[test]
fn ltf_closed_bar_window_is_fenced() {
    // A minute-grid stamp, so each chart bar lands on a bucket open.
    const START: i64 = 1_760_000_160_000;
    let modules = linked(LTF_WINDOW_DRIVER);
    let cases = [1_i64, 10, 50, 99];
    let bars: Vec<Bar> = cases
        .iter()
        .map(|k| ms_bar("100", "101", "99", "100", 1, START + k * 60_000))
        .collect();
    let mut engine = test_engine();
    engine
        .inject_lower_tf("SPY", "1", hundred_minutes(START))
        .expect("dataset validates");
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("fenced call evaluates");
    for (bar, k) in cases.iter().enumerate() {
        assert_eq!(
            num(&outcome, bar, "n"),
            *k as f64,
            "bar {k}: only the buckets closed by the bar's own open"
        );
        assert_eq!(
            num(&outcome, bar, "first"),
            100.0,
            "bar {k}: the window starts at the oldest visible bucket"
        );
        assert_eq!(
            num(&outcome, bar, "last"),
            100.0 + (*k as f64 - 1.0),
            "bar {k}: the window ends at the newest closed bucket"
        );
    }
}

/// One instant, one window: the same bars and the same injection read the same
/// confirmed prefix through either clock, so the tick arm and the closed-bar arm
/// cannot disagree about how much of the tape exists yet.
#[test]
fn ltf_window_agrees_across_modes_at_an_aligned_clock() {
    const START: i64 = 1_760_000_160_000;
    let modules = linked(LTF_WINDOW_DRIVER);
    let cases = [1_i64, 7, 40];
    let bars: Vec<Bar> = cases
        .iter()
        .map(|k| ms_bar("100", "101", "99", "100", 1, START + k * 60_000))
        .collect();
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let mut engine = test_engine();
    engine
        .inject_lower_tf("SPY", "1", hundred_minutes(START))
        .expect("dataset validates");
    let closed = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("closed-bar run evaluates");
    let tick = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("tick run evaluates");
    for (bar, k) in cases.iter().enumerate() {
        for field in ["n", "first", "last"] {
            assert_eq!(
                num(&tick, bar, field),
                num(&closed, bar, field),
                "bar {k} {field}: one instant, one window"
            );
        }
        assert_eq!(
            num(&closed, bar, "n"),
            *k as f64,
            "bar {k}: the aligned clock confirms the same prefix in both modes"
        );
    }
}

/// Serving is a view, never a copy: `serve_security` is a free function over a
/// borrowed dataset (no mutable engine, no clone), and the tail it selects
/// points into the dataset's own columns.
#[test]
fn serve_security_borrows_the_dataset() {
    const START: i64 = 1_760_000_160_000;
    let dataset = hundred_minutes(START);
    // Held across the call: a signature that needed `&mut` or a copy could not
    // borrow the columns this way.
    let times: &[EpochMs] = &dataset.times;
    let window = serve_security(&dataset, EpochMs::from(START + 10 * 60_000), Some(4));
    assert_eq!(window, (6, 10), "ten confirmed, the last four of them");
    let tail = SecurityTail::of(&dataset, window, Some(4));
    assert_eq!(
        tail.column("close").expect("close leg").as_ptr(),
        dataset.closes[6..10].as_ptr(),
        "the served tail is a slice of the injected column, not a copy"
    );
    assert_eq!(
        tail.history().column("close").expect("close leg").as_ptr(),
        dataset.closes.as_ptr(),
        "the computation view starts at the dataset's first confirmed row"
    );
    assert_eq!(times.len(), 100, "the borrow was live across the call");
}

// ---------------------------------------------------------------------------
// Trimmed real-tape windows (checked-in fixtures with recorded baselines).//
// Each file under `tests/data/ticks/` carries verbatim `SPY_ticks.csv`
// rows for one window; baselines below are hand-derived from the fixture
// bytes plus the library rules cited beside them, and the companion file
// `tests/data/tick_harness_baselines.json` records them with their
// bounds. Geometry mirrors the two harnesses that need it:
//
// - quiet/pull: one minute, per-row bar-open times (the proof's minute
//   geometry). A single LTF bucket means no boundary crossings; the
//   final tick confirms with `ctf_vol` equal to its own volume leg, so
//   drift is exactly the seeding loss over the tape total.
// - sparse: three minutes under one constant bar-open (the M6 chart
//   scope), so the middle empty minute registers as an LTF gap instead
//   of three independent bars.
//
// Chart period never enters `Bars` mechanics (legs plus `init` args do),
// so all three run the one `test_engine`. Per M12 rules these tests pin
// fixtures: a red test here fixes the test's understanding or files new
// work, never the engine and never a silent baseline edit.

/// One trimmed window: `(stamp_ms, price, volume)` in file order.
fn load_tick_window(name: &str) -> Vec<(i64, f64, f64)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/ticks")
        .join(name);
    let content = std::fs::read_to_string(&path).expect("tick window loads");
    let mut lines = content.lines();
    assert_eq!(
        lines.next().expect("window header"),
        "timestamp_ms,price,volume,is_out_of_order,original_timestamp_ms",
        "window columns pinned"
    );
    lines
        .map(|line| {
            let mut cells = line.split(',');
            let stamp = cells
                .next()
                .expect("stamp")
                .parse::<i64>()
                .expect("int stamp");
            let price = cells.next().expect("price").parse::<f64>().expect("price");
            let volume = cells
                .next()
                .expect("volume")
                .parse::<f64>()
                .expect("volume");
            (stamp, price, volume)
        })
        .collect()
}

/// M10-style legs over window rows: running bar-so-far OHLC, rounded
/// cumulative volume, per-row bar-open times; tick stamps ride the side
/// channel. Cumulative legs never decrease (all tape volumes are
/// positive), so the rollback path stays quiet on real windows.
fn window_updates(rows: &[(i64, f64, f64)], open_of: impl Fn(i64) -> i64) -> (Vec<Bar>, Vec<i64>) {
    let mut bars = Vec::with_capacity(rows.len());
    let mut stamps = Vec::with_capacity(rows.len());
    let mut open = 0.0;
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let mut cum = 0.0;
    let mut current = -1i64;
    for (stamp, price, volume) in rows {
        let bucket = open_of(*stamp);
        if bucket != current {
            current = bucket;
            open = *price;
            high = f64::NEG_INFINITY;
            low = f64::INFINITY;
        }
        high = high.max(*price);
        low = low.min(*price);
        cum += *volume;
        let (bar, tick_ms) =
            tick_update(bucket, *stamp, open, high, low, *price, cum.round() as u64);
        bars.push(bar);
        stamps.push(tick_ms);
    }
    (bars, stamps)
}

fn run_window(driver: &str, bars: &[Bar], stamps: &[i64]) -> RunOutcome {
    let modules = linked(driver);
    let mut engine = test_engine();
    engine
        .run_linked_ticks(&modules, bars, stamps, InputSet::new())
        .expect("window run evaluates with zero refusals")
}

fn assert_close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() <= EPS * want.abs().max(1.0),
        "{what}: got {got}, want {want}"
    );
}

/// Fifteen real ticks, first-two-tick seeding rounds to zero: the
/// accumulator holds the full 1105-unit tape, drift reads exactly zero,
/// and the remainder top-up has nothing to add (one finalized candle).
#[test]
fn trim_quiet_reconciles_without_pull() {
    let rows = load_tick_window("window_quiet.csv");
    assert_eq!(rows.len(), 15, "quiet window row count pinned");
    let (bars, stamps) = window_updates(&rows, |stamp| stamp.div_euclid(60_000) * 60_000);
    let outcome = run_window(&fetch_driver(), &bars, &stamps);
    let last = rows.len() - 1;
    let state = bars_at(&outcome, last);
    assert_eq!(
        field_num(&state, "count"),
        1.0,
        "finalize only, no remainder"
    );
    assert_close(field_num(&state, "total_vol"), 1105.0, "tape total");
    assert_eq!(
        field_num(&state, "pull_count"),
        0.0,
        "zero drift pulls nothing"
    );
    assert_eq!(field_num(&state, "fetch_check_count"), 0.0);
    assert_eq!(field_num(&state, "boundary_count"), 0.0, "one LTF bucket");
    assert_eq!(field_num(&state, "gap_count"), 0.0);
    assert_eq!(
        field_num(&state, "rollback_count"),
        0.0,
        "cumulatives never decrease"
    );
    assert_close(
        field_num(&state, "tick_accuracy"),
        1.0,
        "full tape accumulates",
    );
    assert_close(field_num(&state, "volume_drift_pct"), 0.0, "no drift");
    assert_close(field_num(&state, "last_bar_drift"), 0.0, "no drift");
    assert!(
        field_bool(&state, "last_bar_healthy"),
        "zero drift is healthy"
    );
}

/// The last real `SPY_1M.csv` row whose bucket has **closed** by `open_ms`,
/// for pull windows whose injection must stay hand-summable. The bucket
/// running into the window is still forming at the pull, so it can never be
/// served; the previous minute is the freshest reference the library can get.
fn closed_minute_row(open_ms: i64) -> LtfDataset {
    let path = data_root().join("historical/SPY_1M.csv");
    let content = std::fs::read_to_string(&path).expect("minute history loads");
    let mut picked: Option<LtfDataset> = None;
    for line in content.lines().skip(1) {
        let mut cells = line.split(',');
        let secs = cells
            .next()
            .expect("time")
            .parse::<i64>()
            .expect("int time");
        if secs * 1000 + 60_000 > open_ms {
            break;
        }
        picked = Some(LtfDataset {
            times: vec![EpochMs::from(secs * 1000)],
            closes_at: vec![Some(EpochMs::from(secs * 1000 + 60_000))],
            opens: vec![cells.next().expect("o").parse::<f64>().expect("open")],
            highs: vec![cells.next().expect("h").parse::<f64>().expect("high")],
            lows: vec![cells.next().expect("l").parse::<f64>().expect("low")],
            closes: vec![cells.next().expect("c").parse::<f64>().expect("close")],
            volumes: vec![cells.next().expect("v").parse::<f64>().expect("volume")],
        });
    }
    picked.expect("a minute row precedes the window")
}

/// Twenty real ticks with an 88-unit seeding loss over a 1062-unit
/// tape (8.29% drift): the pull fires once and rebuilds the ring from the
/// last closed minute — the *previous* bucket (827 units), since the bucket
/// the window runs into has not closed yet — and the absolute remainder tops
/// the bar up by the missing 235, landing exactly on 1062.
#[test]
fn trim_pull_rebuilds_from_injected_row() {
    let rows = load_tick_window("window_pull.csv");
    assert_eq!(rows.len(), 20, "pull window row count pinned");
    let open_ms = rows[0].0.div_euclid(60_000) * 60_000;
    let (bars, stamps) = window_updates(&rows, |stamp| stamp.div_euclid(60_000) * 60_000);
    let modules = linked(&fetch_driver());
    let mut engine = test_engine();
    engine
        .inject_lower_tf("SPY", "1", closed_minute_row(open_ms))
        .expect("pull row injects");
    let outcome = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("pull window evaluates with zero refusals");
    let last = rows.len() - 1;
    let state = bars_at(&outcome, last);
    assert_eq!(field_num(&state, "pull_count"), 1.0, "drift trips one pull");
    assert_eq!(
        field_num(&state, "fetch_check_count"),
        1.0,
        "confirm check counts"
    );
    assert_eq!(
        field_num(&state, "count"),
        2.0,
        "rebuilt row plus abs remainder"
    );
    assert_close(
        field_num(&state, "total_vol"),
        1062.0,
        "827 rebuilt plus the 235 top-up",
    );
    assert_close(
        field_num(&state, "tick_accuracy"),
        974.0 / 1062.0,
        "accumulated over leg",
    );
    assert_close(
        field_num(&state, "volume_drift_pct"),
        88.0 / 1062.0 * 100.0,
        "seeding drift",
    );
    assert_close(
        field_num(&state, "last_bar_drift"),
        88.0 / 1062.0 * 100.0,
        "seeding drift",
    );
    assert!(
        !field_bool(&state, "last_bar_healthy"),
        "drift trips the threshold"
    );
    assert_eq!(field_num(&state, "boundary_count"), 0.0, "one LTF bucket");
    assert_eq!(field_num(&state, "gap_count"), 0.0);
    assert_eq!(
        field_num(&state, "rollback_count"),
        0.0,
        "cumulatives never decrease"
    );
}

/// Twenty real ticks across three minutes with the middle minute empty,
/// under one bar-open: the first tick past the hole crosses two LTF
/// boundaries (one counted gap), both populated minutes finalize, and
/// the remainder tops the 1421-unit bar with no pull (0.28% drift).
#[test]
fn trim_sparse_gap_registers_without_pull() {
    let rows = load_tick_window("window_sparse.csv");
    assert_eq!(rows.len(), 20, "sparse window row count pinned");
    const BAR_OPEN: i64 = 1767995580000;
    assert!(
        rows.iter().all(|(stamp, _, _)| *stamp >= BAR_OPEN),
        "window rides one bar-open"
    );
    let (bars, stamps) = window_updates(&rows, |_| BAR_OPEN);
    let outcome = run_window(&fetch_driver(), &bars, &stamps);
    let last = rows.len() - 1;
    let state = bars_at(&outcome, last);
    assert_eq!(
        field_num(&state, "boundary_count"),
        2.0,
        "two LTF crossings past the hole"
    );
    assert_eq!(
        field_num(&state, "gap_count"),
        1.0,
        "middle minute had no trades"
    );
    assert_eq!(
        field_num(&state, "pull_count"),
        0.0,
        "drift stays under threshold"
    );
    assert_eq!(field_num(&state, "fetch_check_count"), 0.0);
    assert_eq!(
        field_num(&state, "rollback_count"),
        0.0,
        "cumulatives never decrease"
    );
    assert_eq!(
        field_num(&state, "count"),
        3.0,
        "two finalized candles plus remainder"
    );
    assert_close(field_num(&state, "total_vol"), 1421.0, "bar total");
    assert_close(
        field_num(&state, "tick_accuracy"),
        1417.0 / 1421.0,
        "accumulated over leg",
    );
    assert_close(
        field_num(&state, "volume_drift_pct"),
        4.0 / 1421.0 * 100.0,
        "seeding drift",
    );
    assert!(
        field_bool(&state, "last_bar_healthy"),
        "drift stays healthy"
    );
}

/// Thirty real ticks carrying four out-of-order flags, under per-minute
/// bar-open geometry: the flags ride the unread side channel, so
/// accumulation matches the tape exactly (23577 units, equal to the
/// confirmed minute row), the 2.38% seeding drift stays silent, and the
/// 561-unit remainder tops the bar with no pull.
#[test]
fn trim_out_of_order_accumulates_without_pull() {
    let rows = load_tick_window("window_out_of_order.csv");
    assert_eq!(rows.len(), 30, "out-of-order window row count pinned");
    let flags = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/ticks/window_out_of_order.csv"),
    )
    .expect("window loads")
    .lines()
    .skip(1)
    .filter(|line| line.split(',').nth(3).expect("flag cell") == "True")
    .count();
    assert_eq!(flags, 4, "four rows arrive flagged out-of-order");
    let (bars, stamps) = window_updates(&rows, |stamp| stamp.div_euclid(60_000) * 60_000);
    let outcome = run_window(&fetch_driver(), &bars, &stamps);
    let last = rows.len() - 1;
    let state = bars_at(&outcome, last);
    assert_eq!(
        field_num(&state, "count"),
        2.0,
        "finalized candle plus remainder"
    );
    assert_close(field_num(&state, "total_vol"), 23577.0, "tape total");
    assert_eq!(
        field_num(&state, "pull_count"),
        0.0,
        "drift stays under threshold"
    );
    assert_eq!(field_num(&state, "fetch_check_count"), 0.0);
    assert_eq!(field_num(&state, "boundary_count"), 0.0, "one LTF bucket");
    assert_eq!(field_num(&state, "gap_count"), 0.0);
    assert_eq!(
        field_num(&state, "rollback_count"),
        0.0,
        "flags never decrease cumulatives"
    );
    assert_eq!(
        field_num(&state, "tick_count"),
        28.0,
        "ticks 3-30 accumulate"
    );
    assert_close(
        field_num(&state, "tick_accuracy"),
        23016.0 / 23577.0,
        "accumulated over leg",
    );
    assert_close(
        field_num(&state, "volume_drift_pct"),
        561.0 / 23577.0 * 100.0,
        "seeding drift",
    );
    assert!(
        field_bool(&state, "last_bar_healthy"),
        "drift stays healthy"
    );
}

/// Fourteen real ticks with a 107-unit seeding loss over a 298-unit tape
/// (35.91% drift): the pull fires once and rebuilds the ring from the last
/// closed minute, which is made to carry 299 units — one above the rounded
/// tape. The bucket the window runs into is still forming, so what the pull
/// can see is a closed bucket at the previous minute's stamp; the 1-unit gap
/// is this test's calibration, not a claim about that minute's real volume.
/// The abs-remainder rule writes the magnitude, so the second candle ADDS one
/// unit and the bar lands on 300 — one above both the leg and the rebuilt
/// row. That overshoot is the script's own `math.abs` rule, faithfully
/// executed, not a harness artifact: the window's frozen tolerance is the
/// recorded 1-unit gap, asserted literally below.
#[test]
fn trim_pull_remainder_tops_up_to_leg() {
    let rows = load_tick_window("window_pull_remainder.csv");
    assert_eq!(rows.len(), 14, "remainder window row count pinned");
    let open_ms = rows[0].0.div_euclid(60_000) * 60_000;
    let (bars, stamps) = window_updates(&rows, |stamp| stamp.div_euclid(60_000) * 60_000);
    let modules = linked(&fetch_driver());
    let mut engine = test_engine();
    let mut injection = closed_minute_row(open_ms);
    injection.volumes = vec![299.0];
    engine
        .inject_lower_tf("SPY", "1", injection)
        .expect("pull row injects");
    let outcome = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("remainder window evaluates with zero refusals");
    let last = rows.len() - 1;
    let state = bars_at(&outcome, last);
    assert_eq!(field_num(&state, "pull_count"), 1.0, "drift trips one pull");
    assert_eq!(
        field_num(&state, "fetch_check_count"),
        1.0,
        "confirm check counts"
    );
    assert_eq!(
        field_num(&state, "count"),
        2.0,
        "rebuilt row plus abs remainder"
    );
    assert_close(
        field_num(&state, "total_vol"),
        300.0,
        "299 rebuilt plus 1 abs unit",
    );
    assert_close(
        field_num(&state, "tick_accuracy"),
        191.0 / 298.0,
        "accumulated over leg",
    );
    assert_close(
        field_num(&state, "volume_drift_pct"),
        107.0 / 298.0 * 100.0,
        "seeding drift",
    );
    assert_close(
        field_num(&state, "last_bar_drift"),
        107.0 / 298.0 * 100.0,
        "pre-pull drift",
    );
    assert!(
        !field_bool(&state, "last_bar_healthy"),
        "drift trips the threshold"
    );
    assert_eq!(field_num(&state, "boundary_count"), 0.0, "one LTF bucket");
    assert_eq!(field_num(&state, "gap_count"), 0.0);
    assert_eq!(
        field_num(&state, "rollback_count"),
        0.0,
        "cumulatives never decrease"
    );
    // Rebuilt row chains from the last closed minute row (693.84/693.87/
    // 693.83/693.87, its 1105 volume overridden to the near-boundary 299);
    // the remainder carries the 1-unit abs top-up, and both candles conserve
    // buy+sell into volume.
    assert_eq!(
        field_num(&state, "candle_open"),
        693.84,
        "first injected open chains reads"
    );
    assert_close(
        field_num(&state, "total_vol") - 299.0,
        1.0,
        "tape-vs-confirmed overshoot recorded",
    );
}

/// A calibration window: legs, tick stamps, and the optional pull injection.
type GateCase = (&'static str, Vec<Bar>, Vec<i64>, Option<LtfDataset>);

/// A window spec: fixture name, bar-open geometry, and whether a pull
/// injection is registered.
type GateSpec = (&'static str, fn(i64) -> i64, bool);

/// Synthetic high-drift tape shared with the drift-gate calibration: the
/// first two ticks carry 600 of the 1000-unit tape, so seeding starves
/// the deltas to a 60% confirm drift.
fn high_drift_tape() -> Vec<(i64, f64, u64)> {
    vec![
        (5_000, 100.0, 300),
        (10_000, 100.2, 600),
        (20_000, 101.0, 700),
        (30_000, 102.0, 800),
        (70_000, 101.5, 850),
        (80_000, 101.0, 900),
        (190_000, 99.0, 940),
        (200_000, 99.5, 970),
        (250_000, 100.0, 990),
        (290_000, 100.5, 1000),
    ]
}

/// Drift-rebuild gate calibration (producer-confidence axis): the gate
/// fires exactly on windows whose measured drift exceeds the library's
/// own 4% threshold, never otherwise. Labels come from the rule, not from
/// hand tags — each window asserts `fired == (drift > 4%)`, and the
/// recorded precision/recall over the suite must both read 1.0 before
/// the gate is trusted.
///
/// Reading of the "0 false-fires" clause: the verbatim pull (8.29%) and
/// remainder (35.91%) windows fire, and those fires are TRUE positives
/// under the gate's own definition — a false fire is a below-gate window
/// firing, and there are none. The silence controls are the below-gate
/// verbatim windows (quiet 0%, sparse 0.28%, out-of-order 2.38%).
#[test]
fn drift_rebuild_gate_precision_recall() {
    // (label, run): verbatim windows plus the synthetic high-drift tape.
    let mut cases: Vec<GateCase> = Vec::new();
    let per_minute: fn(i64) -> i64 = |stamp| stamp.div_euclid(60_000) * 60_000;
    let specs: Vec<GateSpec> = vec![
        ("window_quiet.csv", per_minute, false),
        ("window_sparse.csv", |_| 1767995580000, false),
        ("window_out_of_order.csv", per_minute, false),
        ("window_pull.csv", per_minute, true),
        ("window_pull_remainder.csv", per_minute, true),
    ];
    for (name, open_of, inject) in specs {
        let rows = load_tick_window(name);
        let open_ms = rows[0].0.div_euclid(60_000) * 60_000;
        let (bars, stamps) = window_updates(&rows, open_of);
        let injection = inject.then(|| closed_minute_row(open_ms));
        cases.push((name, bars, stamps, injection));
    }
    {
        let mut high = f64::NEG_INFINITY;
        let mut low = f64::INFINITY;
        let updates: Vec<(Bar, i64)> = high_drift_tape()
            .iter()
            .map(|(offset, price, cumvol)| {
                high = high.max(*price);
                low = low.min(*price);
                tick_update(T0, T0 + offset, 100.0, high, low, *price, *cumvol)
            })
            .collect();
        cases.push((
            "synthetic high-drift",
            updates.iter().map(|(bar, _)| bar.clone()).collect(),
            updates.iter().map(|(_, stamp)| *stamp).collect(),
            Some(pull_dataset_scaled(200.0)),
        ));
    }
    let mut true_positives = 0;
    let mut false_positives = 0;
    let mut false_negatives = 0;
    for (name, bars, stamps, injection) in &cases {
        let modules = linked(&fetch_driver());
        let mut engine = test_engine();
        if let Some(dataset) = injection {
            engine
                .inject_lower_tf("SPY", "1", dataset.clone())
                .expect("calibration dataset injects");
        }
        let outcome = engine
            .run_linked_ticks(&modules, bars, stamps, InputSet::new())
            .expect("calibration window evaluates");
        let last = bars.len() - 1;
        let state = bars_at(&outcome, last);
        let drift = field_num(&state, "volume_drift_pct");
        let fired = field_num(&state, "pull_count") > 0.0;
        let want = drift > 4.0;
        assert_eq!(
            fired, want,
            "{name} fires exactly above the 4% gate (drift {drift})"
        );
        match (fired, want) {
            (true, true) => true_positives += 1,
            (true, false) => false_positives += 1,
            (false, true) => false_negatives += 1,
            (false, false) => {}
        }
    }
    let precision = f64::from(true_positives) / f64::from(true_positives + false_positives);
    let recall = f64::from(true_positives) / f64::from(true_positives + false_negatives);
    assert_eq!(precision, 1.0, "no below-gate window fires");
    assert_eq!(recall, 1.0, "every above-gate window fires");
}

/// Full-tape soak through the light two-module harness: every
/// `SPY_ticks.csv` row accumulates through `Bars :: fetch` with the real
/// minute history injected for pulls. This is the reconstruction the
/// proof runs inside the full closure, here without the indicator's MTF
/// contexts — completion plus aggregate sanity, with wall time printed
/// for the baseline record. The bound is recorded, never asserted: a
/// slower machine still passes, and a hang reports as an executed-test
/// timeout to file, never as a threshold to tweak.
///
/// Gate placement: soak, behind `#[ignore]` — the full tape reconstructs
/// in ~13 s, so the default gate stays smoke-fast and this runs via
/// `make test-heavy`.
#[test]
#[ignore = "soak: ~13s; full 10,503-tick tape reconstruction; run via `make test-heavy`"]
fn soak_full_tape_reconstructs() {
    let content = std::fs::read_to_string(data_root().join("historical/SPY_ticks.csv"))
        .expect("tick tape loads");
    let mut lines = content.lines();
    assert_eq!(
        lines.next().expect("tape header"),
        "timestamp_ms,price,volume,is_out_of_order,original_timestamp_ms",
        "tape columns pinned"
    );
    let rows: Vec<(i64, f64, f64)> = lines
        .map(|line| {
            let mut cells = line.split(',');
            let stamp = cells
                .next()
                .expect("stamp")
                .parse::<i64>()
                .expect("int stamp");
            let price = cells.next().expect("price").parse::<f64>().expect("price");
            let volume = cells
                .next()
                .expect("volume")
                .parse::<f64>()
                .expect("volume");
            (stamp, price, volume)
        })
        .collect();
    assert_eq!(rows.len(), 10503, "tape row count pinned");
    let (bars, stamps) = window_updates(&rows, |stamp| stamp.div_euclid(60_000) * 60_000);
    let history = std::fs::read_to_string(data_root().join("historical/SPY_1M.csv"))
        .expect("minute history loads");
    let mut dataset = LtfDataset {
        times: Vec::new(),
        closes_at: Vec::new(),
        opens: Vec::new(),
        highs: Vec::new(),
        lows: Vec::new(),
        closes: Vec::new(),
        volumes: Vec::new(),
    };
    for line in history.lines().skip(1) {
        let mut cells = line.split(',');
        let secs = cells
            .next()
            .expect("time")
            .parse::<i64>()
            .expect("int time");
        dataset.times.push(EpochMs::from(secs * 1000));
        dataset
            .opens
            .push(cells.next().expect("o").parse::<f64>().expect("open"));
        dataset
            .highs
            .push(cells.next().expect("h").parse::<f64>().expect("high"));
        dataset
            .lows
            .push(cells.next().expect("l").parse::<f64>().expect("low"));
        dataset
            .closes
            .push(cells.next().expect("c").parse::<f64>().expect("close"));
        dataset
            .volumes
            .push(cells.next().expect("v").parse::<f64>().expect("volume"));
    }
    let bucket_stamps: Vec<i64> = dataset.times.iter().map(|stamp| stamp.get()).collect();
    dataset.closes_at = crate::bars::bucket_closes(&bucket_stamps, 60_000)
        .into_iter()
        .map(|close| close.map(EpochMs::from))
        .collect();
    let minutes = dataset.times.len();
    let modules = linked(&fetch_driver());
    // Proof chart scope: minute geometry with the M1/M1/D1 Bars init.
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("1".to_owned())
            .max_bars(200_000)
            .build(),
    );
    engine
        .inject_lower_tf("SPY", "1", dataset)
        .expect("minute history injects");
    let started = std::time::Instant::now();
    let outcome = engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("soak evaluates with zero refusals");
    let elapsed = started.elapsed();
    assert_eq!(
        outcome.evaluated,
        rows.len(),
        "every tape row evaluates once"
    );
    let state = bars_at(&outcome, rows.len() - 1);
    assert!(
        field_num(&state, "pull_count") >= 1.0,
        "pulled minutes fire on real drift"
    );
    assert!(
        field_num(&state, "total_vol") > 0.0,
        "final bar reconstructs"
    );
    assert!(field_num(&state, "count") > 0.0, "final bar holds candles");
    println!(
        "tick soak: {} ticks over {minutes} injected minutes in {:.1}s",
        rows.len(),
        elapsed.as_secs_f64()
    );
}
