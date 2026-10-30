//! One position, one manager, one place where exits are decided.
//!
//! The manager is run state, not a supervised unit: it is created with the
//! run, mutated per bar, and dropped with the run, exactly like a grant
//! ledger. Its configuration is a [`PositionSpec`] (a schema-bearing view of
//! the strategy dials) so a form can render it; the transitions themselves
//! are pure over the tape and the open position.

use protopyne_types::session::TradingSession;
use protopyne_types::{Bar, Money};
use rust_decimal::prelude::ToPrimitive as _;

use fluent_wvr::{Describable, FieldAccess, FieldError};

use crate::equity::{
    breakeven_price, clamp_stop, normalize_tp_weights, ratchet_stop, stop_distance_atr,
    stop_distance_pct, stop_distance_swing, trail_chandelier, trail_percent, trail_ribbon_edge,
    CooldownGate, DailyLimits, PartialOnce, SessionGate,
};
use crate::integration_lite::IntegrationLiteStrategyConfig;
use crate::signals_lite::{entry_triggers, LiteLegs};
use crate::slots::exit::{ExitIntent, RestingPrice};
use crate::tape::Tape;

/// The manager's configuration: the strategy dials, rendered through the
/// same schema and field contract the strategy already exposes.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionSpec {
    /// The strategy dials the manager reads.
    pub strategy: IntegrationLiteStrategyConfig,
}

impl From<IntegrationLiteStrategyConfig> for PositionSpec {
    fn from(strategy: IntegrationLiteStrategyConfig) -> Self {
        Self { strategy }
    }
}

impl FieldAccess for PositionSpec {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        self.strategy.set_field(name, value)
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        self.strategy.get_field(name)
    }

    fn field_names(&self) -> &'static [&'static str] {
        self.strategy.field_names()
    }
}

impl Describable for PositionSpec {
    fn describe(&self) -> serde_json::Value {
        self.strategy.describe()
    }
}

/// One open position and its management state.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // independent latches, not a state machine
pub struct OpenPosition {
    /// Long or short.
    pub long: bool,
    /// Units held.
    pub qty: u32,
    /// Volume-weighted entry price.
    pub entry_price: f64,
    /// Entry bar index.
    pub entry_bar: usize,
    /// ATR at entry.
    pub entry_atr: f64,
    /// Resting stop.
    pub stop: f64,
    /// Target prices, when the zone is armed.
    pub targets: [Option<f64>; 3],
    /// Target fractions.
    pub target_weights: [f64; 3],
    /// Target zones already filled.
    pub target_fills: [bool; 3],
    /// Breakeven armed.
    pub be_armed: bool,
    /// Tight trailing mode.
    pub tight: bool,
    /// The one-shot partial already fired.
    pub partial: PartialOnce,
    /// Whether the partial fired.
    pub partial_fired: bool,
    /// The exhaustion response already fired.
    pub exhausted: bool,
    /// Best favourable price since entry.
    pub best: f64,
    /// Bars held, entry through this bar.
    pub bars_held: u64,
}

/// Run-owned position state: one open position, cooldown, daily tallies, and
/// the session gate.
pub struct PositionManager {
    spec: PositionSpec,
    position: Option<OpenPosition>,
    cooldown: CooldownGate,
    daily: DailyLimits,
    session: Option<SessionGate>,
    calendar: TradingSession,
    current_day: Option<(i32, u32, u32)>,
    close_min: i64,
    bar_span_ms: i64,
}

impl PositionManager {
    /// Build the manager from its configuration.
    #[must_use]
    pub fn new(
        spec: PositionSpec,
        calendar: TradingSession,
        bar_span_ms: i64,
        session_filter: bool,
    ) -> Self {
        let strategy = &spec.strategy;
        let close_min = i64::from(calendar.close_minute());
        Self {
            cooldown: CooldownGate {
                bars: strategy.cd_bars,
                loss_bars: strategy.cd_loss_bars,
                last_exit: None,
            },
            daily: DailyLimits {
                max_trades: strategy.max_trades_day,
                max_consec_losses: strategy.max_consec_losses,
                max_daily_loss_pct: strategy.max_daily_loss_pct,
                trades_today: 0,
                consec_losses: 0,
                day_open_equity: None,
                equity_now: None,
                halted: false,
            },
            session: (strategy.flatten_eod && session_filter).then_some(SessionGate {
                flatten_min_before: strategy.flatten_min_before,
                noentry_min: strategy.noentry_min,
                close_min,
            }),
            current_day: None,
            close_min,
            bar_span_ms,
            calendar,
            spec,
            position: None,
        }
    }

