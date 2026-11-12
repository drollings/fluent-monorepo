//! WorldQuant-style time-series combinators as free functions over `&[f64]`.
//!
//! These are stateless rolling helpers, not chart kernels: no registry, no
//! compat rows, no warmup metadata. Every windowed function honors the same
//! min-periods-as-warmup convention — output length always equals input
//! length, the first `d - 1` slots read `None`, short inputs never shift
//! values forward, and degenerate widths (`d == 0`) refuse as all-`None`.
//! Window legs must be finite unless the function documents otherwise
//! (products and moments skip `NaN` like the reference's `nan`-ignoring
//! kernels; counts treat `NaN` as truthy like the reference cast).
//!
//! Deferred companions live elsewhere by design: boolean shift packs in the
//! logical-combinator milestone, regressions need solver machinery the
//! reference itself borrows from a plugin, and co-moment/partial/triple
//! correlations stay out until their formulas carry pinning examples (the
//! reference flags partial correlation as unverified). `null` counting has
//! no meaning over `f64` slices, so only `NaN` counting is provided.

use crate::indicator::fin_or_none;
use crate::math::{div_or_none, strict_window, warmup_none};

/// Shift forward (`d > 0`) or backward (`d < 0`) with an optional fill for
/// the exposed edge; `None` fill leaves the edge missing. `d == 0` is the
/// identity (missing values stay missing).
#[must_use]
/// Shift forward (`d > 0`) or backward (`d < 0`) with an optional fill for
/// the exposed edge; `None` fill leaves the edge missing. `d == 0` is the
/// identity (missing values stay missing).
pub fn ts_delay(values: &[f64], d: i64, fill: Option<f64>) -> Vec<Option<f64>> {
    use std::cmp::Ordering;
    let len = values.len();
    match d.cmp(&0) {
        Ordering::Equal => values.iter().map(|v| fin_or_none(*v)).collect(),
        Ordering::Greater => {
            let lag = usize::try_from(d.unsigned_abs()).unwrap_or(usize::MAX);
            (0..len)
                .map(|i| {
                    if i < lag {
                        fill
                    } else {
                        fin_or_none(values[i - lag])
                    }
                })
                .collect()
        }
        Ordering::Less => {
            let lead = usize::try_from(d.unsigned_abs()).unwrap_or(usize::MAX);
            (0..len)
                .map(|i| {
                    if i + lead >= len {
                        fill
                    } else {
                        fin_or_none(values[i + lead])
                    }
                })
                .collect()
        }
    }
}

/// Discrete difference `x[i] - x[i-d]`; the first `d` slots read `None`.
/// A zero lag is degenerate and refuses as all-`None`.
#[must_use]
/// Discrete difference `x[i] - x[i-d]`; the first `d` slots read `None`.
/// A zero lag is degenerate and refuses as all-`None`.
pub fn ts_delta(values: &[f64], d: usize) -> Vec<Option<f64>> {
    if d == 0 {
        return warmup_none(values.len());
    }
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if i < d {
                return None;
            }
            fin_or_none(values[i] - values[i - d])
        })
        .collect()
}

/// Rolling sum over the strict `d`-bar window.
#[must_use]
/// Rolling sum over the strict `d`-bar window.
pub fn ts_sum(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| fin_or_none(strict_window(values, i, d)?.iter().sum()))
        .collect()
}

/// Rolling mean over the strict `d`-bar window.
#[must_use]
/// Rolling mean over the strict `d`-bar window.
pub fn ts_mean(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            fin_or_none(window.iter().sum::<f64>() / d as f64)
        })
        .collect()
}

/// Rolling product, skipping `NaN` like the reference's `nanprod`; `None`
/// only while warming or when the whole window is missing.
#[must_use]
/// Rolling product, skipping `NaN` like the reference's `nanprod`; `None`
/// only while warming or when the whole window is missing.
pub fn ts_product(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if d == 0 || i + 1 < d {
                return None;
            }
            let window = &values[i + 1 - d..=i];
            let mut product = 1.0;
            let mut seen = false;
            for value in window {
                if value.is_nan() {
                    continue;
                }
                product *= *value;
                seen = true;
            }
            if seen {
                fin_or_none(product)
            } else {
                None
            }
        })
        .collect()
}

