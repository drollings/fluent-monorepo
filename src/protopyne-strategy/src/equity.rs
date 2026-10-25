//! Equity trade management: sizing modes, stop/TP construction, breakeven
//! and trailing ratchets, exhaustion response, signal exits, and
//! cooldown/daily/session gates — the script's risk/management book encoded
//! as validated values and pure functions, not inline runner code.
//!
//! The stateless pieces implement the element traits where the signatures
//! fit: [`EquitySizer`] sizes, [`EquityStopLoss`]/[`EquityTarget`] guard one
//! open position each (constructed per trade, since the threshold is a
//! per-trade distance). Everything that needs memory across bars — trailing
//! ratchets, breakeven arms, partial-once flags, cooldown counters, daily
//! tallies — lives in runner-owned state updated through the pure
//! transitions below, so the rules stay testable without a venue. The
//! server's equity runner is the only production caller.

use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use crate::elements::{Monitor, MonitorAlert, Sizing};
use crate::error::StrategyError;
use crate::proposal::Direction;
use crate::risk::PortfolioFacts;
use protopyne_types::{Money, Qty};

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

/// Sizing for one equity trade. The per-trade numbers (equity, price, stop
/// distance) ride the instance because the [`Sizing`] signature only sees
/// the signal and the observed facts; when the observed facts carry net
/// liquidation it wins over the carried equity. Anything unmeasurable
/// sizes zero — downstream refuses a zero quantity with a reason.
#[derive(Debug, Clone, PartialEq)]
pub struct EquitySizer {
    mode: String,
    risk_pct: f64,
    size_pct: f64,
    fixed_qty: f64,
    max_notional_pct: f64,
    equity: Option<f64>,
    price: Option<f64>,
    stop_distance: Option<f64>,
}

impl EquitySizer {
    /// Build the policy from the strategy dials. Refuses unknown modes and
    /// out-of-window numbers; per-trade numbers arrive via
    /// [`for_trade`](Self::for_trade).
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
            equity: None,
            price: None,
            stop_distance: None,
        })
    }

    /// Bind the per-trade numbers: account equity, entry price, and the
    /// clamped stop distance the risk stop will rest at.
    #[must_use]
    pub fn for_trade(&self, equity: f64, price: f64, stop_distance: f64) -> Self {
        let mut out = self.clone();
        out.equity = Some(equity);
        out.price = Some(price);
        out.stop_distance = Some(stop_distance);
        out
    }

    /// Whole shares for the trade, floored, capped by max notional.
    /// Unmeasurable inputs (no equity, no price, no stop where the mode
    /// needs one) size zero.
    #[must_use]
    pub fn shares(&self, facts: &PortfolioFacts) -> u64 {
        let equity = facts
            .net_liq
            .as_ref()
            .and_then(|m| m.as_decimal().to_f64())
            .filter(|v| v.is_finite() && *v > 0.0)
            .or_else(|| self.equity.filter(|v| v.is_finite() && *v > 0.0));
        let price = self.price.filter(|v| v.is_finite() && *v > 0.0);
        let stop = self.stop_distance.filter(|v| v.is_finite() && *v > 0.0);
        let (Some(equity), Some(price)) = (equity, price) else {
            return 0;
        };
        let raw = match self.mode.as_str() {
            "Risk Percent" => stop.map_or(0.0, |d| equity * self.risk_pct / 100.0 / d),
            "Percent of Equity" => equity * self.size_pct / 100.0 / price,
            _ => self.fixed_qty,
        };
        if !raw.is_finite() || raw < 0.0 {
            return 0;
        }
        let cap = equity * self.max_notional_pct / 100.0 / price;
        raw.min(cap).floor().max(0.0) as u64
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

    fn size(&self, _signal: &crate::elements::SignalOutput, facts: &PortfolioFacts) -> Qty {
        Qty::new(Decimal::from(self.shares(facts)))
    }
}

