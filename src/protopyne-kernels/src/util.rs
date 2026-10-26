//! Two-series and window utilities: `crossover`, `crossunder`, `highest`,
//! `lowest`, `change`, `mom`, plus the statistic-operator combinators.
//!
//! These are pure slice functions rather than registry indicators: crossings
//! combine two arbitrary series (no single scheduling id fits), and the
//! window helpers are combinators kernels build on. Each carries its warmup
//! so fetch planning treats them like any kernel. Crossings are strict
//! (`a[1] <= b[1] && a > b`); equality never fires. `NaN` on either side
//! yields `None`, never a phantom signal.
//!
//! Rolling extremes delegate to the shared `math` core; the names here stay
//! so existing callers are byte-identical.
//!
//! The `*_expr` operator maps mirror the reference's horizontal/rolling
//! combinators with strict null semantics: any `NaN` leg poisons its position
//! (warmup included), except the null-ignoring `add_expr`/`mul_expr`, which
//! skip `NaN` legs and read `None` only when every leg is missing — exactly
//! the reference's fold shape. This strictness deliberately differs from
//! [`math::rolling_max`](crate::math::rolling_max)'s Pine skip-`na`
//! convention; the divergence is pinned by test, not accidental.

/// Warmup for crossings: one prior bar must exist.
#[must_use]
pub fn cross_warmup() -> usize {
    1
}

/// Which way a reading crossed a level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Crossing {
    /// The reading moved from at-or-below the level to strictly above it.
    Up,
    /// The reading moved from at-or-above the level to strictly below it.
    Down,
}

/// The one scalar crossing core.
///
/// `level` is held constant across the pair (the level-crossing shape); use
/// [`crossed_between`] when both sides move. The contract is the existing
/// one: the previous side may touch the level (`<=` / `>=`), the current side
/// must be strictly past it, equality on both sides never fires, and a
/// missing or non-finite side yields `None` rather than a phantom crossing.
#[must_use]
pub fn crossed(prev: Option<f64>, curr: Option<f64>, level: f64) -> Option<Crossing> {
    crossed_between(prev, curr, Some(level), Some(level))
}

/// The two-series crossing core: `a` crosses `b` between the previous and
/// current readings. Both sides move, so both previous and current values are
/// supplied; the strict-inequality contract matches [`crossed`].
#[must_use]
pub fn crossed_between(
    prev_a: Option<f64>,
    curr_a: Option<f64>,
    prev_b: Option<f64>,
    curr_b: Option<f64>,
) -> Option<Crossing> {
    let (prev_a, curr_a, prev_b, curr_b) = (prev_a?, curr_a?, prev_b?, curr_b?);
    if !prev_a.is_finite() || !curr_a.is_finite() || !prev_b.is_finite() || !curr_b.is_finite() {
        return None;
    }
    if prev_a <= prev_b && curr_a > curr_b {
        Some(Crossing::Up)
    } else if prev_a >= prev_b && curr_a < curr_b {
        Some(Crossing::Down)
    } else {
        None
    }
}

/// `true` where `a` crosses over `b`; `None` where either side is missing.
#[must_use]
pub fn crossover(a: &[f64], b: &[f64]) -> Vec<Option<bool>> {
    let len = a.len().min(b.len());
    let mut out = vec![None; len];
    for (i, slot) in out.iter_mut().enumerate().skip(1) {
        let (a_now, a_prev, b_now, b_prev) = (a[i], a[i - 1], b[i], b[i - 1]);
        if a_now.is_nan() || a_prev.is_nan() || b_now.is_nan() || b_prev.is_nan() {
            continue;
        }
        *slot = Some(
            crossed_between(Some(a_prev), Some(a_now), Some(b_prev), Some(b_now))
                == Some(Crossing::Up),
        );
    }
    out
}

/// `true` where `a` crosses under `b`; `None` where either side is missing.
#[must_use]
pub fn crossunder(a: &[f64], b: &[f64]) -> Vec<Option<bool>> {
    let len = a.len().min(b.len());
    let mut out = vec![None; len];
    for (i, slot) in out.iter_mut().enumerate().skip(1) {
        let (a_now, a_prev, b_now, b_prev) = (a[i], a[i - 1], b[i], b[i - 1]);
        if a_now.is_nan() || a_prev.is_nan() || b_now.is_nan() || b_prev.is_nan() {
            continue;
        }
        *slot = Some(
            crossed_between(Some(a_prev), Some(a_now), Some(b_prev), Some(b_now))
                == Some(Crossing::Down),
        );
    }
    out
}

/// Rolling highest value over `length` bars; warming positions are `None`.
/// Delegates to the shared [`math`](crate::math) core so combinators and
/// kernels never fork extremes logic.
#[must_use]
pub fn highest(source: &[f64], length: usize) -> Vec<Option<f64>> {
    crate::math::rolling_max(source, length)
}

/// Rolling lowest value over `length` bars; warming positions are `None`.
/// Delegates to the shared [`math`](crate::math) core so combinators and
/// kernels never fork extremes logic.
#[must_use]
pub fn lowest(source: &[f64], length: usize) -> Vec<Option<f64>> {
    crate::math::rolling_min(source, length)
}

/// Bar-to-bar change; the first bar is `None`.
#[must_use]
pub fn change(source: &[f64]) -> Vec<Option<f64>> {
    momentum(source, 1)
}

