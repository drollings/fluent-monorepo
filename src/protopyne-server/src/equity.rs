//! Equity backtest runner: the single decision path over closed bars.
//!
//! Each bar runs the M6 signal, sizes through the equity policy, and routes
//! every order — entries, stops, targets, partials, trailing exits, session
//! flattens — through the shared submission body (`submit_decided`: risk
//! check, grant decision, `safe_submit` into the venue, journal). No raw
//! `Venue::submit` exists on this path; no second fill pricer (fills price
//! at the quoted touch through the installed venue, uncovered bars refuse
//! naming the feed). One open position at a time; the runner owns its state
//! (stop, TP zones, breakeven arm, tight mode, partial-once flag, best
//! price, cooldowns, daily tallies) and updates it through the strategy
//! crate's pure transitions, so the math stays testable without a venue.
//!
//! Stops and targets rest as explicit prices: a touch intrabar submits the
//! exit at that price when marketable, otherwise the position carries to
//! the next bar and retries. Trailing and breakeven only ever tighten the
//! resting stop, never loosen it.

use chrono::DateTime;
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
use protopyne_strategy::signals_lite::{
    market_legs, IntegrationLiteSignal, LiteIndicatorParams, LiteLegs,
};
use protopyne_strategy::{
    breakeven_price, clamp_stop, normalize_tp_weights, ratchet_stop, stop_distance_atr,
    stop_distance_pct, stop_distance_swing, trail_chandelier, trail_percent, trail_ribbon_edge,
    CooldownGate, DailyLimits, Direction, EquitySizer, Grant, GrantLedger, MarketData, MarketView,
    PartialOnce, PortfolioFacts, Proposal, ProposalSide, RiskEngine, SessionGate, Signal,
    SignalConfig, SignalInstance, SignalOutput, StructureKind,
};
use protopyne_types::{session::TradingSession, Bar, Money, Qty, Timeframe};
use protopyne_venue::{Journal, PaperConfig, Recorder};

use crate::backtest::{quote_for_bar, BacktestConfig, BacktestVenue};
use crate::error::ServerError;
use crate::report::{absent_tabs, build_report, BacktestReport, TradePnl, TradesAnalysis};
use crate::runner::{submit_decided, BarDecision, BarHealth, RunnerConfig};

/// One closed equity trade, in close order.
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

impl EquityTrade {
    fn as_pnl(&self, entry_notional: f64) -> TradePnl {
        TradePnl {
            long: self.long,
            pnl: self.pnl,
            notional: entry_notional,
            bars_held: self.bars_held,
        }
    }
}

/// One open equity position and its management state.
#[allow(clippy::struct_excessive_bools)]
struct OpenPosition {
    long: bool,
    qty: u32,
    entry_price: f64,
    entry_bar: usize,
    stop: f64,
    tp_prices: [Option<f64>; 3],
    tp_weights: [f64; 3],
    tp_filled: [bool; 3],
    be_armed: bool,
    tight: bool,
    partial: PartialOnce,
    partial_fired: bool,
    best: f64,
}

