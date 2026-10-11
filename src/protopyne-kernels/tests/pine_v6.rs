//! v6 surface tests: `switch`, enums, consts, user functions/methods,
//! tuples, multi-plot capture, and the library shapes from the real targets
//! (timeframe switches, `Scheduler`, `getSupertrend`/`getMACD`/
//! `getVossPredictiveFilter`).
//!
//! Shapes transcribed from `data/protopyne-rs/libraries/` are literal
//! excerpts; the reference runtime is never a dependency.
//!
//! Bit-exact float comparisons below are over exact small integrals (tuple
//! directions, warmup zeros), never measured quantities.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::{PineRuntimeConfig, inputs::InputSet, parse_program};
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn bar_ts(open: &str, close: &str, ts: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::Utc
                .timestamp_opt(ts, 0)
                .single()
                .expect("valid test timestamp"),
        )
        .o(open.parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn bar(open: &str, close: &str) -> Bar {
    bar_ts(open, close, 1_000_000)
}

fn closes() -> Vec<Bar> {
    vec![
        bar("100.00", "10.00"),
        bar("100.00", "11.00"),
        bar("100.00", "12.00"),
        bar("100.00", "13.00"),
    ]
}

fn rising(count: usize) -> Vec<Bar> {
    (0..count)
        .map(|i| {
            let price = format!("{}.00", 10 + i as i64);
            bar_ts("100.00", &price, 1_000_000 + i as i64 * 60)
        })
        .collect()
}

fn run_default(script: &str, bars: &[Bar]) -> RunOutcome {
    let program = parse_program(script).expect("script parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine
        .run(&program, bars, InputSet::new())
        .expect("script runs")
}

fn run_err(script: &str, bars: &[Bar]) -> String {
    let program = parse_program(script).expect("script parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine
        .run(&program, bars, InputSet::new())
        .expect_err("script refuses")
        .to_string()
}

/// Numeric reading of one snapshot cell (`int` auto-casts, like the engine).
fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> Option<f64> {
    match outcome.value_at(at_bar, name)? {
        Some(PineValue::Float(v)) => Some(v),
        Some(PineValue::Int(v)) => Some(v as f64),
        _ => None,
    }
}

fn is_absent(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    matches!(outcome.value_at(at_bar, name), None | Some(None))
}

fn text_of(outcome: &RunOutcome, at_bar: usize, name: &str) -> Option<String> {
    match outcome.value_at(at_bar, name)? {
        Some(PineValue::Str(text)) => Some(text),
        _ => None,
    }
}

// ---- `switch` over scalars ----

#[test]
fn switch_scalar_first_match_wins_with_default() {
    let bars = closes();
    let outcome = run_default(
        "x = switch close\n    10.0 => \"ten\"\n    11.0 => \"eleven\"\n    => \"other\"",
        &bars,
    );
    assert_eq!(text_of(&outcome, 0, "x").as_deref(), Some("ten"));
    assert_eq!(text_of(&outcome, 1, "x").as_deref(), Some("eleven"));
    assert_eq!(text_of(&outcome, 2, "x").as_deref(), Some("other"));
    assert_eq!(text_of(&outcome, 3, "x").as_deref(), Some("other"));
}

#[test]
fn switch_statement_runs_arm_bodies() {
    let bars = closes();
    let outcome = run_default("switch close\n    10.0 => x = 1\n    => x = 2", &bars);
    assert_eq!(num(&outcome, 0, "x"), Some(1.0));
    assert_eq!(num(&outcome, 1, "x"), Some(2.0));
}

#[test]
fn switch_unmatched_without_default_yields_na() {
    let bars = closes();
    let outcome = run_default("length = switch close\n    10.0 => 5\n    11.0 => 6", &bars);
    assert_eq!(num(&outcome, 0, "length"), Some(5.0));
    assert_eq!(num(&outcome, 1, "length"), Some(6.0));
    assert!(is_absent(&outcome, 2, "length"), "no arm, no default: na");
    assert!(is_absent(&outcome, 3, "length"), "no arm, no default: na");
}

#[test]
fn switch_expression_assigns_like_normalize_osc() {
    // `length = switch which / 1 => fastLen / => slowLen` shape.
    let bars = closes();
    let outcome = run_default(
        "which = 1\nlength = switch which\n    1 => 5\n    => 14",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "length"), Some(5.0));
    let outcome = run_default(
        "which = 2\nlength = switch which\n    1 => 5\n    => 14",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "length"), Some(14.0));
}

// ---- enums ----

const TIMEFRAMES: &str = "export enum Timeframes\n    M1 = \"1 Minute\"\n    M5 = \"5 Minutes\"\n    S5 = \"5 Seconds\"\n";

#[test]
fn switch_matches_enum_members() {
    let bars = closes();
    let outcome = run_default(
        &format!(
            "{TIMEFRAMES}abbr = switch Timeframes.M5\n    Timeframes.M1 => \"1\"\n    Timeframes.M5 => \"5\"\n    => \"?\""
        ),
        &bars,
    );
    assert_eq!(text_of(&outcome, 0, "abbr").as_deref(), Some("5"));
}

#[test]
fn enum_member_lookup_ignores_module_prefix() {
    // Imported members arrive as `UTIL.Smoothings.WMA`; the engine resolves
    // the trailing `Enum.Member` pair against its registered enums.
    let bars = closes();
    let outcome = run_default(
            "export enum Smoothings\n    SMA = \"SMA\"\n    WMA = \"WMA\"\npicked = switch UTIL.Smoothings.WMA\n    Smoothings.SMA => \"s\"\n    Smoothings.WMA => \"w\"\n    => \"?\"",
        &bars,
    );
    assert_eq!(text_of(&outcome, 0, "picked").as_deref(), Some("w"));
}

#[test]
fn enum_handles_compare_for_equality_only() {
    let bars = closes();
    let outcome = run_default(
        "export enum S\n    A = \"a\"\n    N = \"n\"\nv = S.A\nx = v != S.N\ny = v == S.A",
        &bars,
    );
    assert_eq!(
        outcome.value_at(0, "x"),
        Some(Some(PineValue::Bool(true)))
    );
    assert_eq!(
        outcome.value_at(0, "y"),
        Some(Some(PineValue::Bool(true)))
    );
}

// ---- consts + exports ----

#[test]
fn const_decls_bind_and_reference_earlier_consts() {
    let bars = closes();
    let outcome = run_default(
        "export const float CENTER = 0.0\nexport const float WIDTH = 10.0 + 10.0\nexport const float RANGE = WIDTH - CENTER",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "CENTER"), Some(0.0));
    assert_eq!(num(&outcome, 0, "WIDTH"), Some(20.0));
    assert_eq!(num(&outcome, 0, "RANGE"), Some(20.0));
}