    /// The strategy dials.
    #[must_use]
    pub fn spec(&self) -> &PositionSpec {
        &self.spec
    }

    /// The open position, when one is carried.
    #[must_use]
    pub fn position(&self) -> Option<&OpenPosition> {
        self.position.as_ref()
    }

    /// Whether the book is flat.
    #[must_use]
    pub fn is_flat(&self) -> bool {
        self.position.is_none()
    }

    /// The daily tallies.
    #[must_use]
    pub fn daily(&self) -> &DailyLimits {
        &self.daily
    }

    /// The cooldown gate.
    #[must_use]
    pub fn cooldown(&self) -> &CooldownGate {
        &self.cooldown
    }

    /// Open a position from a filled entry.
    pub fn open(
        &mut self,
        long: bool,
        qty: u32,
        entry: f64,
        entry_bar: usize,
        atr: f64,
        bars: &[Bar],
    ) -> OpenPosition {
        let strategy = &self.spec.strategy;
        let stop_distance = entry_stop_distance(strategy, atr, entry, bars, entry_bar, long);
        let stop = if long {
            entry - stop_distance
        } else {
            entry + stop_distance
        };
        let weights = normalize_tp_weights([strategy.tp1_pct, strategy.tp2_pct, strategy.tp3_pct]);
        let mults = [strategy.tp1_mult, strategy.tp2_mult, strategy.tp3_mult];
        let mut targets = [None; 3];
        for (index, mult) in mults.iter().enumerate() {
            let distance = if strategy.tp_basis == "ATR" {
                atr * mult
            } else {
                stop_distance * mult
            };
            if distance.is_finite() && distance > 0.0 && weights[index] > 0.0 {
                targets[index] = Some(if long {
                    entry + distance
                } else {
                    entry - distance
                });
            }
        }
        let position = OpenPosition {
            long,
            qty,
            entry_price: entry,
            entry_bar,
            entry_atr: atr,
            stop,
            targets,
            target_weights: weights,
            target_fills: [false; 3],
            be_armed: false,
            tight: false,
            partial: PartialOnce::default(),
            partial_fired: false,
            exhausted: false,
            best: entry,
            bars_held: 0,
        };
        self.position = Some(position.clone());
        position
    }

    /// Close the book, returning the closed position.
    pub fn close(&mut self) -> Option<OpenPosition> {
        self.position.take()
    }

    /// Take `units` out of the open position, returning the closed slice.
    /// A request at or past the remaining quantity closes the book.
    pub fn take_units(&mut self, units: u32) -> Option<OpenPosition> {
        let position = self.position.as_mut()?;
        let units = units.min(position.qty);
        if units == 0 {
            return None;
        }
        let mut slice = position.clone();
        slice.qty = units;
        position.qty -= units;
        if position.qty == 0 {
            self.position = None;
        }
        Some(slice)
    }

    /// Record a day boundary and the close mark.
    pub fn begin_bar(&mut self, bar: &Bar, _close: f64, equity_now: f64) {
        let ts_ms = bar.ts.timestamp_millis();
        self.close_min = i64::from(TradingSession::wall_minutes(ts_ms + self.bar_span_ms));
        let day = self.calendar.session_date(ts_ms);
        if self.current_day != Some(day) {
            self.current_day = Some(day);
            self.daily.day_open_equity = Some(equity_now);
            self.daily.trades_today = 0;
            self.daily.consec_losses = 0;
            self.daily.halted = false;
        }
        self.daily.equity_now = Some(equity_now);
        if let Some(position) = self.position.as_mut() {
            let high = bar.h.to_f64().unwrap_or(position.best);
            let low = bar.l.to_f64().unwrap_or(position.best);
            position.best = if position.long {
                position.best.max(high)
            } else {
                position.best.min(low)
            };
            position.bars_held = position.bars_held.saturating_add(1);
        }
    }

    /// Whether entries are allowed on this bar.
    #[must_use]
    pub fn may_enter(&self, index: usize) -> bool {
        let close_min = self.close_min;
        let in_window = self.session.is_none_or(|gate| gate.may_enter(close_min));
        in_window && self.cooldown.may_enter(index) && self.daily.may_enter()
    }

