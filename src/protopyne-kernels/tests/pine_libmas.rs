//! Real moving-average-library goldens: the shipped `libMAs` source linked
//! with small driver scripts, pinning each export against its kernel twin
//! or a hand-computed recurrence.
//!
//! Drivers call through the `MAS` alias (and pass `UTIL` enum members for
//! smoothing/oscillator arguments) exactly like the real dependents do, so
//! cross-module dispatch, switch matching on opaque enum members, and Pine
//! recurrences (`getEIT`/`getKalman`/`getVossPredictiveFilter`) run the same
//! path the indicator uses. The kernel batch is the oracle wherever a
//! native twin exists; hand-computed bars pin the seeding and update order
//! of the recurrences where no twin runs the same code.
//!
//! Float goldens below use hand-computed decimal expectations with a tight
//! tolerance, never measured quantities.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::{InputSet, LinkedModule, ModuleInput, PineRuntimeConfig};
use crate::indicator::{OwnedOhlcv, ParamSet};
use crate::parity::assert_expr_parity;
use chrono::TimeZone as _;
use protopyne_types::Bar;

use crate::pine::test_fixture as fixture;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;

/// One shipped library as a graph input (shared fixture constructor).
fn shipped_module(file: &str, publisher: &str, name: &str, version: u64) -> ModuleInput {
    fixture::shipped_module(file, publisher, name, version)
}

/// One synthetic driver script as a graph input (shared fixture constructor).
fn driver_module(text: &str) -> ModuleInput {
    fixture::driver_module(text)
}

/// Link both shipped libraries under graph order with one driver root
/// calling through the `UTIL` and `MAS` aliases.
fn linked(driver: &str) -> Vec<LinkedModule> {
    fixture::linked_modules(vec![
        shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        shipped_module("libraries/_libMAs.pine", "showmethegrail", "libMAs", 95),
        driver_module(driver),
    ])
}

fn bar_full(open: &str, high: &str, low: &str, close: &str, volume: u64, ts_millis: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::Utc
                .timestamp_opt(ts_millis, 0)
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

/// Sixty rising bars with varied volume: exercises every smoothing arm and
/// every oscillator on trending data.
fn ramp_bars() -> Vec<Bar> {
    (0..60)
        .map(|i| {
            let close = 100.0 + i as f64;
            bar_full(
                &format!("{:.2}", close - 0.25),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000 + 10 * i as u64,
                1_700_000_000_000 + i * 60_000,
            )
        })
        .collect()
}

fn run_linked(driver: &str, bars: &[Bar]) -> RunOutcome {
    let modules = linked(driver);
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    );
    engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("linked run evaluates")
}

fn run_linked_result(driver: &str, bars: &[Bar]) -> Result<RunOutcome, crate::KernelError> {
    let modules = linked(driver);
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    );
    engine.run_linked(&modules, bars, InputSet::new())
}

/// One engine series as plain floats: `Int` prints widen, absence stays
/// absence, anything else is a test bug.
fn series_of(outcome: &RunOutcome, name: &str, bars: usize) -> Vec<Option<f64>> {
    (0..bars)
        .map(|bar| match outcome.value_at(bar, name) {
            Some(Some(PineValue::Float(value))) => Some(value),
            Some(Some(PineValue::Int(value))) => Some(value as f64),
            Some(None) => None,
            other => panic!("expected number or na for {name} on bar {bar}, got {other:?}"),
        })
        .collect()
}

fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> f64 {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Float(value))) => value,
        Some(Some(PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn color_raw(outcome: &RunOutcome, at_bar: usize, name: &str) -> u32 {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::ColorHandle(handle))) => handle.raw(),
        other => panic!("expected color for {name} on bar {at_bar}, got {other:?}"),
    }
}

/// The native kernel twin over the same bars, as the agreement oracle.
fn kernel_batch(kernel_id: &str, params: &ParamSet, ohlcv: &OwnedOhlcv) -> Vec<Option<f64>> {
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let kernel = registry.get(kernel_id).expect("kernel registered");
    kernel
        .batch(&ohlcv.views(), params)
        .expect("valid batch")
        .values
}

