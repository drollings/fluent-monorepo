use super::*;
use crate::indicator::ParamSet;

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