/// Running total that skips missing values instead of poisoning: `None`
/// until the first finite print, then the accumulation resumes.
#[must_use]
/// Running total that skips missing values instead of poisoning: `None`
/// until the first finite print, then the accumulation resumes.
pub fn ts_cum_sum(values: &[f64]) -> Vec<Option<f64>> {
    let mut running = 0.0;
    let mut seen = false;
    values
        .iter()
        .map(|v| {
            if v.is_nan() {
                return if seen { Some(running) } else { None };
            }
            running += *v;
            seen = true;
            Some(running)
        })
        .collect()
}

/// Running product with the same skip-missing convention as [`ts_cum_sum`].
#[must_use]
/// Running product with the same skip-missing convention as [`ts_cum_sum`].
pub fn ts_cum_prod(values: &[f64]) -> Vec<Option<f64>> {
    let mut running = 1.0;
    let mut seen = false;
    values
        .iter()
        .map(|v| {
            if v.is_nan() {
                return if seen { Some(running) } else { None };
            }
            running *= *v;
            seen = true;
            Some(running)
        })
        .collect()
}

/// Running maximum (`None` until the first finite print).
#[must_use]
/// Running maximum (`None` until the first finite print).
pub fn ts_cum_max(values: &[f64]) -> Vec<Option<f64>> {
    let mut running = f64::NEG_INFINITY;
    values
        .iter()
        .map(|v| {
            if v.is_nan() {
                return if running.is_finite() {
                    Some(running)
                } else {
                    None
                };
            }
            running = running.max(*v);
            Some(running)
        })
        .collect()
}

/// Running minimum (`None` until the first finite print).
#[must_use]
/// Running minimum (`None` until the first finite print).
pub fn ts_cum_min(values: &[f64]) -> Vec<Option<f64>> {
    let mut running = f64::INFINITY;
    values
        .iter()
        .map(|v| {
            if v.is_nan() {
                return if running.is_finite() {
                    Some(running)
                } else {
                    None
                };
            }
            running = running.min(*v);
            Some(running)
        })
        .collect()
}

/// Running count of finite prints. Leading gaps read `0`, never `None` —
/// counting missing values is defined, matching the reference zeros.
#[must_use]
/// Running count of finite prints. Leading gaps read `0`, never `None` —
/// counting missing values is defined, matching the reference zeros.
pub fn ts_cum_count(values: &[f64]) -> Vec<Option<f64>> {
    let mut count = 0.0;
    values
        .iter()
        .map(|v| {
            if !v.is_nan() {
                count += 1.0;
            }
            Some(count)
        })
        .collect()
}

/// Resetting accumulation, transcribed verbatim: zero and missing reset to
/// `0`, a sign flip restarts from the current print, same-sign prints
/// compound. The output is always defined (missing input emits `0`).
/// Exact-zero comparisons are the transcribed reset levels, not measured
/// thresholds.
#[allow(clippy::float_cmp)]
#[must_use]
/// Resetting accumulation, transcribed verbatim: zero and missing reset to
/// `0`, a sign flip restarts from the current print, same-sign prints
/// compound. The output is always defined (missing input emits `0`).
/// Exact-zero comparisons are the transcribed reset levels, not measured
/// thresholds.
pub fn ts_cum_sum_reset(values: &[f64]) -> Vec<Option<f64>> {
    let mut last = 0.0;
    values
        .iter()
        .map(|v| {
            let current = if v.is_nan() { 0.0 } else { *v };
            let out = if current == 0.0 {
                0.0
            } else if current > 0.0 {
                if last <= 0.0 {
                    current
                } else {
                    current + last
                }
            } else if last >= 0.0 {
                current
            } else {
                current + last
            };
            last = out;
            Some(out)
        })
        .collect()
}

/// Seeded accumulation: an explicit seed wins, gaps compound from the last
/// value, and nothing is emitted before the first seed. Transcribed from
/// the reference loops (`r[i] + acc` / `r[i] * acc`, `NaN` sticky).
#[must_use]
/// Seeded accumulation: an explicit seed wins, gaps compound from the last
/// value, and nothing is emitted before the first seed. Transcribed from
/// the reference loops (`r[i] + acc` / `r[i] * acc`, `NaN` sticky).
pub fn ts_cum_sum_by(gains: &[f64], seeds: &[f64]) -> Vec<Option<f64>> {
    cum_by(gains, seeds, false)
}