fn ramp_ohlcv(n: usize) -> OwnedOhlcv {
    let close: Vec<f64> = (0..n).map(|i| 100.0 + i as f64).collect();
    OwnedOhlcv {
        open: close.iter().map(|c| c - 0.25).collect(),
        high: close.iter().map(|c| c + 0.5).collect(),
        low: close.iter().map(|c| c - 0.5).collect(),
        close,
        volume: (0..n).map(|i| 1_000.0 + 10.0 * i as f64).collect(),
    }
}

const IMPORTS: &str = "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libMAs/95 as MAS\n";

fn smoothing_driver(member: &str) -> String {
    format!("{IMPORTS}out = MAS.factorySmoothing(close, UTIL.Smoothings.{member}, 14)")
}

#[test]
fn factory_smoothing_matches_kernels_within_eps() {
    let bars = ramp_bars();
    let ohlcv = ramp_ohlcv(60);
    // (Smoothings member, kernel twin, kernel params, expected-series builder).
    let params_14 = ParamSet::new().with_int("length", 14);
    let cases: &[(&str, &str, ParamSet)] = &[
        ("SMA", "sma", params_14.clone()),
        ("EMA", "ema", params_14.clone()),
        ("WMA", "wma", params_14.clone()),
        ("HMA", "hma", params_14.clone()),
        ("RMA", "rma", params_14.clone()),
        ("VWMA", "vwma", params_14.clone()),
        (
            "ALMA",
            "alma",
            ParamSet::new()
                .with_int("length", 14)
                .with_float("offset", 0.85)
                .with_float("sigma", 6.0),
        ),
        (
            "EIT",
            "eit",
            ParamSet::new().with_float("alpha", 0.85),
        ),
        (
            "KALMAN",
            "kalman",
            ParamSet::new().with_int("length", 14),
        ),
    ];
    for (member, kernel_id, params) in cases {
        let outcome = run_linked(&smoothing_driver(member), &bars);
        let got = series_of(&outcome, "out", bars.len());
        let expected = kernel_batch(kernel_id, params, &ohlcv);
        assert_expr_parity(&got, &expected, EPS, member).expect("smoothing matches kernel");
    }
    // The NA arm is the identity: engine output equals the raw closes.
    let outcome = run_linked(&smoothing_driver("NA"), &bars);
    let got = series_of(&outcome, "out", bars.len());
    let expected: Vec<Option<f64>> = ohlcv.close.iter().map(|c| Some(*c)).collect();
    assert_expr_parity(&got, &expected, EPS, "NA identity").expect("NA passes data through");
    // ZLEMA has no kernel twin: the oracle is the EMA kernel over the
    // Rust-built transformed series (`2 * data - data[6]`, absent early).
    let lag = 6_usize;
    let transformed: Vec<f64> = ohlcv
        .close
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if i >= lag {
                2.0 * c - ohlcv.close[i - lag]
            } else {
                f64::NAN
            }
        })
        .collect();
    let z_ohlcv = OwnedOhlcv {
        open: transformed.clone(),
        high: transformed.clone(),
        low: transformed.clone(),
        close: transformed,
        volume: ohlcv.volume.clone(),
    };
    let outcome = run_linked(&smoothing_driver("ZLEMA"), &bars);
    let got = series_of(&outcome, "out", bars.len());
    let expected = kernel_batch("ema", &params_14, &z_ohlcv);
    assert_expr_parity(&got, &expected, EPS, "ZLEMA").expect("ZLEMA matches EMA over lagged data");
}

#[test]
fn factory_smoothing_guards_refuse_or_default() {
    let bars = ramp_bars();
    // Absent data flows into the smoothing kernel and propagates as `na`:
    // the `if na(data)` guard yields a discarded `0.0` (a non-trailing
    // `if` returns nothing in the language), and the SMA arm honestly
    // smooths absence into absence.
    let outcome = run_linked(
        &format!("{IMPORTS}out = MAS.factorySmoothing(na, UTIL.Smoothings.SMA, 14)"),
        &bars,
    );
    assert!(
        series_of(&outcome, "out", 1)[0].is_none(),
        "na data smooths to na"
    );
    // A non-positive length aborts with the library's own reason.
    let err = run_linked_result(
        &format!("{IMPORTS}out = MAS.factorySmoothing(close, UTIL.Smoothings.SMA, 0)"),
        &bars,
    )
    .expect_err("zero length refuses");
    assert!(
        err.to_string().contains("Smoothing length must be positive"),
        "length guard names its reason, got {err}"
    );
    // A non-enum smoothing falls into the default arm, whose
    // `runtime.error` carries the unknown-type message.
    let err = run_linked_result(
        &format!("{IMPORTS}out = MAS.factorySmoothing(close, 5, 14)"),
        &bars,
    )
    .expect_err("unknown smoothing refuses");
    assert!(
        err.to_string().contains("Unknown smoothing type"),
        "default arm names its reason, got {err}"
    );
}

