//! `ta.*` bridge tests: every bridged call agrees with its kernel batch or
//! its shared-`util` whole-series function, and every refusal carries a
//! reason. The kernel batch is the oracle; the bridge streams `update`
//! position by position, so agreement is the batch/update identity plus
//! source-honoring plumbing.

use super::*;
use crate::indicator::{BarTick, OwnedOhlcv, ParamSet};
use crate::parity::assert_expr_parity;

fn closes(data: &[f64]) -> Vec<BarTick> {
    data.iter().map(|&c| BarTick::close_only(c)).collect()
}

fn full_ticks(data: &OwnedOhlcv) -> Vec<BarTick> {
    data.close
        .iter()
        .zip(data.high.iter())
        .zip(data.low.iter())
        .zip(data.volume.iter())
        .map(|(((c, h), l), v)| BarTick {
            open: *c,
            high: *h,
            low: *l,
            close: *c,
            volume: *v,
        })
        .collect()
}

fn ramp_legs(n: usize) -> OwnedOhlcv {
    let close: Vec<f64> = (1..=n).map(|i| 100.0 + i as f64 * 0.7).collect();
    OwnedOhlcv {
        open: close.clone(),
        high: close.iter().map(|c| c + 0.5).collect(),
        low: close.iter().map(|c| c - 0.5).collect(),
        close,
        volume: vec![1_000.0; n],
    }
}

fn num(value: Option<&PineValue>) -> Option<f64> {
    match value {
        Some(PineValue::Float(v)) => Some(*v),
        Some(PineValue::Int(v)) => Some(*v as f64),
        _ => None,
    }
}

/// Stream one kernel-backed call over ticks, collecting primary outputs.
fn stream(
    bridge: &mut TaBridge,
    site: &str,
    method: &str,
    make_args: impl Fn(f64) -> Vec<Option<PineValue>>,
    ticks: &[BarTick],
) -> Vec<Option<f64>> {
    ticks
        .iter()
        .map(|&tick| {
            // The source argument rides the tick's close (ramp data), so the
            // bridge honors it exactly like a script-level `close` read.
            let args = make_args(tick.close);
            let out = bridge.call(site, method, &args, tick).expect("valid call");
            num(out.as_ref())
        })
        .collect()
}

/// Pack a script float argument: call slots are optional, so the helper
/// returns the slot directly instead of forcing `Some(...)` at every site.
#[allow(clippy::unnecessary_wraps)]
fn float(v: f64) -> Option<PineValue> {
    Some(PineValue::Float(v))
}

#[test]
fn single_source_kernels_agree_with_batch() {
    // (method, extra arg builders, kernel id, params).
    let legs = ramp_legs(60);
    let ticks = full_ticks(&legs);
    let cases: &[(&str, ParamSet)] = &[
        ("sma", ParamSet::new().with_int("length", 14)),
        ("ema", ParamSet::new().with_int("length", 14)),
        ("wma", ParamSet::new().with_int("length", 14)),
        ("hma", ParamSet::new().with_int("length", 14)),
        ("rma", ParamSet::new().with_int("length", 14)),
        ("vwma", ParamSet::new().with_int("length", 14)),
        ("rsi", ParamSet::new().with_int("length", 14)),
        ("mom", ParamSet::new().with_int("length", 14)),
        ("roc", ParamSet::new().with_int("length", 14)),
        ("cci", ParamSet::new().with_int("length", 14)),
        ("cmo", ParamSet::new().with_int("length", 14)),
        ("mfi", ParamSet::new().with_int("length", 14)),
        (
            "alma",
            ParamSet::new()
                .with_int("length", 14)
                .with_float("offset", 0.85)
                .with_float("sigma", 6.0),
        ),
        (
            "stdev",
            ParamSet::new()
                .with_int("length", 14)
                .with_float("nbdev", 1.0),
        ),
        (
            "variance",
            ParamSet::new()
                .with_int("length", 14)
                .with_float("nbdev", 1.0),
        ),
    ];
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let mut bridge = TaBridge::new().expect("bridge builds");
    for (method, params) in cases {
        let kernel_id = match *method {
            "stdev" => "stddev",
            "variance" => "var",
            other => other,
        };
        let kernel = registry.get(kernel_id).expect("kernel registered");
        let expected = kernel
            .batch(&legs.views(), params)
            .expect("valid batch")
            .values;
        let args_for: Box<dyn Fn(f64) -> Vec<Option<PineValue>>> = if *method == "alma" {
            Box::new(|c| vec![float(c), Some(PineValue::Int(14)), float(0.85), float(6.0)])
        } else {
            Box::new(|c| vec![float(c), Some(PineValue::Int(14))])
        };
        let got = stream(&mut bridge, method, method, args_for, &ticks);
        assert_expr_parity(&got, &expected, 1e-9, method).expect("bridge matches kernel");
    }
}