/// Seeded compounding, same convention as [`ts_cum_sum_by`] with products.
#[must_use]
/// Seeded compounding, same convention as [`ts_cum_sum_by`] with products.
pub fn ts_cum_prod_by(gains: &[f64], seeds: &[f64]) -> Vec<Option<f64>> {
    cum_by(gains, seeds, true)
}

fn cum_by(gains: &[f64], seeds: &[f64], multiply: bool) -> Vec<Option<f64>> {
    let len = gains.len().min(seeds.len());
    let mut running = f64::NAN;
    let mut started = false;
    (0..len)
        .map(|i| {
            if !seeds[i].is_nan() {
                running = seeds[i];
                started = true;
                Some(seeds[i])
            } else if started {
                running = if multiply {
                    gains[i] * running
                } else {
                    gains[i] + running
                };
                fin_or_none(running)
            } else {
                None
            }
        })
        .collect()
}

/// Rolling maximum over the strict `d`-bar window.
#[must_use]
/// Rolling maximum over the strict `d`-bar window.
pub fn ts_max(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            strict_window(values, i, d)?
                .iter()
                .copied()
                .reduce(f64::max)
                .and_then(fin_or_none)
        })
        .collect()
}

/// Rolling minimum over the strict `d`-bar window.
#[must_use]
/// Rolling minimum over the strict `d`-bar window.
pub fn ts_min(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            strict_window(values, i, d)?
                .iter()
                .copied()
                .reduce(f64::min)
                .and_then(fin_or_none)
        })
        .collect()
}

/// Distance below the window maximum: `x - max(x, d)`.
#[must_use]
/// Distance below the window maximum: `x - max(x, d)`.
pub fn ts_max_diff(values: &[f64], d: usize) -> Vec<Option<f64>> {
    elementwise_with_extreme(values, d, true)
}

/// Distance above the window minimum: `x - min(x, d)`.
#[must_use]
/// Distance above the window minimum: `x - min(x, d)`.
pub fn ts_min_diff(values: &[f64], d: usize) -> Vec<Option<f64>> {
    elementwise_with_extreme(values, d, false)
}

fn elementwise_with_extreme(values: &[f64], d: usize, use_max: bool) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &current)| {
            if current.is_nan() {
                return None;
            }
            let window = strict_window(values, i, d)?;
            let extreme = if use_max {
                window.iter().copied().reduce(f64::max)?
            } else {
                window.iter().copied().reduce(f64::min)?
            };
            fin_or_none(current - extreme)
        })
        .collect()
}

/// Range-composite `(min + max) - f * x` over the strict window.
#[must_use]
/// Range-composite `(min + max) - f * x` over the strict window.
pub fn ts_min_max_cps(values: &[f64], d: usize, f: f64) -> Vec<Option<f64>> {
    min_max_composite(values, d, f, true)
}

/// Range-composite `x - f * (min + max)` over the strict window.
#[must_use]
/// Range-composite `x - f * (min + max)` over the strict window.
pub fn ts_min_max_diff(values: &[f64], d: usize, f: f64) -> Vec<Option<f64>> {
    min_max_composite(values, d, f, false)
}

fn min_max_composite(values: &[f64], d: usize, f: f64, min_max_first: bool) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &current)| {
            if current.is_nan() {
                return None;
            }
            let window = strict_window(values, i, d)?;
            let floor = window.iter().copied().reduce(f64::min)?;
            let cap = window.iter().copied().reduce(f64::max)?;
            fin_or_none(if min_max_first {
                floor + cap - f * current
            } else {
                current - f * (floor + cap)
            })
        })
        .collect()
}

/// `k`-th central moment `mean((x - mean)^k)`, skipping `NaN` like the
/// reference's `nanmean` form; `None` while warming or when the window is
/// entirely missing. `k = 0` reads `1` wherever defined.
#[must_use]
/// `k`-th central moment `mean((x - mean)^k)`, skipping `NaN` like the
/// reference's `nanmean` form; `None` while warming or when the window is
/// entirely missing. `k = 0` reads `1` wherever defined.
pub fn ts_moment(values: &[f64], d: usize, k: i32) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if d == 0 || i + 1 < d {
                return None;
            }
            let present: Vec<f64> = values[i + 1 - d..=i]
                .iter()
                .copied()
                .filter(|v| !v.is_nan())
                .collect();
            if present.is_empty() {
                return None;
            }
            let mean = present.iter().sum::<f64>() / present.len() as f64;
            fin_or_none(
                present.iter().map(|v| (v - mean).powi(k)).sum::<f64>() / present.len() as f64,
            )
        })
        .collect()
}

