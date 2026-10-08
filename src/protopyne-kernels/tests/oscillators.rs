use super::*;
use crate::oscillators::{MacdIndicator, RsiIndicator, StochIndicator};

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

fn closes(data: &[f64]) -> Ohlcv<'_> {
    Ohlcv::close_only(data)
}

fn fold_all(
    kernel: &impl Indicator,
    ticks: &[BarTick],
    params: &ParamSet,
) -> (Vec<Option<f64>>, Vec<Vec<NamedValue>>) {
    let mut state = IndicatorState::new();
    let mut primary = Vec::new();
    let mut secondary = Vec::new();
    for &tick in ticks {
        let full = kernel
            .update_full(&mut state, tick, params)
            .expect("valid update");
        primary.push(full.primary);
        secondary.push(full.secondary);
    }
    (primary, secondary)
}

fn close_ticks(data: &[f64]) -> Vec<BarTick> {
    data.iter().map(|&c| BarTick::close_only(c)).collect()
}

#[test]
fn rsi_seed_and_wilder_recurrence_by_hand() {
    let rsi = RsiIndicator::new();
    // Gains [na,1,1,1,1,0,0,0], losses [na,0,0,0,0,1,1,1] (length 3):
    // index 2 still seeds over na -> None; 3 and 4 all-gain -> 100;
    // 5 -> 100 - 100/3; 6 -> 400/9; 7 -> 800/27.
    let data = [1.0, 2.0, 3.0, 4.0, 5.0, 4.0, 3.0, 2.0];
    let ps = ParamSet::new().with_int("length", 3);
    let result = rsi.batch(&closes(&data), &ps).expect("valid");
    approx(
        &result.values,
        &[
            None,
            None,
            None,
            Some(100.0),
            Some(100.0),
            Some(100.0 - 100.0 / 3.0),
            Some(400.0 / 9.0),
            Some(800.0 / 27.0),
        ],
        1e-9,
    );
    let (primary, _) = fold_all(&rsi, &close_ticks(&data), &ps);
    assert_eq!(result.values, primary);
    assert_eq!(rsi.warmup(&ps).expect("warmup"), 4);
}

#[test]
fn rsi_flat_market_stays_na() {
    let rsi = RsiIndicator::new();
    let data = [50.0; 8];
    let result = rsi
        .batch(&closes(&data), &ParamSet::new().with_int("length", 3))
        .expect("valid");
    assert!(result.values.iter().all(Option::is_none));
}

#[test]
fn macd_linear_series_gives_constant_line_by_hand() {
    let macd = MacdIndicator::new();
    // Rising 1..=5, fast 2 / slow 3 / signal 2: line settles at 0.5,
    // signal follows one bar later, hist converges to 0.
    let data = [1.0, 2.0, 3.0, 4.0, 5.0];
    let ps = ParamSet::new()
        .with_int("fast_length", 2)
        .with_int("slow_length", 3)
        .with_int("signal_length", 2);
    let result = macd.batch(&closes(&data), &ps).expect("valid");
    approx(
        &result.values,
        &[None, None, Some(0.5), Some(0.5), Some(0.5)],
        1e-9,
    );
    let signal = result.secondary("signal").expect("signal leg");
    approx(&signal[3..], &[Some(0.5), Some(0.5)], 1e-9);
    let hist = result.secondary("hist").expect("hist leg");
    approx(&hist[3..], &[Some(0.0), Some(0.0)], 1e-9);
    let (primary, secondary) = fold_all(&macd, &close_ticks(&data), &ps);
    assert_eq!(result.values, primary);
    for (i, tick) in secondary.iter().enumerate() {
        assert_eq!(tick[0].value, signal[i]);
        assert_eq!(tick[1].value, hist[i]);
    }
}

#[test]
fn stoch_k_and_smoothed_d_by_hand() {
    let stoch = StochIndicator::new();
    let data = Ohlcv {
        open: &[],
        high: &[10.0, 11.0, 12.0, 13.0, 14.0, 15.0],
        low: &[5.0, 6.0, 7.0, 8.0, 9.0, 10.0],
        close: &[7.0, 8.0, 9.0, 10.0, 11.0, 12.0],
        volume: &[],
    };
    let ps = ParamSet::new().with_int("length", 3);
    let result = stoch.batch(&data, &ps).expect("valid");
    // Every window spans 7 points with close 2 above the low: k = 400/7.
    let k = 400.0 / 7.0;
    approx(
        &result.values,
        &[None, None, Some(k), Some(k), Some(k), Some(k)],
        1e-9,
    );
    let d = result.secondary("d").expect("d leg");
    approx(&d[4..], &[Some(k), Some(k)], 1e-9);
    assert!(d[3].is_none());
}
