//! Half-life exponential weighting as free functions over `&[f64]`.
//!
//! The single [`hl_weights`] helper builds the geometric weight ladder the
//! reference constructs inline; the four rolling forms apply it through the
//! same strict-window convention as the series combinators. Research-only
//! alpha machinery: no registry, no compat rows.

use crate::indicator::fin_or_none;
use crate::research::strict_window;

/// Geometric weight ladder: the newest print carries weight one and older
/// prints decay by `0.5^(1 / half_life)` per bar, formula for formula with
/// the reference construction.
#[must_use]
pub fn hl_weights(window: usize, half_life: u32) -> Vec<f64> {
    let step = 0.5f64.powf(1.0 / f64::from(half_life));
    (0..window).map(|k| step.powi((window - 1 - k) as i32)).collect()
}

/// Half-life weighted mean over the strict window.
#[must_use]
pub fn ts_mean_hl(values: &[f64], d: usize, half_life: u32) -> Vec<Option<f64>> {
    weighted_moment(values, d, half_life, Moment::Mean)
}

/// Half-life weighted sum (unnormalized) over the strict window.
#[must_use]
pub fn ts_sum_hl(values: &[f64], d: usize, half_life: u32) -> Vec<Option<f64>> {
    weighted_moment(values, d, half_life, Moment::Sum)
}

/// Half-life weighted standard deviation (population form
/// `sqrt(sum(w x^2) - (sum(w x))^2)` on sum-one weights, transcribing the
/// reference kernel exactly).
#[must_use]
pub fn ts_std_hl(values: &[f64], d: usize, half_life: u32) -> Vec<Option<f64>> {
    weighted_moment(values, d, half_life, Moment::Std)
}

/// Half-life weighted variance under the same population form.
#[must_use]
pub fn ts_var_hl(values: &[f64], d: usize, half_life: u32) -> Vec<Option<f64>> {
    weighted_moment(values, d, half_life, Moment::Var)
}

#[derive(Clone, Copy)]
enum Moment {
    Mean,
    Sum,
    Std,
    Var,
}

fn weighted_moment(values: &[f64], d: usize, half_life: u32, moment: Moment) -> Vec<Option<f64>> {
    if d == 0 || half_life == 0 {
        return vec![None; values.len()];
    }
    let weights = hl_weights(d, half_life);
    let weight_sum: f64 = weights.iter().sum();
    if weight_sum <= 0.0 {
        return vec![None; values.len()];
    }
    let unit: Vec<f64> = weights.iter().map(|w| w / weight_sum).collect();
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            let (wssq, wmean) = window
                .iter()
                .zip(unit.iter())
                .fold((0.0, 0.0), |(wssq, wsum), (v, w)| {
                    (wssq + v * v * w, wsum + v * w)
                });
            fin_or_none(match moment {
                Moment::Mean => wmean,
                Moment::Sum => window.iter().zip(weights.iter()).map(|(v, w)| v * w).sum(),
                Moment::Var => wssq - wmean * wmean,
                Moment::Std => (wssq - wmean * wmean).sqrt(),
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/wq_half_life.rs"]
mod tests;