/// Rolling standard deviation with an explicit degree-of-freedom parameter
/// (`0` for population, `1` for sample). Refuses (`None`) when `ddof`
/// reaches the window width: there is no variance left to estimate.
#[must_use]
/// Rolling standard deviation with an explicit degree-of-freedom parameter
/// (`0` for population, `1` for sample). Refuses (`None`) when `ddof`
/// reaches the window width: there is no variance left to estimate.
pub fn ts_std_dev(values: &[f64], d: usize, ddof: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            if ddof >= d {
                return None;
            }
            let mean = window.iter().sum::<f64>() / d as f64;
            let variance =
                window.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / (d - ddof) as f64;
            fin_or_none(variance.sqrt())
        })
        .collect()
}

/// Rolling skewness. `bias = false` matches the pandas-adjusted form;
/// degenerate (constant) windows read `None`.
#[must_use]
/// Rolling skewness. `bias = false` matches the pandas-adjusted form;
/// degenerate (constant) windows read `None`.
pub fn ts_skewness(values: &[f64], d: usize, bias: bool) -> Vec<Option<f64>> {
    standardized_moment(values, d, bias, 3)
}

/// Rolling (Fisher) kurtosis. `bias = false` matches the pandas-adjusted
/// form; degenerate windows read `None`.
#[must_use]
/// Rolling (Fisher) kurtosis. `bias = false` matches the pandas-adjusted
/// form; degenerate windows read `None`.
pub fn ts_kurtosis(values: &[f64], d: usize, bias: bool) -> Vec<Option<f64>> {
    standardized_moment(values, d, bias, 4)
}

fn standardized_moment(values: &[f64], d: usize, bias: bool, order: u32) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            let n = d as f64;
            let mean = window.iter().sum::<f64>() / n;
            let m2 = window.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
            if m2 <= 0.0 {
                return None;
            }
            if bias {
                let moment = window
                    .iter()
                    .map(|v| ((v - mean) / m2.sqrt()).powi(order as i32))
                    .sum::<f64>()
                    / n;
                return fin_or_none(if order == 4 { moment - 3.0 } else { moment });
            }
            let variance = m2 * n / (n - 1.0);
            if d < 3 || (order == 4 && d < 4) {
                return None;
            }
            let total: f64 = window
                .iter()
                .map(|v| ((v - mean) / variance.sqrt()).powi(order as i32))
                .sum();
            fin_or_none(if order == 3 {
                n / ((n - 1.0) * (n - 2.0)) * total
            } else {
                n * (n + 1.0) / ((n - 1.0) * (n - 2.0) * (n - 3.0)) * total
                    - 3.0 * (n - 1.0) * (n - 1.0) / ((n - 2.0) * (n - 3.0))
            })
        })
        .collect()
}

/// Euclidean norm `sqrt(sum(x^2))` over the strict window.
#[must_use]
/// Euclidean norm `sqrt(sum(x^2))` over the strict window.
pub fn ts_l2_norm(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            fin_or_none(window.iter().map(|v| v * v).sum::<f64>().sqrt())
        })
        .collect()
}

/// Information ratio `mean / std_dev(ddof = 0)` over the strict window.
#[must_use]
/// Information ratio `mean / std_dev(ddof = 0)` over the strict window.
pub fn ts_ir(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            let mean = window.iter().sum::<f64>() / d as f64;
            let variance = window.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / d as f64;
            if variance <= 0.0 {
                return None;
            }
            fin_or_none(mean / variance.sqrt())
        })
        .collect()
}

/// Rolling median (even windows average the two middle values).
#[must_use]
/// Rolling median (even windows average the two middle values).
pub fn ts_median(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            let mut sorted: Vec<f64> = window.to_vec();
            sorted.sort_by(f64::total_cmp);
            fin_or_none(if d % 2 == 1 {
                sorted[d / 2]
            } else {
                f64::midpoint(sorted[d / 2 - 1], sorted[d / 2])
            })
        })
        .collect()
}

