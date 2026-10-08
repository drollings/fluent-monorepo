use super::*;
use crate::volatility::{AtrIndicator, BbIndicator, KcIndicator, TrIndicator};

fn approx(got: &[Option<f64>], want: &[Option<f64>], eps: f64) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want.iter()) {
        match (g, w) {
            (None, None) => {}
            (Some(g), Some(w)) => assert!((g - w).abs() <= eps, "{g} vs {w}"),
            _ => panic!("na mismatch: {g:?} vs {w:?}"),
        }
    }
}

fn ohlcv<'a>(high: &'a [f64], low: &'a [f64], close: &'a [f64]) -> Ohlcv<'a> {
    Ohlcv {
        open: &[],
        high,
        low,
        close,
        volume: &[],
    }
}

fn ticks(high: &[f64], low: &[f64], close: &[f64]) -> Vec<BarTick> {
    high.iter()
        .zip(low.iter())
        .zip(close.iter())
        .map(|((&h, &l), &c)| BarTick {
            open: c,
            high: h,
            low: l,
            close: c,
            volume: 0.0,
        })
        .collect()
}

#[test]
fn true_range_uses_prev_close_gap_by_hand() {
    let tr = TrIndicator::new();
    let data = ohlcv(&[10.0, 11.0], &[9.0, 9.5], &[8.0, 10.0]);
    let result = tr.batch(&data, &ParamSet::new()).expect("valid");
    // Bar 0: 10 - 9 = 1. Bar 1: max(1.5, |11-8|, |9.5-8|) = 3.
    assert_eq!(result.values, vec![Some(1.0), Some(3.0)]);
    let mut state = IndicatorState::new();
    let mut folded = Vec::new();
    for tick in ticks(&[10.0, 11.0], &[9.0, 9.5], &[8.0, 10.0]) {
        folded.push(tr.update(&mut state, tick, &ParamSet::new()).expect("valid"));
    }
    assert_eq!(result.values, folded);
}

#[test]
fn atr_smooths_ranges_by_hand() {
    let atr = AtrIndicator::new();
    // Ranges [1, 3, 2] (see tr test plus a 2-point third bar), length 2:
    // seed = 2, then (2 + 2)/2 = 2.
    let data = ohlcv(&[10.0, 11.0, 12.0], &[9.0, 9.5, 10.0], &[8.0, 10.0, 11.0]);
    let ps = ParamSet::new().with_int("length", 2);
    let result = atr.batch(&data, &ps).expect("valid");
    assert_eq!(result.values, vec![None, Some(2.0), Some(2.0)]);
    let mut state = IndicatorState::new();
    let mut folded = Vec::new();
    for tick in ticks(&[10.0, 11.0, 12.0], &[9.0, 9.5, 10.0], &[8.0, 10.0, 11.0]) {
        folded.push(atr.update(&mut state, tick, &ps).expect("valid"));
    }
    assert_eq!(result.values, folded);
}

#[test]
fn bollinger_bands_use_population_stdev_by_hand() {
    let bb = BbIndicator::new();
    // mean 3, biased dev sqrt(2/3): sample stdev (1.0) would widen the bands.
    let data = [1.0, 2.0, 3.0, 4.0, 5.0];
    let ps = ParamSet::new()
        .with_int("length", 3)
        .with_float("mult", 2.0);
    let result = bb
        .batch(&Ohlcv::close_only(&data), &ps)
        .expect("valid");
    let dev = (2.0_f64 / 3.0).sqrt();
    approx(&result.values[2..3], &[Some(2.0)], 1e-9);
    let upper = result.secondary("upper").expect("upper leg");
    let lower = result.secondary("lower").expect("lower leg");
    approx(&upper[2..3], &[Some(2.0 + 2.0 * dev)], 1e-9);
    approx(&lower[2..3], &[Some(2.0 - 2.0 * dev)], 1e-9);
    let mut state = IndicatorState::new();
    let mut primary = Vec::new();
    let mut upper_fold = Vec::new();
    let mut lower_fold = Vec::new();
    for &c in &data {
        let full = bb
            .update_full(&mut state, BarTick::close_only(c), &ps)
            .expect("valid");
        primary.push(full.primary);
        upper_fold.push(full.secondary[0].value);
        lower_fold.push(full.secondary[1].value);
    }
    assert_eq!(result.values, primary);
    assert_eq!(upper, &upper_fold);
    assert_eq!(lower, &lower_fold);
}

#[test]
fn keltner_channels_pair_basis_with_smoothed_range_by_hand() {
    let kc = KcIndicator::new();
    // Ranges all 2 (high-low with no gaps), length 3, mult 1:
    // basis[2] = 2, span[2] = 2 -> upper 4, lower 0.
    let data = ohlcv(
        &[2.0, 3.0, 4.0, 5.0, 6.0],
        &[0.0, 1.0, 2.0, 3.0, 4.0],
        &[1.0, 2.0, 3.0, 4.0, 5.0],
    );
    let ps = ParamSet::new()
        .with_int("length", 3)
        .with_float("mult", 1.0);
    let result = kc.batch(&data, &ps).expect("valid");
    approx(&result.values[2..3], &[Some(2.0)], 1e-9);
    let upper = result.secondary("upper").expect("upper leg");
    let lower = result.secondary("lower").expect("lower leg");
    approx(&upper[2..3], &[Some(4.0)], 1e-9);
    approx(&lower[2..3], &[Some(0.0)], 1e-9);
}
