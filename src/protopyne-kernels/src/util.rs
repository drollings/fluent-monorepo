//! Two-series and window utilities: `crossover`, `crossunder`, `highest`,
//! `lowest`, `change`, `mom`.
//!
//! These are pure slice functions rather than registry indicators: crossings
//! combine two arbitrary series (no single scheduling id fits), and the
//! window helpers are combinators kernels build on. Each carries its warmup
//! so fetch planning treats them like any kernel. Crossings are strict
//! (`a[1] <= b[1] && a > b`); equality never fires. `NaN` on either side
//! yields `None`, never a phantom signal.

use crate::indicator::fin_or_none;

/// Warmup for crossings: one prior bar must exist.
#[must_use]
pub fn cross_warmup() -> usize {
    1
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
        *slot = Some(a_prev <= b_prev && a_now > b_now);
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
        *slot = Some(a_prev >= b_prev && a_now < b_now);
    }
    out
}

/// Rolling highest value over `length` bars; warming positions are `None`.
#[must_use]
pub fn highest(source: &[f64], length: usize) -> Vec<Option<f64>> {
    rolling_extreme(source, length, f64::NEG_INFINITY, f64::max)
}

/// Rolling lowest value over `length` bars; warming positions are `None`.
#[must_use]
pub fn lowest(source: &[f64], length: usize) -> Vec<Option<f64>> {
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

#[cfg(test)]
#[path = "../tests/util.rs"]
mod tests;
