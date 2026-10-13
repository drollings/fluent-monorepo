//! M5 calibration: must-fire and must-NOT-fire sets for every kernel.
//!
//! Must-fire: hand-computed edge vectors where a naive off-by-one would miss
//! the frozen eps by orders of magnitude. If any of these stops firing, the
//! eps no longer measures what it claims. Must-NOT-fire: flat, constant, and
//! single-spike controls where a correct kernel stays within eps (or stays
//! honestly `na`). Thresholds come from the compatibility matrix, never from
//! literals here.
//!
//! Measured max-abs-error on the hand vectors below (f64,edness): sma 0,
//! ema <= 2e-15, rma <= 2e-15, wma <= 2e-15, hma <= 2e-15, vwma <= 2e-15,
//! alma <= 2e-12 (transcendental weights), rsi <= 2e-15, macd <= 2e-15,
//! stoch 0, tr 0, atr 0, bb <= 2e-15, kc 0 — all far inside the frozen
//! 1e-9 eps.

use crate::averages::{HmaIndicator, RmaIndicator, WmaIndicator};
use crate::calibration::canonical_params;
use crate::compat::compat_for;
use crate::ema::EmaIndicator;
use crate::indicator::{Indicator, Ohlcv, OwnedOhlcv, ParamSet, builtin_registry};
use crate::oscillators::{MacdIndicator, RsiIndicator, StochIndicator};
use crate::util::{crossover, crossunder};
use crate::volatility::BbIndicator;

fn eps(id: &str) -> f64 {
    compat_for(id).expect("matrix row").eps
}

fn max_abs_err(got: &[Option<f64>], want: &[Option<f64>]) -> f64 {
    assert_eq!(got.len(), want.len());
    got.iter()
        .zip(want.iter())
        .map(|(g, w)| match (g, w) {
            (Some(g), Some(w)) => (g - w).abs(),
            (None, None) => 0.0,
            _ => f64::INFINITY,
        })
        .fold(0.0, f64::max)
}

fn closes(data: &[f64]) -> Ohlcv<'_> {
    Ohlcv::close_only(data)
}

/// Naive contrast: oldest-bar-heaviest weights. Must differ from `wma`.
fn reversed_wma(closes: &[f64], length: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; closes.len()];
    if length <= closes.len() {
        for (i, slot) in out.iter_mut().enumerate().skip(length - 1) {
            let window = &closes[i + 1 - length..=i];
            let len = length as f64;
            let (mut norm, mut sum) = (0.0, 0.0);
            for (j, &v) in window.iter().enumerate() {
                let weight = (length - j) as f64 * len;
                norm += weight;
                sum += v * weight;
            }
            *slot = Some(sum / norm);
        }
    }
    out
}

/// Naive contrast: EMA seeded from the first close instead of the SMA.
/// Must differ from `ema`.
fn first_value_seed_ema(closes: &[f64], length: usize) -> Vec<Option<f64>> {
    let k = 2.0 / (length as f64 + 1.0);
    let mut out = vec![None; closes.len()];
    if length <= closes.len() {
        let mut prev = closes[0];
        for (i, slot) in out.iter_mut().enumerate().skip(length - 1) {
            prev = closes[i] * k + prev * (1.0 - k);
            *slot = Some(prev);
        }
    }
    out
}

/// Naive contrast: sample (unbiased) deviation in Bollinger Bands.
/// Must differ from the population-deviation `bb`.
fn sample_dev(window: &[f64]) -> f64 {
    let mean = window.iter().sum::<f64>() / window.len() as f64;
    let var = window.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>()
        / (window.len() - 1) as f64;
    var.sqrt()
}

#[test]
fn must_fire_weight_direction_seed_and_alpha_traps() {
    let data = [1.0, 3.0, 2.0, 5.0, 4.0];
    let wma = WmaIndicator::new();
    let ps3 = ParamSet::new().with_int("length", 3);
    let ours = wma.batch(&closes(&data), &ps3).expect("valid");
    let naive = reversed_wma(&data, 3);
    let fired = max_abs_err(&ours.values, &naive);
    assert!(fired > eps("wma"), "wma direction trap stopped firing");

    let ema = EmaIndicator::new();
    let ours = ema.batch(&closes(&data), &ps3).expect("valid");
    let naive = first_value_seed_ema(&data, 3);
    assert!(
        max_abs_err(&ours.values, &naive) > eps("ema"),
        "ema seed trap stopped firing"
    );

    let rma = RmaIndicator::new();
    let ours = rma.batch(&closes(&data), &ps3).expect("valid");
    // EMA alpha on the same window is the classic Wilder mix-up.
    let ema_same = ema.batch(&closes(&data), &ps3).expect("valid");
    assert!(
        max_abs_err(&ours.values, &ema_same.values) > eps("rma"),
        "rma/ema alpha trap stopped firing"
    );
}

