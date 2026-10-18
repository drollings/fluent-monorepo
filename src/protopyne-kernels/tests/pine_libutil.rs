//! Real utility-library goldens: the shipped `libUtil` source linked with
//! small driver scripts, pinning each helper's hand-computed output.
//!
//! Drivers call through the `UTIL` alias exactly like the real dependents
//! do, so cross-module enum arguments, method receivers, and type
//! construction run the same path the indicator libraries use. The scripted
//! clock runs in the engine's `time` unit (whole seconds) against the
//! library's millisecond intervals; pure helpers read the first bar only.
//!
//! Float goldens below use hand-computed decimal expectations with a tight
//! tolerance, never measured quantities. Exact integer comparisons (epoch
//! millis, millisecond intervals) are exact by construction.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::{InputSet, LinkedModule, ModuleInput, PineRuntimeConfig};
use chrono::TimeZone as _;
use protopyne_types::Bar;

use crate::pine::test_fixture as fixture;

/// The shipped utility library as a graph input (shared fixture constructor).
fn util_module() -> ModuleInput {
    fixture::shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12)
}

/// One synthetic driver script as a graph input (shared fixture constructor).
fn driver_module(text: &str) -> ModuleInput {
    fixture::driver_module(text)
}

/// Link the real library under its graph order with one driver root calling
/// through the `UTIL` alias.
fn linked(driver: &str) -> Vec<LinkedModule> {
    fixture::linked_modules(vec![util_module(), driver_module(driver)])
}

fn bar_ms(close: &str, ts_millis: i64) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .timestamp_opt(ts_millis, 0)
            .single()
            .expect("valid test timestamp"))
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn run_linked(driver: &str, bars: &[Bar], chart_period: &str) -> RunOutcome {
    let modules = linked(driver);
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period(chart_period.to_owned())
            .build(),
    );
    engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("linked run evaluates")
}

fn two_bars() -> Vec<Bar> {
    vec![
        bar_ms("10.00", 1_700_000_000_000),
        bar_ms("11.00", 1_700_000_060_000),
    ]
}

fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> f64 {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Float(value))) => value,
        Some(Some(PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn text(outcome: &RunOutcome, at_bar: usize, name: &str) -> String {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Str(value))) => value,
        other => panic!("expected string for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn flag(outcome: &RunOutcome, at_bar: usize, name: &str) -> bool {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Bool(value))) => value,
        other => panic!("expected bool for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn member(outcome: &RunOutcome, at_bar: usize, name: &str) -> (String, String) {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::EnumHandle(handle))) => (handle.enum_name, handle.member),
        other => panic!("expected enum member for {name} on bar {at_bar}, got {other:?}"),
    }
}

const IMPORT: &str = "import showmethegrail/libUtil/12 as UTIL\n";

#[test]
fn timeframe_abbreviations_pin_the_string_table() {
    let driver = format!(
        "{IMPORT}a_m1 = UTIL.getTimeframeAbbrev(UTIL.Timeframes.M1)\n\
         a_h1 = UTIL.getTimeframeAbbrev(UTIL.Timeframes.H1)\n\
         a_d = UTIL.getTimeframeAbbrev(UTIL.Timeframes.D1)\n\
         a_w = UTIL.getTimeframeAbbrev(UTIL.Timeframes.W1)\n\
         a_mo = UTIL.getTimeframeAbbrev(UTIL.Timeframes.MO1)\n\
         a_y = UTIL.getTimeframeAbbrev(UTIL.Timeframes.Y)\n\
         a_s5 = UTIL.getTimeframeAbbrev(UTIL.Timeframes.S5)\n\
         a_na = UTIL.getTimeframeAbbrev(UTIL.Timeframes.NA)\n\
         a_c = UTIL.getTimeframeAbbrev(UTIL.Timeframes.C)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    for (name, want) in [
        ("a_m1", "1"),
        ("a_h1", "60"),
        ("a_d", "D"),
        ("a_w", "W"),
        ("a_mo", "1M"),
        ("a_y", "12M"),
        ("a_s5", "5S"),
    ] {
        assert_eq!(text(&outcome, 0, name), want, "{name}");
    }
    // Chart-relative members read the configured period, not a constant.
    assert_eq!(text(&outcome, 0, "a_na"), "5");
    assert_eq!(text(&outcome, 0, "a_c"), "5");
}

#[test]
fn timeframe_milliseconds_pin_armed_values_and_chart_fallback() {
    let driver = format!(
        "{IMPORT}m_m1 = UTIL.getTimeframeMS(UTIL.Timeframes.M1)\n\
         m_m5 = UTIL.getTimeframeMS(UTIL.Timeframes.M5)\n\
         m_h4 = UTIL.getTimeframeMS(UTIL.Timeframes.H4)\n\
         m_w1 = UTIL.getTimeframeMS(UTIL.Timeframes.W1)\n\
         m_s10 = UTIL.getTimeframeMS(UTIL.Timeframes.S10)\n\
         m_mo1 = UTIL.getTimeframeMS(UTIL.Timeframes.MO1)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    for (name, want) in [
        ("m_m1", 60_000.0),
        ("m_m5", 300_000.0),
        ("m_h4", 14_400_000.0),
        ("m_w1", 604_800_000.0),
        ("m_s10", 10_000.0),
    ] {
        assert_eq!(num(&outcome, 0, name), want, "{name}");
    }
    // Months have no armed row, so the chart-period fallback answers: five
    // chart minutes in milliseconds.
    assert_eq!(num(&outcome, 0, "m_mo1"), 300_000.0);
}

#[test]
fn timeframe_from_milliseconds_round_trips_and_falls_back() {
    let driver = format!(
        "{IMPORT}f_m1 = UTIL.getTimeframeFromMS(60000)\n\
         f_h1 = UTIL.getTimeframeFromMS(3600000)\n\
         f_d = UTIL.getTimeframeFromMS(86400000)\n\
         f_w = UTIL.getTimeframeFromMS(604800000)\n\
         f_gap = UTIL.getTimeframeFromMS(12345)\n\
         f_s10 = UTIL.getTimeframeFromMS(10000)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    for (name, want) in [("f_m1", "M1"), ("f_h1", "H1"), ("f_d", "D1"), ("f_w", "W1")] {
        assert_eq!(member(&outcome, 0, name).1, want, "{name}");
    }
    // Unknown intervals read `NA`, including the ten-second row the reverse
    // table never armed.
    assert_eq!(member(&outcome, 0, "f_gap").1, "NA");
    assert_eq!(member(&outcome, 0, "f_s10").1, "NA");
}

#[test]
fn timeframe_from_multiplier_pins_exact_and_ceiling_steps() {
    let driver = format!(
        "{IMPORT}g_exact = UTIL.getTimeframeFromMultiplier(UTIL.Timeframes.M1, 5.0)\n\
         g_ceil = UTIL.getTimeframeFromMultiplier(UTIL.Timeframes.M1, 2.0)\n\
         g_hour = UTIL.getTimeframeFromMultiplier(UTIL.Timeframes.M5, 12.0)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    // 60_000 * 5 lands exactly on the five-minute row.
    assert_eq!(member(&outcome, 0, "g_exact").1, "M5");
    // 60_000 * 2 lands between rows, so the leftmost ceiling (five minutes)
    // answers, not the nearest floor.
    assert_eq!(member(&outcome, 0, "g_ceil").1, "M5");
    // 300_000 * 12 lands exactly on the hourly row.
    assert_eq!(member(&outcome, 0, "g_hour").1, "H1");
}

#[test]
fn timeframe_strings_pin_first_match_shadowing() {
    let driver = format!(
        "{IMPORT}t_min = UTIL.getTimeframe(\"5\")\n\
         t_hour = UTIL.getTimeframe(\"60\")\n\
         t_day = UTIL.getTimeframe(\"D\")\n\
         t_week = UTIL.getTimeframe(\"W\")\n\
         t_shadowed = UTIL.getTimeframe(\"1M\")\n\
         t_month = UTIL.getTimeframe(\"M1\")\n\
         t_m3 = UTIL.getTimeframe(\"3M\")\n\
         t_year = UTIL.getTimeframe(\"12M\")\n\
         t_na = UTIL.getTimeframe(\"nope\")\n\
         t_default = UTIL.getTimeframe()"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    for (name, want) in [
        ("t_min", "M5"),
        ("t_hour", "H1"),
        ("t_day", "D1"),
        ("t_week", "W1"),
        ("t_month", "MO1"),
        ("t_m3", "M3"),
        ("t_year", "Y"),
    ] {
        assert_eq!(member(&outcome, 0, name).1, want, "{name}");
    }
    // The minute `"1M"` arm precedes the month `"1M"` arm, so first match
    // wins and minutes shadow months on that spelling.
    assert_eq!(member(&outcome, 0, "t_shadowed").1, "M1");
    assert_eq!(member(&outcome, 0, "t_na").1, "NA");
    // No argument reads the chart period.
    assert_eq!(member(&outcome, 0, "t_default").1, "M5");
}

#[test]
fn bars_in_timeframes_and_chart_match_pin_ratios() {
    let driver = format!(
        "{IMPORT}b_up = UTIL.getBarsInTimeframes(UTIL.Timeframes.M1, UTIL.Timeframes.M5)\n\
         b_down = UTIL.getBarsInTimeframes(UTIL.Timeframes.M5, UTIL.Timeframes.M1)\n\
         b_same = UTIL.getBarsInTimeframes(UTIL.Timeframes.M1, UTIL.Timeframes.M1)\n\
         c_hit = UTIL.isChartTimeframe(UTIL.Timeframes.M5)\n\
         c_miss = UTIL.isChartTimeframe(UTIL.Timeframes.M1)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    assert_eq!(num(&outcome, 0, "b_up"), 5.0, "five ones fit a five");
    assert_eq!(
        num(&outcome, 0, "b_down"),
        1.0,
        "inverted ratios floor to one"
    );
    assert_eq!(num(&outcome, 0, "b_same"), 1.0, "equal frames read one");
    assert!(flag(&outcome, 0, "c_hit"), "five-minute chart matches");
    assert!(!flag(&outcome, 0, "c_miss"), "one-minute target misses");
}

#[test]
fn regime_emoji_pins_each_regime() {
    let driver = format!(
        "{IMPORT}r_range = UTIL.getRegimeEmoji(UTIL.MarketRegimes.RANGING)\n\
         r_bull = UTIL.getRegimeEmoji(UTIL.MarketRegimes.TRENDING_BULL)\n\
         r_bear = UTIL.getRegimeEmoji(UTIL.MarketRegimes.TRENDING_BEAR)\n\
         r_vol = UTIL.getRegimeEmoji(UTIL.MarketRegimes.VOLATILE)\n\
         r_low = UTIL.getRegimeEmoji(UTIL.MarketRegimes.LOW_VOLUME)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    for (name, want) in [
        ("r_range", "⚖"),
        ("r_bull", "🐂"),
        ("r_bear", "🐻"),
        ("r_vol", "⚡"),
        ("r_low", "💤"),
    ] {
        assert_eq!(text(&outcome, 0, name), want, "{name}");
    }
}

#[test]
fn size_line_position_tables_stay_opaque_and_pane_aware() {
    let driver = format!(
        "{IMPORT}s_tiny = UTIL.getSize(UTIL.Sizes.TINY)\n\
         s_huge = UTIL.getSize(UTIL.Sizes.HUGE)\n\
         l_solid = UTIL.getLineStyle(UTIL.LineStyles.SOLID)\n\
         l_dashed = UTIL.getLineStyle(UTIL.LineStyles.DASHED)\n\
         p_corner = UTIL.getPosition(UTIL.Positions.TOP_RIGHT)\n\
         p_pane = UTIL.getPosition(UTIL.Positions.BOTTOM_LEFT_PANE)\n\
         o_chart = UTIL.isPositionOverlay(UTIL.Positions.TOP_RIGHT)\n\
         o_pane = UTIL.isPositionOverlay(UTIL.Positions.TOP_LEFT_PANE)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    // Display handles compare by namespace and member, never by number.
    for (name, namespace, want) in [
        ("s_tiny", "size", "tiny"),
        ("s_huge", "size", "huge"),
        ("l_solid", "line", "style_solid"),
        ("l_dashed", "line", "style_dashed"),
        ("p_corner", "position", "top_right"),
    ] {
        assert_eq!(
            member(&outcome, 0, name),
            (namespace.to_owned(), want.to_owned()),
            "{name}"
        );
    }
    // Pane positions collapse onto their chart corner.
    assert_eq!(
        member(&outcome, 0, "p_pane"),
        ("position".to_owned(), "bottom_left".to_owned())
    );
    assert!(flag(&outcome, 0, "o_chart"), "chart corner overlays");
    assert!(!flag(&outcome, 0, "o_pane"), "pane slot does not overlay");
}

#[test]
fn scalar_helpers_pin_edge_tables() {
    let driver = format!(
        "{IMPORT}th_zero = UTIL.tanh(0.0)\n\
         th_one = UTIL.tanh(1.0)\n\
         ft_one = UTIL.fast_tanh(1.0)\n\
         ft_zero = UTIL.fast_tanh(0.0)\n\
         ft_neg = UTIL.fast_tanh(-2.0)\n\
         sd_half = UTIL.safeDivide(1.0, 2.0)\n\
         sd_zero = UTIL.safeDivide(1.0, 0.0)\n\
         sd_custom = UTIL.safeDivide(1.0, 0.0, -1.0)\n\
         sd_na = UTIL.safeDivide(na, 2.0)\n\
         ss_pos = UTIL.sameSign(1.0, 2.0)\n\
         ss_neg = UTIL.sameSign(-1.0, -2.0)\n\
         ss_mix = UTIL.sameSign(1.0, -1.0)\n\
         ss_zero = UTIL.sameSign(0.0, 5.0)\n\
         ss_na = UTIL.sameSign(na, 1.0)\n\
         sa_hit = UTIL.safeArrayAccess(array.from(10.0, 20.0, 30.0), 1)\n\
         sa_past = UTIL.safeArrayAccess(array.from(10.0, 20.0, 30.0), 5)\n\
         sa_neg = UTIL.safeArrayAccess(array.from(10.0, 20.0, 30.0), -1)\n\
         sa_custom = UTIL.safeArrayAccess(array.from(10.0, 20.0, 30.0), 5, -7.0)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    assert_eq!(num(&outcome, 0, "th_zero"), 0.0);
    let drift = (num(&outcome, 0, "th_one") - 1.0_f64.tanh()).abs();
    assert!(drift < 1e-12, "series form tracks tanh within {drift}");
    assert_eq!(num(&outcome, 0, "ft_one"), 0.5);
    assert_eq!(num(&outcome, 0, "ft_zero"), 0.0);
    assert!((num(&outcome, 0, "ft_neg") - (-2.0 / 3.0)).abs() < 1e-12);
    assert_eq!(num(&outcome, 0, "sd_half"), 0.5);
    assert_eq!(num(&outcome, 0, "sd_zero"), 0.0);
    assert_eq!(num(&outcome, 0, "sd_custom"), -1.0);
    assert_eq!(num(&outcome, 0, "sd_na"), 0.0);
    assert!(flag(&outcome, 0, "ss_pos"));
    assert!(flag(&outcome, 0, "ss_neg"));
    assert!(!flag(&outcome, 0, "ss_mix"));
    assert!(flag(&outcome, 0, "ss_zero"), "zero counts as non-negative");
    assert!(!flag(&outcome, 0, "ss_na"), "absence never shares a sign");
    assert_eq!(num(&outcome, 0, "sa_hit"), 20.0);
    assert_eq!(num(&outcome, 0, "sa_past"), 0.0);
    assert_eq!(num(&outcome, 0, "sa_neg"), 0.0);
    assert_eq!(num(&outcome, 0, "sa_custom"), -7.0);
}

#[test]
fn meter_characters_pin_floor_steps_and_fallback() {
    let driver = format!(
        "{IMPORT}m_zero = UTIL.risingMeterChar(0.0)\n\
         m_one = UTIL.risingMeterChar(0.125)\n\
         m_two = UTIL.risingMeterChar(0.25)\n\
         m_four = UTIL.risingMeterChar(0.5)\n\
         m_full = UTIL.risingMeterChar(1.0)\n\
         m_over = UTIL.risingMeterChar(1.5)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    for (name, want) in [
        ("m_zero", " "),
        ("m_one", "_"),
        ("m_two", "▁"),
        ("m_four", "▃"),
        ("m_full", "█"),
        ("m_over", " "),
    ] {
        assert_eq!(text(&outcome, 0, name), want, "{name}");
    }
}

#[test]
fn ohlcv_validation_and_delta_volume_pin_hand_computation() {
    let driver = format!(
        "{IMPORT}v_ok = UTIL.validateOHLCV(100.0, 101.0, 99.0, 100.5, 1000.0)\n\
         v_flat_vol = UTIL.validateOHLCV(100.0, 101.0, 99.0, 100.5, 0.0)\n\
         v_no_range = UTIL.validateOHLCV(100.0, 100.0, 100.0, 100.0, 1000.0)\n\
         v_na = UTIL.validateOHLCV(na, 101.0, 99.0, 100.5, 1000.0)\n\
         d_mid = UTIL.estimateDeltaVolume(100.0, 110.0, 90.0, 105.0, 1000.0)\n\
         d_doji = UTIL.estimateDeltaVolume(100.0, 110.0, 90.0, 100.0, 1000.0)\n\
         d_high = UTIL.estimateDeltaVolume(90.0, 110.0, 90.0, 110.0, 1000.0)\n\
         d_low = UTIL.estimateDeltaVolume(110.0, 110.0, 90.0, 90.0, 1000.0)\n\
         d_bad = UTIL.estimateDeltaVolume(100.0, 101.0, 99.0, 100.5, 0.0)"
    );
    let outcome = run_linked(&driver, &two_bars(), "5");
    assert!(flag(&outcome, 0, "v_ok"));
    assert!(!flag(&outcome, 0, "v_flat_vol"), "empty volume invalidates");
    assert!(!flag(&outcome, 0, "v_no_range"), "zero range invalidates");
    assert!(!flag(&outcome, 0, "v_na"), "absence invalidates");
    // Mid-body close: 0.5 + 0.125 directional + 0.075 close bias = 0.70.
    assert!((num(&outcome, 0, "d_mid") - 700.0).abs() < 1e-9);
    // Doji at mid-range carries no directional edge: exactly half.
    assert!((num(&outcome, 0, "d_doji") - 500.0).abs() < 1e-9);
    // Full-range closes saturate the [0.02, 0.98] clamp on both sides.
    assert!((num(&outcome, 0, "d_high") - 980.0).abs() < 1e-9);
    assert!((num(&outcome, 0, "d_low") - 20.0).abs() < 1e-9);
    // Invalid candles fall back to half the (here empty) volume.
    assert!((num(&outcome, 0, "d_bad") - 0.0).abs() < 1e-9);
}

#[test]
fn scheduler_timing_pins_due_reschedule_and_catch_up() {
    // Scripted clock in the engine's `time` unit (whole seconds) against the
    // library's millisecond intervals — the same mix the engine's other
    // scheduler coverage uses. One-minute bars, then a late bar, then a jump
    // past three intervals. Registering two frames also pins the
    // earliest-due scan across entries.
    let base = 1_700_000_000_i64;
    let bars = vec![
        bar_ms("10.00", base),
        bar_ms("11.00", base + 60_000),
        bar_ms("12.00", base + 150_000),
        bar_ms("13.00", base + 480_000),
    ];
    let driver = format!(
        "{IMPORT}var sched = UTIL.Scheduler.new()\n\
         if bar_index == 0\n    \
         sched.init()\n    \
         sched.register(UTIL.Timeframes.M1)\n    \
         sched.register(UTIL.Timeframes.M5)\n\
         due = sched.every(UTIL.Timeframes.M1)"
    );
    let outcome = run_linked(&driver, &bars, "5");
    assert!(!flag(&outcome, 0, "due"), "first bar only arms the frame");
    assert!(flag(&outcome, 1, "due"), "exact slot fires");
    assert!(flag(&outcome, 2, "due"), "late bar still fires");
    assert!(flag(&outcome, 3, "due"), "far jump still fires once");
    for (at_bar, want_next) in [(1, base + 120_000), (2, base + 180_000)] {
        let next = scheduled_at(&outcome, at_bar, 60_000);
        assert_eq!(
            next, want_next,
            "normal case extends the prior slot at {at_bar}"
        );
    }
    // The far jump exceeds three intervals, so the frame resets from the
    // current bar instead of extending the stale slot.
    assert_eq!(scheduled_at(&outcome, 3, 60_000), base + 540_000);
    // The untouched five-minute frame keeps its original slot, and the
    // earliest-due scan reports the minimum across both entries.
    assert_eq!(scheduled_at(&outcome, 3, 300_000), base + 300_000);
    assert_eq!(earliest_due(&outcome, 3), base + 300_000);
}

fn scheduled_at(outcome: &RunOutcome, at_bar: usize, interval_ms: i64) -> i64 {
    match outcome.value_at(at_bar, "sched") {
        Some(Some(PineValue::Object(instance))) => {
            match instance.fields.get("next_update").cloned().flatten() {
                Some(PineValue::IntMap(entries)) => entries
                    .get(&interval_ms)
                    .copied()
                    .unwrap_or_else(|| panic!("frame {interval_ms} scheduled at {at_bar}")),
                other => panic!("schedule holds a map at {at_bar}, got {other:?}"),
            }
        }
        other => panic!("scheduler holds an instance at {at_bar}, got {other:?}"),
    }
}

// Millis read back exactly below 2^53, whether the engine holds them as
// `int` slots or as whole `float` clock arithmetic.
fn millis(value: &PineValue, at_bar: usize) -> i64 {
    match value {
        PineValue::Int(v) => *v,
        PineValue::Float(v)
            if v.fract() == 0.0 && v.is_finite() && v.abs() < 9_007_199_254_740_992.0 =>
        {
            #[allow(clippy::cast_possible_truncation)]
            (*v as i64)
        }
        other => panic!("schedule holds millis at {at_bar}, got {other:?}"),
    }
}
fn earliest_due(outcome: &RunOutcome, at_bar: usize) -> i64 {
    match outcome.value_at(at_bar, "sched") {
        Some(Some(PineValue::Object(instance))) => {
            match instance.fields.get("earliest_due").cloned().flatten() {
                Some(value) => millis(&value, at_bar),
                None => panic!("earliest due holds millis at {at_bar}, got na"),
            }
        }
        other => panic!("scheduler holds an instance at {at_bar}, got {other:?}"),
    }
}