/// Rolling nearest-rank quantile. `pct` outside `[0, 1]` refuses as
/// all-`None`: a quantile outside the unit interval is a caller error.
#[must_use]
/// Rolling nearest-rank quantile. `pct` outside `[0, 1]` refuses as
/// all-`None`: a quantile outside the unit interval is a caller error.
pub fn ts_percentage(values: &[f64], d: usize, pct: f64) -> Vec<Option<f64>> {
    if !(0.0..=1.0).contains(&pct) {
        return warmup_none(values.len());
    }
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            let mut sorted: Vec<f64> = window.to_vec();
            sorted.sort_by(f64::total_cmp);
            let rank = (pct * (d - 1) as f64).round() as usize;
            fin_or_none(sorted[rank])
        })
        .collect()
}

/// Rank of the current print within its window (average ties, 1-based),
/// normalized by the window width. Any missing leg voids the window.
/// Tie identity is exact equality, not a tolerance.
#[allow(clippy::float_cmp)]
#[must_use]
/// Rank of the current print within its window (average ties, 1-based),
/// normalized by the window width. Any missing leg voids the window.
/// Tie identity is exact equality, not a tolerance.
pub fn ts_rank(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &current)| {
            if !current.is_finite() {
                return None;
            }
            let window = strict_window(values, i, d)?;
            let less = window.iter().filter(|v| **v < current).count() as f64;
            let equal = window.iter().filter(|v| **v == current).count() as f64;
            fin_or_none((less + (equal - 1.0) / 2.0 + 1.0) / d as f64)
        })
        .collect()
}

/// Distance from the window end (`reverse = true`, most-recent extreme reads
/// `0`) or from the window start (`reverse = false`) to the extreme print.
/// Ties resolve to the most-recent (reversed) or earliest (forward) match,
/// and a missing current print voids the window — the transcribed numba
/// rule. The reversed mode agrees with the shared argmax/argmin helpers on
/// clean data.
#[must_use]
/// Distance from the window end (`reverse = true`, most-recent extreme reads
/// `0`) or from the window start (`reverse = false`) to the extreme print.
/// Ties resolve to the most-recent (reversed) or earliest (forward) match,
/// and a missing current print voids the window — the transcribed numba
/// rule. The reversed mode agrees with the shared argmax/argmin helpers on
/// clean data.
pub fn ts_arg_max(values: &[f64], d: usize, reverse: bool) -> Vec<Option<f64>> {
    arg_extreme(values, d, reverse, true)
}

/// Distance to the window minimum under the same convention as
/// [`ts_arg_max`].
#[must_use]
/// Distance to the window minimum under the same convention as
/// [`ts_arg_max`].
pub fn ts_arg_min(values: &[f64], d: usize, reverse: bool) -> Vec<Option<f64>> {
    arg_extreme(values, d, reverse, false)
}

/// Shared extreme search: extreme identity is exact equality, not a
/// tolerance (a tie is a tie only when the prints are identical).
#[allow(clippy::float_cmp)]
/// Shared extreme search: extreme identity is exact equality, not a
/// tolerance (a tie is a tie only when the prints are identical).
fn arg_extreme(values: &[f64], d: usize, reverse: bool, want_max: bool) -> Vec<Option<f64>> {
    use crate::math::{extreme_position, TieBreak};
    let tie = if reverse {
        TieBreak::Last
    } else {
        TieBreak::First
    };
    values
        .iter()
        .enumerate()
        .map(|(i, &current)| {
            if d == 0 || i + 1 < d || current.is_nan() {
                return None;
            }
            let window = &values[i + 1 - d..=i];
            if window.iter().any(|v| v.is_nan()) {
                return None;
            }
            let index = extreme_position(window, want_max, tie)?;
            let position = if reverse { d - 1 - index } else { index };
            Some(position as f64).and_then(fin_or_none)
        })
        .collect()
}

