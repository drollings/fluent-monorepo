//! Equity trade management: sizing modes, stop/TP construction, breakeven
//! and trailing ratchets, exhaustion response, signal exits, and
//! cooldown/daily/session gates — the script's risk/management book encoded
//! as validated values and pure functions, not inline runner code.
//!
//! [`EquitySizer`] sizes through the priced unit; the pure transitions
//! below (breakeven, ratchets, trailing candidates, cooldown/daily/session
//! gates, partial-once latches) carry no memory across bars — the run-owned
//! position manager holds the counters and threads them through, so the rules
//! stay testable without a venue.

use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use crate::axes::GateAxis;
use crate::elements::{Sizing, SizingCtx};
use crate::error::StrategyError;
use crate::risk::PortfolioFacts;
use protopyne_types::Qty;

/// Sizing-mode options (script spelling).
#[must_use]
pub fn equity_size_modes() -> Vec<String> {
    ["Risk Percent", "Percent of Equity", "Fixed Quantity"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Normalize TP-zone weights when they exceed 100%: scale down
/// proportionally (the script's normalization), leave smaller sums alone.
/// A zero total stays zero — disabled zones, whatever is left runs on the
/// trailing stop.
#[must_use]
pub fn normalize_tp_weights(weights: [f64; 3]) -> [f64; 3] {
    let total: f64 = weights.iter().sum();
    if total > 100.0 && total.is_finite() && total > 0.0 {
        let scale = 100.0 / total;
        [weights[0] * scale, weights[1] * scale, weights[2] * scale]
    } else {
        weights
    }
}

/// ATR stop distance: a multiple of the bar ATR. Refuses non-positive or
/// non-finite ATR and non-positive multiples.
pub fn stop_distance_atr(atr: f64, mult: f64) -> Result<f64, StrategyError> {
    if !atr.is_finite() || atr <= 0.0 {
        return Err(StrategyError::InvalidElement(format!(
            "ATR stop needs a positive finite ATR, got {atr}"
        )));
    }
    if !mult.is_finite() || mult <= 0.0 {
        return Err(StrategyError::InvalidElement(format!(
            "ATR multiple must be positive, got {mult}"
        )));
    }
    Ok(atr * mult)
}

/// Percent stop distance off the entry price.
pub fn stop_distance_pct(price: f64, pct: f64) -> Result<f64, StrategyError> {
    if !price.is_finite() || price <= 0.0 {
        return Err(StrategyError::InvalidElement(format!(
            "percent stop needs a positive finite price, got {price}"
        )));
    }
    if !pct.is_finite() || pct <= 0.0 {
        return Err(StrategyError::InvalidElement(format!(
            "stop percent must be positive, got {pct}"
        )));
    }
    Ok(price * pct / 100.0)
}

/// Swing stop distance: beyond the recent extreme by a buffer in ATRs.
/// Long reads the lowest low, short the highest high.
pub fn stop_distance_swing(
    long: bool,
    price: f64,
    extreme: f64,
    buf_atr: f64,
    atr: f64,
) -> Result<f64, StrategyError> {
    if !atr.is_finite() || atr <= 0.0 {
        return Err(StrategyError::InvalidElement(format!(
            "swing stop needs a positive finite ATR, got {atr}"
        )));
    }
    if !price.is_finite() || !extreme.is_finite() {
        return Err(StrategyError::InvalidElement(
            "swing stop needs finite price and extreme".to_owned(),
        ));
    }
    let raw = if long {
        price - extreme
    } else {
        extreme - price
    };
    Ok((raw + buf_atr * atr).max(0.0))
}

/// Clamp a stop distance into `[min_mult, max_mult]` ATRs. The floor keeps
/// dust stops from tripping on spread; the ceiling keeps far stops honest.
pub fn clamp_stop(distance: f64, atr: f64, min_mult: f64, max_mult: f64) -> f64 {
    distance.clamp(min_mult * atr, max_mult * atr)
}

/// Sizing for one equity trade. The per-trade numbers arrive with the
/// priced unit through [`Sizing::size_for`]: the equity path builds the
/// unit from the close and the clamped stop distance every bar, so no
/// per-trade state rides the instance and no
/// second sizing path can drift from the first. Anything unmeasurable
/// sizes zero — downstream refuses a zero quantity with a reason.
#[derive(Debug, Clone, PartialEq)]
pub struct EquitySizer {
    mode: String,
    risk_pct: f64,
    size_pct: f64,
    fixed_qty: f64,
    max_notional_pct: f64,
}

impl EquitySizer {
    /// Build the policy from the strategy dials. Refuses unknown modes and
    /// out-of-window numbers; per-trade numbers arrive with the priced unit
    /// through [`size_for`](Sizing::size_for).
    pub fn new(
        mode: &str,
        risk_pct: f64,
        size_pct: f64,
        fixed_qty: f64,
        max_notional_pct: f64,
    ) -> Result<Self, StrategyError> {
        if !equity_size_modes().iter().any(|m| m == mode) {
            return Err(StrategyError::InvalidElement(format!(
                "unknown sizing mode {mode:?}"
            )));
        }
        if !(risk_pct > 0.0 && risk_pct <= 100.0 && risk_pct.is_finite()) {
            return Err(StrategyError::InvalidElement(format!(
                "risk percent {risk_pct} outside (0, 100]"
            )));
        }
        if !(size_pct > 0.0 && size_pct <= 1000.0 && size_pct.is_finite()) {
            return Err(StrategyError::InvalidElement(format!(
                "notional percent {size_pct} outside (0, 1000]"
            )));
        }
        if !(fixed_qty >= 0.0 && fixed_qty.is_finite()) {
            return Err(StrategyError::InvalidElement(format!(
                "fixed quantity {fixed_qty} must be finite and non-negative"
            )));
        }
        if !(max_notional_pct > 0.0 && max_notional_pct.is_finite()) {
            return Err(StrategyError::InvalidElement(format!(
                "max notional {max_notional_pct} must be positive and finite"
            )));
        }
        Ok(Self {
            mode: mode.to_owned(),
            risk_pct,
            size_pct,
            fixed_qty,
            max_notional_pct,
        })
    }
}

impl Sizing for EquitySizer {
    fn name(&self) -> &str {
        match self.mode.as_str() {
            "Risk Percent" => "risk-percent",
            "Percent of Equity" => "percent-equity",
            _ => "fixed",
        }
    }

    fn axis(&self) -> GateAxis {
        // The policy expresses allocation (TaskValue). Its internal
        // max-notional cap (`size_for`: `raw.min(cap)`) is the Correctness
        // bound on this path, and it lives in exactly that one place.
        GateAxis::TaskValue
    }

    /// Compatibility shim: the unit context is absent here, so this always
    /// sizes zero. Size through [`size_for`](Sizing::size_for) with a priced
    /// unit instead — a zero here is the documented absence, never a trade.
    fn size(&self, _signal: &crate::elements::SignalOutput, _facts: &PortfolioFacts) -> Qty {
        Qty::zero()
    }

    /// Whole shares for the trade, floored, capped by max notional.
    /// Unmeasurable inputs (no equity, no price, no stop where the mode
    /// needs one) size zero. Observed net liquidation wins over the
    /// caller's marked equity when both are present.
    fn size_for(&self, _signal: &crate::elements::SignalOutput, ctx: &SizingCtx<'_>) -> Qty {
        let equity = ctx
            .facts
            .net_liq
            .as_ref()
            .and_then(|m| m.as_decimal().to_f64())
            .filter(|v| v.is_finite() && *v > 0.0)
            .or_else(|| ctx.equity.filter(|v| v.is_finite() && *v > 0.0));
        let price = ctx
            .unit
            .price_per_unit
            .as_decimal()
            .to_f64()
            .filter(|v| v.is_finite() && *v > 0.0);
        let stop = ctx
            .unit
            .max_loss_per_unit
            .as_decimal()
            .to_f64()
            .filter(|v| v.is_finite() && *v > 0.0);
        let (Some(equity), Some(price)) = (equity, price) else {
            return Qty::zero();
        };
        let raw = match self.mode.as_str() {
            "Risk Percent" => stop.map_or(0.0, |d| equity * self.risk_pct / 100.0 / d),
            "Percent of Equity" => equity * self.size_pct / 100.0 / price,
            _ => self.fixed_qty,
        };
        if !raw.is_finite() || raw < 0.0 {
            return Qty::zero();
        }
        let cap = equity * self.max_notional_pct / 100.0 / price;
        Qty::new(Decimal::from(raw.min(cap).floor().max(0.0) as u64))
    }
}

/// Breakeven stop price: entry shifted past zero by the offset in ATRs, so
/// fees and slippage do not turn a scratch into a loss. Long adds, short
/// subtracts.
#[must_use]
pub fn breakeven_price(long: bool, entry: f64, offset_atr: f64, atr: f64) -> f64 {
    if long {
        entry + offset_atr * atr
    } else {
        entry - offset_atr * atr
    }
}

/// Ratchet a long stop upward only (short: downward only). A candidate that
/// would loosen the stop is ignored — stops never loosen, so the worst
/// exit only ever improves.
#[must_use]
pub fn ratchet_stop(long: bool, current: f64, candidate: f64) -> f64 {
    if long {
        current.max(candidate)
    } else {
        current.min(candidate)
    }
}

/// Chandelier trailing candidate: the best price since entry pulled back by
/// the multiple in ATRs.
#[must_use]
pub fn trail_chandelier(long: bool, best: f64, atr: f64, mult: f64) -> f64 {
    if long {
        best - mult * atr
    } else {
        best + mult * atr
    }
}

/// Percent trailing candidate off the best price since entry.
#[must_use]
pub fn trail_percent(long: bool, best: f64, pct: f64) -> f64 {
    if long {
        best * (1.0 - pct / 100.0)
    } else {
        best * (1.0 + pct / 100.0)
    }
}

/// Ribbon-edge trailing candidate: the band edge pulled back by the buffer
/// in ATRs (long trails the lower edge, short the upper).
#[must_use]
pub fn trail_ribbon_edge(long: bool, edge: f64, buf_atr: f64, atr: f64) -> f64 {
    if long {
        edge - buf_atr * atr
    } else {
        edge + buf_atr * atr
    }
}

/// Cooldown gate as a pure value: the runner carries the last exit, this
/// answers whether a bar may enter. A losing exit adds the extra loss
/// cooldown. Constructed per bar from carried state — no hidden clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CooldownGate {
    /// Cooldown after any exit, in bars.
    pub bars: i64,
    /// Extra cooldown after a losing exit, in bars.
    pub loss_bars: i64,
    /// Last exit bar and whether it lost, when one happened.
    pub last_exit: Option<(usize, bool)>,
}

impl CooldownGate {
    /// Whether `bar` may enter. No exit on record always passes.
    #[must_use]
    pub fn may_enter(&self, bar: usize) -> bool {
        let Some((exit_bar, lost)) = self.last_exit else {
            return true;
        };
        let wait = self.bars + if lost { self.loss_bars } else { 0 };
        bar.saturating_sub(exit_bar) as i64 > wait
    }
}

/// Daily trade limits as pure values: the runner carries the day's counts,
/// these answer whether a new entry may arm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DailyLimits {
    /// Maximum trades per day (0 disables).
    pub max_trades: i64,
    /// Halt after this many consecutive losses (0 disables).
    pub max_consec_losses: i64,
    /// Flatten and halt at this daily loss percent (0 disables).
    pub max_daily_loss_pct: f64,
    /// Trades taken today.
    pub trades_today: i64,
    /// Consecutive losses into today.
    pub consec_losses: i64,
    /// Day-open equity for the loss gauge (`None` = unmeasurable, blocks
    /// only the loss gauge, never entries on its own).
    pub day_open_equity: Option<f64>,
    /// Current equity for the loss gauge.
    pub equity_now: Option<f64>,
    /// Halt latched for the rest of the day.
    pub halted: bool,
}