#[test]
fn multi_output_and_multi_leg_kernels_agree_with_batch() {
    let legs = ramp_legs(60);
    let ticks = full_ticks(&legs);
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let mut bridge = TaBridge::new().expect("bridge builds");

    // macd primary (line) only; tuples unpack in a later stage.
    let macd_ps = ParamSet::new()
        .with_int("fast_length", 12)
        .with_int("slow_length", 26)
        .with_int("signal_length", 9);
    let macd = registry.get("macd").expect("registered");
    let expected = macd.batch(&legs.views(), &macd_ps).expect("valid").values;
    let got = stream(
        &mut bridge,
        "macd",
        "macd",
        |c| {
            vec![
                float(c),
                Some(PineValue::Int(12)),
                Some(PineValue::Int(26)),
                Some(PineValue::Int(9)),
            ]
        },
        &ticks,
    );
    assert_expr_parity(&got, &expected, 1e-9, "macd").expect("macd line matches");

    // bb primary (basis) only.
    let bb_ps = ParamSet::new()
        .with_int("length", 14)
        .with_float("mult", 2.0);
    let bb = registry.get("bb").expect("registered");
    let expected = bb.batch(&legs.views(), &bb_ps).expect("valid").values;
    let got = stream(
        &mut bridge,
        "bb",
        "bb",
        |c| vec![float(c), Some(PineValue::Int(14)), float(2.0)],
        &ticks,
    );
    assert_expr_parity(&got, &expected, 1e-9, "bb").expect("basis matches");

    // atr/tr/obv read chart legs with no source.
    for (method, params) in [
        ("atr", ParamSet::new().with_int("length", 14)),
        ("tr", ParamSet::new()),
        ("obv", ParamSet::new()),
    ] {
        let kernel = registry.get(method).expect("registered");
        let expected = kernel.batch(&legs.views(), &params).expect("valid").values;
        let len_arg = [float(14.0)];
        let got: Vec<Option<f64>> = ticks
            .iter()
            .map(|&tick| {
                let args: &[Option<PineValue>] = if method == "atr" { &len_arg } else { &[] };
                let out = bridge.call(method, method, args, tick).expect("valid call");
                num(out.as_ref())
            })
            .collect();
        assert_expr_parity(&got, &expected, 1e-9, method).expect("leg kernel matches");
    }

    // stoch assembles its tick from evaluated args (source, high, low).
    let stoch = registry.get("stoch").expect("registered");
    let stoch_ps = ParamSet::new().with_int("length", 14);
    let expected = stoch.batch(&legs.views(), &stoch_ps).expect("valid").values;
    let got: Vec<Option<f64>> = ticks
        .iter()
        .map(|&tick| {
            let out = bridge
                .call(
                    "stoch",
                    "stoch",
                    &[
                        float(tick.close),
                        float(tick.high),
                        float(tick.low),
                        float(14.0),
                    ],
                    tick,
                )
                .expect("valid call");
            num(out.as_ref())
        })
        .collect();
    assert_expr_parity(&got, &expected, 1e-9, "stoch").expect("%K matches");
}

#[test]
fn vwap_and_wpr_agree_with_batch() {
    let legs = ramp_legs(60);
    let ticks = full_ticks(&legs);
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let mut bridge = TaBridge::new().expect("bridge builds");
    let vwap = registry.get("vwap").expect("registered");
    let expected = vwap
        .batch(&legs.views(), &ParamSet::new())
        .expect("valid")
        .values;
    let got = stream(&mut bridge, "vwap", "vwap", |c| vec![float(c)], &ticks);
    assert_expr_parity(&got, &expected, 1e-9, "vwap").expect("vwap matches");

    let willr = registry.get("willr").expect("registered");
    let ps = ParamSet::new().with_int("length", 14);
    let expected = willr.batch(&legs.views(), &ps).expect("valid").values;
    let got: Vec<Option<f64>> = ticks
        .iter()
        .map(|&tick| {
            num(bridge
                .call("wpr", "wpr", &[float(14.0)], tick)
                .expect("valid")
                .as_ref())
        })
        .collect();
    assert_expr_parity(&got, &expected, 1e-9, "wpr").expect("wpr matches");
}

