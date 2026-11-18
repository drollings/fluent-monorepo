//! Shared equity-run types and per-bar projections: the trade record, the
//! run frame, the status-table projection, the resting-level points, the
//! closed-cycle tally, and the cycle fold. The simple decision stack
//! (`simple_backtest`) owns the loop; this module owns the shapes the loop,
//! the views, and the report all read, so the math stays testable without
//! a venue.
//!
//! One open position at a time; stops and targets rest as explicit prices.
//! Trailing and breakeven only ever tighten the resting stop, never
//! loosen it.

use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use protopyne_strategy::position::{OpenPosition, PositionManager};
use protopyne_types::Money;
use protopyne_view::BacktestStatusView;

use crate::report::{BacktestReport, TradePnl};
use crate::runner::BarDecision;

/// Which resting line a level point belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelKind {
    /// The trailing/breakeven resting stop.
    Stop,
    /// A take-profit zone that has not filled yet.
    Target,
}

/// One resting level as it stood at a bar close: the stop or an unfilled
/// target while a position is open, nothing while flat. The terminal draws
/// one polyline per kind from these points, breaking the line wherever a
/// bar carries no point for it.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelPoint {
    /// Bar index.
    pub bar: usize,
    /// Resting price.
    pub price: f64,
    /// Which line this point belongs to.
    pub kind: LevelKind,
}

/// One closed equity trade, in close order.
///
/// Deliberately not the session book's trade record: this run is
/// single-symbol (no `underlying`), prices in `f64`, carries `bars_held`,
/// and reuses this same record for partial fills, while the session book
/// averages in `Money` and never books partials. The field names differ
/// (`qty`/`entry_price`/`exit_reason` here vs
/// `quantity`/`entry_avg`/`reason` there) and the canonical JSON covers
/// this shape, so renaming either side would move pinned bytes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EquityTrade {
    /// Signal side that opened the trade.
    pub long: bool,
    /// Shares opened.
    pub qty: u32,
    /// Entry bar index.
    pub entry_bar: usize,
    /// Volume-weighted entry price.
    pub entry_price: f64,
    /// Exit bar index.
    pub exit_bar: usize,
    /// Volume-weighted exit price.
    pub exit_price: f64,
    /// Signed realized P&L after fees.
    pub pnl: f64,
    /// Bars held, entry to exit inclusive.
    pub bars_held: u64,
    /// Why the position closed.
    pub exit_reason: String,
}

/// An equity backtest run: per-bar verdicts, closed trades, the honest
/// report, and the reference status table once per bar. Reruns over identical
/// inputs are byte-identical (the canonical form covers decisions, trades,
/// and partials; the status rows project from the same state).
pub struct EquityRun {
    /// Reproducibility pin (data, code, config).
    pub manifest: crate::report::RunManifest,
    /// Per-bar verdicts in replay order.
    pub decisions: Vec<BarDecision>,
    /// Closed positions in close order (partial fills excluded).
    pub trades: Vec<EquityTrade>,
    /// Partial fills (TP zones, exhaustion slices) in fill order.
    pub partials: Vec<EquityTrade>,
    /// The reference status table's eight rows, once per bar.
    pub status: Vec<BacktestStatusView>,
    /// Resting stop/target points, in bar order. The canonical JSON covers
    /// decisions, trades, and partials only, so these project without moving
    /// a single pinned byte.
    pub levels: Vec<LevelPoint>,
    /// Metrics with stated assumptions and limits, plus trades analysis.
    pub report: BacktestReport,
}

impl EquityRun {
    /// Deterministic JSON over manifest, decisions, trades, and partials.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        let body = serde_json::json!({
            "decisions": self.decisions,
            "trades": self.trades,
            "partials": self.partials,
        });
        let decisions = serde_json::to_string(&body).unwrap_or_else(|e| {
            format!(
                "{{\"serialization_error\":{}}}",
                serde_json::to_string(&e.to_string())
                    .unwrap_or_else(|_| "\"unencodable\"".to_owned())
            )
        });
        format!("{}\n{decisions}", self.manifest.to_canonical_json())
    }
}

/// Fold closed slices and their take-profit partials into per-cycle trades.
/// A cycle is one entry through flat: the closing slice plus every partial
/// that shares its entry bar and side. Units and P&L both accumulate, so the
/// cycle's notional prices every share the entry opened.
pub(crate) fn cycles_of(trades: &[EquityTrade], partials: &[EquityTrade]) -> Vec<TradePnl> {
    trades
        .iter()
        .map(|trade| {
            let mut pnl = trade.pnl;
            let mut units = trade.qty;
            for partial in partials {
                if partial.entry_bar == trade.entry_bar && partial.long == trade.long {
                    pnl += partial.pnl;
                    units += partial.qty;
                }
            }
            TradePnl {
                long: trade.long,
                pnl,
                notional: trade.entry_price * f64::from(units),
                bars_held: trade.bars_held,
            }
        })
        .collect()
}

/// Everything one status row reads at one bar.
///
/// `pub(crate)` so the simple backtest shares the exact status projection
/// instead of re-deriving it; the old path is untouched.
pub(crate) struct StatusCtx<'a> {
    pub(crate) manager: &'a PositionManager,
    pub(crate) enabled: bool,
    pub(crate) index: usize,
    pub(crate) at: String,
    pub(crate) tally: &'a ClosedTally,
}

