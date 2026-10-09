use super::*;
use crate::indicator::ParamSet;
use crate::util::{highest, lowest};

fn params_with_length(length: i64) -> ParamSet {
    ParamSet::new().with_int("length", length)
}

#[test]
fn math_placeholder() {
    // Scaffolding placeholder: replaced by the real math suites below.
    assert_eq!(warmup_none(3).len(), 3);
}

#[test]
fn period_validation_accepts_and_refuses() {
    assert_eq!(period_len("sma", &params_with_length(14), "length").unwrap(), 14);
    let err = period_len("sma", &params_with_length(0), "length").unwrap_err();
    assert_eq!(err.to_string(), "invalid parameter: sma: length must be at least 1, got 0");
    // Missing parameter surfaces the ParamSet error, not a panic.
    assert!(period_len("sma", &ParamSet::new(), "length").is_err());
    // Named parameters (macd's fast/slow/signal legs) share the helper.
    assert_eq!(period_len("macd", &ParamSet::new().with_int("fast_length", 12), "fast_length").unwrap(), 12);
    let err = period_len("macd", &ParamSet::new().with_int("fast_length", 0), "fast_length").unwrap_err();
    assert!(err.to_string().contains("fast_length must be at least 1"));
}

#[test]
fn window_mean_matches_direct_sum() {
    let window = [1.0, 2.0, 3.0, 4.0];
    assert!((window_sum(&window) - 10.0).abs() <= f64::EPSILON);
    assert!((window_mean(&window) - 2.5).abs() <= f64::EPSILON);
    assert!((window_mean(&window) - window_sum(&window) / window.len() as f64).abs() <= f64::EPSILON);
}

#[test]
fn warmup_mask_length_invariants() {
    for len in [0usize, 1, 14, 200] {
        let mask = warmup_none(len);
        assert_eq!(mask.len(), len);
        assert!(mask.iter().all(Option::is_none));
    }
}

#[test]
fn ema_and_wilder_steps_reproduce_recurrences() {
    let period = 14usize;
    let factor = ema_factor(period);
    assert!((factor - 2.0 / 15.0).abs() <= f64::EPSILON);
    // Same op order as the kernel recurrences, bit for bit.
    assert_eq!(ema_step(100.0, 102.0, factor).to_bits(), (102.0 * factor + 100.0 * (1.0 - factor)).to_bits());
    assert_eq!(
        wilder_step(100.0, 102.0, period).to_bits(),
        ((102.0 + 100.0 * (period as f64 - 1.0)) / period as f64).to_bits()
    );
}

#[test]
fn div_guard_refuses_zero_and_non_finite_denominators() {
    assert_eq!(div_or_none(1.0, 2.0), Some(0.5));
    assert_eq!(div_or_none(1.0, 0.0), None);
    assert_eq!(div_or_none(1.0, -0.0), None);
    assert_eq!(div_or_none(1.0, f64::NAN), None);
    assert_eq!(div_or_none(1.0, f64::INFINITY), None);
    // NaN numerators stay `na` through the guard.
    assert_eq!(div_or_none(f64::NAN, 2.0), None);
}

#[test]
fn population_and_sample_stdev_differ_by_bessel_factor() {
    // Textbook window: population variance is exactly 4, stdev exactly 2.
    let window = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
    let n = window.len() as f64;
    assert!((std_pop(&window) - 2.0).abs() <= 1e-12);
    assert!((variance_sample(&window) - 32.0 / 7.0).abs() <= 1e-12);
    let ratio = std_sample(&window) / std_pop(&window);
    let expected = (n / (n - 1.0)).sqrt();
    assert!(
        (ratio - expected).abs() <= 1e-12,
        "sample/pop ratio {ratio} vs Bessel factor {expected}"
    );
}

