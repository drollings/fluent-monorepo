use crate::util::{change, crossover, crossunder, highest, lowest, mom};

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
    assert_eq!(crossunder(&[1.0, 1.0], &[1.0, 1.0]), vec![None, Some(false)]);
    // NaN on either side is missing data, never a signal.
    assert_eq!(
        crossover(&[1.0, f64::NAN], &[0.0, 0.0]),
        vec![None, None]
    );
}

#[test]
fn window_extremes_and_momentum_by_hand() {
    assert_eq!(highest(&[3.0, 1.0, 2.0], 2), vec![None, Some(3.0), Some(2.0)]);
    assert_eq!(lowest(&[3.0, 1.0, 2.0], 2), vec![None, Some(1.0), Some(1.0)]);
    assert_eq!(change(&[5.0, 7.0, 6.0]), vec![None, Some(2.0), Some(-1.0)]);
    assert_eq!(mom(&[5.0, 7.0, 6.0], 2), vec![None, None, Some(1.0)]);
    // Oversize windows stay honest instead of shrinking silently.
    assert_eq!(highest(&[1.0], 5), vec![None]);
}
