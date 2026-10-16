//! Risk-adjusted performance metrics over decimal inputs.
//!
//! The single home for Sharpe/Sortino/Calmar/win-rate/profit-factor math
//! (the analytics extraction every future report or sweep comparison calls
//! instead of copying formulas). Shapes follow the barter metric references
//! (excess over a baseline divided by a dispersion); every number is
//! reimplemented in [`Decimal`], never `f64`, and every degenerate input —
//! empty series, zero variance, zero drawdown, one-sided books — reports
//! [`None`], never an invented zero or an unbounded placeholder. That is the
//! correctness axis: absence of evidence is absence, not a number.
//!
//! Conventions shared by all five metrics:
//!
//! * Inputs are [`Copy`] decimal slices; no I/O, no clock reads.
//! * Annualization multiplies by `periods_per_year` (Calmar) or its square
//!   root (Sharpe/Sortino), under the usual IID-steps assumption, stated here
//!   so callers scale deliberately.
//! * Sharpe/Sortino take per-step *returns*; Calmar takes the *equity* curve
//!   (it needs the drawdown fraction too); win-rate/profit-factor take
//!   per-step *moves* (signed P&L deltas, zeros excluded as flat).
//! * Dispersion is the *population* deviation (divides by N): the series in
//!   hand is the whole population being judged, not a sample of it.

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps as _;

/// Daily-equivalent steps per year backing report-level annualization.
///
/// Reports annualize with this constant and say so beside the numbers;
/// bar-native callers with a known step size pass their own
/// `periods_per_year` to the functions below instead.
pub const TRADING_DAYS_PER_YEAR: u32 = 252;

/// Arithmetic mean of a series.
///
/// Returns [`None`] on empty input: no observations, no mean.
#[must_use]
pub fn mean(values: &[Decimal]) -> Option<Decimal> {
    if values.is_empty() {
        return None;
    }
    let total = values
        .iter()
        .try_fold(Decimal::ZERO, |sum, value| sum.checked_add(*value))?;
    total.checked_div(Decimal::from(values.len() as u64))
}

/// Population standard deviation of a series (divides by N).
///
/// Returns [`None`] on empty input or unrepresentable intermediate math. A
/// single observation (or a constant series) yields zero, which is defined —
/// callers map zero dispersion to [`None`] where a ratio needs it.
#[must_use]
pub fn population_std(values: &[Decimal]) -> Option<Decimal> {
    let center = mean(values)?;
    let squared = values.iter().try_fold(Decimal::ZERO, |sum, value| {
        let deviation = value.checked_sub(center)?;
        sum.checked_add(deviation.checked_mul(deviation)?)
    })?;
    squared
        .checked_div(Decimal::from(values.len() as u64))?
        .sqrt()
}

/// Annualized Sharpe ratio of per-step returns.
///
/// `(mean - risk_free_per_step) / population_std * sqrt(periods_per_year)`.
/// Returns [`None`] when the series is empty, the dispersion is zero, or any
/// step overflows decimal range.
#[must_use]
pub fn sharpe(
    step_returns: &[Decimal],
    risk_free_per_step: Decimal,
    periods_per_year: Decimal,
) -> Option<Decimal> {
    let spread = population_std(step_returns)?;
    if spread.is_zero() {
        return None;
    }
    let excess = mean(step_returns)?.checked_sub(risk_free_per_step)?;
    excess
        .checked_div(spread)?
        .checked_mul(periods_per_year.sqrt()?)
}

/// Annualized Sortino ratio of per-step returns.
///
/// `(mean - target) / downside_deviation * sqrt(periods_per_year)`, where
/// the downside deviation roots the mean squared shortfall below `target`
/// over *all* N steps (population-style, matching [`population_std`]).
/// `target_per_step` doubles as the shortfall threshold and the numerator
/// baseline. Returns [`None`] when the series is empty, nothing ever falls
/// short of target (no downside to price), or math overflows.
#[must_use]
pub fn sortino(
    step_returns: &[Decimal],
    target_per_step: Decimal,
    periods_per_year: Decimal,
) -> Option<Decimal> {
    if step_returns.is_empty() {
        return None;
    }
    let shortfall_squared = step_returns.iter().try_fold(Decimal::ZERO, |sum, value| {
        if *value >= target_per_step {
            return Some(sum);
        }
        let gap = target_per_step.checked_sub(*value)?;
        sum.checked_add(gap.checked_mul(gap)?)
    })?;
    let downside = shortfall_squared
        .checked_div(Decimal::from(step_returns.len() as u64))?
        .sqrt()?;
    if downside.is_zero() {
        return None;
    }
    let excess = mean(step_returns)?.checked_sub(target_per_step)?;
    excess
        .checked_div(downside)?
        .checked_mul(periods_per_year.sqrt()?)
}