#[test]
fn self_correlation_is_one_constant_is_none() {
    let series = [1.0, 2.0, 3.0, 5.0, 8.0];
    let corr = corr_pearson(&series, &series).expect("self correlation defined");
    assert!((corr - 1.0).abs() <= 1e-12, "self correlation {corr}");
    let neg = corr_pearson(&[1.0, 2.0, 3.0], &[3.0, 2.0, 1.0]).expect("defined");
    assert!((neg + 1.0).abs() <= 1e-12, "anti-correlation {neg}");
    // Constant legs carry no direction; mismatched and empty inputs refuse.
    assert_eq!(corr_pearson(&[3.0, 3.0, 3.0], &[3.0, 3.0, 3.0]), None);
    assert_eq!(corr_pearson(&[1.0, 2.0], &[1.0]), None);
    assert_eq!(corr_pearson(&[], &[]), None);
    assert_eq!(corr_pearson(&[1.0, f64::NAN], &[1.0, 2.0]), None);
}

#[test]
fn linreg_recovers_affine_line_and_refuses_constant_x() {
    let x = [0.0, 1.0, 2.0, 3.0, 4.0];
    let y = [1.0, 3.0, 5.0, 7.0, 9.0];
    let (slope, intercept) = linreg_slope_intercept(&x, &y).expect("line defined");
    assert!((slope - 2.0).abs() <= 1e-12, "slope {slope}");
    assert!((intercept - 1.0).abs() <= 1e-12, "intercept {intercept}");
    assert_eq!(
        linreg_slope_intercept(&[5.0, 5.0, 5.0], &[1.0, 2.0, 3.0]),
        None
    );
    assert_eq!(linreg_slope_intercept(&[1.0], &[1.0, 2.0]), None);
}

#[test]
fn span_one_ewm_is_identity_and_delegates_to_ema_step() {
    // Bit-exact identity: span 1 carries `alpha = 1`, so the input passes
    // through untouched (compared by bits, as in the golden suites).
    for &input in &[0.0, 100.5, -3.25, 1e12] {
        assert_eq!(
            ewm_span_step(42.0, input, 1).to_bits(),
            input.to_bits(),
            "span-1 must reproduce the input"
        );
    }
    // The span form never forks from the factor recurrence.
    for span in [2usize, 5, 14] {
        assert_eq!(
            ewm_span_step(100.0, 102.0, span).to_bits(),
            ema_step(100.0, 102.0, ema_factor(span)).to_bits()
        );
    }
}

#[test]
fn midpoint_keeps_reference_rounding() {
    // Reference operation order: one rounding of the sum, then an exact
    // halving. Pinned against literal bits so the test never restates the
    // formula it guards.
    assert_eq!(midpoint_of(1.0, 2.0).to_bits(), 1.5f64.to_bits());
    assert_eq!(midpoint_of(10.0, 20.0).to_bits(), 15.0f64.to_bits());
}

#[test]
fn min_periods_mask_marks_positional_warmup() {
    assert_eq!(
        rolling_min_periods_mask(5, 3),
        vec![false, false, true, true, true]
    );
    assert_eq!(rolling_min_periods_mask(0, 3), Vec::<bool>::new());
    assert_eq!(rolling_min_periods_mask(2, 5), vec![false, false]);
    assert_eq!(rolling_min_periods_mask(3, 1), vec![true, true, true]);
}

#[test]
fn promoted_extremes_match_util_on_flat_nan_gap_inputs() {
    let cases: Vec<Vec<f64>> = vec![
        vec![5.0, 5.0, 5.0, 5.0],
        vec![f64::NAN, f64::NAN],
        vec![1.0, f64::NAN, 3.0, f64::NAN, 2.0],
        vec![10.0, 1.0, 10.0, 1.0, 10.0],
        vec![],
        vec![7.0],
    ];
    for source in &cases {
        for length in [1usize, 2, 3, 5] {
            assert_eq!(
                rolling_max(source, length),
                highest(source, length),
                "max drift at length {length} on {source:?}"
            );
            assert_eq!(
                rolling_min(source, length),
                lowest(source, length),
                "min drift at length {length} on {source:?}"
            );
        }
    }
}