    /// Whether the session or the daily cap demands a flatten now.
    #[must_use]
    pub fn forced_flatten(&self) -> Option<&'static str> {
        if self
            .session
            .is_some_and(|gate| gate.must_flatten(self.close_min))
        {
            return Some("session-flatten");
        }
        if self.daily.must_flatten() {
            return Some("daily-loss");
        }
        None
    }

    /// Record an exit into the cooldown and daily tallies.
    pub fn note_exit(&mut self, index: usize, pnl: f64) {
        let lost = pnl < 0.0;
        self.cooldown.last_exit = Some((index, lost));
        if lost {
            self.daily.consec_losses += 1;
        } else {
            self.daily.consec_losses = 0;
        }
    }

    /// Record a filled entry into the daily trade count.
    pub fn note_entry(&mut self) {
        self.daily.trades_today = self.daily.trades_today.saturating_add(1);
    }

    /// Latch the rest-of-day halt after a daily-loss flatten. The reference
    /// halts entries for the rest of the day on breach; the day rollover in
    /// [`begin_bar`](Self::begin_bar) clears it. Callers that only flatten
    /// (the legacy equity path) never call this and are unaffected.
    pub fn latch_day_halt(&mut self) {
        self.daily.halted = true;
    }

    /// The exit pass for one bar: exhaustion, stops, targets, signal exits,
    /// time stop, then the breakeven/trailing ratchet. At most one intent is
    /// returned; the runner submits it and feeds the fill back.
    ///
    /// Single authority: the firing order lives only here. Drivers check
    /// [`forced_flatten`](Self::forced_flatten) first, then submit these
    /// intents in order without re-sorting; advisors ([`ExitRule`](crate::slots::exit::ExitRule),
    /// equity helpers) propose values, they never reorder.
    pub fn review_exits(&mut self, tape: &Tape<'_>, bars: &[Bar], index: usize) -> Vec<ExitIntent> {
        let strategy = &self.spec.strategy;
        let Some(position) = self.position.as_mut() else {
            return Vec::new();
        };
        let legs = tape.legs();
        let close = tape.close(index).unwrap_or(f64::NAN);
        let high = bars[index].h.to_f64().unwrap_or(f64::NAN);
        let low = bars[index].l.to_f64().unwrap_or(f64::NAN);
        let atr = legs.atr.get(index).copied().flatten().unwrap_or(f64::NAN);
        let (prev_fast, fast) = pair_at(&legs.fast, index);
        let crossed_back_against = match (prev_fast, fast) {
            (Some(prev), Some(curr)) if position.long => {
                prev >= strategy.sig_exhaust_level && curr < strategy.sig_exhaust_level
            }
            (Some(prev), Some(curr)) if !position.long => {
                prev <= -strategy.sig_exhaust_level && curr > -strategy.sig_exhaust_level
            }
            _ => false,
        };
        if !position.exhausted
            && (crossed_back_against || dual_against(strategy, legs, index, position.long))
        {
            position.exhausted = true;
            let in_profit = if position.long {
                close > position.entry_price
            } else {
                close < position.entry_price
            };
            match strategy.exit_action.as_str() {
                "Off" => {}
                "Close All" => {
                    return vec![ExitIntent::close("exhaustion-close", "exhaustion-close")];
                }
                "Partial Close" | "Tighten + Partial" => {
                    if position
                        .partial
                        .may_fire(true, in_profit, strategy.exit_only_profit)
                    {
                        let qty = (f64::from(position.qty) * strategy.exit_partial_pct / 100.0)
                            .floor() as u32;
                        if qty > 0 && qty < position.qty {
                            position.partial.latch();
                            position.partial_fired = true;
                            position.tight = true;
                            return vec![ExitIntent::partial_units(
                                "exhaustion-partial",
                                "exhaustion-partial",
                                qty,
                                RestingPrice::Market,
                            )];
                        }
                    }
                    if strategy.exit_action == "Tighten + Partial" {
                        position.tight = true;
                    }
                }
                _ => position.tight = true,
            }
            if strategy.trail_on_exhaustion {
                position.tight = true;
            }
        }
        if position.long && low <= position.stop {
            return vec![ExitIntent::resting("stop", "stop", money(position.stop))];
        }
        if !position.long && high >= position.stop {
            return vec![ExitIntent::resting("stop", "stop", money(position.stop))];
        }
        for zone in 0..3 {
            if position.target_fills[zone] {
                continue;
            }
            let Some(target) = position.targets[zone] else {
                continue;
            };
            let touched = if position.long {
                high >= target
            } else {
                low <= target
            };
            if !touched {
                continue;
            }
            let qty =
                (f64::from(position.qty) * position.target_weights[zone] / 100.0).floor() as u32;
            if qty > 0 && qty < position.qty {
                position.target_fills[zone] = true;
                if strategy.be_on_tp && atr.is_finite() && atr > 0.0 {
                    position.be_armed = true;
                    let be = breakeven_price(
                        position.long,
                        position.entry_price,
                        strategy.be_offset_atr,
                        atr,
                    );
                    position.stop = ratchet_stop(position.long, position.stop, be);
                }
                return vec![ExitIntent::partial_units(
                    "take-profit",
                    "take-profit",
                    qty,
                    RestingPrice::At(money(target)),
                )];
            }
            if qty >= position.qty && position.qty > 0 {
                position.target_fills[zone] = true;
                return vec![ExitIntent::resting(
                    "take-profit",
                    "take-profit",
                    money(target),
                )];
            }
        }
        if strategy.exit_opposite {
            let (long_armed, short_armed) = entry_triggers(strategy, legs, bars, index);
            let opposite = if position.long {
                short_armed
            } else {
                long_armed
            };
            if opposite {
                return vec![ExitIntent::close("opposite-signal", "opposite-signal")];
            }
        }
        if strategy.exit_ribbon && ribbon_against(legs, index, position.long) {
            return vec![ExitIntent::close("ribbon-flip", "ribbon-flip")];
        }
        if strategy.exit_zero_cross && zero_against(legs, index, position.long) {
            return vec![ExitIntent::close("zero-cross", "zero-cross")];
        }
        if strategy.exit_time_bars > 0
            && index.saturating_sub(position.entry_bar) as i64 >= strategy.exit_time_bars
        {
            return vec![ExitIntent::close("time-stop", "time-stop")];
        }
        if strategy.be_enable && atr.is_finite() && atr > 0.0 {
            let profit_trigger = strategy.be_trigger_atr <= 0.0
                || (if position.long {
                    close - position.entry_price
                } else {
                    position.entry_price - close
                } >= strategy.be_trigger_atr * atr);
            if profit_trigger || position.be_armed {
                position.be_armed = true;
                let be = breakeven_price(
                    position.long,
                    position.entry_price,
                    strategy.be_offset_atr,
                    atr,
                );
                position.stop = ratchet_stop(position.long, position.stop, be);
            }
        }
        let profit_atr = if atr.is_finite() && atr > 0.0 {
            (if position.long {
                close - position.entry_price
            } else {
                position.entry_price - close
            }) / atr
        } else {
            0.0
        };
        let active = strategy.trail_immediate
            || (strategy.trail_on_tp && position.target_fills.iter().any(|filled| *filled))
            || (strategy.trail_on_exhaustion && position.tight)
            || (strategy.trail_on_profit && profit_atr >= strategy.trail_profit_atr);
        if active && atr.is_finite() && atr > 0.0 {
            let mult = if position.tight {
                strategy.trail_tight_mult
            } else {
                strategy.trail_mult
            };
            let candidate = match strategy.trail_type.as_str() {
                "Percent" => {
                    let pct = if position.tight {
                        strategy.trail_pct / 2.0
                    } else {
                        strategy.trail_pct
                    };
                    trail_percent(position.long, position.best, pct)
                }
                "Ribbon Edge" => {
                    let buffer = if position.tight {
                        0.0
                    } else {
                        strategy.trail_ribbon_buf
                    };
                    let edge = ribbon_edge(legs, index, position.long);
                    trail_ribbon_edge(position.long, edge, buffer, atr)
                }
                _ => trail_chandelier(position.long, position.best, atr, mult),
            };
            position.stop = ratchet_stop(position.long, position.stop, candidate);
        }
        Vec::new()
    }
}