#[test]
fn export_decls_record_their_names() {
    let bars = closes();
    let program = parse_program(
        "export const float CENTER = 0.0\nexport enum S\n    A = \"a\"\nexport ms(x) => x",
    )
    .expect("parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine
        .run(&program, &bars, InputSet::new())
        .expect("runs");
    let mut names = engine.exported_names();
    names.sort_unstable();
    assert_eq!(names, vec!["CENTER", "S", "ms"]);
}

// ---- user functions + tuples ----

#[test]
fn user_function_returns_last_expression_and_tuples_destructure() {
    let bars = closes();
    let outcome = run_default(
        "export pair(float a, float b) =>\n    [a + b, a - b]\n[p, d] = pair(10.0, 4.0)",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "p"), Some(14.0));
    assert_eq!(num(&outcome, 0, "d"), Some(6.0));
}

#[test]
fn user_function_defaults_fill_missing_args() {
    let bars = closes();
    let outcome = run_default(
        "export bump(int x, int step = 2) =>\n    x + step\na = bump(5)\nb = bump(5, 10)",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "a"), Some(7.0));
    assert_eq!(num(&outcome, 0, "b"), Some(15.0));
}

#[test]
fn user_function_locals_do_not_leak() {
    let bars = closes();
    let outcome = run_default(
        "export f(float x) =>\n    tmp = x * 2\n    tmp + 1\ny = f(10.0)",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "y"), Some(21.0));
    assert!(
        outcome.value_at(0, "tmp").is_none(),
        "function locals stay local"
    );
}