#[test]
fn get_eit_pins_hand_steps() {
    let bars: Vec<Bar> = [10.0, 11.0, 12.0, 13.0]
        .into_iter()
        .enumerate()
        .map(|(i, close)| {
            bar_full(
                &format!("{close:.2}"),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000,
                1_700_000_000_000 + i as i64 * 60_000,
            )
        })
        .collect();
    let outcome = run_linked(&format!("{IMPORTS}out = MAS.getEIT(close, 0.5)"), &bars);
    // Hand-computed from the recurrence with alpha = 0.5: seeding reads
    // absent lookbacks as the current source, and `eit[1]`/`eit[2]` read
    // prior outputs once defined.
    assert_eq!(num(&outcome, 0, "out"), 10.0, "bar 0 seeds from source");
    assert_eq!(num(&outcome, 1, "out"), 10.0, "bar 1 carries the seed");
    assert_eq!(num(&outcome, 2, "out"), 11.0, "bar 2 engages both lookbacks");
    assert_eq!(num(&outcome, 3, "out"), 12.25, "bar 3 runs the full recurrence");
}

#[test]
fn get_eit_and_kalman_match_kernel_batches() {
    let bars = ramp_bars();
    let ohlcv = ramp_ohlcv(60);
    let outcome = run_linked(&format!("{IMPORTS}out = MAS.getEIT(close, 0.85)"), &bars);
    let got = series_of(&outcome, "out", bars.len());
    let expected = kernel_batch("eit", &ParamSet::new().with_float("alpha", 0.85), &ohlcv);
    assert_expr_parity(&got, &expected, EPS, "getEIT").expect("library EIT matches kernel EIT");
    let outcome = run_linked(&format!("{IMPORTS}out = MAS.getKalman(close, 14)"), &bars);
    let got = series_of(&outcome, "out", bars.len());
    let expected = kernel_batch("kalman", &ParamSet::new().with_int("length", 14), &ohlcv);
    assert_expr_parity(&got, &expected, EPS, "getKalman")
        .expect("library Kalman matches kernel Kalman");
}

#[test]
fn get_kalman_pins_hand_steps() {
    let bars: Vec<Bar> = [10.0, 11.0]
        .into_iter()
        .enumerate()
        .map(|(i, close)| {
            bar_full(
                &format!("{close:.2}"),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000,
                1_700_000_000_000 + i as i64 * 60_000,
            )
        })
        .collect();
    let outcome = run_linked(&format!("{IMPORTS}out = MAS.getKalman(close, 5)"), &bars);
    assert_eq!(num(&outcome, 0, "out"), 10.0, "bar 0 seeds the estimate from source");
    // Independent stepwise transcription: v3 = alpha * period, the gain
    // updates the estimate, then the error accrues `beta / period`.
    let (alpha, beta, period) = (0.012_f64, 0.1_f64, 5.0_f64);
    let v3 = alpha * period;
    let v4_first = 1.0 / (1.0 + v3);
    let v2_after_first = (1.0 - v4_first) * 1.0 + beta / period;
    let v4_second = v2_after_first / (v2_after_first + v3);
    let expected_second = 10.0 + v4_second * (11.0 - 10.0);
    let got = num(&outcome, 1, "out");
    assert!(
        (got - expected_second).abs() < EPS,
        "bar 1 follows the gain update, got {got}, want {expected_second}"
    );
}