/// Entry stop distance from the configured construction, clamped into the
/// ATR band.
#[must_use]
pub fn entry_stop_distance(
    strategy: &IntegrationLiteStrategyConfig,
    atr: f64,
    close: f64,
    bars: &[Bar],
    index: usize,
    long: bool,
) -> f64 {
    if !atr.is_finite() || atr <= 0.0 {
        return f64::NAN;
    }
    let raw = match strategy.stop_type.as_str() {
        "Percent" => stop_distance_pct(close, strategy.stop_pct).unwrap_or(f64::NAN),
        "Swing" => {
            let span = strategy.swing_len.max(1) as usize;
            let start = (index + 1).saturating_sub(span);
            let window = bars.get(start..=index).unwrap_or(&[]);
            let extreme = if long {
                window
                    .iter()
                    .filter_map(|bar| bar.l.to_f64())
                    .fold(f64::INFINITY, f64::min)
            } else {
                window
                    .iter()
                    .filter_map(|bar| bar.h.to_f64())
                    .fold(f64::NEG_INFINITY, f64::max)
            };
            if !extreme.is_finite() {
                return f64::NAN;
            }
            stop_distance_swing(long, close, extreme, strategy.swing_buf_atr, atr)
                .unwrap_or(f64::NAN)
        }
        _ => stop_distance_atr(atr, strategy.stop_atr_mult).unwrap_or(f64::NAN),
    };
    if !raw.is_finite() {
        return f64::NAN;
    }
    clamp_stop(raw, atr, strategy.min_stop_atr, strategy.max_stop_atr)
}