/// Rolling Pearson correlation with an explicit degree-of-freedom parameter
/// matching the reference signature. `ddof` cancels out of the ratio, so it
/// only documents the convention — the value is identical for any `ddof`.
/// `None` on zero-variance legs.
#[must_use]
/// Rolling Pearson correlation with an explicit degree-of-freedom parameter
/// matching the reference signature. `ddof` cancels out of the ratio, so it
/// only documents the convention — the value is identical for any `ddof`.
/// `None` on zero-variance legs.
pub fn ts_corr(xs: &[f64], ys: &[f64], d: usize, _ddof: usize) -> Vec<Option<f64>> {
    let len = xs.len().min(ys.len());
    (0..len)
        .map(|i| {
            let (sxy, sxx, syy) = pair_sums(xs, ys, i, d)?;
            if sxx <= 0.0 || syy <= 0.0 {
                return None;
            }
            fin_or_none(sxy / (sxx * syy).sqrt())
        })
        .collect()
}

/// Rolling covariance with an explicit `ddof` scale (`n - ddof` denominator).
/// `None` while warming or when `ddof` reaches the window width.
#[must_use]
/// Rolling covariance with an explicit `ddof` scale (`n - ddof` denominator).
/// `None` while warming or when `ddof` reaches the window width.
pub fn ts_covariance(xs: &[f64], ys: &[f64], d: usize, ddof: usize) -> Vec<Option<f64>> {
    let len = xs.len().min(ys.len());
    (0..len)
        .map(|i| {
            if ddof >= d {
                return None;
            }
            let (sxy, _, _) = pair_sums(xs, ys, i, d)?;
            fin_or_none(sxy / (d - ddof) as f64)
        })
        .collect()
}

fn pair_sums(xs: &[f64], ys: &[f64], end: usize, d: usize) -> Option<(f64, f64, f64)> {
    let xw = strict_window(xs, end, d)?;
    let yw = strict_window(ys, end, d)?;
    let (mean_x, mean_y) = (
        xw.iter().sum::<f64>() / d as f64,
        yw.iter().sum::<f64>() / d as f64,
    );
    let mut sums = (0.0, 0.0, 0.0);
    for (x, y) in xw.iter().zip(yw.iter()) {
        sums.0 += (x - mean_x) * (y - mean_y);
        sums.1 += (x - mean_x) * (x - mean_x);
        sums.2 += (y - mean_y) * (y - mean_y);
    }
    Some(sums)
}

/// Rolling z-score `(x - mean) / std_dev(ddof = 0)`.
#[must_use]
/// Rolling z-score `(x - mean) / std_dev(ddof = 0)`.
pub fn ts_zscore(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &current)| {
            if current.is_nan() {
                return None;
            }
            let window = strict_window(values, i, d)?;
            let mean = window.iter().sum::<f64>() / d as f64;
            let variance = window.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / d as f64;
            if variance <= 0.0 {
                return None;
            }
            fin_or_none((current - mean) / variance.sqrt())
        })
        .collect()
}

/// Min-max normalization `(x - min) / (max - min)`; a flat window reads
/// `0`, not `None` — a defined level, matching the reference `otherwise`.
/// The flat check is exact equality of two window prints, not a tolerance.
#[allow(clippy::float_cmp)]
#[must_use]
/// Min-max normalization `(x - min) / (max - min)`; a flat window reads
/// `0`, not `None` — a defined level, matching the reference `otherwise`.
/// The flat check is exact equality of two window prints, not a tolerance.
pub fn ts_scale(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &current)| {
            if current.is_nan() {
                return None;
            }
            let window = strict_window(values, i, d)?;
            let floor = window.iter().copied().reduce(f64::min)?;
            let cap = window.iter().copied().reduce(f64::max)?;
            if cap == floor {
                return Some(0.0);
            }
            fin_or_none((current - floor) / (cap - floor))
        })
        .collect()
}

/// Linear-decay weighted mean: the oldest print carries weight `1`, the
/// newest weight `d`.
#[must_use]
/// Linear-decay weighted mean: the oldest print carries weight `1`, the
/// newest weight `d`.
pub fn ts_decay_linear(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            let total: f64 = window
                .iter()
                .enumerate()
                .map(|(k, v)| (k + 1) as f64 * v)
                .sum();
            fin_or_none(total / (d * (d + 1) / 2) as f64)
        })
        .collect()
}