#[test]
fn recurrence_filters_agree_with_batch() {
    let legs = ramp_legs(60);
    let ticks = full_ticks(&legs);
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let mut bridge = TaBridge::new().expect("bridge builds");
    let eit_ps = ParamSet::new().with_float("alpha", 0.5);
    let eit = registry.get("eit").expect("registered");
    let expected = eit.batch(&legs.views(), &eit_ps).expect("valid").values;
    let got = stream(
        &mut bridge,
        "eit",
        "eit",
        |c| vec![float(c), float(0.5)],
        &ticks,
    );
    assert_expr_parity(&got, &expected, 1e-9, "eit").expect("eit matches");

    let kalman_ps = ParamSet::new().with_int("length", 14);
    let kalman = registry.get("kalman").expect("registered");
    let expected = kalman
        .batch(&legs.views(), &kalman_ps)
        .expect("valid")
        .values;
    let got = stream(
        &mut bridge,
        "kalman",
        "kalman",
        |c| vec![float(c), float(14.0)],
        &ticks,
    );
    assert_expr_parity(&got, &expected, 1e-9, "kalman").expect("kalman matches");

    let voss_ps = ParamSet::new()
        .with_int("length", 20)
        .with_int("predict", 3)
        .with_float("band", 0.25);
    let voss = registry.get("voss").expect("registered");
    let expected = voss.batch(&legs.views(), &voss_ps).expect("valid").values;
    let got = stream(
        &mut bridge,
        "voss",
        "voss",
        |c| vec![float(c), float(20.0)],
        &ticks,
    );
    assert_expr_parity(&got, &expected, 1e-9, "voss").expect("voss matches");
}

#[test]
fn window_operators_agree_with_shared_utils() {
    let legs = ramp_legs(60);
    let ticks = closes(&legs.close);
    let mut bridge = TaBridge::new().expect("bridge builds");
    let len = 14.0;
    let highest = stream(
        &mut bridge,
        "hi",
        "highest",
        |c| vec![float(c), float(len)],
        &ticks,
    );
    let expected = crate::util::highest(&legs.close, 14);
    assert_expr_parity(&highest, &expected, 1e-12, "highest").expect("highest matches util");
    let lowest = stream(
        &mut bridge,
        "lo",
        "lowest",
        |c| vec![float(c), float(len)],
        &ticks,
    );
    let expected = crate::util::lowest(&legs.close, 14);
    assert_expr_parity(&lowest, &expected, 1e-12, "lowest").expect("lowest matches util");

    let highestbars: Vec<Option<f64>> = ticks
        .iter()
        .map(|&tick| {
            let out = bridge
                .call("hib", "highestbars", &[float(tick.close), float(len)], tick)
                .expect("valid");
            num(out.as_ref())
        })
        .collect();
    let expected: Vec<Option<f64>> = crate::util::argmax_window(&legs.close, 14)
        .iter()
        .map(|slot| slot.map(|ago| -(ago as f64)))
        .collect();
    assert_expr_parity(&highestbars, &expected, 1e-12, "highestbars").expect("bars match util");

    // Crosses over two histories agree with the shared pair operator.
    let mut bridge = TaBridge::new().expect("bridge builds");
    let a: Vec<f64> = (0..60).map(|i| (f64::from(i) - 30.0).abs()).collect();
    let b = vec![15.0; 60];
    let ticks_a = closes(&a);
    let crossed: Vec<Option<bool>> = ticks_a
        .iter()
        .zip(b.iter())
        .map(|(&tick, &level)| {
            let out = bridge
                .call(
                    "cross",
                    "crossover",
                    &[float(tick.close), float(level)],
                    tick,
                )
                .expect("valid");
            match out {
                Some(PineValue::Bool(flag)) => Some(flag),
                _ => None,
            }
        })
        .collect();
    assert_eq!(crossed, crate::util::crossover(&a, &b));
}