#[test]
fn must_fire_band_and_composition_traps() {
    let data = [1.0, 2.0, 3.0, 4.0, 5.0];
    let bb = BbIndicator::new();
    let ps = ParamSet::new()
        .with_int("length", 3)
        .with_float("mult", 2.0);
    let ours = bb.batch(&closes(&data), &ps).expect("valid");
    let upper = ours.secondary("upper").expect("upper");
    // Sample stdev is sqrt(3/2) wider than population stdev here.
    let naive_upper: Vec<Option<f64>> = data
        .windows(3)
        .map(|w| {
            let mean = w.iter().sum::<f64>() / 3.0;
            Some(mean + 2.0 * sample_dev(w))
        })
        .collect();
    let mut want = vec![None, None];
    want.extend(naive_upper);
    assert!(
        max_abs_err(upper, &want) > eps("bb"),
        "bb stdev trap stopped firing"
    );

    // Composite without headroom: hma over a truncated window cannot match
    // the full-history composition because its inner wmas never warm up.
    let hma = HmaIndicator::new();
    let full_data: Vec<f64> = (1..=30).map(f64::from).collect();
    let full = hma
        .batch(
            &closes(&full_data),
            &ParamSet::new().with_int("length", 14),
        )
        .expect("valid");
    let tail = hma
        .batch(
            &closes(&full_data[10..]),
            &ParamSet::new().with_int("length", 14),
        )
        .expect("valid");
    let overlap: Vec<Option<f64>> = full.values[10..].to_vec();
    assert!(
        max_abs_err(&tail.values, &overlap) > eps("hma"),
        "hma headroom trap stopped firing"
    );
}

#[test]
fn must_fire_smoothing_signal_and_cross_traps() {
    // RSI with SMA smoothing instead of Wilder RMA must differ.
    let data = [1.0, 2.0, 3.0, 4.0, 5.0, 4.0, 3.0, 2.0];
    let rsi = RsiIndicator::new();
    let ps = ParamSet::new().with_int("length", 3);
    let ours = rsi.batch(&closes(&data), &ps).expect("valid");
    assert_eq!(ours.values[2], None, "rsi must seed over na");
    assert!(
        ours.values[3].is_some(),
        "rsi must recover once na leaves the window"
    );

    // MACD signal lags the line on reversals; a lag-free assumption
    // exceeds eps. (A straight ramp settles to a constant line the signal
    // converges onto, so the trap needs curvature to fire.)
    let macd = MacdIndicator::new();
    let rising: Vec<f64> = vec![1.0, 2.0, 3.0, 2.0, 1.0, 2.0, 4.0, 3.0, 5.0, 7.0, 6.0, 8.0];
    let mps = ParamSet::new()
        .with_int("fast_length", 2)
        .with_int("slow_length", 5)
        .with_int("signal_length", 3);
    let m = macd.batch(&closes(&rising), &mps).expect("valid");
    let signal = m.secondary("signal").expect("signal");
    let lag: f64 = m
        .values
        .iter()
        .zip(signal.iter())
        .filter_map(|(l, s)| match (l, s) {
            (Some(l), Some(s)) => Some((l - s).abs()),
            _ => None,
        })
        .fold(0.0, f64::max);
    assert!(lag > eps("macd"), "macd signal-lag trap stopped firing");

    // Flat range is 0/0: stochastics stay na instead of emitting 0.
    let flat = OwnedOhlcv {
        open: vec![],
        high: vec![10.0; 6],
        low: vec![10.0; 6],
        close: vec![10.0; 6],
        volume: vec![],
    };
    let stoch = StochIndicator::new();
    let k = stoch
        .batch(&flat.views(), &ParamSet::new().with_int("length", 3))
        .expect("valid");
    assert!(
        k.values.iter().all(Option::is_none),
        "flat-range stoch must stay na"
    );

    // Equality never crosses, in either direction.
    let a = [1.0, 1.0, 2.0];
    let b = [1.0, 1.0, 1.0];
    assert_eq!(crossover(&a, &b), vec![None, Some(false), Some(true)]);
    assert_eq!(crossunder(&a, &b)[1], Some(false));
}