/// Exponential-decay weighted mean with per-bar factor `factor`: weights
/// run `factor^(d-1)` (oldest) to `1` (newest), formula for formula with the
/// reference construction.
#[must_use]
/// Exponential-decay weighted mean with per-bar factor `factor`: weights
/// run `factor^(d-1)` (oldest) to `1` (newest), formula for formula with the
/// reference construction.
pub fn ts_decay_exp_window(values: &[f64], d: usize, factor: f64) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            let mut total = 0.0;
            let mut weight_sum = 0.0;
            for (k, v) in window.iter().enumerate() {
                let weight = factor.powf((d - 1 - k) as f64);
                total += weight * v;
                weight_sum += weight;
            }
            fin_or_none(total / weight_sum)
        })
        .collect()
}

/// Two-bar weighted carry `(1 - k) * prev + k * current`: a weighted sum,
/// not an average, matching the reference construction.
#[must_use]
/// Two-bar weighted carry `(1 - k) * prev + k * current`: a weighted sum,
/// not an average, matching the reference construction.
pub fn ts_weighted_decay(values: &[f64], k: f64) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, &current)| {
            if i < 1 || current.is_nan() || values[i - 1].is_nan() {
                return None;
            }
            fin_or_none((1.0 - k) * values[i - 1] + k * current)
        })
        .collect()
}

/// Weighted mean over aligned `values`/`weights` windows; a zero weight
/// total refuses (`None`). Mismatched leg lengths refuse as all-`None`.
#[must_use]
/// Weighted mean over aligned `values`/`weights` windows; a zero weight
/// total refuses (`None`). Mismatched leg lengths refuse as all-`None`.
pub fn ts_weighted_mean(values: &[f64], weights: &[f64], d: usize) -> Vec<Option<f64>> {
    weighted_sum(values, weights, d, true)
}

/// Weighted sum over aligned windows (no normalization).
#[must_use]
/// Weighted sum over aligned windows (no normalization).
pub fn ts_weighted_sum(values: &[f64], weights: &[f64], d: usize) -> Vec<Option<f64>> {
    weighted_sum(values, weights, d, false)
}

fn weighted_sum(values: &[f64], weights: &[f64], d: usize, normalize: bool) -> Vec<Option<f64>> {
    if weights.len() != values.len() {
        return warmup_none(values.len());
    }
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(values, i, d)?;
            let w = strict_window(weights, i, d)?;
            let total: f64 = window.iter().zip(w.iter()).map(|(v, w)| v * w).sum();
            if normalize {
                let weight_sum: f64 = w.iter().sum();
                div_or_none(total, weight_sum).and_then(fin_or_none)
            } else {
                fin_or_none(total)
            }
        })
        .collect()
}

/// Simple returns `(x - shift) / shift`; a zero base refuses (`None`) where
/// the reference yields infinities.
#[must_use]
/// Simple returns `(x - shift) / shift`; a zero base refuses (`None`) where
/// the reference yields infinities.
pub fn ts_returns(values: &[f64], d: usize) -> Vec<Option<f64>> {
    if d == 0 {
        return warmup_none(values.len());
    }
    values
        .iter()
        .enumerate()
        .map(|(i, &current)| {
            if i < d || current.is_nan() || values[i - d].is_nan() {
                return None;
            }
            div_or_none(current - values[i - d], values[i - d]).and_then(fin_or_none)
        })
        .collect()
}

/// Log differences `ln(x[i]) - ln(x[i-d])`; non-positive prints void the
/// window through the `NaN` they produce.
#[must_use]
/// Log differences `ln(x[i]) - ln(x[i-d])`; non-positive prints void the
/// window through the `NaN` they produce.
pub fn ts_log_diff(values: &[f64], d: usize) -> Vec<Option<f64>> {
    if d == 0 {
        return warmup_none(values.len());
    }
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if i < d {
                return None;
            }
            fin_or_none(values[i].ln() - values[i - d].ln())
        })
        .collect()
}

/// Realized volatility: population std-dev of the trailing `d` log
/// differences (needs `d + 1` closes).
#[must_use]
/// Realized volatility: population std-dev of the trailing `d` log
/// differences (needs `d + 1` closes).
pub fn ts_realized_volatility(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if d == 0 || i < d {
                return None;
            }
            let mut diffs = Vec::with_capacity(d);
            for k in i + 1 - d..=i {
                let diff = values[k].ln() - values[k - 1].ln();
                if !diff.is_finite() {
                    return None;
                }
                diffs.push(diff);
            }
            let mean = diffs.iter().sum::<f64>() / d as f64;
            let variance = diffs.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / d as f64;
            fin_or_none(variance.sqrt())
        })
        .collect()
}

