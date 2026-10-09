//! Shared indicator math: one home for the recurrences every kernel repeats.
//!
//! Each helper reproduces the exact arithmetic (including op order) of the
//! kernels that compose it, so migrating a kernel onto these helpers never
//! changes its output — the golden suites guard that. [`period_len`] is the
//! DRY home of the per-kernel `length must be at least 1` validation (twelve
//! identical bodies plus the named-leg variant); [`div_or_none`] is the
//! division guard for sites whose denominator is provably nonzero and
//! finite. Sites where the denominator can legitimately be zero (volume
//! -weighted means over empty volume, e.g. `vwma`'s `num / den` fold) keep
//! their raw division on purpose — changing those would alter outputs, so
//! they are documented at the call site instead of migrated.
//!
//! Dispersion has exactly two spellings here and they never mix:
//! [`std_pop`] divides by `n` (band envelopes), [`std_sample`] and
//! [`variance_sample`] divide by `n - 1` (statistic kernels). Rolling
//! extremes likewise live here ([`rolling_max`]/[`rolling_min`]); the
//! window utilities in `util.rs` delegate to them so callers stay
//! byte-identical.

use crate::error::KernelError;
use crate::indicator::{fin_or_none, ParamSet};

/// Validate one positive integer parameter shared by batch and update paths.
///
/// Error text matches the historical per-kernel validators exactly
/// (`"{id}: {name} must be at least 1, got {v}"`), so migrating a kernel's
/// `period`/`lengths` body onto this helper changes no message.
pub fn period_len(indicator: &str, params: &ParamSet, name: &str) -> Result<usize, KernelError> {
    let value = params.int(indicator, name)?;
    if value < 1 {
        return Err(KernelError::InvalidParam(format!(
            "{indicator}: {name} must be at least 1, got {value}"
        )));
    }
    Ok(value as usize)
}

/// Sum of a window, oldest to newest — the direct fold every average starts
/// from.
#[must_use]
pub fn window_sum(window: &[f64]) -> f64 {
    window.iter().sum::<f64>()
}

/// Arithmetic mean of a window (`sum / len`).
#[must_use]
pub fn window_mean(window: &[f64]) -> f64 {
    window_sum(window) / window.len() as f64
}

/// EMA smoothing factor for a period (`2 / (n + 1)`).
#[must_use]
pub fn ema_factor(period: usize) -> f64 {
    2.0 / (period as f64 + 1.0)
}

/// One EMA recurrence step, in kernel op order.
#[must_use]
pub fn ema_step(prev: f64, close: f64, factor: f64) -> f64 {
    close * factor + prev * (1.0 - factor)
}

/// One Wilder-smoothing recurrence step, in kernel op order.
#[must_use]
pub fn wilder_step(prev: f64, close: f64, period: usize) -> f64 {
    (close + prev * (period as f64 - 1.0)) / period as f64
}

/// Guarded division: `Some` of the quotient when the denominator is finite
/// and nonzero, `None` otherwise (Pine `na`). NaN numerators stay `na`
/// through [`fin_or_none`].
#[must_use]
pub fn div_or_none(num: f64, den: f64) -> Option<f64> {
    if den == 0.0 || !den.is_finite() {
        return None;
    }
    fin_or_none(num / den)
}

/// Warmup mask: `len` uncomputable positions. Length always equals the input
/// length — the invariant every kernel's output alignment depends on.
#[must_use]
pub fn warmup_none(len: usize) -> Vec<Option<f64>> {
    vec![None; len]
}

/// Population variance of a window (dividing by `n`). Public alongside
/// [`variance_sample`] so variance kernels use the reference's denominator
/// without re-squaring a deviation (which would fork rounding).
#[must_use]
pub fn variance_pop(window: &[f64]) -> f64 {
    let mean = window_mean(window);
    window
        .iter()
        .map(|v| (v - mean) * (v - mean))
        .sum::<f64>()
        / window.len() as f64
}

/// Population standard deviation of a window (dividing by `n`, matching the
/// band kernels' biased envelope). Same op order as the historical
/// per-kernel loop, so migration is output-identical.
#[must_use]
pub fn std_pop(window: &[f64]) -> f64 {
    variance_pop(window).sqrt()
}

/// Sample variance of a window (dividing by `n - 1`, matching statistic
/// kernels). A single-bar window is `NaN` (no dispersion exists yet), which
/// kernels map back to `None` at the output boundary.
#[must_use]
pub fn variance_sample(window: &[f64]) -> f64 {
    let mean = window_mean(window);
    window
        .iter()
        .map(|v| (v - mean) * (v - mean))
        .sum::<f64>()
        / (window.len() as f64 - 1.0)
}