/// Test-visible money conversion for hand-checked stop prices.
#[doc(hidden)]
#[must_use]
pub fn money_for_test(value: f64) -> Money {
    money(value)
}

fn money(value: f64) -> Money {
    // (M7.3c) `Money::from_f64_or_zero` — the one f64→Money spelling.
    Money::from_f64_or_zero(value)
}

fn pair_at(series: &[Option<f64>], index: usize) -> (Option<f64>, Option<f64>) {
    // (M4.1) The one index-guard behind the crossing wrappers: the previous
    // and current readings, each flattened. Callers keep their own
    // threshold/level math and kernel delegates.
    let prev = index
        .checked_sub(1)
        .and_then(|prev| series.get(prev).copied().flatten());
    (prev, series.get(index).copied().flatten())
}

fn dual_against(
    strategy: &IntegrationLiteStrategyConfig,
    legs: &LiteLegs,
    index: usize,
    long: bool,
) -> bool {
    if !strategy.sig_dual || !strategy.sig_dual_exit {
        return false;
    }
    let lookback = strategy.sig_dual_lookback.max(0) as usize;
    let down = crossed_down_at(&legs.slow, strategy.sig_dual_slow, index);
    let up = crossed_up_at(&legs.slow, -strategy.sig_dual_slow, index);
    let fast_beyond =
        |level: f64, below: bool| {
            (0..=lookback.min(index)).any(|back| {
                legs.fast[index - back].is_some_and(|value| {
                    if below {
                        value < level
                    } else {
                        value > level
                    }
                })
            })
        };
    if long {
        down && fast_beyond(strategy.sig_dual_fast, false)
    } else {
        up && fast_beyond(-strategy.sig_dual_fast, true)
    }
}

fn crossed_up_at(series: &[Option<f64>], level: f64, index: usize) -> bool {
    let (prev, curr) = pair_at(series, index);
    protopyne_kernels::util::crossed(prev, curr, level)
        == Some(protopyne_kernels::util::Crossing::Up)
}

fn crossed_down_at(series: &[Option<f64>], level: f64, index: usize) -> bool {
    let (prev, curr) = pair_at(series, index);
    protopyne_kernels::util::crossed(prev, curr, level)
        == Some(protopyne_kernels::util::Crossing::Down)
}

fn ribbon_against(legs: &LiteLegs, index: usize, long: bool) -> bool {
    let (prev_adaptive, curr_adaptive) = pair_at(&legs.adaptive, index);
    let (prev_signal, curr_signal) = pair_at(&legs.signal, index);
    protopyne_kernels::util::crossed_between(prev_adaptive, curr_adaptive, prev_signal, curr_signal)
        == Some(if long {
            protopyne_kernels::util::Crossing::Down
        } else {
            protopyne_kernels::util::Crossing::Up
        })
}

fn zero_against(legs: &LiteLegs, index: usize, long: bool) -> bool {
    if long {
        crossed_down_at(&legs.fast, 0.0, index)
    } else {
        crossed_up_at(&legs.fast, 0.0, index)
    }
}

fn ribbon_edge(legs: &LiteLegs, index: usize, long: bool) -> f64 {
    let (Some(adaptive), Some(signal)) = (legs.adaptive[index], legs.signal[index]) else {
        return f64::NAN;
    };
    if long {
        adaptive.min(signal)
    } else {
        adaptive.max(signal)
    }
}

#[cfg(test)]
#[path = "../tests/position.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/heuristic_calibration.rs"]
mod heuristic_calibration_tests;
