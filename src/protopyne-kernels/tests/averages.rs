use super::*;
use crate::averages::{
    AlmaIndicator, DemaIndicator, HmaIndicator, RmaIndicator, TemaIndicator, VwmaIndicator,
    WmaIndicator,
};

fn len_params(length: i64) -> ParamSet {
    ParamSet::new().with_int("length", length)
}

fn closes(data: &[f64]) -> Ohlcv<'_> {
    Ohlcv::close_only(data)
}

/// Approximate equality for hand-computed vectors; exact equality is proven
/// separately by the batch/update fold tests below.
fn assert_approx(got: &[Option<f64>], want: &[Option<f64>], eps: f64) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want.iter()) {
        match (g, w) {
            (None, None) => {}
            (Some(g), Some(w)) => assert!((g - w).abs() <= eps, "{g} vs {w}"),
            _ => panic!("na mismatch: {g:?} vs {w:?}"),
        }
    }
}

fn fold_closes(kernel: &impl Indicator, closes: &[f64], params: &ParamSet) -> Vec<Option<f64>> {
    let mut state = IndicatorState::new();
    closes
        .iter()
        .map(|&c| {
            kernel
                .update(&mut state, BarTick::close_only(c), params)
                .expect("valid update")
        })
        .collect()
}

#[test]
fn rma_matches_hand_computed_wilder_recurrence() {
    let rma = RmaIndicator::new();
    let data = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let result = rma.batch(&closes(&data), &len_params(3)).expect("valid");
    // Seed = 2; then (x + prev*2)/3: 8/3, 31/9, 116/27.
    assert_approx(
        &result.values,
        &[
            None,
            None,
            Some(2.0),
            Some(8.0 / 3.0),
            Some(31.0 / 9.0),
            Some(116.0 / 27.0),
        ],
        1e-9,
    );
    assert_eq!(result.values, fold_closes(&rma, &data, &len_params(3)));
}

#[test]
fn wma_weights_current_bar_heaviest() {
    let wma = WmaIndicator::new();
    let data = [1.0, 2.0, 3.0, 4.0];
    let result = wma.batch(&closes(&data), &len_params(3)).expect("valid");
    // (3*9 + 2*6 + 1*3)/18 = 42/18; (4*9 + 3*6 + 2*3)/18 = 60/18.
    // Reversed weights would give 30/18 — the direction is the trap.
    assert_approx(
        &result.values,
        &[None, None, Some(42.0 / 18.0), Some(60.0 / 18.0)],
        1e-9,
    );
    assert_eq!(result.values, fold_closes(&wma, &data, &len_params(3)));
}

#[test]
fn hma_composes_wma_without_divergence() {
    let hma = HmaIndicator::new();
    // length 4 -> half 2, full 4, root 2. First value lands at index 4.
    let data = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
    let result = hma.batch(&closes(&data), &len_params(4)).expect("valid");
    assert_approx(
        &result.values,
        &[
            None,
            None,
            None,
            None,
            Some(5.0),
            Some(6.0),
            Some(7.0),
            Some(8.0),
        ],
        1e-9,
    );
    assert_eq!(result.values, fold_closes(&hma, &data, &len_params(4)));
    assert_eq!(hma.warmup(&len_params(4)).expect("warmup"), 5);
}

#[test]
fn vwma_weights_by_volume() {
    let vwma = VwmaIndicator::new();
    let data = Ohlcv {
        open: &[],
        high: &[],
        low: &[],
        close: &[1.0, 2.0, 3.0, 4.0],
        volume: &[10.0, 20.0, 30.0, 40.0],
    };
    let result = vwma.batch(&data, &len_params(2)).expect("valid");
    assert_approx(
        &result.values,
        &[
            None,
            Some(50.0 / 30.0),
            Some(130.0 / 50.0),
            Some(250.0 / 70.0),
        ],
        1e-9,
    );
    // Incremental path over matching ticks.
    let ticks = [(1.0, 10.0), (2.0, 20.0), (3.0, 30.0), (4.0, 40.0)];
    let mut state = IndicatorState::new();
    let ps = len_params(2);
    let mut folded = Vec::new();
    for (c, v) in ticks {
        folded.push(
            vwma.update(
                &mut state,
                BarTick {
                    open: c,
                    high: c,
                    low: c,
                    close: c,
                    volume: v,
                },
                &ps,
            )
            .expect("valid"),
        );
    }
    assert_eq!(result.values, folded);
}