/// Sample standard deviation of a window (dividing by `n - 1`).
#[must_use]
pub fn std_sample(window: &[f64]) -> f64 {
    variance_sample(window).sqrt()
}

/// Pearson correlation of two aligned windows; `None` when undefined:
/// length mismatch, empty input, or zero/non-finite variance on either side
/// (constant and `na`-bearing windows carry no direction).
#[must_use]
pub fn corr_pearson(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mean_a, mean_b) = (window_mean(a), window_mean(b));
    let mut cov = 0.0;
    let mut var_a = 0.0;
    let mut var_b = 0.0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let (dx, dy) = (x - mean_a, y - mean_b);
        cov += dx * dy;
        var_a += dx * dx;
        var_b += dy * dy;
    }
    let n = a.len() as f64;
    div_or_none(cov / n, (var_a / n).sqrt() * (var_b / n).sqrt())
}

/// Least-squares slope and intercept of `y` on `x`; `None` when the fit is
/// undefined: length mismatch, empty input, or constant/non-finite `x`
/// (no slope exists there).
#[must_use]
pub fn linreg_slope_intercept(x: &[f64], y: &[f64]) -> Option<(f64, f64)> {
    if x.len() != y.len() || x.is_empty() {
        return None;
    }
    let (mean_x, mean_y) = (window_mean(x), window_mean(y));
    let mut cov = 0.0;
    let mut var_x = 0.0;
    for (&xi, &yi) in x.iter().zip(y.iter()) {
        cov += (xi - mean_x) * (yi - mean_y);
        var_x += (xi - mean_x) * (xi - mean_x);
    }
    let slope = div_or_none(cov, var_x)?;
    fin_or_none(mean_y - slope * mean_x).map(|intercept| (slope, intercept))
}

/// One span-parameterized EWM step (`alpha = 2 / (span + 1)`), delegating to
/// the shared [`ema_step`] recurrence so span and factor forms never fork.
/// `span` must be at least 1 (validated by [`period_len`] at call sites);
/// span 1 reproduces the input exactly.
#[must_use]
pub fn ewm_span_step(prev: f64, input: f64, span: usize) -> f64 {
    ema_step(prev, input, ema_factor(span))
}

/// Positional warmup mask for `min_periods` conventions: `len` flags where
/// `true` marks positions with at least `min_periods` preceding values
/// (position `i` sees `i + 1` values). Null-counting stays at the call site;
/// this is the positional floor only.
#[must_use]
pub fn rolling_min_periods_mask(len: usize, min_periods: usize) -> Vec<bool> {
    let floor = min_periods.max(1);
    (0..len).map(|i| i + 1 >= floor).collect()
}

/// Reference-order midpoint of two prices (`(h + l) / 2`).
///
/// This deliberately keeps the reference's rounding instead of the
/// overflow-proof `f64::midpoint`: prices never approach overflow, and
/// matching the reference operation-for-operation is what the parity suites
/// check.
#[must_use]
#[allow(clippy::manual_midpoint)]
pub fn midpoint_of(high: f64, low: f64) -> f64 {
    (high + low) / 2.0
}

/// Rolling highest value over `length` bars; warming positions are `None`.
/// Pine's extremes skip `na` values; only an all-`na` window is `na`.
#[must_use]
pub fn rolling_max(source: &[f64], length: usize) -> Vec<Option<f64>> {
    rolling_extreme(source, length, f64::NEG_INFINITY, f64::max)
}

/// Rolling lowest value over `length` bars; warming positions are `None`.
#[must_use]
pub fn rolling_min(source: &[f64], length: usize) -> Vec<Option<f64>> {
    rolling_extreme(source, length, f64::INFINITY, f64::min)
}

fn rolling_extreme(
    source: &[f64],
    length: usize,
    identity: f64,
    fold: fn(f64, f64) -> f64,
) -> Vec<Option<f64>> {
    let mut out = vec![None; source.len()];
    if length >= 1 && length <= source.len() {
        for (i, slot) in out.iter_mut().enumerate().skip(length - 1) {
            // Pine's extremes skip `na` values; only an all-`na` window is `na`.
            let mut extreme = identity;
            let mut seen = false;
            for &v in &source[i + 1 - length..=i] {
                if !v.is_nan() {
                    extreme = fold(extreme, v);
                    seen = true;
                }
            }
            *slot = if seen { fin_or_none(extreme) } else { None };
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/math.rs"]
mod tests;
