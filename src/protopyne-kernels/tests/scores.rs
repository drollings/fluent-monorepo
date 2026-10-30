//! Focused checks for the featureless scorer home. The full RSRS/Expr parity
//! suite lives with the research module; these pin the shared entry points so
//! the move into a featureless module cannot quietly change them.

use crate::math::linreg_slope;
use crate::scores::{rolling_slope, rsrs};

#[test]
fn rolling_slope_delegates_to_the_shared_fit() {
    let x: Vec<f64> = (0..20).map(|i| 100.0 + f64::from(i) * 0.4).collect();
    let y: Vec<f64> = (0..20).map(|i| 99.0 + f64::from(i) * 0.9).collect();
    let window = 8;
    let end = 19;
    let expected = linreg_slope(&x[end + 1 - window..=end], &y[end + 1 - window..=end]);
    assert_eq!(rolling_slope(&y, &x, end, window), expected);
}

#[test]
fn rsrs_reads_none_on_a_constant_series_and_aligns_its_output() {
    let flat = vec![100.0; 40];
    assert!(rsrs(&flat, &flat, 5, 10).iter().all(Option::is_none));
    let highs: Vec<f64> = (0..40)
        .map(|i| 100.0 + f64::from(i) * 0.7 + (f64::from(i) * 1.3).sin())
        .collect();
    let lows: Vec<f64> = (0..40)
        .map(|i| 99.0 + f64::from(i) * 0.3 + (f64::from(i) * 0.7).cos())
        .collect();
    assert_eq!(rsrs(&highs, &lows, 5, 10).len(), highs.len());
}

fn sample_series(len: usize) -> (Vec<f64>, Vec<f64>) {
    let highs: Vec<f64> = (0..len)
        .map(|i| 100.0 + f64::from(i as u32) * 0.7 + (f64::from(i as u32) * 1.3).sin())
        .collect();
    let lows: Vec<f64> = (0..len)
        .map(|i| 99.0 + f64::from(i as u32) * 0.3 + (f64::from(i as u32) * 0.7).cos())
        .collect();
    (highs, lows)
}

fn rsrs_last_of(highs: &[f64], lows: &[f64], n: usize, m: usize) -> Option<f64> {
    crate::scores::rsrs_last(highs, lows, n, m)
}

#[test]
fn rsrs_last_matches_the_full_vector_tail() {
    // Clean series of several shapes (trend, flat, short, empty, m = 0).
    for len in [0usize, 3, 9, 10, 40, 200] {
        let (highs, lows) = sample_series(len);
        for (n, m) in [(5, 10), (18, 20), (5, 1)] {
            assert_eq!(
                rsrs_last_of(&highs, &lows, n, m),
                rsrs(&highs, &lows, n, m).into_iter().flatten().next_back(),
                "last-value agreement at len {len}, n {n}, m {m}"
            );
        }
    }
    // Constant series: every z-score refuses, so the last refuses too.
    let flat = vec![100.0; 40];
    assert_eq!(rsrs_last_of(&flat, &flat, 5, 10), None);
    assert_eq!(
        rsrs_last_of(&flat, &flat, 5, 10),
        rsrs(&flat, &flat, 5, 10).into_iter().flatten().next_back()
    );
    // Hole in the tail: the last defined score sits before the hole, and
    // `rsrs_last` finds it without building every window after it.
    let (mut highs, lows) = sample_series(60);
    for hole in &mut highs[55..] {
        *hole = f64::NAN;
    }
    assert_eq!(
        rsrs_last_of(&highs, &lows, 5, 10),
        rsrs(&highs, &lows, 5, 10).into_iter().flatten().next_back()
    );
    // Hole at the head only: the tail still defines a score.
    let (mut highs, lows) = sample_series(60);
    highs[0] = f64::NAN;
    assert_eq!(
        rsrs_last_of(&highs, &lows, 5, 10),
        rsrs(&highs, &lows, 5, 10).into_iter().flatten().next_back()
    );
}