#[test]
fn alma_peaks_at_offset_and_stays_in_window() {
    let alma = AlmaIndicator::new();
    let ps = ParamSet::new()
        .with_int("length", 3)
        .with_float("offset", 0.85)
        .with_float("sigma", 6.0);
    let data = [1.0, 2.0, 3.0, 10.0, 3.0, 2.0, 1.0];
    let result = alma.batch(&closes(&data), &ps).expect("valid");
    // Gaussian weights peak 1.7 bars back: the response tracks the window
    // mean tilted toward recent bars, always inside the window extremes.
    assert!(result.values[0].is_none() && result.values[1].is_none());
    for (v, w) in result.values.iter().skip(2).zip(data.windows(3)) {
        let v = v.expect("warmed");
        let lo = w.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = w.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(v >= lo && v <= hi, "{v} outside [{lo}, {hi}]");
    }
    assert_eq!(result.values, fold_closes(&alma, &data, &ps));
    assert!(alma
        .batch(&closes(&data), &ParamSet::new().with_int("length", 3))
        .is_err());
}

#[test]
fn invalid_periods_are_typed_errors() {
    let ps = len_params(0);
    let data = closes(&[1.0, 2.0, 3.0]);
    assert!(RmaIndicator::new().batch(&data, &ps).is_err());
    assert!(WmaIndicator::new().batch(&data, &ps).is_err());
    assert!(HmaIndicator::new().batch(&data, &ps).is_err());
    assert!(VwmaIndicator::new().batch(&data, &ps).is_err());
    assert!(DemaIndicator::new().batch(&data, &ps).is_err());
    assert!(TemaIndicator::new().batch(&data, &ps).is_err());
}

#[test]
fn dema_removes_single_pass_lag_on_ramps() {
    let dema = DemaIndicator::new();
    let data = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let result = dema.batch(&closes(&data), &len_params(3)).expect("valid");
    // ema1 = [na, na, 2, 3, 4, 5]; ema2 = [na ×4, 3, 4];
    // dema = [na ×4, 2·4−3, 2·5−4] = [na ×4, 5, 6].
    assert_approx(
        &result.values,
        &[None, None, None, None, Some(5.0), Some(6.0)],
        1e-9,
    );
    assert_eq!(result.values, fold_closes(&dema, &data, &len_params(3)));
    assert_eq!(dema.warmup(&len_params(3)).expect("valid"), 5);
}

#[test]
fn tema_tracks_ramps_with_no_residual_lag() {
    let tema = TemaIndicator::new();
    let data = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
    let result = tema.batch(&closes(&data), &len_params(3)).expect("valid");
    // ema3 seeds at index 6 with mean(3, 4, 5) = 4; tema = 3·e1 − 3·e2 + e3
    // reads [na ×6, 7, 8]: zero lag on a steady ramp.
    assert_approx(
        &result.values,
        &[None, None, None, None, None, None, Some(7.0), Some(8.0)],
        1e-9,
    );
    assert_eq!(result.values, fold_closes(&tema, &data, &len_params(3)));
    assert_eq!(tema.warmup(&len_params(3)).expect("valid"), 7);
}

#[test]
fn double_and_triple_passes_agree_incrementally() {
    // A non-monotone window exercises the NaN-reseed shape in every pass.
    let data = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0, 5.0, 3.0];
    for length in [2, 3, 5] {
        let dema = DemaIndicator::new();
        let params = len_params(length);
        let batch = dema.batch(&closes(&data), &params).expect("valid");
        assert_eq!(batch.values, fold_closes(&dema, &data, &params));
        let tema = TemaIndicator::new();
        let batch = tema.batch(&closes(&data), &params).expect("valid");
        assert_eq!(batch.values, fold_closes(&tema, &data, &params));
        assert_eq!(batch.len(), data.len());
    }
}