#[test]
fn cum_change_median_and_correlation_behave() {
    let ticks = closes(&[1.0, 2.0, 3.0, 5.0]);
    let mut bridge = TaBridge::new().expect("bridge builds");
    let sums: Vec<Option<f64>> = ticks
        .iter()
        .map(|&tick| {
            num(bridge
                .call("c", "cum", &[float(tick.close)], tick)
                .expect("valid")
                .as_ref())
        })
        .collect();
    assert_eq!(sums, vec![Some(1.0), Some(3.0), Some(6.0), Some(11.0)]);

    let mut bridge = TaBridge::new().expect("bridge builds");
    let changes: Vec<Option<f64>> = ticks
        .iter()
        .map(|&tick| {
            num(bridge
                .call("ch", "change", &[float(tick.close)], tick)
                .expect("valid")
                .as_ref())
        })
        .collect();
    assert_eq!(changes, vec![None, Some(1.0), Some(1.0), Some(2.0)]);

    let mut bridge = TaBridge::new().expect("bridge builds");
    let medians: Vec<Option<f64>> = ticks
        .iter()
        .map(|&tick| {
            num(bridge
                .call("m", "median", &[float(tick.close), float(3.0)], tick)
                .expect("valid")
                .as_ref())
        })
        .collect();
    assert_eq!(medians, vec![None, None, Some(2.0), Some(3.0)]);

    // Perfectly correlated ramps read 1 once the window fills.
    let mut bridge = TaBridge::new().expect("bridge builds");
    let series: Vec<f64> = (1..=20).map(f64::from).collect();
    let ticks = closes(&series);
    let last = ticks
        .iter()
        .map(|&tick| {
            bridge
                .call(
                    "r",
                    "correlation",
                    &[float(tick.close), float(tick.close), float(14.0)],
                    tick,
                )
                .expect("valid")
        })
        .last()
        .expect("one value");
    let rho = num(last.as_ref()).expect("correlated");
    assert!((rho - 1.0).abs() <= 1e-12, "perfect rank reads 1: {rho}");
}

#[test]
fn arg_validation_refuses_with_reasons() {
    let mut bridge = TaBridge::new().expect("bridge builds");
    let tick = BarTick::close_only(10.0);
    let cases: &[(&str, Vec<Option<PineValue>>, &str)] = &[
        ("sma", vec![float(1.0)], "argument"),
        ("sma", vec![float(1.0), float(0.0)], "positive"),
        ("sma", vec![float(1.0), float(2.5)], "whole"),
        (
            "sma",
            vec![float(1.0), Some(PineValue::Bool(true))],
            "positive",
        ),
        (
            "sma",
            vec![Some(PineValue::Str("x".to_owned())), float(3.0)],
            "numeric",
        ),
        ("macd", vec![float(1.0), float(2.0)], "signal"),
        ("bb", vec![float(1.0), float(14.0)], "mult"),
        ("stoch", vec![float(1.0), float(2.0)], "high"),
        ("highestbars", vec![float(1.0)], "length"),
        ("tr", vec![Some(PineValue::Bool(true))], "na handling"),
        ("nope", vec![], "unknown"),
        ("dmi", vec![float(1.0)], "diLength"),
        ("median", vec![float(1.0), float(0.0)], "positive"),
    ];
    for (method, args, want) in cases {
        let err = bridge
            .call(method, method, args, tick)
            .expect_err("refuses")
            .to_string();
        assert!(
            err.contains(want),
            "ta.{method} refusal names the problem: {err}"
        );
    }
}

/// Pack a script bool argument: call slots are optional, so the helper
/// returns the slot directly instead of forcing `Some(...)` at every site.
#[allow(clippy::unnecessary_wraps)]
fn flag(value: bool) -> Option<PineValue> {
    Some(PineValue::Bool(value))
}

fn ago_of(value: Option<&PineValue>) -> Option<i64> {
    match value {
        Some(PineValue::Int(n)) => Some(*n),
        None => None,
        other => panic!("barssince reads int or na, got {other:?}"),
    }
}

#[test]
fn barssince_counts_back_to_the_last_true_condition() {
    // Conditions fire on bars 2 and 4: ago reads na until the first
    // firing, then 0 on a firing bar and the distance after it.
    let ticks = closes(&[1.0, 2.0, 3.0, 4.0, 5.0]);
    let cond = [false, false, true, false, true];
    let mut bridge = TaBridge::new().expect("bridge builds");
    let ago: Vec<Option<i64>> = ticks
        .iter()
        .zip(cond)
        .map(|(&tick, hit)| {
            ago_of(
                bridge
                    .call("bs", "barssince", &[flag(hit)], tick)
                    .expect("valid")
                    .as_ref(),
            )
        })
        .collect();
    assert_eq!(ago, vec![None, None, Some(0), Some(1), Some(0)]);
}