/// Change over `length` bars; the first `length` bars are `None`.
#[must_use]
pub fn mom(source: &[f64], length: usize) -> Vec<Option<f64>> {
    momentum(source, length.max(1))
}

fn momentum(source: &[f64], length: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; source.len()];
    for (i, slot) in out.iter_mut().enumerate().skip(length) {
        let (now, then) = (source[i], source[i - length]);
        *slot = if now.is_nan() || then.is_nan() {
            None
        } else {
            Some(now - then)
        };
    }
    out
}

/// Null-ignoring horizontal sum over two or more legs; `None` only where
/// every leg is missing, matching the reference fold shape.
#[must_use]
pub fn add_expr(a: &[f64], b: &[f64], rest: &[&[f64]]) -> Vec<Option<f64>> {
    let len = rest
        .iter()
        .fold(a.len().min(b.len()), |m, s| m.min(s.len()));
    (0..len)
        .map(|i| {
            let mut sum = 0.0;
            let mut seen = false;
            for v in [a[i], b[i]].into_iter().chain(rest.iter().map(|s| s[i])) {
                if !v.is_nan() {
                    sum += v;
                    seen = true;
                }
            }
            if seen {
                Some(sum)
            } else {
                None
            }
        })
        .collect()
}

/// Null-ignoring horizontal product over two or more legs; `None` only where
/// every leg is missing.
#[must_use]
pub fn mul_expr(a: &[f64], b: &[f64], rest: &[&[f64]]) -> Vec<Option<f64>> {
    let len = rest
        .iter()
        .fold(a.len().min(b.len()), |m, s| m.min(s.len()));
    (0..len)
        .map(|i| {
            let mut product = 1.0;
            let mut seen = false;
            for v in [a[i], b[i]].into_iter().chain(rest.iter().map(|s| s[i])) {
                if !v.is_nan() {
                    product *= v;
                    seen = true;
                }
            }
            if seen {
                Some(product)
            } else {
                None
            }
        })
        .collect()
}

/// Elementwise difference; any missing leg poisons its position.
#[must_use]
pub fn sub_expr(a: &[f64], b: &[f64]) -> Vec<Option<f64>> {
    let len = a.len().min(b.len());
    (0..len)
        .map(|i| {
            if a[i].is_nan() || b[i].is_nan() {
                None
            } else {
                Some(a[i] - b[i])
            }
        })
        .collect()
}

/// Guarded elementwise division via the shared division guard: zero and
/// missing denominators refuse (`None`) instead of emitting `inf`, the one
/// documented departure from the reference's raw `x / y`.
#[must_use]
pub fn div_expr(a: &[f64], b: &[f64]) -> Vec<Option<f64>> {
    let len = a.len().min(b.len());
    (0..len)
        .map(|i| crate::math::div_or_none(a[i], b[i]))
        .collect()
}

/// Strict rolling maximum: warming positions and any `NaN`-bearing window
/// read `None` (the reference's full-window `min_samples`, not Pine's
/// skip-`na`).
#[must_use]
pub fn max_window(source: &[f64], length: usize) -> Vec<Option<f64>> {
    strict_window(source, length, |window| {
        window.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    })
}

/// Strict rolling minimum, same null contract as [`max_window`].
#[must_use]
pub fn min_window(source: &[f64], length: usize) -> Vec<Option<f64>> {
    strict_window(source, length, |window| {
        window.iter().copied().fold(f64::INFINITY, f64::min)
    })
}

/// Strict rolling sum, same null contract as [`max_window`].
#[must_use]
pub fn sum_window(source: &[f64], length: usize) -> Vec<Option<f64>> {
    strict_window(source, length, |window| window.iter().sum())
}

fn strict_window(source: &[f64], length: usize, fold: impl Fn(&[f64]) -> f64) -> Vec<Option<f64>> {
    let mut out = vec![None; source.len()];
    if length >= 1 && length <= source.len() {
        for (i, slot) in out.iter_mut().enumerate().skip(length - 1) {
            let window = &source[i + 1 - length..=i];
            if window.iter().all(|v| !v.is_nan()) {
                *slot = crate::indicator::fin_or_none(fold(window));
            }
        }
    }
    out
}

/// Strict distance back to the most recent window maximum (`0` = current
/// bar); ties resolve to the most recent bar, matching the reference's
/// reversed argmax. Warming positions and any `NaN`-bearing window are `None`.
#[must_use]
pub fn argmax_window(source: &[f64], length: usize) -> Vec<Option<usize>> {
    strict_arg(source, length, true)
}

/// Strict distance back to the most recent window minimum, same contract as
/// [`argmax_window`].
#[must_use]
pub fn argmin_window(source: &[f64], length: usize) -> Vec<Option<usize>> {
    strict_arg(source, length, false)
}

fn strict_arg(source: &[f64], length: usize, find_max: bool) -> Vec<Option<usize>> {
    let mut out = vec![None; source.len()];
    if length >= 1 && length <= source.len() {
        for (i, slot) in out.iter_mut().enumerate().skip(length - 1) {
            let window = &source[i + 1 - length..=i];
            if window.iter().any(|v| v.is_nan()) {
                continue;
            }
            let mut best = if find_max {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
            let mut index = 0;
            for (j, &v) in window.iter().enumerate() {
                let takes = if find_max { v >= best } else { v <= best };
                if takes {
                    best = v;
                    index = j;
                }
            }
            *slot = Some(length - 1 - index);
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/util.rs"]
mod tests;
