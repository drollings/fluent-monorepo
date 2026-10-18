use crate::util::{
    add_expr, argmax_window, argmin_window, change, crossover, crossunder, div_expr, highest,
    lowest, max_window, min_window, mom, mul_expr, sub_expr, sum_window,
};

#[test]
fn crossings_fire_only_on_strict_transitions() {
    let a = [1.0, 2.0, 1.0, 1.0];
    let b = [2.0, 1.0, 2.0, 1.0];
    assert_eq!(
        crossover(&a, &b),
        vec![None, Some(true), Some(false), Some(false)]
    );
    assert_eq!(
        crossunder(&a, &b),
        vec![None, Some(false), Some(true), Some(false)]
    );
}

#[test]
fn crossings_reject_equality_and_missing_data() {
    // Touching without passing through never fires.
    assert_eq!(crossover(&[1.0, 1.0], &[1.0, 1.0]), vec![None, Some(false)]);
    assert_eq!(
        crossunder(&[1.0, 1.0], &[1.0, 1.0]),
        vec![None, Some(false)]
    );
    // NaN on either side is missing data, never a signal.
    assert_eq!(crossover(&[1.0, f64::NAN], &[0.0, 0.0]), vec![None, None]);
}

#[test]
fn window_extremes_and_momentum_by_hand() {
    assert_eq!(
        highest(&[3.0, 1.0, 2.0], 2),
        vec![None, Some(3.0), Some(2.0)]
    );
    assert_eq!(
        lowest(&[3.0, 1.0, 2.0], 2),
        vec![None, Some(1.0), Some(1.0)]
    );
    assert_eq!(change(&[5.0, 7.0, 6.0]), vec![None, Some(2.0), Some(-1.0)]);
    assert_eq!(mom(&[5.0, 7.0, 6.0], 2), vec![None, None, Some(1.0)]);
    // Oversize windows stay honest instead of shrinking silently.
    assert_eq!(highest(&[1.0], 5), vec![None]);
}

#[test]
fn operator_add_mul_commute_and_skip_missing() {
    let nan = f64::NAN;
    let a = [1.0, nan, 3.0, nan];
    let b = [10.0, 20.0, nan, nan];
    // Commutativity holds positionally, including the missing slots.
    assert_eq!(add_expr(&a, &b, &[]), add_expr(&b, &a, &[]));
    assert_eq!(mul_expr(&a, &b, &[]), mul_expr(&b, &a, &[]));
    assert_eq!(
        add_expr(&a, &b, &[]),
        vec![Some(11.0), Some(20.0), Some(3.0), None]
    );
    assert_eq!(
        mul_expr(&a, &b, &[]),
        vec![Some(10.0), Some(20.0), Some(3.0), None]
    );
    // Extra legs fold in; all-missing stays missing.
    let c = [100.0, 200.0, 300.0, nan];
    assert_eq!(
        add_expr(&a, &b, &[&c]),
        vec![Some(111.0), Some(220.0), Some(303.0), None]
    );
}

#[test]
fn operator_sub_div_propagate_and_refuse() {
    let nan = f64::NAN;
    assert_eq!(
        sub_expr(&[5.0, nan, 5.0], &[2.0, 2.0, nan]),
        vec![Some(3.0), None, None]
    );
    // Guarded division: zero and missing denominators refuse instead of
    // emitting inf, the one documented departure from raw `x / y`.
    assert_eq!(
        div_expr(&[1.0, 0.0, nan, 4.0], &[2.0, 0.0, 1.0, 0.0]),
        vec![Some(0.5), None, None, None]
    );
}

#[test]
fn operator_windows_are_strict() {
    let clean = [3.0, 1.0, 2.0, 5.0];
    assert_eq!(
        max_window(&clean, 2),
        vec![None, Some(3.0), Some(2.0), Some(5.0)]
    );
    assert_eq!(
        min_window(&clean, 2),
        vec![None, Some(1.0), Some(1.0), Some(2.0)]
    );
    assert_eq!(
        sum_window(&clean, 2),
        vec![None, Some(4.0), Some(3.0), Some(7.0)]
    );
    // Any NaN-bearing window is None (strict full-window nulling), and
    // oversize windows stay honest.
    let gapped = [3.0, f64::NAN, 2.0, 5.0];
    assert_eq!(max_window(&gapped, 2), vec![None, None, None, Some(5.0)]);
    assert_eq!(min_window(&gapped, 2), vec![None, None, None, Some(2.0)]);
    assert_eq!(sum_window(&gapped, 2), vec![None, None, None, Some(7.0)]);
    assert_eq!(max_window(&[1.0], 5), vec![None]);
}

#[test]
fn operator_strict_windows_diverge_from_pine_skip() {
    // The Pine-skip shared core ignores NaNs; the strict operator maps null
    // any window touching one. Both behaviors are intentional; this test
    // locks the fork point so neither drifts silently.
    let gapped = [3.0, f64::NAN, 2.0];
    assert_eq!(
        crate::math::rolling_max(&gapped, 2),
        vec![None, Some(3.0), Some(2.0)]
    );
    assert_eq!(max_window(&gapped, 2), vec![None, None, None]);
}

#[test]
fn operator_arg_ties_prefer_most_recent() {
    // Ties resolve to distance 0 (most recent bar owns the extreme), the
    // reference's reversed-argmax rule; warming and NaN windows are None.
    assert_eq!(
        argmax_window(&[1.0, 5.0, 5.0, 2.0], 3),
        vec![None, None, Some(0), Some(1)]
    );
    assert_eq!(
        argmin_window(&[4.0, 1.0, 1.0, 3.0], 3),
        vec![None, None, Some(0), Some(1)]
    );
    assert_eq!(
        argmax_window(&[1.0, f64::NAN, 2.0], 2),
        vec![None, None, None]
    );
    assert_eq!(argmin_window(&[1.0], 5), vec![None]);
}