#[test]
fn barssince_treats_na_condition_as_unsatisfied() {
    // Absence never fires: an `na` condition behaves like `false`, and a
    // run with no firing at all reads `na` throughout.
    let ticks = closes(&[1.0, 2.0, 3.0]);
    let mut bridge = TaBridge::new().expect("bridge builds");
    let ago: Vec<Option<i64>> = ticks
        .iter()
        .map(|&tick| {
            ago_of(
                bridge
                    .call("na", "barssince", &[None], tick)
                    .expect("valid")
                    .as_ref(),
            )
        })
        .collect();
    assert_eq!(ago, vec![None, None, None]);
    let mut bridge = TaBridge::new().expect("bridge builds");
    let out = bridge
        .call("one", "barssince", &[flag(true)], ticks[0])
        .expect("valid");
    assert_eq!(out, Some(PineValue::Int(0)), "fires on its own bar");
}

#[test]
fn valuewhen_reads_the_nth_firing_source_value() {
    // Sources `[10..50]`, firings on bars 0 and 2: occurrence 0 tracks
    // the latest firing, occurrence 1 the one before, occurrence 2
    // never exists.
    let ticks = closes(&[10.0, 20.0, 30.0, 40.0, 50.0]);
    let cond = [true, false, true, false, false];
    for (occurrence, want) in [
        (
            0_i64,
            vec![Some(10.0), Some(10.0), Some(30.0), Some(30.0), Some(30.0)],
        ),
        (1_i64, vec![None, None, Some(10.0), Some(10.0), Some(10.0)]),
        (2_i64, vec![None, None, None, None, None]),
    ] {
        let mut bridge = TaBridge::new().expect("bridge builds");
        let got: Vec<Option<f64>> = ticks
            .iter()
            .zip(cond)
            .map(|(&tick, hit)| {
                num(bridge
                    .call(
                        "vw",
                        "valuewhen",
                        &[
                            flag(hit),
                            float(tick.close),
                            Some(PineValue::Int(occurrence)),
                        ],
                        tick,
                    )
                    .expect("valid")
                    .as_ref())
            })
            .collect();
        assert_eq!(got, want, "occurrence {occurrence} tracks firings");
    }
}

#[test]
fn valuewhen_propagates_na_and_refuses_bad_occurrence() {
    // An `na` source on the firing bar reads `na` (absence in, absence
    // out); an `na` condition never fires.
    let ticks = closes(&[1.0, 2.0]);
    let mut bridge = TaBridge::new().expect("bridge builds");
    let out = bridge
        .call(
            "na",
            "valuewhen",
            &[flag(true), None, Some(PineValue::Int(0))],
            ticks[0],
        )
        .expect("valid");
    assert_eq!(out, None, "na source on a firing bar reads na");
    let mut bridge = TaBridge::new().expect("bridge builds");
    let out = bridge
        .call(
            "nc",
            "valuewhen",
            &[None, float(9.0), Some(PineValue::Int(0))],
            ticks[0],
        )
        .expect("valid");
    assert_eq!(out, None, "na condition never fires");
    // Negative occurrences and non-bool conditions refuse with reasons.
    let mut bridge = TaBridge::new().expect("bridge builds");
    let err = bridge
        .call(
            "neg",
            "valuewhen",
            &[flag(true), float(1.0), Some(PineValue::Int(-1))],
            ticks[0],
        )
        .expect_err("negative occurrence refuses")
        .to_string();
    assert!(err.contains("non-negative"), "occurrence refusal: {err}");
    let err = bridge
        .call(
            "cond",
            "valuewhen",
            &[float(1.0), float(1.0), Some(PineValue::Int(0))],
            ticks[0],
        )
        .expect_err("numeric condition refuses")
        .to_string();
    assert!(err.contains("bool condition"), "condition refusal: {err}");
}

