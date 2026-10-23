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
use rust_decimal::Decimal;

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

/// Strict trailing window ending at `end` (inclusive): `None` while warming
/// or when any leg in the window is non-finite. Output length always equals
/// the input length; callers map the window to a value or `None`.
///
/// Shared with the series combinators, which impose the same
/// min-periods-as-warmup convention over bare slices.
pub(crate) fn strict_window(values: &[f64], end: usize, period: usize) -> Option<&[f64]> {
    if period == 0 || end + 1 < period || values.len() <= end {
        return None;
    }
    let window = &values[end + 1 - period..=end];
    if window.iter().all(|v| v.is_finite()) {
        Some(window)
    } else {
        None
    }
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
    window.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / window.len() as f64
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
    window.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / (window.len() as f64 - 1.0)
}

/// Drawdown of one reading against its peak, as a fraction of the peak:
/// `(peak − now) / peak`. The one home for peak-to-trough arithmetic — the
/// series fold in `protopyne-server::metrics` delegates to this per step, so
/// there is exactly one spelling of the division.
///
/// `None` when there is no peak to fall from (a non-positive peak) or the
/// math overflows: no peak, no drawdown, never a zero. A reading at or above
/// the peak reports zero or headroom — defined, and never a breach.
#[must_use]
pub fn drawdown_of_peak(peak: Decimal, now: Decimal) -> Option<Decimal> {
    if peak <= Decimal::ZERO {
        return None;
    }
    peak.checked_sub(now)?.checked_div(peak)
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

/// Least-squares slope of `y` on `x`; `None` when the fit is undefined:
/// length mismatch, empty input, or constant/non-finite `x` (no slope
/// exists there). Holds the shared fit core: [`linreg_slope_intercept`]
/// delegates to this for its slope leg, so the two never fork.
///
/// This is the canonical per-window fit for RSRS-style rolling regressions;
/// the scoring tail composes [`variance_pop`] + [`fin_or_none`] (same
/// sum-then-divide-by-`m`, `sqrt` once) or [`standardize_scores`] for the
/// whole-window form. The slope leg deliberately carries no intercept
/// finiteness gate: a finite slope stays `Some` even where
/// `mean_y - slope * mean_x` would overflow, matching the historical
/// rolling-fit callers that only score the slope.
#[must_use]
pub fn linreg_slope(x: &[f64], y: &[f64]) -> Option<f64> {
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
    div_or_none(cov, var_x)
}

/// Least-squares slope and intercept of `y` on `x`; `None` when the fit is
/// undefined: length mismatch, empty input, or constant/non-finite `x`
/// (no slope exists there). The slope leg is [`linreg_slope`]; the
/// intercept adds `fin_or_none(mean_y - slope * mean_x)`, so an
/// overflowing intercept still refuses the pair while the slope reads
/// `Some`.
#[must_use]
pub fn linreg_slope_intercept(x: &[f64], y: &[f64]) -> Option<(f64, f64)> {
    let slope = linreg_slope(x, y)?;
    let (mean_x, mean_y) = (window_mean(x), window_mean(y));
    fin_or_none(mean_y - slope * mean_x).map(|intercept| (slope, intercept))
}

/// Median of present values over a sorted copy: the middle element on odd
/// counts, the midpoint of the two middles on even counts. Same rule as the
/// `ta.median` bridge arm, which delegates here so the two never fork.
#[must_use]
pub fn median_of(values: &[f64]) -> Option<f64> {
    let mut scratch = Vec::new();
    median_of_with(values, &mut scratch)
}

/// [`median_of`] with a caller-provided scratch buffer: copies the window in
/// (the caller's slice is never mutated) and **selects** the middle instead of
/// sorting the whole window. Same values, O(n) instead of O(n log n) per call;
/// `scratch` keeps its capacity across calls, so a hot caller allocates once.
#[must_use]
pub fn median_of_with(values: &[f64], scratch: &mut Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    scratch.clear();
    scratch.extend_from_slice(values);
    let len = scratch.len();
    let middle = len / 2;
    let (less, &mut middle_value, _) = scratch.select_nth_unstable_by(middle, f64::total_cmp);
    if len % 2 == 1 {
        Some(middle_value)
    } else {
        // An even window: the lower median is the largest of the `middle`
        // elements the selection put before the pivot.
        let lower = less
            .iter()
            .copied()
            .max_by(f64::total_cmp)
            .expect("an even non-empty window has a lower half");
        Some(f64::midpoint(lower, middle_value))
    }
}

/// First most-frequent value: counts run in first-appearance order and only
/// a strictly greater count displaces the leader, so frequency ties keep
/// the earliest value. Equality is exact (`==`, not an epsilon band): a
/// mode counts identical values, and any tolerance would merge distinct
/// prices into one bucket.
#[must_use]
#[allow(clippy::float_cmp)]
pub fn mode_of(values: &[f64]) -> Option<f64> {
    let mut scratch = Vec::new();
    mode_of_with(values, &mut scratch)
}

/// [`mode_of`] with a caller-provided scratch buffer: one sorted pass counting
/// runs, then the leader is recovered in **first-appearance order**. The old
/// shape scanned the prefix for a duplicate and recounted the whole slice for
/// every element, O(n²); this is O(n log n) with the same answer.
///
/// `-0.0` and `0.0` count as one bucket (they compare equal), so the sorted
/// copy folds the sign of zero before bucketing. `NaN` is never equal to
/// itself, so it forms no bucket — an all-`NaN` input keeps the old answer,
/// its first value.
#[must_use]
#[allow(clippy::float_cmp)]
pub fn mode_of_with(values: &[f64], scratch: &mut Vec<f64>) -> Option<f64> {
    let first = *values.first()?;
    scratch.clear();
    scratch.extend(
        values
            .iter()
            .map(|value| if *value == 0.0 { 0.0 } else { *value }),
    );
    scratch.sort_by(f64::total_cmp);
    let mut best = 0_usize;
    let mut start = 0_usize;
    while start < scratch.len() {
        let mut end = start + 1;
        while end < scratch.len() && scratch[end] == scratch[start] {
            end += 1;
        }
        if !scratch[start].is_nan() {
            best = best.max(end - start);
        }
        start = end;
    }
    if best == 0 {
        return Some(first);
    }
    values
        .iter()
        .copied()
        .find(|candidate| run_len(scratch, *candidate) == best)
}

/// Length of `value`'s run in a `total_cmp`-sorted slice; zero for `NaN`,
/// which forms no bucket.
fn run_len(sorted: &[f64], value: f64) -> usize {
    if value.is_nan() {
        return 0;
    }
    let start = sorted.partition_point(|candidate| candidate.total_cmp(&value).is_lt());
    let end = sorted.partition_point(|candidate| candidate.total_cmp(&value).is_le());
    end - start
}

/// Linear-interpolation percentile: `rank = percentage / 100 * (n - 1)`
/// over the sorted values, lerping between the bracketing ranks. The
/// caller validates `percentage` into `0..=100`, which keeps the rank
/// inside the sorted range.
#[must_use]
pub fn percentile_linear(values: &[f64], percentage: f64) -> Option<f64> {
    let mut scratch = Vec::new();
    percentile_linear_with(values, percentage, &mut scratch)
}

/// [`percentile_linear`] with a caller-provided scratch buffer: copies the
/// window in and **selects** the two bracketing ranks instead of sorting the
/// whole window. The interpolation is unchanged (`rank = p/100 * (n - 1)`,
/// lerp between floor and ceil), and the ends stay the plain min/max ranks.
#[must_use]
pub fn percentile_linear_with(
    values: &[f64],
    percentage: f64,
    scratch: &mut Vec<f64>,
) -> Option<f64> {
    if values.is_empty() || !percentage.is_finite() {
        return None;
    }
    scratch.clear();
    scratch.extend_from_slice(values);
    let rank = percentage / 100.0 * (values.len() - 1) as f64;
    let below = rank.floor() as usize;
    let above = rank.ceil() as usize;
    let frac = rank - rank.floor();
    let (_, &mut lo, greater) = scratch.select_nth_unstable_by(below, f64::total_cmp);
    let hi = if above == below {
        lo
    } else {
        // The ranks differ by at most one, so the upper rank is the smallest
        // element the selection left above the lower one.
        *greater
            .iter()
            .min_by(|a, b| a.total_cmp(b))
            .expect("a higher rank exists when ceil > floor")
    };
    fin_or_none(lo * (1.0 - frac) + hi * frac)
}

/// Z-scores under the population spread: `(x - mean) / std_pop`. A
/// zero or non-finite spread carries no direction, so every slot reads
/// absent instead of refusing — the language yields `na` for `0 / 0`.
#[must_use]
pub fn standardize_scores(values: &[f64]) -> Vec<Option<f64>> {
    let mut out = Vec::new();
    standardize_scores_into(values, &mut out);
    out
}

/// [`standardize_scores`] into a caller-provided buffer: clears and refills
/// `out`, so a hot caller allocates once instead of once per evaluation. A
/// zero or non-finite spread fills the buffer with `None` (the language's
/// `0 / 0` is `na`), never a refusal.
pub fn standardize_scores_into(values: &[f64], out: &mut Vec<Option<f64>>) {
    out.clear();
    if values.is_empty() {
        return;
    }
    let mean = window_mean(values);
    let spread = std_pop(values);
    if !spread.is_finite() || spread == 0.0 {
        out.resize(values.len(), None);
        return;
    }
    out.extend(
        values
            .iter()
            .map(|value| fin_or_none((value - mean) / spread)),
    );
}

/// Population covariance over equal-length legs (dividing by `n`); `None`
/// on length mismatch or fewer than two points, where no dispersion
/// exists yet.
#[must_use]
pub fn covariance_pop(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 2 {
        return None;
    }
    let (mean_a, mean_b) = (window_mean(a), window_mean(b));
    let joint = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x - mean_a) * (y - mean_b))
        .sum::<f64>()
        / a.len() as f64;
    fin_or_none(joint)
}

/// Sample covariance over equal-length legs (dividing by `n - 1`);
/// same shape contract as [`covariance_pop`].
#[must_use]
pub fn covariance_sample(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 2 {
        return None;
    }
    let (mean_a, mean_b) = (window_mean(a), window_mean(b));
    let joint = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x - mean_a) * (y - mean_b))
        .sum::<f64>()
        / (a.len() as f64 - 1.0);
    fin_or_none(joint)
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
/// matching the reference operation-for-operation is what the Expr-parity suites
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

/// Value pins for the selection-based helpers and the scratch-reuse contract.
#[cfg(test)]
#[path = "../tests/math_selection.rs"]
mod selection_tests;