/// An equity backtest run: per-bar verdicts, closed trades, and the honest
/// report. Reruns over identical inputs are byte-identical.
pub struct EquityRun {
    /// Reproducibility pin (data, code, config).
    pub manifest: crate::report::RunManifest,
    /// Per-bar verdicts in replay order.
    pub decisions: Vec<BarDecision>,
    /// Closed positions in close order (partial fills excluded).
    pub trades: Vec<EquityTrade>,
    /// Partial fills (TP zones, exhaustion slices) in fill order.
    pub partials: Vec<EquityTrade>,
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

/// Run the equity decision stack over closed bars.
///
/// The signal proposes, the equity policy sizes, risk checks, the grant
/// allows or denies, and `safe_submit` journals, dry-runs, submits, and
/// queries by id — the same body the replay session steps, never a fork.
/// Management (stops, TP zones, breakeven, trailing, exhaustion, signal
/// exits, cooldowns, daily limits, session flatten) rides the runner-owned
/// state beside it.
#[allow(clippy::too_many_arguments)]
pub async fn run_equity_backtest(
    indicator: &LiteIndicatorParams,
    strategy: &IntegrationLiteStrategyConfig,
    risk: &RiskEngine,
    grant: &Grant,
    venue_config: &BacktestConfig,
    runner_config: &RunnerConfig,
    signal_name: &str,
    underlying: &str,
    timeframe: Timeframe,
    bars: &[Bar],
    seed: u64,
    initial_equity: f64,
) -> Result<EquityRun, crate::error::ServerError> {
    runner_config.validate()?;
    venue_config.validate()?;
    if bars.is_empty() {
        return Err(ServerError::Backtest(
            "history must not be empty".to_owned(),
        ));
    }
    let signal = IntegrationLiteSignal::new(indicator.clone(), strategy.clone())
        .map_err(|e| ServerError::Backtest(format!("signal refused its dials: {e}")))?;
    let session = TradingSession::parse(&indicator.session)
        .map_err(|e| ServerError::Backtest(format!("malformed session: {e}")))?;
    let paper = PaperConfig::new().build();
    let venue = BacktestVenue::new(venue_config.clone(), bars.to_vec(), paper)
        .map_err(|e| ServerError::Backtest(e.to_string()))?;
    let ledger = GrantLedger::new(grant.clone());
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let config_json = serde_json::json!({
        "signal": signal_name,
        "underlying": underlying,
        "indicator": indicator,
        "strategy": strategy,
        "initial_equity": initial_equity,
    });
    let manifest = crate::report::RunManifest::new(
        venue_config.data_version.clone(),
        serde_json::to_string(&config_json).unwrap_or_default(),
        venue.bars(),
        seed,
    );
    let mut instance = signal
        .bind(&SignalConfig::new(timeframe))
        .map_err(|e| ServerError::Backtest(format!("signal bind refused: {e}")))?;
    let tp_weights = normalize_tp_weights([strategy.tp1_pct, strategy.tp2_pct, strategy.tp3_pct]);
    let mut decisions: Vec<BarDecision> = Vec::with_capacity(bars.len());
    let mut trades: Vec<EquityTrade> = Vec::new();
    let mut partials: Vec<EquityTrade> = Vec::new();
    let mut cash = initial_equity;
    let mut fees_paid = 0.0;
    let mut position: Option<OpenPosition> = None;
    let mut cooldown = CooldownGate {
        bars: strategy.cd_bars,
        loss_bars: strategy.cd_loss_bars,
        last_exit: None,
    };
    let mut daily = DailyLimits {
        max_trades: strategy.max_trades_day,
        max_consec_losses: strategy.max_consec_losses,
        max_daily_loss_pct: strategy.max_daily_loss_pct,
        trades_today: 0,
        consec_losses: 0,
        day_open_equity: None,
        equity_now: None,
        halted: false,
    };
    let gate = SessionGate {
        flatten_min_before: strategy.flatten_min_before,
        noentry_min: strategy.noentry_min,
        close_min: i64::from(session.close_minute()),
    };
    let mut current_day: Option<(i32, u32, u32)> = None;
    let mut curve: Vec<protopyne_types::Money> = Vec::with_capacity(bars.len());
    let mut closes: Vec<f64> = Vec::with_capacity(bars.len());
    let warmup = instance.warmup();
    let mut proposals = 0_u64;
    let mut fills = 0_u64;
    let mut filled_notional = Decimal::ZERO;

    for (index, bar) in bars.iter().enumerate() {
        let Some(close) = bar.c.to_f64() else {
            decisions.push(crate::runner::skipped_decision(
                index,
                bar.ts.to_rfc3339(),
                format!("equity-{seed}-{index}"),
                "close not representable",
            ));
            curve.push(money(cash));
            continue;
        };
        closes.push(close);
        let ts_ms = bar.ts.timestamp_millis();
        let day = session.session_date(ts_ms);
        if current_day != Some(day) {
            current_day = Some(day);
            daily.day_open_equity = Some(mark_equity(cash, position.as_ref(), close));
            daily.trades_today = 0;
            daily.halted = false;
        }
        let equity_now = mark_equity(cash, position.as_ref(), close);
        daily.equity_now = Some(equity_now);
        let et_min = i64::from(TradingSession::wall_minutes(ts_ms));
        let proposal_id = |tag: &str| format!("equity-{seed}-{index}-{tag}");

        // Warmup honesty first: no decision before every leg is seeded.
        if closes.len() <= warmup.max(runner_config.warmup_bars) {
            decisions.push(crate::runner::skipped_decision(
                index,
                bar.ts.to_rfc3339(),
                proposal_id("warmup"),
                "warmup",
            ));
            curve.push(money(mark_equity(cash, position.as_ref(), close)));
            continue;
        }

        let legs = market_legs(indicator, &bars[..=index])
            .map_err(|e| ServerError::Backtest(format!("leg batch refused: {e}")))?;
        let atr = legs.atr[index].unwrap_or(f64::NAN);

        // --- Manage the open position before considering entries.
        if position.is_some() {
            {
                let pos = position.as_mut().expect("open");
                pos.best = if pos.long {
                    pos.best.max(bar.h.to_f64().unwrap_or(pos.best))
                } else {
                    pos.best.min(bar.l.to_f64().unwrap_or(pos.best))
                };
            }
            // Session flatten first: the close prices out everything.
            if gate.must_flatten(et_min) {
                let qty = position.as_ref().expect("open").qty;
                close_position(
                    &mut position,
                    &mut trades,
                    &mut cash,
                    &mut fees_paid,
                    &mut cooldown,
                    &mut daily,
                    &mut decisions,
                    &mut fills,
                    &mut filled_notional,
                    &venue,
                    &ledger,
                    &recorder,
                    risk,
                    runner_config,
                    signal_name,
                    underlying,
                    bar,
                    index,
                    &proposal_id("session"),
                    "session-flatten",
                    close,
                    qty,
                )
                .await;
                curve.push(money(mark_equity(cash, position.as_ref(), close)));
                continue;
            }
            // Daily-loss flatten.
            if daily.must_flatten() {
                let qty = position.as_ref().expect("open").qty;
                close_position(
                    &mut position,
                    &mut trades,
                    &mut cash,
                    &mut fees_paid,
                    &mut cooldown,
                    &mut daily,
                    &mut decisions,
                    &mut fills,
                    &mut filled_notional,
                    &venue,
                    &ledger,
                    &recorder,
                    risk,
                    runner_config,
                    signal_name,
                    underlying,
                    bar,
                    index,
                    &proposal_id("dayloss"),
                    "daily-loss",
                    close,
                    qty,
                )
                .await;
                daily.halted = true;
                curve.push(money(mark_equity(cash, position.as_ref(), close)));
                continue;
            }
            let exit = manage_position(
                strategy,
                indicator,
                &legs,
                bars,
                index,
                position.as_mut().expect("open"),
                &mut daily,
                &mut instance,
                &bars[..=index],
                &closes,
            );
            if let Some(action) = exit {
                apply_exit(
                    action,
                    &mut position,
                    &mut trades,
                    &mut partials,
                    &mut cash,
                    &mut fees_paid,
                    &mut cooldown,
                    &mut daily,
                    &mut decisions,
                    &mut fills,
                    &mut filled_notional,
                    &venue,
                    &ledger,
                    &recorder,
                    risk,
                    runner_config,
                    signal_name,
                    underlying,
                    bar,
                    index,
                    seed,
                    atr,
                )
                .await;
            }
        }

        // --- Entries only when flat.
        if position.is_none() {
            let in_window = gate.may_enter(et_min);
            let may_cool = cooldown.may_enter(index);
            let may_daily = daily.may_enter();
            if in_window && may_cool && may_daily {
                let data = MarketData {
                    bars: &bars[..=index],
                    chain: None,
                    dte_days: runner_config.dte_days,
                    timeframe,
                };
                let view = MarketView {
                    data: &data,
                    closes: &closes,
                    as_of: bar.ts,
                };
                if let Ok(out) = instance.evaluate(&view) {
                    if grant_covers(grant, &out) {
                        let Some(quote) = quote_for_bar(bar, venue_config) else {
                            decisions.push(crate::runner::skipped_decision(
                                index,
                                bar.ts.to_rfc3339(),
                                proposal_id("entry"),
                                "unquotable bar",
                            ));
                            curve.push(money(mark_equity(cash, position.as_ref(), close)));
                            continue;
                        };
                        let equity_now = cash;
                        let stop_distance = entry_stop_distance(strategy, atr, close);
                        let sizer = EquitySizer::new(
                            &strategy.size_mode,
                            strategy.risk_pct,
                            strategy.size_pct,
                            strategy.fixed_qty,
                            strategy.max_notional_pct,
                        )
                        .map_err(|e| ServerError::Backtest(format!("sizer refused: {e}")))?;
                        let facts = PortfolioFacts::unobserved();
                        let qty = sizer
                            .for_trade(equity_now, close, stop_distance)
                            .shares(&facts)
                            .min(u64::from(u32::MAX)) as u32;
                        if qty > 0 {
                            let touch = if out.direction == Direction::Long {
                                quote.ask
                            } else {
                                quote.bid
                            };
                            let proposal = entry_proposal(
                                &proposal_id("entry"),
                                signal_name,
                                underlying,
                                &out,
                                qty,
                                touch,
                                stop_distance,
                                close,
                                bar.ts,
                            );
                            proposals += 1;
                            let decision = submit_decided(
                                proposal,
                                quote,
                                risk,
                                &ledger,
                                &venue,
                                &recorder,
                                BarHealth::rehearsed(),
                                None,
                                index,
                                bar,
                                runner_config,
                            )
                            .await;
                            let filled = decision.submitted;
                            fills += u64::from(filled);
                            let (Some(price), qty) =
                                (decision.fill_price, decision.filled_quantity)
                            else {
                                decisions.push(decision);
                                curve.push(money(mark_equity(cash, position.as_ref(), close)));
                                continue;
                            };
                            {
                                if filled && qty > 0 {
                                    let entry = price.as_decimal().to_f64().unwrap_or(close);
                                    filled_notional += price.as_decimal() * Decimal::from(qty);
                                    fees_paid += decision
                                        .fee
                                        .map_or(0.0, |f| f.as_decimal().to_f64().unwrap_or(0.0));
                                    // Longs pay the notional; shorts receive the
                                    // proceeds (full-notional margin model is
                                    // not used — equity marks the open leg).
                                    if out.direction == Direction::Long {
                                        cash -= entry * f64::from(qty);
                                    } else {
                                        cash += entry * f64::from(qty);
                                    }
                                    position = Some(open_position(
                                        strategy, &out, qty, entry, index, atr, &legs, tp_weights,
                                    ));
                                    daily.trades_today += 1;
                                }
                            }
                            decisions.push(decision);
                            curve.push(money(mark_equity(cash, position.as_ref(), close)));
                            continue;
                        }
                    }
                    decisions.push(crate::runner::skipped_decision(
                        index,
                        bar.ts.to_rfc3339(),
                        proposal_id("entry"),
                        "signal fired but sizing or grant left no quantity",
                    ));
                    curve.push(money(mark_equity(cash, position.as_ref(), close)));
                    continue;
                }
            }
            decisions.push(crate::runner::skipped_decision(
                index,
                bar.ts.to_rfc3339(),
                proposal_id("quiet"),
                "no entry: gate, cooldown, daily limit, or no trigger",
            ));
            curve.push(money(mark_equity(cash, position.as_ref(), close)));
        } else {
            curve.push(money(mark_equity(cash, position.as_ref(), close)));
        }
    }

    let mut report = build_report(
        manifest.clone(),
        vec![
            "equity fills price at the quoted touch through the installed venue".to_owned(),
            "stops and targets rest as explicit prices; trailing and breakeven only tighten"
                .to_owned(),
            "slippage is a fill-model assumption, never a gate".to_owned(),
        ],
        vec![
            format!("equity backtest over {} bars", bars.len()),
            crate::report::ABSENT_CAPABILITIES.to_owned(),
        ],
        proposals,
        fills,
        Money::new(Decimal::from_f64_retain(fees_paid).unwrap_or(Decimal::ZERO)),
        Money::new(filled_notional),
        Money::zero(),
        curve,
        crate::runner::report_rsrs(bars),
        None,
        protopyne_types::MarkBasis::FillPrice,
    );
    let pnls: Vec<TradePnl> = trades
        .iter()
        .map(|t| t.as_pnl(t.entry_price * f64::from(t.qty)))
        .collect();
    report.trades = Some(TradesAnalysis::of(&pnls));
    report.unavailable_tabs = absent_tabs();
    Ok(EquityRun {
        manifest,
        decisions,
        trades,
        partials,
        report,
    })
}

fn money(value: f64) -> Money {
    Money::new(Decimal::from_f64_retain(value).unwrap_or(Decimal::ZERO))
}

fn mark_equity(cash: f64, position: Option<&OpenPosition>, close: f64) -> f64 {
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

/// Grant merit gate for the equity path: the conviction floor must pass.
/// Reasons ride the skip line when it does not.
fn grant_covers(grant: &Grant, out: &SignalOutput) -> bool {
    out.conviction >= grant.signal.min_conviction
        && out.activity_score >= grant.signal.min_activity_score
}

/// Entry stop distance from the configured construction, clamped into the
/// ATR band.
fn entry_stop_distance(strategy: &IntegrationLiteStrategyConfig, atr: f64, close: f64) -> f64 {
    if !atr.is_finite() || atr <= 0.0 {
        return f64::NAN;
    }
    let raw = match strategy.stop_type.as_str() {
        "Percent" => stop_distance_pct(close, strategy.stop_pct).unwrap_or(f64::NAN),
        "Swing" => {
            stop_distance_swing(true, close, close, strategy.swing_buf_atr, atr).unwrap_or(f64::NAN)
        }
        _ => stop_distance_atr(atr, strategy.stop_atr_mult).unwrap_or(f64::NAN),
    };
    if !raw.is_finite() {
        return f64::NAN;
    }
    clamp_stop(raw, atr, strategy.min_stop_atr, strategy.max_stop_atr)
}

fn entry_proposal(
    id: &str,
    strategy_name: &str,
    underlying: &str,
    out: &SignalOutput,
    qty: u32,
    touch: Money,
    stop_distance: f64,
    close: f64,
    at: DateTime<chrono::Utc>,
) -> Proposal {
    Proposal::new()
        .id(id.to_owned())
        .strategy(strategy_name.to_owned())
        .underlying(underlying.to_owned())
        .direction(out.direction)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Single)
        .quantity(Qty::new(Decimal::from(qty)))
        .limit_price(touch)
        .max_loss(money(stop_distance * f64::from(qty)))
        .notional(money(close * f64::from(qty)))
        .dte_days(0)
        .conviction(out.conviction)
        .activity_score(out.activity_score)
        .created_at(at)
        .build()
}

#[allow(clippy::too_many_arguments)]
fn open_position(
    strategy: &IntegrationLiteStrategyConfig,
    out: &SignalOutput,
    qty: u32,
    entry: f64,
    entry_bar: usize,
    atr: f64,
    legs: &LiteLegs,
    tp_weights: [f64; 3],
) -> OpenPosition {
    let long = out.direction == Direction::Long;
    let stop_distance = entry_stop_distance(strategy, atr, entry);
    let stop = if long {
        entry - stop_distance
    } else {
        entry + stop_distance
    };
    let mults = [strategy.tp1_mult, strategy.tp2_mult, strategy.tp3_mult];
    let mut tp_prices = [None; 3];
    for (i, mult) in mults.iter().enumerate() {
        let dist = if strategy.tp_basis == "ATR" {
            atr * mult
        } else {
            stop_distance * mult
        };
        if dist.is_finite() && dist > 0.0 && tp_weights[i] > 0.0 {
            tp_prices[i] = Some(if long { entry + dist } else { entry - dist });
        }
    }
    let _ = legs;
    OpenPosition {
        long,
        qty,
        entry_price: entry,
        entry_bar,
        stop,
        tp_prices,
        tp_weights,
        tp_filled: [false; 3],
        be_armed: false,
        tight: false,
        partial: PartialOnce::default(),
        partial_fired: false,
        best: entry,
    }
}

/// One management verdict for the open position on this bar.
enum ExitAction {
    Close {
        price: f64,
        reason: &'static str,
    },
    Partial {
        price: f64,
        qty: u32,
        reason: &'static str,
    },
}

/// Inspect the open position: stops, TP zones, exhaustion response, signal
/// exits, and time stop, in that order. Trailing and breakeven only adjust
/// the resting stop (ratchet-only) and return no action.
#[allow(clippy::too_many_arguments)]
fn manage_position(
    strategy: &IntegrationLiteStrategyConfig,
    indicator: &LiteIndicatorParams,
    legs: &LiteLegs,
    bars: &[Bar],
    index: usize,
    pos: &mut OpenPosition,
    daily: &mut DailyLimits,
    instance: &mut Box<dyn SignalInstance>,
    window: &[Bar],
    closes: &[f64],
) -> Option<ExitAction> {
    let bar = &bars[index];
    let high = bar.h.to_f64().unwrap_or(f64::NAN);
    let low = bar.l.to_f64().unwrap_or(f64::NAN);
    let atr = legs.atr[index].unwrap_or(f64::NAN);
    let fast = legs.fast[index];
    let prev_fast = if index > 0 {
        legs.fast[index - 1]
    } else {
        None
    };
    let crossed_back_against = match (prev_fast, fast) {
        (Some(p), Some(c)) if pos.long => {
            p >= strategy.sig_exhaust_level && c < strategy.sig_exhaust_level
        }
        (Some(p), Some(c)) if !pos.long => {
            p <= -strategy.sig_exhaust_level && c > -strategy.sig_exhaust_level
        }
        _ => false,
    };
    // Exhaustion response.
    if crossed_back_against || dual_against(strategy, legs, index, pos.long) {
        let in_profit = if pos.long {
            close_at(bars, index) > pos.entry_price
        } else {
            close_at(bars, index) < pos.entry_price
        };
        match strategy.exit_action.as_str() {
            "Off" => {}
            "Close All" => {
                return Some(ExitAction::Close {
                    price: close_at(bars, index),
                    reason: "exhaustion-close",
                });
            }
            action @ ("Partial Close" | "Tighten + Partial") => {
                let _ = action;
                if pos
                    .partial
                    .may_fire(true, in_profit, strategy.exit_only_profit)
                {
                    let qty =
                        (f64::from(pos.qty) * strategy.exit_partial_pct / 100.0).floor() as u32;
                    if qty > 0 && qty < pos.qty {
                        pos.partial.latch();
                        pos.partial_fired = true;
                        pos.tight = true;
                        return Some(ExitAction::Partial {
                            price: close_at(bars, index),
                            qty,
                            reason: "exhaustion-partial",
                        });
                    }
                }
                if strategy.exit_action == "Tighten + Partial" {
                    pos.tight = true;
                }
            }
            _ => {
                pos.tight = true;
            }
        }
        if strategy.trail_on_exhaustion {
            pos.tight = true;
        }
    }
    // Stop first: a touched stop exits the full remainder at the stop.
    if pos.long && low <= pos.stop {
        return Some(ExitAction::Close {
            price: pos.stop,
            reason: "stop",
        });
    }
    if !pos.long && high >= pos.stop {
        return Some(ExitAction::Close {
            price: pos.stop,
            reason: "stop",
        });
    }
    // TP zones in order: the first touched unfilled zone partials at its
    // price. Whatever is left runs on the (possibly trailing) stop.
    for zone in 0..3 {
        if pos.tp_filled[zone] {
            continue;
        }
        if let Some(tp) = pos.tp_prices[zone] {
            let touched = if pos.long { high >= tp } else { low <= tp };
            if touched {
                let qty = (f64::from(pos.qty) * pos.tp_weights[zone] / 100.0).floor() as u32;
                if qty > 0 && qty < pos.qty {
                    pos.tp_filled[zone] = true;
                    // Move the stop to breakeven immediately: this arm
                    // returns below, which would otherwise skip the arm
                    // until a later bar.
                    if strategy.be_on_tp && atr.is_finite() && atr > 0.0 {
                        pos.be_armed = true;
                        let be =
                            breakeven_price(pos.long, pos.entry_price, strategy.be_offset_atr, atr);
                        pos.stop = ratchet_stop(pos.long, pos.stop, be);
                    }
                    return Some(ExitAction::Partial {
                        price: tp,
                        qty,
                        reason: "take-profit",
                    });
                }
                if qty >= pos.qty && pos.qty > 0 {
                    pos.tp_filled[zone] = true;
                    return Some(ExitAction::Close {
                        price: tp,
                        reason: "take-profit",
                    });
                }
            }
        }
    }
    // Signal exits: opposite trigger, ribbon flip against, fast zero-cross
    // against, and the time stop.
    if strategy.exit_opposite || strategy.exit_ribbon || strategy.exit_zero_cross {
        let data = MarketData {
            bars: window,
            chain: None,
            dte_days: 0,
            timeframe: Timeframe::M1,
        };
        let view = MarketView {
            data: &data,
            closes,
            as_of: bar.ts,
        };
        if let Ok(out) = instance.evaluate(&view) {
            let opposite = out.direction
                != if pos.long {
                    Direction::Long
                } else {
                    Direction::Short
                };
            if strategy.exit_opposite && opposite {
                return Some(ExitAction::Close {
                    price: close_at(bars, index),
                    reason: "opposite-signal",
                });
            }
            if strategy.exit_ribbon && ribbon_against(legs, index, pos.long) {
                return Some(ExitAction::Close {
                    price: close_at(bars, index),
                    reason: "ribbon-flip",
                });
            }
            if strategy.exit_zero_cross && zero_against(legs, index, pos.long) {
                return Some(ExitAction::Close {
                    price: close_at(bars, index),
                    reason: "zero-cross",
                });
            }
        }
    }
    if strategy.exit_time_bars > 0
        && index.saturating_sub(pos.entry_bar) as i64 >= strategy.exit_time_bars
    {
        return Some(ExitAction::Close {
            price: close_at(bars, index),
            reason: "time-stop",
        });
    }
    // Breakeven arm: once armed, the stop rests past entry and only ever
    // tightens from there.
    if strategy.be_enable && atr.is_finite() && atr > 0.0 {
        let profit_trigger = strategy.be_trigger_atr <= 0.0
            || (if pos.long {
                close_at(bars, index) - pos.entry_price
            } else {
                pos.entry_price - close_at(bars, index)
            } >= strategy.be_trigger_atr * atr);
        if profit_trigger || pos.be_armed {
            pos.be_armed = true;
            let be = breakeven_price(pos.long, pos.entry_price, strategy.be_offset_atr, atr);
            pos.stop = ratchet_stop(pos.long, pos.stop, be);
        }
    }
    // Trailing activation: immediate, on-TP, on-exhaustion, or on-profit.
    // The candidate only ever tightens the resting stop.
    let profit_atr = if atr.is_finite() && atr > 0.0 {
        (if pos.long {
            close_at(bars, index) - pos.entry_price
        } else {
            pos.entry_price - close_at(bars, index)
        }) / atr
    } else {
        0.0
    };
    let active = strategy.trail_immediate
        || (strategy.trail_on_tp && pos.tp_filled.iter().any(|f| *f))
        || (strategy.trail_on_exhaustion && pos.tight)
        || (strategy.trail_on_profit && profit_atr >= strategy.trail_profit_atr);
    if active && atr.is_finite() && atr > 0.0 {
        let mult = if pos.tight {
            strategy.trail_tight_mult
        } else {
            strategy.trail_mult
        };
        let candidate = match strategy.trail_type.as_str() {
            "Percent" => {
                let pct = if pos.tight {
                    strategy.trail_pct / 2.0
                } else {
                    strategy.trail_pct
                };
                trail_percent(pos.long, pos.best, pct)
            }
            "Ribbon Edge" => {
                let edge = ribbon_edge(legs, index, pos.long);
                trail_ribbon_edge(pos.long, edge, strategy.trail_ribbon_buf, atr)
            }
            _ => trail_chandelier(pos.long, pos.best, atr, mult),
        };
        pos.stop = ratchet_stop(pos.long, pos.stop, candidate);
    }
    let _ = (daily, indicator);
    None
}

fn close_at(bars: &[Bar], index: usize) -> f64 {
    bars[index].c.to_f64().unwrap_or(f64::NAN)
}

/// Dual-signal exhaustion mirror: the opposite dual trigger also fires
/// exhaustion exits when armed.
fn dual_against(
    strategy: &IntegrationLiteStrategyConfig,
    legs: &LiteLegs,
    i: usize,
    long: bool,
) -> bool {
    if !strategy.sig_dual || !strategy.sig_dual_exit {
        return false;
    }
    let lookback = strategy.sig_dual_lookback.max(0) as usize;
    let slow_cross_down = (0..=1).any(|_| false);
    let _ = slow_cross_down;
    let down = crossed_down_at(&legs.slow, strategy.sig_dual_slow, i);
    let up = crossed_up_at(&legs.slow, -strategy.sig_dual_slow, i);
    let fast_beyond = |level: f64, below: bool| {
        (0..=lookback.min(i)).any(|back| {
            legs.fast[i - back].is_some_and(|v| if below { v < level } else { v > level })
        })
    };
    if long {
        down && fast_beyond(strategy.sig_dual_fast, false)
    } else {
        up && fast_beyond(-strategy.sig_dual_fast, true)
    }
}

fn crossed_up_at(series: &[Option<f64>], level: f64, i: usize) -> bool {
    if i < 1 {
        return false;
    }
    matches!(
        (series.get(i - 1), series.get(i)),
        (Some(Some(p)), Some(Some(c))) if *p <= level && *c > level
    )
}

fn crossed_down_at(series: &[Option<f64>], level: f64, i: usize) -> bool {
    if i < 1 {
        return false;
    }
    matches!(
        (series.get(i - 1), series.get(i)),
        (Some(Some(p)), Some(Some(c))) if *p >= level && *c < level
    )
}

fn ribbon_against(legs: &LiteLegs, i: usize, long: bool) -> bool {
    if i < 1 {
        return false;
    }
    match (
        legs.adaptive.get(i - 1),
        legs.adaptive.get(i),
        legs.signal.get(i - 1),
        legs.signal.get(i),
    ) {
        (Some(Some(pa)), Some(Some(ca)), Some(Some(ps)), Some(Some(cs))) => {
            if long {
                pa >= ps && ca < cs
            } else {
                pa <= ps && ca > cs
            }
        }
        _ => false,
    }
}

fn zero_against(legs: &LiteLegs, i: usize, long: bool) -> bool {
    if long {
        crossed_down_at(&legs.fast, 0.0, i)
    } else {
        crossed_up_at(&legs.fast, 0.0, i)
    }
}

fn ribbon_edge(legs: &LiteLegs, i: usize, long: bool) -> f64 {
    let a = legs.adaptive.get(i).and_then(|v| *v).unwrap_or(f64::NAN);
    let g = legs.signal.get(i).and_then(|v| *v).unwrap_or(f64::NAN);
    if long {
        a.min(g)
    } else {
        a.max(g)
    }
}

/// Submit one exit (full or partial) through the shared submission body
/// and book the fill into runner state.
#[allow(clippy::too_many_arguments)]
async fn apply_exit(
    action: ExitAction,
    position: &mut Option<OpenPosition>,
    trades: &mut Vec<EquityTrade>,
    partials: &mut Vec<EquityTrade>,
    cash: &mut f64,
    fees_paid: &mut f64,
    cooldown: &mut CooldownGate,
    daily: &mut DailyLimits,
    decisions: &mut Vec<BarDecision>,
    fills: &mut u64,
    filled_notional: &mut Decimal,
    venue: &BacktestVenue,
    ledger: &GrantLedger,
    recorder: &Recorder<'_>,
    risk: &RiskEngine,
    runner_config: &RunnerConfig,
    signal_name: &str,
    underlying: &str,
    bar: &Bar,
    index: usize,
    seed: u64,
    atr: f64,
) {
    let Some(pos) = position.as_mut() else {
        return;
    };
    let (qty, price, reason) = match action {
        ExitAction::Close { price, reason } => (pos.qty, price, reason),
        ExitAction::Partial { price, qty, reason } => (qty.min(pos.qty), price, reason),
    };
    if qty == 0 || !price.is_finite() {
        return;
    }
    let Some(quote) = quote_for_bar(bar, &venue_config_of(venue)) else {
        return;
    };
    let touch = if pos.long { quote.bid } else { quote.ask };
    // Resting stop/target prices submit as limits; marketable flow still
    // clears through the venue's own liquidity filter.
    let _ = atr;
    let proposal = Proposal::new()
        .id(format!("equity-{seed}-{index}-x-{reason}"))
        .strategy(signal_name.to_owned())
        .underlying(underlying.to_owned())
        .direction(if pos.long {
            Direction::Long
        } else {
            Direction::Short
        })
        .side(ProposalSide::Exit)
        .structure(StructureKind::Single)
        .quantity(Qty::new(Decimal::from(qty)))
        .limit_price(limit_price_for_exit(pos.long, price, touch))
        .max_loss(money(0.0))
        .notional(money(price * f64::from(qty)))
        .dte_days(0)
        .conviction(1.0)
        .activity_score(1.0)
        .created_at(bar.ts)
        .build();
    let decision = submit_decided(
        proposal,
        quote,
        risk,
        ledger,
        venue,
        recorder,
        BarHealth::rehearsed(),
        None,
        index,
        bar,
        runner_config,
    )
    .await;
    if decision.submitted {
        *fills += 1;
    }
    let fill_price = decision
        .fill_price
        .map_or(price, |m| m.as_decimal().to_f64().unwrap_or(price));
    let filled_qty = if decision.submitted {
        decision.filled_quantity.min(pos.qty)
    } else {
        // Unfilled exits carry to the next bar at the resting price: the
        // position stays open, so nothing books here.
        decisions.push(decision);
        return;
    };
    if filled_qty == 0 {
        decisions.push(decision);
        return;
    }
    let fee = decision
        .fee
        .map_or(0.0, |f| f.as_decimal().to_f64().unwrap_or(0.0));
    *fees_paid += fee;
    *filled_notional +=
        Decimal::from_f64_retain(fill_price * f64::from(filled_qty)).unwrap_or(Decimal::ZERO);
    let proceeds = fill_price * f64::from(filled_qty);
    if pos.long {
        *cash += proceeds - fee;
    } else {
        *cash -= proceeds + fee;
    }
    let closing_all = filled_qty >= pos.qty;
    let pnl = if pos.long {
        (fill_price - pos.entry_price) * f64::from(filled_qty) - fee
    } else {
        (pos.entry_price - fill_price) * f64::from(filled_qty) - fee
    };
    let trade = EquityTrade {
        long: pos.long,
        qty: filled_qty,
        entry_price: pos.entry_price,
        entry_bar: pos.entry_bar,
        exit_price: fill_price,
        exit_bar: index,
        pnl,
        bars_held: (index.saturating_sub(pos.entry_bar) + 1) as u64,
        exit_reason: reason.to_owned(),
    };
    // Partial exits do not close the book: shrink the position, keep the
    // resting stop, and record the slice as its own trade.
    if closing_all {
        let lost = pnl < 0.0;
        cooldown.last_exit = Some((index, lost));
        if lost {
            daily.consec_losses += 1;
        } else {
            daily.consec_losses = 0;
        }
        *position = None;
        trades.push(trade);
    } else {
        pos.qty -= filled_qty;
        partials.push(trade);
    }
    decisions.push(decision);
}

/// Submit a full close through the shared body (session/day-loss flatten).
#[allow(clippy::too_many_arguments)]
async fn close_position(
    position: &mut Option<OpenPosition>,
    trades: &mut Vec<EquityTrade>,
    cash: &mut f64,
    fees_paid: &mut f64,
    cooldown: &mut CooldownGate,
    daily: &mut DailyLimits,
    decisions: &mut Vec<BarDecision>,
    fills: &mut u64,
    filled_notional: &mut Decimal,
    venue: &BacktestVenue,
    ledger: &GrantLedger,
    recorder: &Recorder<'_>,
    risk: &RiskEngine,
    runner_config: &RunnerConfig,
    signal_name: &str,
    underlying: &str,
    bar: &Bar,
    index: usize,
    proposal_id: &str,
    reason: &str,
    price: f64,
    qty: u32,
) {
    if qty == 0 {
        return;
    }
    let Some(pos) = position.as_ref() else {
        return;
    };
    let long = pos.long;
    let Some(quote) = quote_for_bar(bar, &venue_config_of(venue)) else {
        return;
    };
    let proposal = Proposal::new()
        .id(proposal_id.to_owned())
        .strategy(signal_name.to_owned())
        .underlying(underlying.to_owned())
        .direction(if long {
            Direction::Long
        } else {
            Direction::Short
        })
        .side(ProposalSide::Exit)
        .structure(StructureKind::Single)
        .quantity(Qty::new(Decimal::from(qty)))
        .limit_price(limit_price_for_exit(
            long,
            price,
            if long { quote.bid } else { quote.ask },
        ))
        .max_loss(money(0.0))
        .notional(money(price * f64::from(qty)))
        .dte_days(0)
        .conviction(1.0)
        .activity_score(1.0)
        .created_at(bar.ts)
        .build();
    let decision = submit_decided(
        proposal,
        quote,
        risk,
        ledger,
        venue,
        recorder,
        BarHealth::rehearsed(),
        None,
        index,
        bar,
        runner_config,
    )
    .await;
    if decision.submitted {
        *fills += 1;
    }
    let fill_price = decision
        .fill_price
        .map_or(price, |m| m.as_decimal().to_f64().unwrap_or(price));
    let filled_qty = if decision.submitted {
        decision.filled_quantity.min(qty)
    } else {
        decisions.push(decision);
        return;
    };
    if filled_qty == 0 {
        decisions.push(decision);
        return;
    }
    let fee = decision
        .fee
        .map_or(0.0, |f| f.as_decimal().to_f64().unwrap_or(0.0));
    *fees_paid += fee;
    *filled_notional +=
        Decimal::from_f64_retain(fill_price * f64::from(filled_qty)).unwrap_or(Decimal::ZERO);
    let pos = position.as_ref().expect("open");
    let proceeds = fill_price * f64::from(filled_qty);
    if pos.long {
        *cash += proceeds - fee;
    } else {
        *cash -= proceeds + fee;
    }
    let pnl = if pos.long {
        (fill_price - pos.entry_price) * f64::from(filled_qty) - fee
    } else {
        (pos.entry_price - fill_price) * f64::from(filled_qty) - fee
    };
    let lost = pnl < 0.0;
    let trade = EquityTrade {
        long: pos.long,
        qty: filled_qty,
        entry_price: pos.entry_price,
        entry_bar: pos.entry_bar,
        exit_price: fill_price,
        exit_bar: index,
        pnl,
        bars_held: (index.saturating_sub(pos.entry_bar) + 1) as u64,
        exit_reason: reason.to_owned(),
    };
    if filled_qty >= pos.qty {
        cooldown.last_exit = Some((index, lost));
        if lost {
            daily.consec_losses += 1;
        } else {
            daily.consec_losses = 0;
        }
        *position = None;
    } else if let Some(open) = position.as_mut() {
        open.qty -= filled_qty;
    }
    trades.push(trade);
    decisions.push(decision);
}

/// Exit limit that can actually rest: the stop/target when it is marketable
/// at the touch, otherwise the touch itself so flow still clears through
/// the venue's own filter.
fn limit_price_for_exit(long: bool, price: f64, touch: Money) -> Money {
    let touch_px = touch.as_decimal().to_f64().unwrap_or(price);
    let marketable = if long {
        price <= touch_px
    } else {
        price >= touch_px
    };
    money(if marketable { price } else { touch_px })
}

fn venue_config_of(venue: &BacktestVenue) -> BacktestConfig {
    venue.config().clone()
}

#[cfg(test)]
#[path = "../tests/equity.rs"]
mod tests;