#[test]
fn get_voss_matches_kernel_batch() {
    // `order = 9` gates output until bar 9, so the run is long enough for
    // full-strength predictions.
    let bars: Vec<Bar> = (0..80)
        .map(|i| {
            let close = 100.0 + 0.3 * (i as f64) + ((i % 7) as f64);
            bar_full(
                &format!("{:.2}", close - 0.25),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000,
                1_700_000_000_000 + i * 60_000,
            )
        })
        .collect();
    let closes: Vec<f64> = (0..80).map(|i| 100.0 + 0.3 * f64::from(i) + f64::from(i % 7)).collect();
    let ohlcv = OwnedOhlcv {
        open: closes.clone(),
        high: closes.iter().map(|c| c + 0.5).collect(),
        low: closes.iter().map(|c| c - 0.5).collect(),
        close: closes,
        volume: vec![1_000.0; 80],
    };
    let outcome = run_linked(
        &format!("{IMPORTS}[v, f] = MAS.getVossPredictiveFilter(close, 12, 3, 0.25)\n"),
        &bars,
    );
    let params = ParamSet::new()
        .with_int("length", 12)
        .with_int("predict", 3)
        .with_float("band", 0.25);
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let kernel = registry.get("voss").expect("voss registered");
    let result = kernel.batch(&ohlcv.views(), &params).expect("valid batch");
    let got = series_of(&outcome, "v", bars.len());
    assert_expr_parity(&got, &result.values, EPS, "voss primary")
        .expect("library Voss matches kernel Voss");
    let filter = result
        .secondary
        .iter()
        .find(|series| series.name == "filter")
        .expect("kernel carries the bandpass filter");
    let got_filter = series_of(&outcome, "f", bars.len());
    assert_expr_parity(&got_filter, &filter.values, EPS, "voss filter")
        .expect("library filter matches kernel filter");
    // The initialization gate holds both outputs at zero through bar 9.
    for bar in 0..=9 {
        assert_eq!(num(&outcome, bar, "v"), 0.0, "gated prediction on bar {bar}");
        assert_eq!(num(&outcome, bar, "f"), 0.0, "gated filter on bar {bar}");
    }
}

#[test]
fn get_macd_pins_composition() {
    fn sma(values: &[Option<f64>], length: usize) -> Vec<Option<f64>> {
        values
            .iter()
            .enumerate()
            .map(|(i, _)| {
                if i + 1 < length {
                    return None;
                }
                let window = &values[i + 1 - length..=i];
                if window.iter().any(Option::is_none) {
                    return None;
                }
                Some(window.iter().map(|v| v.unwrap()).sum::<f64>() / length as f64)
            })
            .collect()
    }
    let bars = ramp_bars();
    let closes: Vec<Option<f64>> = (0..60).map(|i| Some(100.0 + f64::from(i))).collect();
    let fast = sma(&closes, 12);
    let slow = sma(&closes, 26);
    let macd: Vec<Option<f64>> = fast
        .iter()
        .zip(slow.iter())
        .map(|(f, s)| match (f, s) {
            (Some(a), Some(b)) => Some(a - b),
            _ => None,
        })
        .collect();
    let signal = sma(&macd, 9);
    let hist: Vec<Option<f64>> = macd
        .iter()
        .zip(signal.iter())
        .map(|(m, s)| match (m, s) {
            (Some(a), Some(b)) => Some(a - b),
            _ => None,
        })
        .collect();
    let outcome = run_linked(&format!("{IMPORTS}[m, s, h] = MAS.getMACD(close)"), &bars);
    for (name, expected) in [("m", macd), ("s", signal), ("h", hist)] {
        let got = series_of(&outcome, name, bars.len());
        assert_expr_parity(&got, &expected, EPS, name).expect("MACD leg matches hand roll");
    }
    // Absent data propagates through the composition: the `if na(data)`
    // guard body is discarded like any non-trailing `if`, and every
    // smoothing leg smooths absence into absence.
    let outcome = run_linked(&format!("{IMPORTS}[m, s, h] = MAS.getMACD(na)"), &bars);
    for name in ["m", "s", "h"] {
        assert!(
            series_of(&outcome, name, 1)[0].is_none(),
            "na data leaves {name} absent"
        );
    }
    // A fast length at or above the slow length aborts with its reason.
    let err = run_linked_result(
        &format!("{IMPORTS}[m, s, h] = MAS.getMACD(close, 26, 12)"),
        &bars,
    )
    .expect_err("fast >= slow refuses");
    assert!(
        err.to_string().contains("Fast length must be less than slow length"),
        "MACD guard names its reason, got {err}"
    );
}

