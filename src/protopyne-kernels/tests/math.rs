use super::*;
use crate::indicator::{fin_or_none, ParamSet};
use crate::util::{highest, lowest, max_window};

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
    assert_eq!(
        period_len("sma", &params_with_length(14), "length").unwrap(),
        14
    );
    let err = period_len("sma", &params_with_length(0), "length").unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid parameter: sma: length must be at least 1, got 0"
    );
    // Missing parameter surfaces the ParamSet error, not a panic.
    assert!(period_len("sma", &ParamSet::new(), "length").is_err());
    // Named parameters (macd's fast/slow/signal legs) share the helper.
    assert_eq!(
        period_len(
            "macd",
            &ParamSet::new().with_int("fast_length", 12),
            "fast_length"
        )
        .unwrap(),
        12
    );
    let err = period_len(
        "macd",
        &ParamSet::new().with_int("fast_length", 0),
        "fast_length",
    )
    .unwrap_err();
    assert!(err.to_string().contains("fast_length must be at least 1"));
}

#[test]
fn window_mean_matches_direct_sum() {
    let window = [1.0, 2.0, 3.0, 4.0];
    assert!((window_sum(&window) - 10.0).abs() <= f64::EPSILON);
    assert!((window_mean(&window) - 2.5).abs() <= f64::EPSILON);
    assert!(
        (window_mean(&window) - window_sum(&window) / window.len() as f64).abs() <= f64::EPSILON
    );
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
    assert_eq!(
        ema_step(100.0, 102.0, factor).to_bits(),
        (102.0 * factor + 100.0 * (1.0 - factor)).to_bits()
    );
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
fn linreg_slope_leg_matches_pair_form_bit_exact() {
    // The slope leg never forks from the pair form: bit-identical first
    // element on every defined fit, `None` wherever the pair refuses.
    let cases: Vec<(Vec<f64>, Vec<f64>)> = vec![
        (vec![0.0, 1.0, 2.0, 3.0, 4.0], vec![1.0, 3.0, 5.0, 7.0, 9.0]),
        (vec![1.0, 2.0, 3.0], vec![2.0, 2.0, 2.0]),
        (vec![-3.0, -1.0, 0.5, 2.0], vec![4.0, 1.0, -2.0, 0.0]),
        (vec![5.0, 5.0, 5.0], vec![1.0, 2.0, 3.0]),
        (vec![], vec![]),
        (vec![1.0], vec![1.0, 2.0]),
        (vec![1.0, f64::NAN], vec![1.0, 2.0]),
        (vec![1.0, 2.0], vec![1.0, f64::NAN]),
        (vec![1.0, f64::INFINITY], vec![1.0, 2.0]),
        (vec![1e-300, 2e-300], vec![3.0, 5.0]),
    ];
    for (x, y) in &cases {
        let pair = linreg_slope_intercept(x, y);
        let slope = linreg_slope(x, y);
        assert_eq!(
            slope,
            pair.map(|(s, _)| s),
            "slope leg agrees on {x:?} / {y:?}"
        );
        if let (Some(got), Some((want, _))) = (slope, pair) {
            assert_eq!(got.to_bits(), want.to_bits(), "bit-exact slope on {x:?}");
        }
    }
    // Exact `sxx == 0` boundary: an exactly constant window refuses, while
    // a window with any exact spread defines a slope.
    assert_eq!(linreg_slope(&[5.0, 5.0, 5.0], &[1.0, 2.0, 3.0]), None);
    assert!(linreg_slope(&[5.0, 5.0, 6.0], &[1.0, 2.0, 3.0]).is_some());
    // Non-finite legs refuse through the shared guards, never a value.
    assert_eq!(linreg_slope(&[1.0, f64::NAN], &[1.0, 2.0]), None);
    assert_eq!(linreg_slope(&[1.0, 2.0], &[1.0, f64::INFINITY]), None);
    assert_eq!(linreg_slope(&[], &[]), None);
    assert_eq!(linreg_slope(&[1.0], &[1.0, 2.0]), None);
}

#[test]
fn div_or_none_agrees_with_guarded_division_exactly() {
    // The migration contract for every `fin_or_none(num / den)` call site
    // (`fin_or_none` keeps ±inf: only NaN reads `None`). `div_or_none`
    // agrees with the guarded division on every input except two
    // overflow-class edges: a zero denominator with a nonzero numerator
    // (guarded reads `Some(±inf)`, the primitive refuses) and an infinite
    // denominator with a finite numerator (guarded reads `Some(±0)`).
    // Every migrated site proves its numerator cannot hit those edges
    // (bounded by its denominator, or a provably positive constant);
    // the sweep below pins the exact divergence set so the proofs check
    // against something.
    let mut seed = 0x51E3_25C7_9B11_F00Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        f64::from_bits(seed)
    };
    let mut specials: Vec<(f64, f64)> = vec![
        (1.0, 0.0),
        (0.0, 0.0),
        (-1.0, 0.0),
        (f64::INFINITY, 0.0),
        (f64::NAN, 1.0),
        (1.0, f64::NAN),
        (f64::NAN, 0.0),
        (1.0, f64::INFINITY),
        (1.0, f64::NEG_INFINITY),
        (f64::INFINITY, 2.0),
        (f64::INFINITY, f64::INFINITY),
        (0.0, f64::INFINITY),
        (f64::NAN, f64::INFINITY),
        (5.0, f64::MIN_POSITIVE),
        (5.0, -0.0),
        (0.0, -0.0),
    ];
    for _ in 0..500 {
        specials.push((next(), next()));
    }
    for (num, den) in specials {
        let guarded = fin_or_none(num / den);
        let primitive = div_or_none(num, den);
        let diverges =
            (den == 0.0 && num != 0.0 && !num.is_nan()) || (den.is_infinite() && num.is_finite());
        if diverges {
            assert_eq!(primitive, None, "primitive refuses {num} / {den}");
            assert!(
                guarded.is_some(),
                "guarded division keeps ±inf/±0 on {num} / {den}"
            );
        } else {
            assert_eq!(primitive, guarded, "agreement on {num} / {den}");
        }
    }
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

#[test]
fn drawdown_of_peak_measures_peak_to_trough() {
    use rust_decimal::Decimal;
    let peak = Decimal::from(1000);
    // (1000 − 900) / 1000 = 0.1 exactly in decimal.
    assert_eq!(
        drawdown_of_peak(peak, Decimal::from(900)),
        Some(Decimal::new(1, 1))
    );
    // At the peak there is no drawdown — defined, and never a breach.
    assert_eq!(drawdown_of_peak(peak, peak), Some(Decimal::ZERO));
    // Above the peak is negative headroom, not a breach.
    assert_eq!(
        drawdown_of_peak(peak, Decimal::from(1100)),
        Some(Decimal::new(-1, 1))
    );
    // No peak to fall from: zero, negative, and overflowed peaks refuse.
    assert_eq!(drawdown_of_peak(Decimal::ZERO, Decimal::from(100)), None);
    assert_eq!(
        drawdown_of_peak(Decimal::from(-50), Decimal::from(-60)),
        None
    );
    assert_eq!(
        drawdown_of_peak(Decimal::MAX, Decimal::MIN),
        None,
        "a trough that overflows the subtraction refuses"
    );
}

#[test]
fn strict_window_characterization_warming_and_nan_policy() {
    // Warming: fewer than `period` bars ending at `end` refuses.
    assert_eq!(strict_window(&[1.0, 2.0], 1, 3), None);
    // Exact-length window at the boundary is accepted.
    assert_eq!(
        strict_window(&[1.0, 2.0, 3.0], 2, 3),
        Some(&[1.0, 2.0, 3.0][..])
    );
    // Empty input and zero period always refuse.
    assert_eq!(strict_window(&[], 0, 3), None);
    assert_eq!(strict_window(&[1.0, 2.0], 1, 0), None);
    // `end` past the input refuses instead of panicking.
    assert_eq!(strict_window(&[1.0, 2.0], 5, 1), None);
    // NaN anywhere in the window refuses (head, middle, tail).
    assert_eq!(strict_window(&[f64::NAN, 2.0, 3.0], 2, 3), None);
    assert_eq!(strict_window(&[1.0, f64::NAN, 3.0], 2, 3), None);
    assert_eq!(strict_window(&[1.0, 2.0, f64::NAN], 2, 3), None);
    // All-NaN refuses.
    assert_eq!(strict_window(&[f64::NAN, f64::NAN, f64::NAN], 2, 3), None);
}

#[test]
#[allow(clippy::float_cmp)]
fn fold_helpers_skip_nan_and_pin_seed_on_all_missing() {
    // Holes are skipped, not poisoning (Pine skip-`na` contract).
    assert_eq!(fold_skip_nan_max([3.0, f64::NAN, 2.0]), 3.0);
    assert_eq!(fold_skip_nan_min([3.0, f64::NAN, 2.0]), 2.0);
    // Single-value windows read the value.
    assert_eq!(fold_skip_nan_max([5.0]), 5.0);
    assert_eq!(fold_skip_nan_min([5.0]), 5.0);
    // All-`NaN` reads the seed; callers map it (flat-range checks).
    assert_eq!(fold_skip_nan_max([f64::NAN, f64::NAN]), f64::NEG_INFINITY);
    assert_eq!(fold_skip_nan_min([f64::NAN, f64::NAN]), f64::INFINITY);
    // Strict twins agree on finite inputs (same implementation, named
    // contract only).
    assert_eq!(fold_strict_max([3.0, 1.0, 2.0]), 3.0);
    assert_eq!(fold_strict_min([3.0, 1.0, 2.0]), 1.0);
    // `inf` is content, not a hole, in both spellings.
    assert_eq!(fold_skip_nan_max([1.0, f64::INFINITY]), f64::INFINITY);
    assert_eq!(
        fold_skip_nan_min([1.0, f64::NEG_INFINITY]),
        f64::NEG_INFINITY
    );
}

#[test]
fn extreme_position_tie_breaks_and_nan_policy() {
    // Most-recent tie wins under `Last` (the reference reversed-argmax).
    assert_eq!(
        extreme_position(&[1.0, 5.0, 5.0, 2.0], true, TieBreak::Last),
        Some(2)
    );
    // Earliest tie wins under `First` (WorldQuant forward search).
    assert_eq!(
        extreme_position(&[1.0, 5.0, 5.0, 2.0], true, TieBreak::First),
        Some(1)
    );
    assert_eq!(
        extreme_position(&[4.0, 1.0, 1.0, 3.0], false, TieBreak::Last),
        Some(2)
    );
    assert_eq!(
        extreme_position(&[4.0, 1.0, 1.0, 3.0], false, TieBreak::First),
        Some(1)
    );
    // Any hole voids the window in both breaks.
    assert_eq!(
        extreme_position(&[1.0, f64::NAN, 2.0], true, TieBreak::Last),
        None
    );
    assert_eq!(
        extreme_position(&[1.0, f64::NAN, 2.0], false, TieBreak::First),
        None
    );
    // Empty window has no extreme.
    assert_eq!(extreme_position(&[], true, TieBreak::Last), None);
}

#[test]
fn strict_window_refuses_inf_while_util_allows_it() {
    // Pinned disagreement (M1.1): `math::strict_window` gates on
    // `is_finite` and refuses `inf`; `util`'s strict windows gate on
    // `!is_nan` and allow `inf` through `fin_or_none` (which only strips
    // NaN). Unifying the predicate would change one side's output, so
    // both spellings stay until a behavior proposal says otherwise.
    assert_eq!(strict_window(&[1.0, f64::INFINITY, 3.0], 2, 3), None);
    assert_eq!(strict_window(&[1.0, 2.0, f64::NEG_INFINITY], 2, 3), None);
    assert_eq!(
        max_window(&[1.0, f64::INFINITY, 3.0], 3),
        vec![None, None, Some(f64::INFINITY)]
    );
}

#[test]
#[allow(clippy::float_cmp)]
fn seed_or_step_recurses_on_finite_previous_and_seeds_otherwise() {
    // Finite previous steps forward with the closure's recurrence.
    assert_eq!(
        seed_or_step(Some(1.0), 2.0, 99.0, |p, n| p + n),
        3.0,
        "finite previous recurses"
    );
    // Missing or `NaN` previous seeds from the window mean.
    assert_eq!(
        seed_or_step(None, 2.0, 1.5, |p, n| p + n),
        1.5,
        "missing previous seeds"
    );
    assert_eq!(
        seed_or_step(Some(f64::NAN), 2.0, 1.5, |p, n| p + n),
        1.5,
        "`NaN` previous seeds"
    );
}