/// Per-step simple returns of an equity curve, skipping pre-deployment zeros.
///
/// Steps whose base is zero (the flat-zero prefix before capital deploys)
/// have no rate to report and are skipped, the summary analog of the
/// first-nonzero-base rule in the cumulative-return helper. Steps that
/// overflow decimal range are skipped rather than panicking.
#[must_use]
pub fn step_returns(equity: &[Decimal]) -> Vec<Decimal> {
    let mut out = Vec::with_capacity(equity.len().saturating_sub(1));
    for window in equity.windows(2) {
        if window[0].is_zero() {
            continue;
        }
        let movement = window[1].checked_sub(window[0]);
        if let Some(rate) = movement.and_then(|delta| delta.checked_div(window[0])) {
            out.push(rate);
        }
    }
    out
}

/// Peak-to-trough drawdown as a fraction of the running peak.
///
/// Returns [`None`] when the curve never prints a positive peak (empty,
/// all-zero, or all-negative curves): there is no peak to fall from. A flat
/// positive curve reports zero, which is defined — callers map it to
/// [`None`] where a ratio needs it.
#[must_use]
pub fn drawdown_fraction(equity: &[Decimal]) -> Option<Decimal> {
    let mut peak = Decimal::ZERO;
    let mut worst: Option<Decimal> = None;
    for value in equity {
        if *value > peak {
            peak = *value;
        } else if !peak.is_zero() {
            let fraction = peak.checked_sub(*value)?.checked_div(peak)?;
            worst = Some(worst.map_or(fraction, |held| held.max(fraction)));
        }
    }
    worst
}

/// Annualized Calmar-style ratio of an equity curve.
///
/// `(mean_step_return - risk_free_per_step) * periods_per_year /
/// drawdown_fraction`. Returns [`None`] when no step return exists, the
/// drawdown is zero or absent, or math overflows.
#[must_use]
pub fn calmar(
    equity: &[Decimal],
    risk_free_per_step: Decimal,
    periods_per_year: Decimal,
) -> Option<Decimal> {
    let returns = step_returns(equity);
    let average = mean(&returns)?;
    let drawdown = drawdown_fraction(equity)?;
    if drawdown.is_zero() {
        return None;
    }
    average
        .checked_sub(risk_free_per_step)?
        .checked_mul(periods_per_year)?
        .checked_div(drawdown)
}

/// Win rate over per-step moves: winning steps over decisive steps.
///
/// Flat (zero) steps are excluded — holding still is neither a win nor a
/// loss. Returns [`None`] when no step moved.
#[must_use]
pub fn win_rate(step_moves: &[Decimal]) -> Option<Decimal> {
    let mut wins: u64 = 0;
    let mut decisive: u64 = 0;
    for step in step_moves {
        if step.is_zero() {
            continue;
        }
        decisive += 1;
        if step.is_sign_positive() {
            wins += 1;
        }
    }
    if decisive == 0 {
        return None;
    }
    Decimal::from(wins).checked_div(Decimal::from(decisive))
}

/// Profit factor over per-step moves: gross gains over gross losses.
///
/// Returns [`None`] for an empty book (nothing to divide) and for an
/// all-win book (unbounded upside is absence of a ratio, never an invented
/// ceiling). An all-loss book is genuinely zero and reports `Some(0)`.
#[must_use]
pub fn profit_factor(step_moves: &[Decimal]) -> Option<Decimal> {
    let mut gains = Decimal::ZERO;
    let mut losses = Decimal::ZERO;
    for step in step_moves {
        if step.is_sign_positive() {
            gains = gains.checked_add(*step)?;
        } else if step.is_sign_negative() {
            losses = losses.checked_sub(*step)?;
        }
    }
    if losses.is_zero() {
        return None;
    }
    gains.checked_div(losses)
}

#[cfg(test)]
#[path = "../tests/metrics.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/stats_spike.rs"]
mod stats_spike_tests;

#[cfg(test)]
#[path = "../tests/stats_generic_spike.rs"]
mod stats_generic_spike_tests;