#[test]
fn get_supertrend_pins_direction_recurrence() {
    // A V-shaped series: ten falling bars, then a twenty-bar rally, so the
    // direction must flip at least once and the print must track the
    // correct band on each side.
    let closes: Vec<f64> = (0..30)
        .map(|i| {
            if i < 10 {
                110.0 - f64::from(i)
            } else {
                101.0 + f64::from(i - 10)
            }
        })
        .collect();
    let bars: Vec<Bar> = closes
        .iter()
        .enumerate()
        .map(|(i, close)| {
            bar_full(
                &format!("{:.2}", close - 0.2),
                &format!("{:.2}", close + 0.4),
                &format!("{:.2}", close - 0.4),
                &format!("{close:.2}"),
                1_000,
                1_700_000_000_000 + i as i64 * 60_000,
            )
        })
        .collect();
    let ohlcv = OwnedOhlcv {
        open: closes.iter().map(|c| c - 0.2).collect(),
        high: closes.iter().map(|c| c + 0.4).collect(),
        low: closes.iter().map(|c| c - 0.4).collect(),
        close: closes.clone(),
        volume: vec![1_000.0; 30],
    };
    let atr = kernel_batch("atr", &ParamSet::new().with_int("length", 10), &ohlcv);
    // Independent transcription of the library recurrence over options.
    // Comparisons with an absent side read false (two-valued, like the
    // reference: `na == na` is false), but a true condition still takes a
    // `na` band value — so through warmup the bands hold the nz-seeded
    // previous value until a real close comparison fires, exactly like
    // `band < prev or close[1] > prev ? band : prev` evaluates.
    let factor = 3.0_f64;
    let mut lower_prev = 0.0_f64;
    let mut upper_prev = 0.0_f64;
    let mut st_prev: Option<f64> = None;
    let mut want_st = Vec::with_capacity(30);
    let mut want_dir = Vec::with_capacity(30);
    for (i, src) in closes.iter().enumerate() {
        let upper = src + factor * atr[i].unwrap_or(f64::NAN);
        let lower = src - factor * atr[i].unwrap_or(f64::NAN);
        let close_prev = if i == 0 { None } else { Some(closes[i - 1]) };
        let lower = match close_prev {
            Some(prev) if (lower.is_finite() && lower > lower_prev) || prev < lower_prev => lower,
            None if lower.is_finite() && lower > lower_prev => lower,
            _ => lower_prev,
        };
        let upper = match close_prev {
            Some(prev) if (upper.is_finite() && upper < upper_prev) || prev > upper_prev => upper,
            None if upper.is_finite() && upper < upper_prev => upper,
            _ => upper_prev,
        };
        let atr_prev = if i == 0 { None } else { atr[i - 1] };
        let direction = if atr_prev.is_none() {
            1_i64
        } else if st_prev == Some(upper_prev) {
            if closes[i] > upper { -1 } else { 1 }
        } else if closes[i] < lower {
            1
        } else {
            -1
        };
        let st = if direction == -1 { lower } else { upper };
        want_dir.push(direction);
        want_st.push(if st.is_finite() { Some(st) } else { None });
        lower_prev = if lower.is_finite() { lower } else { lower_prev };
        upper_prev = if upper.is_finite() { upper } else { upper_prev };
        st_prev = if st.is_finite() { Some(st) } else { None };
    }
    let outcome = run_linked(
        &format!("{IMPORTS}atr = ta.atr(10)\n[st, d] = MAS.getSupertrend(close, atr, 3.0)"),
        &bars,
    );
    let got_st = series_of(&outcome, "st", bars.len());
    assert_expr_parity(&got_st, &want_st, EPS, "supertrend").expect("print tracks the bands");
    let got_dir = series_of(&outcome, "d", bars.len());
    let want_dir: Vec<Option<f64>> = want_dir.into_iter().map(|d| Some(d as f64)).collect();
    assert_expr_parity(&got_dir, &want_dir, EPS, "direction").expect("direction flips correctly");
    assert!(
        got_dir.contains(&Some(-1.0)),
        "the rally side prints -1 at least once"
    );
    assert!(
        got_dir.contains(&Some(1.0)),
        "the falling side prints 1 at least once"
    );
}

fn squash(x: f64) -> f64 {
    x / (1.0 + x.abs())
}

fn normalize_driver(osc: &str, which: i64, smoothing: &str, smoothing_len: i64) -> String {
    format!(
        "{IMPORTS}out = MAS.normalizeOsc(close, UTIL.Oscillators.{osc}, {which}, 5, 14, UTIL.Smoothings.{smoothing}, {smoothing_len})"
    )
}

