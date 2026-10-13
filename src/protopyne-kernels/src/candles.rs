//! Single-bar candle compositions and two-bar gap predicates as free
//! functions over [`BarTick`](crate::indicator::BarTick).
//!
//! These are stateless per-bar combinators, not chart kernels: they carry no
//! warmup, enter no registry, and own no compatibility rows. The single-bar
//! family mirrors the reference's `(open, high, low, close)` signatures one
//! for one; the two-bar gap family takes the predecessor explicitly instead
//! of reaching back through a shifted series, which keeps the no-predecessor
//! opener honest (the caller reports no gap rather than inventing one).
//!
//! Measures return `f64` with natural `NaN` propagation. Predicates return
//! `bool` and never affirm on a non-finite leg: a missing bar is no signal,
//! which is the fail-closed direction for pattern detection.

use crate::indicator::BarTick;

/// Absolute tolerance shared by every threshold below, matching the
/// reference constant exactly. This is a task-value heuristic (it decides
/// pattern correctness), frozen by the calibration suite — never widened to
/// make a test pass.
pub const TA_EPSILON: f64 = 1e-8;

/// Body length: `|close - open|`.
#[must_use]
pub fn real_body(bar: BarTick) -> f64 {
    (bar.close - bar.open).abs()
}

/// Upper shadow: `high - max(open, close)`.
#[must_use]
pub fn upper_shadow(bar: BarTick) -> f64 {
    bar.high - upper_body(bar)
}

/// Lower shadow: `min(open, close) - low`.
#[must_use]
pub fn lower_shadow(bar: BarTick) -> f64 {
    lower_body(bar) - bar.low
}

/// Full range: `high - low`.
#[must_use]
pub fn high_low_range(bar: BarTick) -> f64 {
    bar.high - bar.low
}

/// Upper edge of the body: `max(open, close)`.
#[must_use]
pub fn upper_body(bar: BarTick) -> f64 {
    bar.open.max(bar.close)
}

/// Lower edge of the body: `min(open, close)`.
#[must_use]
pub fn lower_body(bar: BarTick) -> f64 {
    bar.open.min(bar.close)
}

/// Combined shadow length: range minus body.
#[must_use]
pub fn shadows(bar: BarTick) -> f64 {
    high_low_range(bar) - real_body(bar)
}

/// In-bar market efficiency: displacement over approximate distance
/// travelled, `body / (2 * range - body + eps)`.
#[must_use]
pub fn efficiency_ratio(bar: BarTick) -> f64 {
    let body = real_body(bar);
    let distance = 2.0 * high_low_range(bar) - body;
    body / (distance + TA_EPSILON)
}

/// All four legs finite: the precondition for affirming any pattern.
fn legs_finite(bar: BarTick) -> bool {
    bar.open.is_finite() && bar.high.is_finite() && bar.low.is_finite() && bar.close.is_finite()
}

/// Bullish-or-flat bar: `close >= open`.
#[must_use]
pub fn candle_color(bar: BarTick) -> bool {
    legs_finite(bar) && bar.close >= bar.open
}

/// Four-price bar: the whole range fits inside epsilon.
#[must_use]
pub fn four_price_doji(bar: BarTick) -> bool {
    legs_finite(bar) && bar.low >= bar.high - TA_EPSILON
}

/// Doji (including four-price and T shapes): `|open - close| <= eps`.
#[must_use]
pub fn doji(bar: BarTick) -> bool {
    legs_finite(bar) && (bar.open - bar.close).abs() <= TA_EPSILON
}

/// Dragonfly: a doji with a lower tail.
#[must_use]
pub fn dragonfly(bar: BarTick) -> bool {
    doji(bar) && bar.low < bar.close - TA_EPSILON
}

/// Gravestone: a doji with an upper tail.
#[must_use]
pub fn gravestone(bar: BarTick) -> bool {
    doji(bar) && bar.high > bar.close + TA_EPSILON
}

/// Gap up: the bar's range opens above the prior bar's high.
#[must_use]
pub fn gap_up(prev: BarTick, bar: BarTick) -> bool {
    legs_finite(prev) && legs_finite(bar) && bar.low > prev.high
}

/// Gap down: the bar's range opens below the prior bar's low.
#[must_use]
pub fn gap_down(prev: BarTick, bar: BarTick) -> bool {
    legs_finite(prev) && legs_finite(bar) && bar.high < prev.low
}

/// Real-body gap up: the body's lower edge clears the prior upper edge.
#[must_use]
pub fn real_body_gap_up(prev: BarTick, bar: BarTick) -> bool {
    legs_finite(prev) && legs_finite(bar) && lower_body(bar) > upper_body(prev)
}

/// Real-body gap down: the body's upper edge sits under the prior lower edge.
#[must_use]
pub fn real_body_gap_down(prev: BarTick, bar: BarTick) -> bool {
    legs_finite(prev) && legs_finite(bar) && upper_body(bar) < lower_body(prev)
}

#[cfg(all(test, feature = "research"))]
#[path = "../tests/candles.rs"]
mod tests;
