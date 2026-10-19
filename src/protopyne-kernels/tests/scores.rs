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