impl DailyLimits {
    /// Whether a new entry may arm. Counts gate first; the loss gauge
    /// flattens only on a measured breach, never on missing data.
    #[must_use]
    pub fn may_enter(&self) -> bool {
        if self.halted {
            return false;
        }
        if self.max_trades > 0 && self.trades_today >= self.max_trades {
            return false;
        }
        if self.max_consec_losses > 0 && self.consec_losses >= self.max_consec_losses {
            return false;
        }
        true
    }

    /// Whether the day must flatten now: a measured daily loss at or past
    /// the cap. Unmeasurable equity never flattens.
    #[must_use]
    pub fn must_flatten(&self) -> bool {
        if self.max_daily_loss_pct <= 0.0 {
            return false;
        }
        match (self.day_open_equity, self.equity_now) {
            (Some(open), Some(now)) if open > 0.0 => {
                (now - open) / open * 100.0 <= -self.max_daily_loss_pct
            }
            _ => false,
        }
    }
}

/// Session end-of-day gate as a pure value: flatten this many minutes before
/// the session close, and refuse new entries inside the no-entry window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionGate {
    /// Minutes before the session close to flatten (0 = at the close bar).
    pub flatten_min_before: i64,
    /// No new entries inside the final minutes.
    pub noentry_min: i64,
    /// Session close in ET minutes.
    pub close_min: i64,
}

impl SessionGate {
    /// Whether the bar (in ET minutes) must flatten an open position.
    #[must_use]
    pub fn must_flatten(&self, bar_min: i64) -> bool {
        bar_min >= self.close_min - self.flatten_min_before
    }

    /// Whether the bar may open a new position.
    #[must_use]
    pub fn may_enter(&self, bar_min: i64) -> bool {
        bar_min < self.close_min - self.noentry_min
    }
}

/// Exhaustion partial state: fires at most once per position. The runner
/// carries the flag; this answers whether the current bar may take the
/// partial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PartialOnce {
    taken: bool,
}

impl PartialOnce {
    /// Whether the partial may fire now: only when the response calls for
    /// it, the trade is in profit if required, and nothing fired before.
    #[must_use]
    pub fn may_fire(&self, armed: bool, in_profit: bool, only_profit: bool) -> bool {
        !self.taken && armed && (!only_profit || in_profit)
    }

    /// Latch the fill so a later exhaustion cannot re-fire.
    pub fn latch(&mut self) {
        self.taken = true;
    }
}

#[cfg(test)]
#[path = "../tests/equity.rs"]
mod tests;
