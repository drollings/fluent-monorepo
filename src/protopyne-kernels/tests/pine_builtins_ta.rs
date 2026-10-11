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
            ParamSet::new().with_int("length", 14).with_float("nbdev", 1.0),
        ),
        (
            "variance",
            ParamSet::new().with_int("length", 14).with_float("nbdev", 1.0),
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
        let expected = kernel.batch(&legs.views(), params).expect("valid batch").values;
        let args_for: Box<dyn Fn(f64) -> Vec<Option<PineValue>>> = if *method == "alma" {
            Box::new(|c| {
                vec![
                    float(c),
                    Some(PineValue::Int(14)),
                    float(0.85),
                    float(6.0),
                ]
            })
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
    let bb_ps = ParamSet::new().with_int("length", 14).with_float("mult", 2.0);
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
                    &[float(tick.close), float(tick.high), float(tick.low), float(14.0)],
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
    let got = stream(&mut bridge, "eit", "eit", |c| vec![float(c), float(0.5)], &ticks);
    assert_expr_parity(&got, &expected, 1e-9, "eit").expect("eit matches");

    let kalman_ps = ParamSet::new().with_int("length", 14);
    let kalman = registry.get("kalman").expect("registered");
    let expected = kalman.batch(&legs.views(), &kalman_ps).expect("valid").values;
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
    let highest = stream(&mut bridge, "hi", "highest", |c| vec![float(c), float(len)], &ticks);
    let expected = crate::util::highest(&legs.close, 14);
    assert_expr_parity(&highest, &expected, 1e-12, "highest").expect("highest matches util");
    let lowest = stream(&mut bridge, "lo", "lowest", |c| vec![float(c), float(len)], &ticks);
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
                .call("cross", "crossover", &[float(tick.close), float(level)], tick)
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
        .map(|&tick| num(bridge.call("c", "cum", &[float(tick.close)], tick).expect("valid").as_ref()))
        .collect();
    assert_eq!(sums, vec![Some(1.0), Some(3.0), Some(6.0), Some(11.0)]);

    let mut bridge = TaBridge::new().expect("bridge builds");
    let changes: Vec<Option<f64>> = ticks
        .iter()
        .map(|&tick| num(bridge.call("ch", "change", &[float(tick.close)], tick).expect("valid").as_ref()))
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
                .call("r", "correlation", &[float(tick.close), float(tick.close), float(14.0)], tick)
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
        ("sma", vec![float(1.0), Some(PineValue::Bool(true))], "positive"),
        ("sma", vec![Some(PineValue::Str("x".to_owned())), float(3.0)], "numeric"),
        ("macd", vec![float(1.0), float(2.0)], "signal"),
        ("bb", vec![float(1.0), float(14.0)], "mult"),
        ("stoch", vec![float(1.0), float(2.0)], "high"),
        ("highestbars", vec![float(1.0)], "length"),
        ("tr", vec![Some(PineValue::Bool(true))], "na handling"),
        ("nope", vec![], "unknown"),
        ("dmi", vec![float(1.0)], "unknown"),
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