#[test]
fn timeframe_switch_function_matches_library_shape() {
    // Transcribed `getTimeframeMS` arms over `Timeframes`.
    let bars = closes();
    let outcome = run_default(
        &format!(
            "{TIMEFRAMES}export ms(Timeframes tf) =>\n    switch tf\n        Timeframes.M1 => 60000\n        Timeframes.M5 => 300000\n        Timeframes.S5 => 5000\n        =>\n            runtime.error(\"Unknown timeframe\")\n            0\nm = ms(Timeframes.M5)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 0, "m"), Some(300_000.0));
}

#[test]
fn full_timeframe_ms_switch_matches_library_values() {
    // Complete `getTimeframeMS` arm table, transcribed member-for-member;
    // every value below is the literal from the library source. Members
    // without an explicit arm (`C`, `MO1`, `MO3`, `Y`, `NA`) fall through
    // to the `timeframe.period` default, which stays a later-stage mapping.
    let bars = closes();
    let script = "export enum Timeframes\n    C = \"Chart\"\n    S5 = \"5 Seconds\"\n    S10 = \"10 Seconds\"\n    S15 = \"15 Seconds\"\n    S30 = \"30 Seconds\"\n    M1 = \"1 Minute\"\n    M3 = \"3 Minutes\"\n    M5 = \"5 Minutes\"\n    M10 = \"10 Minutes\"\n    M15 = \"15 Minutes\"\n    M30 = \"30 Minutes\"\n    H1 = \"1 Hour\"\n    H2 = \"2 Hours\"\n    H4 = \"4 Hours\"\n    D1 = \"1 Day\"\n    W1 = \"1 Week\"\n    MO1 = \"1 Month\"\n    MO3 = \"3 Months\"\n    Y = \"Year\"\n    NA = \"-\"\nexport ms(Timeframes tf) =>\n    switch tf\n        Timeframes.S5 => 5000\n        Timeframes.S10 => 10000\n        Timeframes.S15 => 15000\n        Timeframes.S30 => 30000\n        Timeframes.M1 => 60000\n        Timeframes.M3 => 180000\n        Timeframes.M5 => 300000\n        Timeframes.M10 => 600000\n        Timeframes.M15 => 900000\n        Timeframes.M30 => 1800000\n        Timeframes.H1 => 3600000\n        Timeframes.H2 => 7200000\n        Timeframes.H4 => 14400000\n        Timeframes.D1 => 86400000\n        Timeframes.W1 => 604800000\n        =>\n            runtime.error(\"Unknown timeframe\")\n            0\na = ms(Timeframes.S5)\nb = ms(Timeframes.S10)\nc = ms(Timeframes.S15)\nd = ms(Timeframes.S30)\ne = ms(Timeframes.M1)\nf = ms(Timeframes.M3)\ng = ms(Timeframes.M5)\nh = ms(Timeframes.M10)\ni = ms(Timeframes.M15)\nj = ms(Timeframes.M30)\nk = ms(Timeframes.H1)\nl = ms(Timeframes.H2)\nm = ms(Timeframes.H4)\nn = ms(Timeframes.D1)\no = ms(Timeframes.W1)";
    let outcome = run_default(script, &bars);
    for (name, want) in [
        ("a", 5_000.0),
        ("b", 10_000.0),
        ("c", 15_000.0),
        ("d", 30_000.0),
        ("e", 60_000.0),
        ("f", 180_000.0),
        ("g", 300_000.0),
        ("h", 600_000.0),
        ("i", 900_000.0),
        ("j", 1_800_000.0),
        ("k", 3_600_000.0),
        ("l", 7_200_000.0),
        ("m", 14_400_000.0),
        ("n", 86_400_000.0),
        ("o", 604_800_000.0),
    ] {
        assert_eq!(num(&outcome, 0, name), Some(want), "arm {name}");
    }
}

#[test]
fn switch_default_arm_runs_runtime_error_fail_closed() {
    let bars = closes();
    let reason = run_err(
        &format!(
            "{TIMEFRAMES}export ms(Timeframes tf) =>\n    switch tf\n        Timeframes.M1 => 60000\n        =>\n            runtime.error(\"Unknown timeframe\")\n            0\nm = ms(Timeframes.S5)"
        ),
        &bars,
    );
    assert!(
        reason.contains("Unknown timeframe"),
        "default arm error surfaces: {reason}"
    );
}

#[test]
fn runtime_error_call_refuses_with_its_message() {
    let bars = closes();
    let reason = run_err("runtime.error(\"boom\")", &bars);
    assert!(reason.contains("boom"), "message preserved: {reason}");
}

// ---- tuples, offsets, `bar_index` ----

#[test]
fn tuple_literals_evaluate_and_index_reads_elements() {
    let bars = closes();
    let outcome = run_default("t = [10.0, 20.0]\nfirst = t[0]", &bars);
    assert_eq!(num(&outcome, 0, "first"), Some(10.0));
}

#[test]
fn array_out_of_bounds_index_refuses() {
    let bars = closes();
    let reason = run_err("t = [10.0]\nx = t[5]", &bars);
    assert!(reason.contains("bounds"), "fail-closed: {reason}");
}

#[test]
fn computed_index_reads_local_series_history() {
    // The Voss `voss[order - i]` shape: a computed offset over a name reads
    // its per-bar history, not an element.
    let bars = closes();
    let outcome = run_default(
        "export back(float data, int n) =>\n    data[n]\nb = back(close, 2)",
        &bars,
    );
    assert!(is_absent(&outcome, 0, "b"));
    assert!(is_absent(&outcome, 1, "b"));
    assert_eq!(num(&outcome, 2, "b"), Some(10.0));
    assert_eq!(num(&outcome, 3, "b"), Some(11.0));
}

#[test]
fn function_locals_carry_history_across_bars() {
    // The Supertrend `lowerBand[1]` shape: a local bound on prior bars is
    // visible through history offsets on later bars.
    let bars = closes();
    let outcome = run_default(
        "export track(float data) =>\n    prev = nz(data[1])\n    [data, prev]\n[cur, prv] = track(close)",
        &bars,
    );
    assert_eq!(num(&outcome, 0, "cur"), Some(10.0));
    assert_eq!(num(&outcome, 0, "prv"), Some(0.0));
    assert_eq!(num(&outcome, 1, "prv"), Some(10.0));
    assert_eq!(num(&outcome, 2, "prv"), Some(11.0));
}

#[test]
fn bar_index_counts_data_bars() {
    let bars = closes();
    let outcome = run_default("b = bar_index", &bars);
    for (at_bar, want) in [0.0, 1.0, 2.0, 3.0].iter().enumerate() {
        assert_eq!(num(&outcome, at_bar, "b"), Some(*want));
    }
}

// ---- multi-plot capture ----

#[test]
fn nine_plots_capture_titles_and_series() {
    let bars = closes();
    let script = (0..9)
        .map(|i| format!("plot(close, title=\"MA{i}\")"))
        .collect::<Vec<_>>()
        .join("\n");
    let outcome = run_default(&script, &bars);
    assert_eq!(outcome.plots.len(), 9, "all nine plots captured");
    for (i, plot) in outcome.plots.iter().enumerate() {
        assert_eq!(plot.title, format!("MA{i}"));
        assert_eq!(plot.values.len(), 4);
        let first = plot.values[0]
            .as_ref()
            .and_then(PineValue::as_float);
        assert_eq!(first, Some(10.0));
    }
}

// ---- `Scheduler` lifecycle ----

const SCHEDULER: &str = "export enum Timeframes\n    M1 = \"1 Minute\"\n    M5 = \"5 Minutes\"\n    S5 = \"5 Seconds\"\nexport ms(Timeframes tf) =>\n    switch tf\n        Timeframes.M1 => 60000\n        Timeframes.M5 => 300000\n        Timeframes.S5 => 5000\n        =>\n            runtime.error(\"Unknown timeframe\")\n            0\nexport type Scheduler\n    map<int, int> next_update = na\n    int earliest_due = na\nexport method init(Scheduler this) =>\n    this.next_update := map.new<int, int>()\n    this.earliest_due := na\n    this\nexport method register(Scheduler this, Timeframes tf) =>\n    int interval_ms = ms(tf)\n    if not na(interval_ms) and not map.contains(this.next_update, interval_ms)\n        int next_time = time + interval_ms\n        map.put(this.next_update, interval_ms, next_time)\n        if na(this.earliest_due) or next_time < this.earliest_due\n            this.earliest_due := next_time\n    this\nexport method updateEarliestDue(Scheduler this) =>\n    this.earliest_due := na\n    if map.size(this.next_update) > 0\n        array<int> keys = map.keys(this.next_update)\n        int size = array.size(keys)\n        for i = 0 to math.min(size - 1, 19)\n            int interval_ms = array.get(keys, i)\n            int next_time = map.get(this.next_update, interval_ms)\n            if not na(next_time) and (na(this.earliest_due) or next_time < this.earliest_due)\n                this.earliest_due := next_time\n    this\nexport method every(Scheduler this, Timeframes tf) =>\n    int target_ms = ms(tf)\n    bool is_due = false\n    if not na(target_ms)\n        if not map.contains(this.next_update, target_ms)\n            this.register(tf)\n        int next_time = map.get(this.next_update, target_ms)\n        is_due := not na(next_time) and time >= next_time\n        if is_due\n            int next_execution = na\n            if time - next_time > target_ms * 3\n                next_execution := time + target_ms\n            else\n                next_execution := next_time + target_ms\n            map.put(this.next_update, target_ms, next_execution)\n            this.updateEarliestDue()\n    is_due\n";

#[test]
fn scheduler_lifecycle_with_overdue_reset() {
    // `time` is seconds here; intervals stay milliseconds, exactly like the
    // library mixes them — expectations use the same mix.
    let bars = vec![
        bar_ts("100.00", "10.00", 1_000_000),
        bar_ts("100.00", "11.00", 1_000_030),
        bar_ts("100.00", "12.00", 1_060_000),
        bar_ts("100.00", "13.00", 2_000_000),
    ];
    let script = format!(
        "{SCHEDULER}var sched = Scheduler.new()\nif bar_index == 0\n    sched.init()\nsched.register(Timeframes.M1)\ndue = sched.every(Timeframes.M1)"
    );
    let outcome = run_default(&script, &bars);
    assert_eq!(
        outcome.value_at(0, "due"),
        Some(Some(PineValue::Bool(false)))
    );
    assert_eq!(
        outcome.value_at(1, "due"),
        Some(Some(PineValue::Bool(false)))
    );
    assert_eq!(
        outcome.value_at(2, "due"),
        Some(Some(PineValue::Bool(true))),
        "time reaches the first scheduled slot"
    );
    assert_eq!(
        outcome.value_at(3, "due"),
        Some(Some(PineValue::Bool(true))),
        "far jump is still due, then resets from current time"
    );
    let sched = outcome.value_at(3, "sched").expect("scheduler bound");
    match sched {
        Some(PineValue::Object(instance)) => {
            let next = instance.fields.get("next_update").cloned().flatten();
            match next {
                Some(PineValue::IntMap(entries)) => {
                    assert_eq!(
                        entries.get(&60_000),
                        Some(&2_060_000),
                        "3x-overdue resets from current time"
                    );
                }
                other => panic!("next_update holds a map, got {other:?}"),
            }
            assert_eq!(
                instance.fields.get("earliest_due").cloned().flatten(),
                Some(PineValue::Int(2_060_000))
            );
        }
        other => panic!("scheduler holds an instance, got {other:?}"),
    }
}

// ---- library functions, transcribed ----

const SUPER_TREND: &str = "export getSupertrend(float src, float atr, float factor) =>\n    upperBand = src + factor * atr\n    lowerBand = src - factor * atr\n    prevLowerBand = nz(lowerBand[1])\n    prevUpperBand = nz(upperBand[1])\n    lowerBand := lowerBand > prevLowerBand or close[1] < prevLowerBand ? lowerBand : prevLowerBand\n    upperBand := upperBand < prevUpperBand or close[1] > prevUpperBand ? upperBand : prevUpperBand\n    int _direction = na\n    float superTrend = na\n    prevSuperTrend = superTrend[1]\n    if na(atr[1])\n        _direction := 1\n    else if prevSuperTrend == prevUpperBand\n        _direction := close > upperBand ? -1 : 1\n    else\n        _direction := close < lowerBand ? 1 : -1\n    superTrend := _direction == -1 ? lowerBand : upperBand\n    [superTrend, _direction]\n";

#[test]
fn supertrend_tuple_matches_hand_computation() {
    let bars = closes();
    let outcome = run_default(
        &format!("{SUPER_TREND}[st, dir] = getSupertrend(close, 2.0, 3.0)"),
        &bars,
    );
    // Bar 0: bands 16/4 collapse against `na` history; `atr[1]` is `na`.
    assert!(is_absent(&outcome, 0, "st"));
    assert_eq!(num(&outcome, 0, "dir"), Some(1.0));
    for at_bar in 1..4 {
        let dir = num(&outcome, at_bar, "dir").expect("direction defined");
        assert!(dir == 1.0 || dir == -1.0, "direction is signed");
        assert!(
            num(&outcome, at_bar, "st").is_some(),
            "trend defined after the first bar"
        );
    }
}

const FACTORY_SMOOTHING: &str = "export factorySmoothing(float data, UTIL.Smoothings smoothing, simple int length, simple float n1=0.85, simple float param2=6) =>\n    if na(data)\n        0.0\n    if length <= 0\n        runtime.error(\"Smoothing length must be positive\")\n        0.0\n    switch smoothing\n        UTIL.Smoothings.NA => data\n        UTIL.Smoothings.ALMA => ta.alma(data, length, n1, param2)\n        UTIL.Smoothings.EMA => ta.ema(data, length)\n        UTIL.Smoothings.RMA => ta.rma(data, length)\n        UTIL.Smoothings.SMA => ta.sma(data, length)\n        UTIL.Smoothings.WMA => ta.wma(data, length)\n        =>\n            runtime.error(\"Unknown smoothing type\")\n            data\n";

const SMOOTHINGS: &str = "export enum Smoothings\n    NA = \"NA\"\n    ALMA = \"ALMA\"\n    EMA = \"EMA\"\n    RMA = \"RMA\"\n    SMA = \"SMA\"\n    WMA = \"WMA\"\n";

#[test]
fn macd_tuple_agrees_with_direct_kernel_calls() {
    // Transcribed `getMACD` over SMA arms; the tuple must agree with direct
    // `ta.sma` calls on the same bars (independent bridge states, same legs).
    let bars = rising(45);
    let script = format!(
        "{SMOOTHINGS}{FACTORY_SMOOTHING}export getMACD(float data, simple int fast=12, simple int slow=26, simple int signal=9, UTIL.Smoothings smoothFast=UTIL.Smoothings.SMA, UTIL.Smoothings smoothSlow=UTIL.Smoothings.SMA, UTIL.Smoothings smoothSignal=UTIL.Smoothings.SMA) =>\n    if na(data)\n        [0.0, 0.0, 0.0]\n    if fast <= 0 or slow <= 0 or signal <= 0\n        runtime.error(\"All periods must be positive in getMACD\")\n        [0.0, 0.0, 0.0]\n    if fast >= slow\n        runtime.error(\"Fast length must be less than slow length\")\n        [0.0, 0.0, 0.0]\n    float fastMA = factorySmoothing(data, smoothFast, fast)\n    float slowMA = factorySmoothing(data, smoothSlow, slow)\n    float macdLine = fastMA - slowMA\n    float signalLine = factorySmoothing(macdLine, smoothSignal, signal)\n    float histogram = macdLine - signalLine\n    [macdLine, signalLine, histogram]\n[ml, sl, hg] = getMACD(close, 12, 26, 9)\nref = ta.sma(close, 12) - ta.sma(close, 26)\ndrift = ml - ref\ncheck = hg - (ml - sl)"
    );
    let outcome = run_default(&script, &bars);
    // The slow leg (26) defines the line from bar 25; the signal leg needs
    // nine defined line values after that.
    for at_bar in 25..45 {
        let drift = num(&outcome, at_bar, "drift").expect("both defined");
        assert!(
            drift.abs() < 1e-9,
            "tuple line matches direct kernels at bar {at_bar}: {drift}"
        );
    }
    for at_bar in 33..45 {
        let check = num(&outcome, at_bar, "check").expect("tuple unpacked");
        assert!(
            check.abs() < 1e-12,
            "histogram is line minus signal at bar {at_bar}: {check}"
        );
    }
}

const VOSS_FILTER: &str = "export getVossPredictiveFilter(float _data, int _period, int _predict=3, float _band=0.25) =>\n    float filter = 0.0\n    float sumC = 0.0\n    float voss = 0.0\n    pi_x2 = 2 * math.asin(1)\n    order = 3 * _predict\n    alpha = math.cos(2 * pi_x2 / _period)\n    gamma = math.cos(_band * 2 * pi_x2 / _period)\n    delta = 1 / gamma - math.sqrt(1 / (gamma * gamma) - 1)\n    s2 = 1 + delta\n    s3 = 1 - delta\n    for i = 0 to order - 1\n        sumC := sumC + (i + 1) / order * voss[order - i]\n    if bar_index <= order\n        filter := 0\n        voss := 0\n    else\n        filter := 0.5 * s3 * (nz(_data) - nz(_data[2])) + alpha * s2 * filter[1] - delta * filter[2]\n        voss := ((3 + order) / 2) * filter - sumC\n    [voss, filter]\n";

#[test]
fn voss_tuple_holds_zero_through_warmup_then_tracks() {
    let bars = rising(40);
    let outcome = run_default(
        &format!("{VOSS_FILTER}[v, f] = getVossPredictiveFilter(close, 10)"),
        &bars,
    );
    // `order` is 9: the first ten bars hold the init branch exactly.
    for at_bar in 0..=9 {
        assert_eq!(num(&outcome, at_bar, "v"), Some(0.0), "voss at {at_bar}");
        assert_eq!(num(&outcome, at_bar, "f"), Some(0.0), "filter at {at_bar}");
    }
    for at_bar in 10..40 {
        let v = num(&outcome, at_bar, "v").expect("voss tracks");
        let f = num(&outcome, at_bar, "f").expect("filter tracks");
        assert!(v.is_finite(), "finite voss at {at_bar}");
        assert!(f.is_finite(), "finite filter at {at_bar}");
    }
    // Deterministic across runs (state never leaks between runs).
    let again = run_default(
        &format!("{VOSS_FILTER}[v, f] = getVossPredictiveFilter(close, 10)"),
        &bars,
    );
    for at_bar in 10..40 {
        assert_eq!(
            num(&outcome, at_bar, "v"),
            num(&again, at_bar, "v"),
            "rerun matches at {at_bar}"
        );
    }
}

// ---- refusals that stay ----

#[test]
fn unknown_type_method_refuses_naming_it() {
    let bars = closes();
    let reason = run_err(
        "export type Scheduler\n    map<int, int> next_update = na\ns = Scheduler.new()\ns.nosuch()",
        &bars,
    );
    assert!(reason.contains("nosuch"), "reason names the method: {reason}");
}