#[test]
fn normalize_osc_pins_spot_goldens() {
    let bars = ramp_bars();
    let ohlcv = ramp_ohlcv(60);
    let params_14 = ParamSet::new().with_int("length", 14);
    // RSI arm: default offset/k/multiplier over the kernel RSI.
    let rsi = kernel_batch("rsi", &params_14, &ohlcv);
    let want_rsi: Vec<Option<f64>> = rsi
        .iter()
        .map(|v| v.map(|r| squash((r - 50.0) * 0.0084) * 740.0))
        .collect();
    let outcome = run_linked(&normalize_driver("RSI", 0, "NA", 0), &bars);
    assert_expr_parity(&series_of(&outcome, "out", 60), &want_rsi, EPS, "RSI")
        .expect("RSI normalization matches");
    // CMO arm: zero offset with its own gain.
    let cmo = kernel_batch("cmo", &params_14, &ohlcv);
    let want_cmo: Vec<Option<f64>> = cmo
        .iter()
        .map(|v| v.map(|c| squash(c * 0.002) * 1000.0))
        .collect();
    let outcome = run_linked(&normalize_driver("CMO", 0, "NA", 0), &bars);
    assert_expr_parity(&series_of(&outcome, "out", 60), &want_cmo, EPS, "CMO")
        .expect("CMO normalization matches");
    // CCI arm: its own offset and gain.
    let cci = kernel_batch("cci", &params_14, &ohlcv);
    let want_cci: Vec<Option<f64>> = cci
        .iter()
        .map(|v| v.map(|c| squash((c - 50.0) * 0.0035) * 500.0))
        .collect();
    let outcome = run_linked(&normalize_driver("CCI", 0, "NA", 0), &bars);
    assert_expr_parity(&series_of(&outcome, "out", 60), &want_cci, EPS, "CCI")
        .expect("CCI normalization matches");
    // AROON arm on the rising ramp: the high side pins at 100, the low
    // side at 0, so the spread squashes to exactly 160.
    for which in [0, 1, -1] {
        let outcome = run_linked(&normalize_driver("AROON", which, "NA", 0), &bars);
        let got = num(&outcome, 59, "out");
        assert!(
            (got - 160.0).abs() < EPS,
            "AROON which={which} pins 160 on the ramp, got {got}"
        );
    }
    // TRENDSI arm: the ramp correlates perfectly with the bar index.
    let outcome = run_linked(&normalize_driver("TRENDSI", 0, "NA", 0), &bars);
    let closes: Vec<f64> = (0..60).map(|i| 100.0 + f64::from(i)).collect();
    let index: Vec<f64> = (0..60).map(f64::from).collect();
    let tail_c = &closes[46..60];
    let tail_i = &index[46..60];
    let rho = crate::math::corr_pearson(tail_c, tail_i).expect("ramp correlates");
    let want_trendsi = squash(rho * 0.016) * 9000.0;
    let got = num(&outcome, 59, "out");
    assert!(
        (got - want_trendsi).abs() < EPS,
        "TRENDSI pins the correlation squash, got {got}, want {want_trendsi}"
    );
    // VOSS arm against its kernel twin with the VOSS gain table. The arm
    // prints the prediction-minus-filter spread, so the oracle subtracts
    // the kernel's secondary filter series first.
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let voss_kernel = registry.get("voss").expect("voss registered");
    let voss_result = voss_kernel
        .batch(
            &ohlcv.views(),
            &ParamSet::new()
                .with_int("length", 14)
                .with_int("predict", 3)
                .with_float("band", 0.25),
        )
        .expect("valid batch");
    let voss_filter = voss_result
        .secondary
        .iter()
        .find(|series| series.name == "filter")
        .expect("kernel carries the bandpass filter");
    let want_voss: Vec<Option<f64>> = voss_result
        .values
        .iter()
        .zip(voss_filter.values.iter())
        .map(|(v, f)| match (v, f) {
            (Some(a), Some(b)) => Some(squash((a - b) * 1.5) * 300.0),
            _ => None,
        })
        .collect();
    let outcome = run_linked(&normalize_driver("VOSS", 0, "NA", 0), &bars);
    assert_expr_parity(&series_of(&outcome, "out", 60), &want_voss, EPS, "VOSS")
        .expect("VOSS normalization matches");
    // MFI, SRSI, and STOCH arms stay finite on trending data with range.
    // (A pure linear ramp starves the stochastic arms into honest `na`,
    // so this runs on the wobble instead.)
    let wobble = wobble_bars();
    for osc in ["MFI", "SRSI", "STOCH"] {
        let outcome = run_linked(&normalize_driver(osc, 0, "NA", 0), &wobble);
        let got = num(&outcome, 59, "out");
        assert!(got.is_finite(), "{osc} fires finite on the wobble, got {got}");
    }
}