#[test]
fn pivots_confirm_on_source_history_not_chart_legs() {
    // Spike at index 2 with (2, 2) windows confirms on bar 4 only; the
    // first four bars read `na` (short windows confirm nothing).
    let spike = [50.0, 50.0, 1000.0, 50.0, 50.0];
    let ticks = closes(&spike);
    let mut bridge = TaBridge::new().expect("bridge builds");
    let high: Vec<Option<f64>> = ticks
        .iter()
        .map(|&tick| {
            num(bridge
                .call(
                    "ph",
                    "pivothigh",
                    &[float(tick.close), float(2.0), float(2.0)],
                    tick,
                )
                .expect("valid")
                .as_ref())
        })
        .collect();
    assert_eq!(high, vec![None, None, None, None, Some(1000.0)]);
    // Chart legs stay flat while the source spikes: the pivot still
    // confirms (source honored), and a flat source with spiking legs
    // never confirms (legs ignored).
    let flat_legs = BarTick {
        open: 50.0,
        high: 101.0,
        low: 99.0,
        close: 50.0,
        volume: 1_000.0,
    };
    let mut bridge = TaBridge::new().expect("bridge builds");
    let mut last = None;
    for &value in &spike {
        last = bridge
            .call(
                "legs",
                "pivothigh",
                &[float(value), float(2.0), float(2.0)],
                flat_legs,
            )
            .expect("valid");
    }
    assert_eq!(last, Some(PineValue::Float(1000.0)), "legs ignored");
    let mut bridge = TaBridge::new().expect("bridge builds");
    let mut last = None;
    for &value in &[50.0; 5] {
        let spiking = BarTick {
            high: 1000.0,
            ..flat_legs
        };
        last = bridge
            .call(
                "flat",
                "pivothigh",
                &[float(value), float(2.0), float(2.0)],
                spiking,
            )
            .expect("valid");
    }
    assert_eq!(last, None, "flat source never pivots");
    // Low mirror: the trough confirms symmetrically.
    let trough = [50.0, 50.0, 0.0, 50.0, 50.0];
    let ticks = closes(&trough);
    let mut bridge = TaBridge::new().expect("bridge builds");
    let low: Vec<Option<f64>> = ticks
        .iter()
        .map(|&tick| {
            num(bridge
                .call(
                    "pl",
                    "pivotlow",
                    &[float(tick.close), float(2.0), float(2.0)],
                    tick,
                )
                .expect("valid")
                .as_ref())
        })
        .collect();
    assert_eq!(low, vec![None, None, None, None, Some(0.0)]);
}

#[test]
fn pivots_reject_equal_extremes_and_na_windows() {
    // Equal highs break the pivot (strictly extreme or nothing), and any
    // absence in the window reads absence.
    let ticks = closes(&[50.0, 1000.0, 1000.0, 50.0, 50.0]);
    let mut bridge = TaBridge::new().expect("bridge builds");
    let mut last = None;
    for &tick in &ticks {
        last = bridge
            .call(
                "eq",
                "pivothigh",
                &[float(tick.close), float(2.0), float(2.0)],
                tick,
            )
            .expect("valid");
    }
    assert_eq!(last, None, "equal extremes never pivot");
    let mut bridge = TaBridge::new().expect("bridge builds");
    let out = bridge
        .call(
            "short",
            "pivothigh",
            &[float(1.0), float(0.0), float(2.0)],
            ticks[0],
        )
        .expect_err("zero leftbars refuses")
        .to_string();
    assert!(out.contains("positive"), "length refusal: {out}");
    let mut bridge = TaBridge::new().expect("bridge builds");
    let mut last = None;
    for (index, &tick) in ticks.iter().enumerate() {
        let source = if index == 2 { None } else { float(tick.close) };
        last = bridge
            .call("nav", "pivothigh", &[source, float(2.0), float(2.0)], tick)
            .expect("valid");
    }
    assert_eq!(last, None, "na in the window reads na");
}

