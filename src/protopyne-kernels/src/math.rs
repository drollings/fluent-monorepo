//! Shared indicator math: one home for the recurrences every kernel repeats.
//!
//! Each helper reproduces the exact arithmetic (including op order) of the
//! kernels that compose it, so migrating a kernel onto these helpers never
//! changes its output — the golden suites guard that. [`period_len`] is the
//! DRY home of the per-kernel `length must be at least 1` validation (twelve
//! identical bodies plus the named-leg variant); [`div_or_none`] is the
//! division guard for sites whose denominator is provably nonzero and
//! finite. Sites where the denominator can legitimately be zero (volume
//!-weighted means over empty volume) keep their raw division on purpose —
//! changing those would alter outputs, so they are documented at the call
//! site instead of migrated.

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

#[cfg(test)]
#[path = "../tests/math.rs"]
mod tests;