/// The reference status table's eight rows at one bar, read from the run
/// state the equity loop already carries. Closed-trade rows (win rate, net
/// P&L) fold the same cycles the report counts, so the table and the metrics
/// cannot disagree; the day rows mark at the curve value the bar closed at.
pub(crate) fn status_of(equity: f64, ctx: &StatusCtx<'_>) -> BacktestStatusView {
    let daily = ctx.manager.daily();
    let position = match ctx.manager.position() {
        Some(open) if open.long => format!("LONG {}", open.qty),
        Some(open) => format!("SHORT {}", open.qty),
        None => "Flat".to_owned(),
    };
    let day_pnl_pct = match daily.day_open_equity {
        Some(open) if open > 0.0 => format!("{:.2}%", (equity - open) / open * 100.0),
        _ => "0.00%".to_owned(),
    };
    let trades_today = if daily.max_trades > 0 {
        format!("{} / {}", daily.trades_today, daily.max_trades)
    } else {
        format!("{}", daily.trades_today)
    };
    let cooldown = match ctx.manager.cooldown().last_exit {
        Some((exit_bar, lost)) => {
            let gate = ctx.manager.cooldown();
            let wait = gate.bars + if lost { gate.loss_bars } else { 0 };
            let left = (wait - ctx.index.saturating_sub(exit_bar) as i64).max(0);
            if left > 0 {
                format!("{left} bars")
            } else {
                "Ready".to_owned()
            }
        }
        None => "Ready".to_owned(),
    };
    let status = if daily.halted {
        "HALTED: daily loss".to_owned()
    } else if !ctx.enabled {
        "Disabled".to_owned()
    } else {
        "Active".to_owned()
    };
    // Closed-cycle counts read the running tally (identical values to
    // refolding `cycles_of` over the done prefix every bar — see
    // `ClosedTally` for why the running values match).
    let closed = ctx.tally.closed;
    let wins = ctx.tally.wins;
    let win_rate = if closed > 0 {
        format!("{:.1}%", wins as f64 / closed as f64 * 100.0)
    } else {
        "-".to_owned()
    };
    let net: f64 = ctx.tally.net;
    BacktestStatusView {
        at: ctx.at.clone(),
        position,
        day_pnl_pct,
        trades_today,
        loss_streak: daily.consec_losses.max(0) as u64,
        cooldown,
        status,
        win_rate,
        net_pnl: Money::new(Decimal::from_f64_retain(net).unwrap_or(Decimal::ZERO)).to_string(),
    }
}

/// Push one equity-curve point with its status row and resting levels: the
/// three always move together, so no bar can carry a curve value without its
/// table row, and a resting line never invents a point for a flat bar.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_point(
    curve: &mut Vec<Money>,
    status: &mut Vec<BacktestStatusView>,
    levels: &mut Vec<LevelPoint>,
    equity: Money,
    index: usize,
    ctx: &StatusCtx<'_>,
) {
    let marked = equity.as_decimal().to_f64().unwrap_or(f64::NAN);
    status.push(status_of(marked, ctx));
    if let Some(position) = ctx.manager.position() {
        if position.stop.is_finite() {
            levels.push(LevelPoint {
                bar: index,
                price: position.stop,
                kind: LevelKind::Stop,
            });
        }
        for (zone, target) in position.targets.iter().enumerate() {
            if !position.target_fills[zone] {
                if let Some(price) = target.filter(|price| price.is_finite()) {
                    levels.push(LevelPoint {
                        bar: index,
                        price,
                        kind: LevelKind::Target,
                    });
                }
            }
        }
    }
    curve.push(equity);
}

pub(crate) fn money(value: f64) -> Money {
    // (M7.3c) `Money::from_f64_or_zero` — the one f64→Money spelling,
    // shared with the strategy position path.
    Money::from_f64_or_zero(value)
}

pub(crate) fn mark_equity(cash: f64, position: Option<&OpenPosition>, close: f64) -> f64 {
    // Cash already reflects booked fills; the open leg marks at the close.
    // Longs paid the notional at entry, shorts banked the proceeds, so the
    // mark adds the leg back for longs and subtracts the buyback for
    // shorts. Equity always equals cash plus unrealized P&L.
    cash + position.as_ref().map_or(0.0, |pos| {
        if pos.long {
            close * f64::from(pos.qty)
        } else {
            -(close * f64::from(pos.qty))
        }
    })
}

/// Running closed-cycle tally: the per-bar status row's win rate and net
/// P&L without refolding every closed trade on every bar.
///
/// Updated once per full close, in close order (the same order
/// [`cycles_of`] folds): a cycle's P&L is its closing slice plus every
/// partial sharing its entry bar and side. Later partials can never join
/// an earlier cycle — positions are sequential (single book), entries
/// precede their exits within the loop's exits-before-entries order, so a
/// later partial's entry bar always exceeds a closed cycle's. The final
/// report still folds [`cycles_of`] over the complete vectors; the tally
/// only serves the per-bar row, with identical values.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ClosedTally {
    /// Closed cycles so far.
    pub(crate) closed: u64,
    /// Cycles with positive P&L.
    pub(crate) wins: u64,
    /// Sum of cycle P&Ls, in close order.
    pub(crate) net: f64,
}

impl ClosedTally {
    pub(crate) fn note_close(&mut self, cycle_pnl: f64) {
        self.closed += 1;
        if cycle_pnl > 0.0 {
            self.wins += 1;
        }
        self.net += cycle_pnl;
    }
}

#[cfg(test)]
#[path = "../tests/equity.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/pine_parity.rs"]
mod pine_parity_tests;