#[test]
fn linreg_fits_the_window_least_squares() {
    // `ta.linreg(source, length, offset)` evaluates the least-squares line
    // over the window at `offset` bars back: a perfect ramp reproduces
    // itself (offset 0) and its own lags (offset > 0); short windows and
    // `na` poison read absent. Hand-computed: fits are exact on lines, so
    // no tolerance beyond float rounding applies.
    let legs = ramp_legs(10);
    let ticks = full_ticks(&legs);
    let mut bridge = TaBridge::new().expect("bridge builds");
    let mut at = |offset: i64| -> Vec<Option<f64>> {
        ticks
            .iter()
            .map(|&tick| {
                let args = vec![
                    float(tick.close),
                    Some(PineValue::Int(5)),
                    Some(PineValue::Int(offset)),
                ];
                let out = bridge
                    .call("linreg-site", "linreg", &args, tick)
                    .expect("valid call");
                num(out.as_ref())
            })
            .collect()
    };
    let fitted = at(0);
    assert_eq!(fitted.len(), 10);
    assert!(
        fitted[..4].iter().all(Option::is_none),
        "short windows read absent: {fitted:?}"
    );
    for (index, value) in fitted.iter().enumerate().skip(4) {
        let want = 100.0 + (index + 1) as f64 * 0.7;
        let got = value.expect("warmed window fits");
        assert!(
            (got - want).abs() < 1e-9,
            "offset 0 reproduces the ramp at bar {index}: got {got}, want {want}"
        );
    }
    let lagged = at(2);
    for (index, value) in lagged.iter().enumerate().skip(4) {
        let want = 100.0 + (index + 1) as f64 * 0.7 - 2.0 * 0.7;
        let got = value.expect("warmed window fits");
        assert!(
            (got - want).abs() < 1e-9,
            "offset 2 lags two steps at bar {index}: got {got}, want {want}"
        );
    }
}

#[test]
fn dmi_reports_directional_triples() {
    // `ta.dmi(diLength, adxSmoothing)` returns [+DI, -DI, ADX]: Wilder
    // smoothing with the kernel's seed-then-recurse order (first full
    // window seeds from its mean). Hand-computed on a steady climber
    // with lengths 2: bars (10,9,9.5), (11,10,10.5), (12,11,11.5) give
    // raw (0,0,1), (1,0,1.5), (1,0,1.5); smoothed at bar 2 are
    // (0.75, 0, 1.375), so +DI is 54.54.., -DI is 0, and DX pins at 100
    // for both warmed bars, seeding ADX at 100.
    let ticks = vec![
        BarTick { open: 9.5, high: 10.0, low: 9.0, close: 9.5, volume: 1_000.0 },
        BarTick { open: 10.5, high: 11.0, low: 10.0, close: 10.5, volume: 1_000.0 },
        BarTick { open: 11.5, high: 12.0, low: 11.0, close: 11.5, volume: 1_000.0 },
    ];
    let mut bridge = TaBridge::new().expect("bridge builds");
    let mut out = Vec::new();
    for tick in &ticks {
        let args = vec![Some(PineValue::Int(2)), Some(PineValue::Int(2))];
        out.push(
            bridge
                .call("dmi-site", "dmi", &args, *tick)
                .expect("valid call"),
        );
    }
    assert_eq!(out.len(), 3);
    match &out[0] {
        Some(PineValue::Array(items)) => {
            assert!(
                items.iter().all(Option::is_none),
                "the first bar reads absent: {items:?}"
            );
        }
        other => panic!("dmi always returns its triple, got {other:?}"),
    }
    match &out[1] {
        Some(PineValue::Array(items)) => {
            let triple: Vec<Option<f64>> = items
                .iter()
                .map(|slot| match slot {
                    Some(PineValue::Float(v)) => Some(*v),
                    Some(PineValue::Int(v)) => Some(*v as f64),
                    _ => None,
                })
                .collect();
            // Seeded from the first full window (means 0.5, 0, 1.25), so
            // +DI serves 40 while ADX still warms (one DX of two needed).
            assert_eq!(triple.len(), 3);
            assert!((triple[0].expect("+DI serves") - 40.0).abs() < 1e-9);
            assert_eq!(triple[1], Some(0.0));
            assert_eq!(triple[2], None, "ADX warms one bar later");
        }
        other => panic!("dmi always returns its triple, got {other:?}"),
    }
    match &out[2] {
        Some(PineValue::Array(items)) => {
            let triple: Vec<Option<f64>> = items
                .iter()
                .map(|slot| match slot {
                    Some(PineValue::Float(v)) => Some(*v),
                    Some(PineValue::Int(v)) => Some(*v as f64),
                    _ => None,
                })
                .collect();
            assert_eq!(triple.len(), 3);
            let plus = triple[0].expect("directional plus serves");
            let minus = triple[1].expect("directional minus serves");
            let adx = triple[2].expect("adx serves");
            assert!((plus - 54.545_454_545_454_55).abs() < 1e-9, "+DI: {plus}");
            assert!((minus - 0.0).abs() < 1e-9, "-DI is flat zero: {minus}");
            assert!((adx - 100.0).abs() < 1e-9, "ADX pins at full trend: {adx}");
        }
        other => panic!("dmi always returns its triple, got {other:?}"),
    }
}