#[test]
fn normalize_osc_smoothing_gate() {
    let bars = ramp_bars();
    let plain = series_of(
        &run_linked(&normalize_driver("RSI", 0, "NA", 0), &bars),
        "out",
        bars.len(),
    );
    // Zero length or the NA smoother leaves the series untouched.
    for (smoothing, len) in [("SMA", 0), ("NA", 5)] {
        let outcome = run_linked(&normalize_driver("RSI", 0, smoothing, len), &bars);
        assert_expr_parity(&series_of(&outcome, "out", 60), &plain, EPS, smoothing)
            .expect("gate-off smoothing is the identity");
    }
    // An armed smoother composes exactly: SMA(5) over the raw series.
    let outcome = run_linked(&normalize_driver("RSI", 0, "SMA", 5), &bars);
    let got = series_of(&outcome, "out", bars.len());
    let nan_plain: Vec<f64> = plain.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
    let smooth_ohlcv = OwnedOhlcv {
        open: nan_plain.clone(),
        high: nan_plain.clone(),
        low: nan_plain.clone(),
        close: nan_plain,
        volume: vec![1_000.0; 60],
    };
    let expected = kernel_batch("sma", &ParamSet::new().with_int("length", 5), &smooth_ohlcv);
    assert_expr_parity(&got, &expected, EPS, "SMA(5)").expect("armed smoothing composes");
    // Non-vacuity without assuming saturation behavior: the smoother needs
    // five defined prints, so it still lags where the raw series starts.
    let first_plain = plain
        .iter()
        .position(Option::is_some)
        .expect("raw RSI defines");
    assert!(
        got[first_plain].is_none(),
        "armed smoothing lags the raw series start"
    );
}

/// Sixty wobbling-but-rising bars: trending with enough range that even
/// the range-hungry oscillators (SRSI/STOCH) fire. A pure linear ramp
/// starves them (highest == lowest gives `0/0`), which is correct `na`
/// in the language, not a missed fire.
fn wobble_bars() -> Vec<Bar> {
    (0..60)
        .map(|i| {
            let close = 100.0 + 0.3 * (i as f64) + ((i % 7) as f64);
            bar_full(
                &format!("{:.2}", close - 0.25),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000 + 10 * i as u64,
                1_700_000_000_000 + i * 60_000,
            )
        })
        .collect()
}

#[test]
fn nested_scope_history_reads_its_own_past() {
    // Two calls to one helper with different arguments must not share
    // history through the bare parameter name: each scope reads the past
    // of the values it was actually called with. This pins the production
    // shape (an outer local and an inner parameter spelling the same
    // name, as `normalizeOsc._data` and the Voss filter's `_data` do).
    let bars: Vec<Bar> = [100.0, 101.0, 102.0, 103.0]
        .into_iter()
        .enumerate()
        .map(|(i, close)| {
            bar_full(
                &format!("{close:.2}"),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000,
                1_700_000_000_000 + i as i64 * 60_000,
            )
        })
        .collect();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}seg(float v) =>\n    v[1]\nouter(float v) =>\n    a = seg(v * 10.0)\n    b = seg(v)\n    [a, b]\n[first, second] = outer(close)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 2, "first"), 1010.0, "scaled call reads scaled past");
    assert_eq!(num(&outcome, 2, "second"), 101.0, "plain call reads plain past");
    assert_eq!(num(&outcome, 3, "first"), 1020.0, "scaled call advances on its own past");
    assert_eq!(num(&outcome, 3, "second"), 102.0, "plain call advances on its own past");
}