/// One open position's stop as a [`LossMgmt`](crate::elements::LossMgmt)
/// rule: the runner constructs it per trade from the clamped stop distance
/// times the held quantity, and replaces it when a ratchet tightens. The
/// threshold only ever tightens — the runner enforces that, this rule just
/// fires past its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquityStopLoss {
    max_loss: Money,
}

impl EquityStopLoss {
    /// Build from the whole-position risk distance. Refuses non-positive
    /// thresholds: a stop that cannot be a trigger is a construction error.
    pub fn new(max_loss: Money) -> Result<Self, StrategyError> {
        if max_loss.as_decimal() <= Decimal::ZERO {
            return Err(StrategyError::InvalidElement(format!(
                "equity stop needs a positive threshold, got {max_loss}"
            )));
        }
        Ok(Self { max_loss })
    }

    /// The threshold this instance guards.
    #[must_use]
    pub fn threshold(&self) -> Money {
        self.max_loss
    }
}

impl crate::elements::LossMgmt for EquityStopLoss {
    fn name(&self) -> &str {
        "equity-stop"
    }

    fn review(
        &self,
        position: &crate::elements::PositionView<'_>,
        mark: &crate::elements::MarkContext,
    ) -> Vec<crate::elements::LossAction> {
        use crate::elements::LossAction;
        let Some(price) = mark.mark else {
            return Vec::new();
        };
        let per_unit = match position.side {
            Direction::Long => price.as_decimal() - position.entry_avg.as_decimal(),
            Direction::Short => position.entry_avg.as_decimal() - price.as_decimal(),
        };
        let pnl = per_unit * Decimal::from(position.quantity);
        if pnl < -self.max_loss.as_decimal() {
            vec![LossAction {
                kind: "stop".to_owned(),
                reason: format!("unrealised {pnl} past equity stop {}", self.max_loss),
            }]
        } else {
            Vec::new()
        }
    }
}

/// One take-profit zone as a rule: fires once the unrealised gain runs past
/// its line. The runner owns the weight and the once-per-zone fill; the
/// rule only names the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquityTarget {
    target: protopyne_types::Money,
}

impl EquityTarget {
    /// Build from the whole-position gain at this zone. Refuses
    /// non-positive lines.
    pub fn new(target: Money) -> Result<Self, StrategyError> {
        if target.as_decimal() <= Decimal::ZERO {
            return Err(StrategyError::InvalidElement(format!(
                "equity target needs a positive line, got {target}"
            )));
        }
        Ok(Self { target })
    }
}

impl crate::elements::LossMgmt for EquityTarget {
    fn name(&self) -> &str {
        "equity-target"
    }

    fn review(
        &self,
        position: &crate::elements::PositionView<'_>,
        mark: &crate::elements::MarkContext,
    ) -> Vec<crate::elements::LossAction> {
        use crate::elements::LossAction;
        let Some(price) = mark.mark else {
            return Vec::new();
        };
        let per_unit = match position.side {
            Direction::Long => price.as_decimal() - position.entry_avg.as_decimal(),
            Direction::Short => position.entry_avg.as_decimal() - price.as_decimal(),
        };
        let pnl = per_unit * Decimal::from(position.quantity);
        if pnl > self.target.as_decimal() {
            vec![LossAction {
                kind: "profit-target".to_owned(),
                reason: format!("unrealised {pnl} past equity target {}", self.target),
            }]
        } else {
            Vec::new()
        }
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

/// Monitor that surfaces gate breaches as alerts: the gate values above
/// decide, this names them on the [`Monitor`] channel. Stateless over the
/// values the runner threads in — the runner still owns the counters.
pub struct GateMonitor {
    /// What to blame in the alert.
    pub gate: &'static str,
    /// Whether the gate currently blocks.
    pub blocked: bool,
}

impl Monitor for GateMonitor {
    fn observe(
        &self,
        _facts: &PortfolioFacts,
        _positions: &[crate::elements::PositionView<'_>],
    ) -> Vec<MonitorAlert> {
        if self.blocked {
            vec![MonitorAlert {
                portfolio: "equity".to_owned(),
                message: format!("{} gate blocks new entries", self.gate),
            }]
        } else {
            Vec::new()
        }
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