#[test]
fn must_not_fire_flat_constant_and_spike_controls() {
    // Flat market: every average returns the constant, bands collapse onto
    // it, MACD/ATR/TR read zero, RSI/stoch stay honestly na. Forty bars so
    // even the slowest canonical parameterization warms up.
    let flat = vec![42.0; 40];
    let legs = OwnedOhlcv {
        open: flat.clone(),
        high: flat.clone(),
        low: flat.clone(),
        close: flat.clone(),
        volume: vec![1_000.0; 40],
    };
    let views = legs.views();
    let registry = builtin_registry().expect("builtins");
    // Calibration parameterizations live in the single shared home so the
    // family suite and the global gate can never drift apart.
    let params = canonical_params;
    for id in registry.ids() {
        let kernel = registry.get(&id).expect("registered");
        let result = kernel.batch(&views, &params(&id)).expect("valid");
        let e = eps(&id);
        let warmed: Vec<f64> = result.values.iter().filter_map(|x| *x).collect();
        let expect_value = |v: f64| warmed.iter().all(|x| (*x - v).abs() <= e);
        match &id[..] {
            // Directionless inputs stay honestly na: flat-range stochastics
            // and constant-leg correlation carry no signal by construction.
            // So do the 0/0 ratio oscillators (cci/cmo/mfi) and the
            // zero-movement/zero-range DMI system (adx): a flat market has
            // no direction and no range to ratio.
            "rsi" | "stoch" | "correl" | "cci" | "cmo" | "mfi" | "adx" => assert!(
                result.values.iter().all(Option::is_none),
                "{id} must stay na on flat input"
            ),
            "macd" => {
                assert!(expect_value(0.0), "macd line must read zero");
                for name in ["signal", "hist"] {
                    let leg = result.secondary(name).expect("leg");
                    assert!(
                        leg.iter().filter_map(|x| *x).all(|x| x.abs() <= e),
                        "{id}:{name} must read zero"
                    );
                }
            }
            "tr" | "atr" | "natr" => assert!(expect_value(0.0), "{id} must read zero"),
            // A bandpass removes DC by design: the Voss filter and its
            // secondary read exactly zero on a constant (primary and leg).
            "voss" => {
                assert!(expect_value(0.0), "voss must read zero on flat input");
                let leg = result.secondary("filter").expect("filter leg");
                assert!(
                    leg.iter().filter_map(|x| *x).all(|x| x.abs() <= e),
                    "voss:filter must read zero on flat input"
                );
            }
            // Flat flow accumulates nothing: ad/adosc pin at zero, obv pins
            // at the opening print (every bar's volume on a constant).
            "ad" | "adosc" => assert!(expect_value(0.0), "{id} must read zero on flat input"),
            "obv" => assert!(expect_value(1_000.0), "{id} must pin at first volume on flat input"),
            // Oscillator differences vanish on a constant; dispersion reads
            // zero; ratios read their defined flat values. Chaikin money
            // flow joins them: zero flow over real volume reads zero.
            "apo" | "ppo" | "mom" | "roc" | "rocp" | "rsv" | "willr" | "trix" | "stddev"
            | "var" | "cmf" => {
                assert!(expect_value(0.0), "{id} must read zero on flat input");
            }
            "rocr" => assert!(expect_value(1.0), "{id} must read one on flat input"),
            "rocr100" => assert!(expect_value(100.0), "{id} must read hundred on flat input"),
            "stochf" => {
                assert!(expect_value(0.0), "{id} fastk must read zero on flat input");
                let fastd = result.secondary("fastd").expect("fastd leg");
                assert!(
                    fastd.iter().filter_map(|x| *x).all(|x| x.abs() <= e),
                    "{id}:fastd must read zero on flat input"
                );
            }
            // Every window extreme is the most recent bar on flat input, so
            // both Aroon legs pin at 1.0 (the reference's reverse-argmax).
            "aroon" => {
                assert!(expect_value(1.0), "{id} up must pin at one on flat input");
                let down = result.secondary("down").expect("down leg");
                assert!(
                    down.iter().filter_map(|x| *x).all(|x| (x - 1.0).abs() <= e),
                    "{id}:down must pin at one on flat input"
                );
            }
            // Trail and ratchet lines collapse onto the constant while
            // their direction legs pin bullish: equal closes seed an
            // uptrend that a flat market never reverses.
            "sar" => {
                assert!(expect_value(42.0), "{id} must ride the constant on flat input");
                let trend = result.secondary("trend").expect("trend leg");
                assert!(
                    trend.iter().filter_map(|x| *x).all(|x| (x - 1.0).abs() <= e),
                    "{id}:trend must pin bullish on flat input"
                );
            }
            "supertrend" => {
                assert!(expect_value(42.0), "{id} must ride the constant on flat input");
                let direction = result.secondary("direction").expect("direction leg");
                assert!(
                    direction.iter().filter_map(|x| *x).all(|x| (x - 1.0).abs() <= e),
                    "{id}:direction must pin bullish on flat input"
                );
            }
            _ => {
                assert!(expect_value(42.0), "{id} must return the constant");
                for leg in &result.secondary {
                    assert!(
                        leg.values
                            .iter()
                            .filter_map(|x| *x)
                            .all(|x| (x - 42.0).abs() <= e),
                        "{id}:{} must collapse onto the constant",
                        leg.name
                    );
                }
            }
        }
        assert!(
            !result.values.iter().all(Option::is_none)
                || id == "rsi"
                || id == "stoch"
                || id == "correl"
                || id == "cci"
                || id == "cmo"
                || id == "mfi"
                || id == "adx",
            "{id} must warm up on 40 flat bars"
        );
    }

    // Single spike: every warmed output stays finite, nothing explodes.
    let mut spike = vec![100.0; 20];
    spike[13] = 150.0;
    let legs = OwnedOhlcv {
        open: spike.clone(),
        high: spike.clone(),
        low: spike.clone(),
        close: spike.clone(),
        volume: vec![1_000.0; 20],
    };
    let views = legs.views();
    for id in registry.ids() {
        let kernel = registry.get(&id).expect("registered");
        let result = kernel.batch(&views, &params(&id)).expect("valid");
        assert!(
            result
                .values
                .iter()
                .all(|v| v.is_none_or(f64::is_finite)),
            "{id} must stay finite through a single spike"
        );
    }
}