/// Count of nonzero prints in the window (`NaN` counts as truthy, mirroring
/// the reference cast). `None` only while warming. Truthiness is an exact
/// zero test, not a measured threshold.
#[allow(clippy::float_cmp)]
#[must_use]
/// Count of nonzero prints in the window (`NaN` counts as truthy, mirroring
/// the reference cast). `None` only while warming. Truthiness is an exact
/// zero test, not a measured threshold.
pub fn ts_count(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if d == 0 || i + 1 < d {
                return None;
            }
            Some(values[i + 1 - d..=i].iter().filter(|v| **v != 0.0).count() as f64)
        })
        .collect()
}

/// Whether the trailing `n` prints are all set and the trailing-`d` total
/// hits exactly `n`: Kleene-`AND` of the two conditions, so short windows
/// report `false` rather than missing whenever either side is decidable.
/// Truthiness is an exact zero test.
#[allow(clippy::float_cmp)]
#[must_use]
/// Whether the trailing `n` prints are all set and the trailing-`d` total
/// hits exactly `n`: Kleene-`AND` of the two conditions, so short windows
/// report `false` rather than missing whenever either side is decidable.
/// Truthiness is an exact zero test.
pub fn ts_count_eq(values: &[f64], d: usize, n: usize) -> Vec<Option<f64>> {
    count_condition(values, d, n, false)
}

/// Whether the trailing `n` prints are all set and the trailing-`d` total
/// reaches at least `n`, under the same Kleene convention.
#[allow(clippy::float_cmp)]
#[must_use]
/// Whether the trailing `n` prints are all set and the trailing-`d` total
/// reaches at least `n`, under the same Kleene convention.
pub fn ts_count_ge(values: &[f64], d: usize, n: usize) -> Vec<Option<f64>> {
    count_condition(values, d, n, true)
}

fn count_condition(values: &[f64], d: usize, n: usize, at_least: bool) -> Vec<Option<f64>> {
    if d == 0 || n == 0 {
        return warmup_none(values.len());
    }
    let truthy = |end: usize, width: usize| {
        if end + 1 < width {
            return None;
        }
        Some(
            values[end + 1 - width..=end]
                .iter()
                .filter(|v| **v != 0.0)
                .count(),
        )
    };
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let recent = truthy(i, n).map(|count| count == n);
            let total = truthy(i, d).map(|count| if at_least { count >= n } else { count == n });
            match (recent, total) {
                (Some(false), _) | (_, Some(false)) => Some(0.0),
                (Some(true), Some(true)) => Some(1.0),
                _ => None,
            }
        })
        .collect()
}

/// Count of `NaN` prints in the window (`None` only while warming).
#[must_use]
/// Count of `NaN` prints in the window (`None` only while warming).
pub fn ts_count_nans(values: &[f64], d: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if d == 0 || i + 1 < d {
                return None;
            }
            Some(values[i + 1 - d..=i].iter().filter(|v| v.is_nan()).count() as f64)
        })
        .collect()
}

/// Forward-fill missing prints with the last finite value; `limit` caps the
/// consecutive-fill run (`None` means unbounded). Leading gaps stay `None`.
#[must_use]
/// Forward-fill missing prints with the last finite value; `limit` caps the
/// consecutive-fill run (`None` means unbounded). Leading gaps stay `None`.
pub fn ts_fill_null(values: &[f64], limit: Option<usize>) -> Vec<Option<f64>> {
    let mut last = f64::NAN;
    let mut run = 0usize;
    values
        .iter()
        .map(|v| {
            if !v.is_nan() {
                last = *v;
                run = 0;
                return Some(*v);
            }
            if last.is_nan() {
                return None;
            }
            if let Some(cap) = limit {
                if run >= cap {
                    return None;
                }
            }
            run += 1;
            Some(last)
        })
        .collect()
}

#[cfg(all(test, feature = "research"))]
#[path = "../tests/wq_series.rs"]
mod tests;