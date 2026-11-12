//! Featureless slice scorers shared by the decision path and the offline
//! research surface.
//!
//! These operate over plain `&[f64]` slices and allocate only their output.
//! They carry no dataframe weight, so a decision crate can reach a scorer
//! without pulling the columnar research closure. The research module
//! re-exports them rather than keeping a second implementation.

use crate::math::{div_or_none, linreg_slope, strict_window, variance_pop};

/// Rolling least-squares slope of `high` on `low` over `n` bars, then its
/// `m`-bar z-score (population denominator): the RSRS timing score.
/// Constant windows (zero slope variance) and short histories read `None` —
/// there is nothing to rank. Window means are demeaned per window for
/// numerical stability; the reference moment form agrees to Expr-parity eps.
#[must_use]
/// Rolling least-squares slope of `high` on `low` over `n` bars, then its
/// `m`-bar z-score (population denominator): the RSRS timing score.
/// Constant windows (zero slope variance) and short histories read `None` —
/// there is nothing to rank. Window means are demeaned per window for
/// numerical stability; the reference moment form agrees to Expr-parity eps.
pub fn rsrs(highs: &[f64], lows: &[f64], n: usize, m: usize) -> Vec<Option<f64>> {
    let len = highs.len().min(lows.len());
    let slopes: Vec<Option<f64>> = (0..len).map(|i| rolling_slope(highs, lows, i, n)).collect();
    (0..len)
        .map(|i| {
            if m == 0 || i + 1 < m {
                return None;
            }
            let window = &slopes[i + 1 - m..=i];
            let mut scores = Vec::with_capacity(m);
            for slope in window.iter().flatten() {
                if slope.is_finite() {
                    scores.push(*slope);
                } else {
                    return None;
                }
            }
            if scores.len() != m {
                return None;
            }
            let mean = scores.iter().sum::<f64>() / m as f64;
            // Population tail shared with the band kernels: `scores` holds
            // exactly `m` finite legs here, so this is the same
            // sum-then-divide-by-`m` fold. The exact-zero guard below is
            // load-bearing (exact sums of squares), not an epsilon lint.
            let variance = variance_pop(&scores);
            #[allow(clippy::float_cmp)]
            if variance == 0.0 {
                return None;
            }
            // zero, so the guarded quotient matches `div_or_none` exactly
            // here: same numerator, same single `sqrt`, same refusal.
            div_or_none(scores[m - 1] - mean, variance.sqrt())
        })
        .collect()
}

/// Least-squares slope of `y` on `x` over the `n`-bar window ending at `end`.
#[must_use]
/// Least-squares slope of `y` on `x` over the `n`-bar window ending at `end`.
pub fn rolling_slope(highs: &[f64], lows: &[f64], end: usize, n: usize) -> Option<f64> {
    let high = strict_window(highs, end, n)?;
    let low = strict_window(lows, end, n)?;
    linreg_slope(low, high)
}

/// The last defined RSRS score: identical to
/// `rsrs(highs, lows, n, m).into_iter().flatten().next_back()`, without
/// building the whole series.
///
/// The full vector exists for the offline research surface; the report
/// path only reads the last score, so materializing 10k slopes and 9.4k
/// z-windows to keep one value is pure waste (profiled at over a third
/// of the equity run). This computes the slope vector once, scans from
/// the tail for the last all-finite `m`-window, and runs the single
/// z-score with the exact same operations — bit-identical output.
#[must_use]
/// The last defined RSRS score: identical to
/// `rsrs(highs, lows, n, m).into_iter().flatten().next_back()`, without
/// building the whole series.
/// 
/// The full vector exists for the offline research surface; the report
/// path only reads the last score, so materializing 10k slopes and 9.4k
/// z-windows to keep one value is pure waste (profiled at over a third
/// of the equity run). This computes the slope vector once, scans from
/// the tail for the last all-finite `m`-window, and runs the single
/// z-score with the exact same operations — bit-identical output.
pub fn rsrs_last(highs: &[f64], lows: &[f64], n: usize, m: usize) -> Option<f64> {
    if m == 0 {
        return None;
    }
    let len = highs.len().min(lows.len());
    if len < m {
        return None;
    }
    let slopes: Vec<Option<f64>> = (0..len).map(|i| rolling_slope(highs, lows, i, n)).collect();
    // Last bar whose `m`-window is fully finite, scanning from the tail:
    // the same acceptance predicate as [`rsrs`], which refuses a window on
    // its first non-finite leg.
    let end = (m..=len).rev().find(|&end| {
        slopes[end - m..end]
            .iter()
            .all(|slope| matches!(slope, Some(value) if value.is_finite()))
    })?;
    let mut scores = Vec::with_capacity(m);
    for slope in slopes[end - m..end].iter().flatten() {
        if slope.is_finite() {
            scores.push(*slope);
        } else {
            return None;
        }
    }
    if scores.len() != m {
        return None;
    }
    let mean = scores.iter().sum::<f64>() / m as f64;
    let variance = variance_pop(&scores);
    #[allow(clippy::float_cmp)]
    if variance == 0.0 {
        return None;
    }
    div_or_none(scores[m - 1] - mean, variance.sqrt())
}

#[cfg(test)]
#[path = "../tests/scores.rs"]
mod tests;