#[test]
fn oscillator_calibration_gate_is_green() {
    let bars = wobble_bars();
    let flat: Vec<Bar> = (0..60)
        .map(|i| bar_full("100.00", "100.50", "99.50", "100.00", 1_000, 1_700_000_000_000 + i * 60_000))
        .collect();
    let members = [
        "AROON", "CCI", "CMO", "MFI", "RSI", "SRSI", "STOCH", "TRENDSI", "VOSS",
    ];
    // Must-fire: every oscillator prints finite on trending data.
    let mut fired = 0;
    for osc in members {
        let outcome = run_linked(&normalize_driver(osc, 0, "NA", 0), &bars);
        if num(&outcome, 59, "out").is_finite() {
            fired += 1;
        }
    }
    assert_eq!(fired, members.len(), "must-fire set is 9/9 on the wobble");
    // Must-NOT-fire: flat data stays quiet — absent or bounded by the
    // oscillator's own multiplier — and absent sources stay absent for
    // the source-consuming arms.
    let bounds = [800.0, 500.0, 1000.0, 740.0, 740.0, 7000.0, 7000.0, 9000.0, 300.0];
    let mut quiet = 0;
    for (osc, bound) in members.iter().zip(bounds.iter()) {
        let outcome = run_linked(&normalize_driver(osc, 0, "NA", 0), &flat);
        match series_of(&outcome, "out", 60)[59] {
            None => quiet += 1,
            Some(v) if v.abs() <= *bound => quiet += 1,
            Some(v) => panic!("{osc} fires {v} on flat data, bound {bound}"),
        }
    }
    assert_eq!(quiet, members.len(), "flat-data control is 9/9 quiet");
    let mut absent = 0;
    let mut checked = 0;
    for osc in ["CCI", "CMO", "MFI", "RSI", "SRSI", "TRENDSI"] {
        let outcome = run_linked(
            &format!(
                "{IMPORTS}out = MAS.normalizeOsc(na, UTIL.Oscillators.{osc}, 0, 5, 14, UTIL.Smoothings.NA, 0)"
            ),
            &bars,
        );
        checked += 1;
        if series_of(&outcome, "out", 60)[59].is_none() {
            absent += 1;
        }
    }
    assert_eq!(absent, checked, "na in yields na out on source arms");
    // The Voss arm absorbs absence through its `nz` seeding: all-zero
    // prints, never `na`.
    let outcome = run_linked(
        &format!(
            "{IMPORTS}out = MAS.normalizeOsc(na, UTIL.Oscillators.VOSS, 0, 5, 14, UTIL.Smoothings.NA, 0)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 59, "out"), 0.0, "Voss seeds absence to zero");
}

#[test]
fn leg_driven_arms_ignore_the_source() {
    // AROON and STOCH read chart legs, not `src`: an absent source prints
    // bit-identically to the live-source run. (MFI looks leg-driven but
    // consumes the source, so it pins `na`-out with the source arms.)
    let bars = ramp_bars();
    for osc in ["AROON", "STOCH"] {
        let live = series_of(&run_linked(&normalize_driver(osc, 0, "NA", 0), &bars), "out", 60);
        let absent = series_of(
            &run_linked(
                &format!(
                    "{IMPORTS}out = MAS.normalizeOsc(na, UTIL.Oscillators.{osc}, 0, 5, 14, UTIL.Smoothings.NA, 0)"
                ),
                &bars,
            ),
            "out",
            60,
        );
        assert_expr_parity(&absent, &live, EPS, osc).expect("absent source changes nothing");
    }
}

#[test]
fn oscillator_colors_pin_rgb_table() {
    // `(member, packed r << 24 | g << 16 | b << 8 | transparency)`.
    let cases: &[(&str, u32)] = &[
        ("NA", 0xFF_FF_FF_00),
        ("AROON", 0xFA_F0_28_14),
        ("CCI", 0xB4_F0_28_14),
        ("CDV", 0xF0_64_C8_14),
        ("CMO", 0x8C_F0_50_28),
        ("MFI", 0xF0_20_F0_32_u32),
        ("RSI", 0x20_C8_F0_28),
        ("SRSI", 0xF0_B4_40_14),
        ("STOCH", 0xB4_F0_C8_14),
        ("TRENDSI", 0x40_F0_40_14),
        // VOSS has no color arm: the switch default answers white.
        ("VOSS", 0xFF_FF_FF_00),
    ];
    let bars = ramp_bars();
    for (member, want) in cases {
        let outcome = run_linked(
            &format!("{IMPORTS}out = MAS.getOscillatorColor(UTIL.Oscillators.{member})"),
            &bars,
        );
        assert_eq!(
            color_raw(&outcome, 0, "out"),
            *want,
            "{member} pins its packed color"
        );
    }
}
