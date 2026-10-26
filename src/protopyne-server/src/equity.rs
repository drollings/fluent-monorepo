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

use std::sync::Arc;

use chrono::DateTime;
use fluent_wvr::validate::Validate;
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use protopyne_kernels::lite::lite_legs;
use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
use protopyne_strategy::position::{
    entry_stop_distance, OpenPosition, PositionManager, PositionSpec,
};
use protopyne_strategy::signals_lite::{IntegrationLiteSignal, LiteIndicatorParams};
use protopyne_strategy::slots::exit::{ExitIntent, ExitQuantity, RestingPrice};
use protopyne_strategy::{
    Direction, EquitySizer, Grant, GrantLedger, MarketData, MarketView, PortfolioFacts, Proposal,
    ProposalSide, RiskEngine, SignalConfig, SignalOutput, Sizing as _, StructureKind,
};
use protopyne_types::{session::TradingSession, Bar, Money, Qty, Timeframe};
use protopyne_venue::{Journal, PaperConfig, Recorder};
use protopyne_view::BacktestStatusView;

use crate::backtest::{quote_for_bar, BacktestConfig, BacktestVenue};
use crate::error::ServerError;
use crate::marks::MarkPoint;
use crate::report::{absent_tabs, build_report, BacktestReport, TradePnl, TradesAnalysis};
use crate::runner::{submit_decided, BarDecision, BarHealth, RunnerConfig};

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

/// One equity backtest run, named: every policy the loop reads, stated once.
/// The builder replaces the positional twelve-argument call, so a new policy
/// joins as a named field instead of a thirteenth position.
pub struct BacktestRequest<'a> {
    indicator: &'a LiteIndicatorParams,
    strategy: &'a IntegrationLiteStrategyConfig,
    risk: &'a RiskEngine,
    grant: &'a Grant,
    venue_config: &'a BacktestConfig,
    runner_config: &'a RunnerConfig,
    signal_name: &'a str,
    underlying: &'a str,
    timeframe: Timeframe,
    bars: &'a [Bar],
    seed: u64,
    initial_equity: f64,
}

/// The request builder: one setter per policy, `run` when complete. A
/// missing policy refuses naming itself — no positional default fills in.
#[derive(Default)]
pub struct BacktestRequestBuilder<'a> {
    indicator: Option<&'a LiteIndicatorParams>,
    strategy: Option<&'a IntegrationLiteStrategyConfig>,
    risk: Option<&'a RiskEngine>,
    grant: Option<&'a Grant>,
    venue_config: Option<&'a BacktestConfig>,
    runner_config: Option<&'a RunnerConfig>,
    signal_name: Option<&'a str>,
    underlying: Option<&'a str>,
    timeframe: Option<Timeframe>,
    bars: Option<&'a [Bar]>,
    seed: Option<u64>,
    initial_equity: Option<f64>,
}

macro_rules! backtest_setter {
    ($name:ident, $field:ident, $ty:ty) => {
        #[doc = concat!("Set the request's `", stringify!($field), "` policy.")]
        #[must_use]
        pub fn $name(mut self, value: $ty) -> Self {
            self.$field = Some(value);
            self
        }
    };
}

impl<'a> BacktestRequestBuilder<'a> {
    backtest_setter!(indicator, indicator, &'a LiteIndicatorParams);
    backtest_setter!(strategy, strategy, &'a IntegrationLiteStrategyConfig);
    backtest_setter!(risk, risk, &'a RiskEngine);
    backtest_setter!(grant, grant, &'a Grant);
    backtest_setter!(venue_config, venue_config, &'a BacktestConfig);
    backtest_setter!(runner_config, runner_config, &'a RunnerConfig);
    backtest_setter!(signal_name, signal_name, &'a str);
    backtest_setter!(underlying, underlying, &'a str);
    backtest_setter!(timeframe, timeframe, Timeframe);
    backtest_setter!(bars, bars, &'a [Bar]);
    backtest_setter!(seed, seed, u64);
    backtest_setter!(initial_equity, initial_equity, f64);

    /// Build the named request. A missing policy refuses naming itself —
    /// no positional default fills in.
    pub fn build(self) -> Result<BacktestRequest<'a>, ServerError> {
        macro_rules! take {
            ($field:ident) => {
                self.$field.ok_or_else(|| {
                    ServerError::Backtest(format!(
                        "backtest request refuses: no {} was set",
                        stringify!($field)
                    ))
                })?
            };
        }
        Ok(BacktestRequest {
            indicator: take!(indicator),
            strategy: take!(strategy),
            risk: take!(risk),
            grant: take!(grant),
            venue_config: take!(venue_config),
            runner_config: take!(runner_config),
            signal_name: take!(signal_name),
            underlying: take!(underlying),
            timeframe: take!(timeframe),
            bars: take!(bars),
            seed: take!(seed),
            initial_equity: take!(initial_equity),
        })
    }

    /// Build and run the request through the equity decision stack.
    pub async fn run(self) -> Result<EquityRun, ServerError> {
        self.build()?.run().await
    }
}

impl<'a> BacktestRequest<'a> {
    /// Start a named backtest request. Every policy is set explicitly; there
    /// are no positional defaults to misread.
    #[must_use]
    pub fn builder() -> BacktestRequestBuilder<'a> {
        BacktestRequestBuilder::default()
    }

    /// Run the named request through the equity decision stack.
    pub async fn run(self) -> Result<EquityRun, ServerError> {
        run_equity_backtest(
            self.indicator,
            self.strategy,
            self.risk,
            self.grant,
            self.venue_config,
            self.runner_config,
            self.signal_name,
            self.underlying,
            self.timeframe,
            self.bars,
            self.seed,
            self.initial_equity,
        )
        .await
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
    // The reference strategy commissions a percentage of notional (0.03 %),
    // not a flat per-share charge. State it as the venue's schedule so every
    // fill, fee total, and trade P&L uses the declared model.
    let fees = protopyne_venue::FeeSchedule::new()
        .per_contract(Money::zero())
        .percent_bps(3)
        .build();
    let paper = PaperConfig::new().fees(fees).build();
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
    // One leg batch for the whole run. Every kernel in it is causal, so the
    // value at a bar is the value a prefix batch would have produced; the
    // signal indexes this instead of re-batching every bar.
    let legs = Arc::new(
        lite_legs(bars, indicator, strategy.risk_atr_len)
            .map_err(|e| ServerError::Backtest(format!("leg batch refused: {e}")))?,
    );
    let mut instance = signal
        .bind_with_legs(&SignalConfig::new(timeframe), Arc::clone(&legs))
        .map_err(|e| ServerError::Backtest(format!("signal bind refused: {e}")))?;
    let mut decisions: Vec<BarDecision> = Vec::with_capacity(bars.len());
    let mut trades: Vec<EquityTrade> = Vec::new();
    let mut partials: Vec<EquityTrade> = Vec::new();
    let mut cash = initial_equity;
    let mut fees_paid = 0.0;
    let bar_span_ms = timeframe.duration_ms();
    let mut manager = PositionManager::new(
        PositionSpec::from(strategy.clone()),
        session,
        bar_span_ms,
        indicator.session_filter,
    );
    let mut curve: Vec<protopyne_types::Money> = Vec::with_capacity(bars.len());
    let mut status: Vec<BacktestStatusView> = Vec::with_capacity(bars.len());
    let mut levels: Vec<LevelPoint> = Vec::with_capacity(bars.len());
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
            push_point(
                &mut curve,
                &mut status,
                &mut levels,
                money(cash),
                index,
                &StatusCtx {
                    manager: &manager,
                    enabled: strategy.enabled,
                    index,
                    at: bar.ts.to_rfc3339(),
                    trades: &trades,
                    partials: &partials,
                },
            );
            continue;
        };
        closes.push(close);
        let equity_now = mark_equity(cash, manager.position(), close);
        manager.begin_bar(bar, close, equity_now);
        let proposal_id = |tag: &str| format!("equity-{seed}-{index}-{tag}");

        // Warmup honesty first: no decision before every leg is seeded.
        if closes.len() <= warmup.max(runner_config.warmup_bars) {
            decisions.push(crate::runner::skipped_decision(
                index,
                bar.ts.to_rfc3339(),
                proposal_id("warmup"),
                "warmup",
            ));
            push_point(
                &mut curve,
                &mut status,
                &mut levels,
                money(mark_equity(cash, manager.position(), close)),
                index,
                &StatusCtx {
                    manager: &manager,
                    enabled: strategy.enabled,
                    index,
                    at: bar.ts.to_rfc3339(),
                    trades: &trades,
                    partials: &partials,
                },
            );
            continue;
        }

        let atr = legs.atr_risk[index].unwrap_or(f64::NAN);

        // --- Exits before entries, in the documented order: session flatten,
        // daily-loss flatten, the rule pass, then the breakeven/trailing
        // ratchet (which only tightens the resting stop).
        if manager.position().is_some() {
            if let Some(reason) = manager.forced_flatten() {
                submit_exit_intent(
                    ExitIntent::close(reason, reason),
                    &mut manager,
                    &mut trades,
                    &mut partials,
                    &mut cash,
                    &mut fees_paid,
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
                )
                .await;
                push_point(
                    &mut curve,
                    &mut status,
                    &mut levels,
                    money(mark_equity(cash, manager.position(), close)),
                    index,
                    &StatusCtx {
                        manager: &manager,
                        enabled: strategy.enabled,
                        index,
                        at: bar.ts.to_rfc3339(),
                        trades: &trades,
                        partials: &partials,
                    },
                );
                continue;
            }
            if let Some(tape) =
                protopyne_strategy::tape::Tape::new(&bars[..=index], &legs, &closes, index)
            {
                for intent in manager.review_exits(&tape, bars, index) {
                    submit_exit_intent(
                        intent,
                        &mut manager,
                        &mut trades,
                        &mut partials,
                        &mut cash,
                        &mut fees_paid,
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
                    )
                    .await;
                }
            }
        }

        // --- Entries when flat, and reversals when the operator allows them.
        if manager.is_flat() || strategy.allow_reverse {
            if manager.may_enter(index) {
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
                    // A reversal closes the old side first through the same
                    // submission body, then opens the new side only if that
                    // close filled.
                    if let Some(open) = manager.position() {
                        if open.long == (out.direction == Direction::Long) {
                            decisions.push(crate::runner::skipped_decision(
                                index,
                                bar.ts.to_rfc3339(),
                                proposal_id("hold"),
                                "position already on the signal's side",
                            ));
                            push_point(
                                &mut curve,
                                &mut status,
                                &mut levels,
                                money(mark_equity(cash, manager.position(), close)),
                                index,
                                &StatusCtx {
                                    manager: &manager,
                                    enabled: strategy.enabled,
                                    index,
                                    at: bar.ts.to_rfc3339(),
                                    trades: &trades,
                                    partials: &partials,
                                },
                            );
                            continue;
                        }
                        submit_exit_intent(
                            ExitIntent::close("reverse", "reverse"),
                            &mut manager,
                            &mut trades,
                            &mut partials,
                            &mut cash,
                            &mut fees_paid,
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
                        )
                        .await;
                        if manager.position().is_some() {
                            decisions.push(crate::runner::skipped_decision(
                                index,
                                bar.ts.to_rfc3339(),
                                proposal_id("reverse"),
                                "reversal refused: the closing order did not fill",
                            ));
                            push_point(
                                &mut curve,
                                &mut status,
                                &mut levels,
                                money(mark_equity(cash, manager.position(), close)),
                                index,
                                &StatusCtx {
                                    manager: &manager,
                                    enabled: strategy.enabled,
                                    index,
                                    at: bar.ts.to_rfc3339(),
                                    trades: &trades,
                                    partials: &partials,
                                },
                            );
                            continue;
                        }
                    }
                    if grant_covers(grant, &out) {
                        let Some(quote) = quote_for_bar(bar, venue.config()) else {
                            decisions.push(crate::runner::skipped_decision(
                                index,
                                bar.ts.to_rfc3339(),
                                proposal_id("entry"),
                                "unquotable bar",
                            ));
                            push_point(
                                &mut curve,
                                &mut status,
                                &mut levels,
                                money(mark_equity(cash, manager.position(), close)),
                                index,
                                &StatusCtx {
                                    manager: &manager,
                                    enabled: strategy.enabled,
                                    index,
                                    at: bar.ts.to_rfc3339(),
                                    trades: &trades,
                                    partials: &partials,
                                },
                            );
                            continue;
                        };
                        let equity_now = mark_equity(cash, manager.position(), close);
                        let long = out.direction == Direction::Long;
                        let stop_distance =
                            entry_stop_distance(strategy, atr, close, bars, index, long);
                        let sizer = EquitySizer::new(
                            &strategy.size_mode,
                            strategy.risk_pct,
                            strategy.size_pct,
                            strategy.fixed_qty,
                            strategy.max_notional_pct,
                        )
                        .map_err(|e| ServerError::Backtest(format!("sizer refused: {e}")))?;
                        let facts = PortfolioFacts::unobserved();
                        // The unit is priced per bar from the close and the
                        // clamped stop distance: the sizer never sees a
                        // quantity-scaled number.
                        let unit = protopyne_strategy::UnitEconomics {
                            price_per_unit: money(close),
                            max_loss_per_unit: money(stop_distance),
                            bid_ask_width: Money::zero(),
                            side: if long {
                                protopyne_strategy::PackageSide::Buy
                            } else {
                                protopyne_strategy::PackageSide::Sell
                            },
                        };
                        let ctx = protopyne_strategy::SizingCtx {
                            unit: &unit,
                            facts: &facts,
                            equity: Some(equity_now),
                        };
                        let qty = sizer
                            .size_for(&out, &ctx)
                            .as_decimal()
                            .to_f64()
                            .unwrap_or(0.0)
                            .min(u64::from(u32::MAX) as f64)
                            as u32;
                        if qty > 0 {
                            let touch = if long { quote.ask } else { quote.bid };
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
                            fills += u64::from(decision.submitted);
                            let (Some(price), qty) =
                                (decision.fill_price, decision.filled_quantity)
                            else {
                                decisions.push(decision);
                                push_point(
                                    &mut curve,
                                    &mut status,
                                    &mut levels,
                                    money(mark_equity(cash, manager.position(), close)),
                                    index,
                                    &StatusCtx {
                                        manager: &manager,
                                        enabled: strategy.enabled,
                                        index,
                                        at: bar.ts.to_rfc3339(),
                                        trades: &trades,
                                        partials: &partials,
                                    },
                                );
                                continue;
                            };
                            if qty > 0 {
                                let entry = price.as_decimal().to_f64().unwrap_or(close);
                                filled_notional += price.as_decimal() * Decimal::from(qty);
                                fees_paid += decision
                                    .fee
                                    .map_or(0.0, |f| f.as_decimal().to_f64().unwrap_or(0.0));
                                if long {
                                    cash -= entry * f64::from(qty);
                                } else {
                                    cash += entry * f64::from(qty);
                                }
                                manager.open(long, qty, entry, index, atr, bars);
                                manager.note_entry();
                            }
                            decisions.push(decision);
                            push_point(
                                &mut curve,
                                &mut status,
                                &mut levels,
                                money(mark_equity(cash, manager.position(), close)),
                                index,
                                &StatusCtx {
                                    manager: &manager,
                                    enabled: strategy.enabled,
                                    index,
                                    at: bar.ts.to_rfc3339(),
                                    trades: &trades,
                                    partials: &partials,
                                },
                            );
                            continue;
                        }
                    }
                    decisions.push(crate::runner::skipped_decision(
                        index,
                        bar.ts.to_rfc3339(),
                        proposal_id("entry"),
                        "signal fired but sizing or grant left no quantity",
                    ));
                    push_point(
                        &mut curve,
                        &mut status,
                        &mut levels,
                        money(mark_equity(cash, manager.position(), close)),
                        index,
                        &StatusCtx {
                            manager: &manager,
                            enabled: strategy.enabled,
                            index,
                            at: bar.ts.to_rfc3339(),
                            trades: &trades,
                            partials: &partials,
                        },
                    );
                    continue;
                }
            }
            decisions.push(crate::runner::skipped_decision(
                index,
                bar.ts.to_rfc3339(),
                proposal_id("quiet"),
                "no entry: gate, cooldown, daily limit, or no trigger",
            ));
            push_point(
                &mut curve,
                &mut status,
                &mut levels,
                money(mark_equity(cash, manager.position(), close)),
                index,
                &StatusCtx {
                    manager: &manager,
                    enabled: strategy.enabled,
                    index,
                    at: bar.ts.to_rfc3339(),
                    trades: &trades,
                    partials: &partials,
                },
            );
        } else {
            curve.push(money(mark_equity(cash, manager.position(), close)));
        }
    }

    let mut report = build_report(
        manifest.clone(),
        vec![
            "equity fills price at the quoted touch through the installed venue".to_owned(),
            "stops and targets rest as explicit prices; trailing and breakeven only tighten"
                .to_owned(),
            "a bar that touches both the stop and a target resolves stop-first (the              exchange-conservative both-touched rule)"
                .to_owned(),
            "commissions are 3 basis points of notional (the reference percentage schedule),              with no per-contract charge"
                .to_owned(),
            "quotes are synthesized from the bar close with a symmetric spread, then the              venue's adverse slippage tolerance applies; the reference assumes one tick"
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
    // Trade statistics describe **closed cycles**, not slices: a cycle is one
    // entry through flat, so its P&L is the closing slice plus every partial
    // that belonged to the same entry. Reporting only the closing slice would
    // understate winners and hide the take-profit legs from the win rate.
    report.trades = Some(TradesAnalysis::of(&cycles_of(&trades, &partials)));
    report.unavailable_tabs = absent_tabs();
    // Portfolio aggregation over the same fills the report counts, folded once
    // by the shared aggregator so an equity run and an option run describe
    // their books identically.
    let booked = crate::runner::booked_fills_of(
        &decisions,
        crate::portfolio::PAPER_PORTFOLIO,
        grant.id.as_str(),
        &venue_config.chain_history,
    )?;
    let marks: Vec<MarkPoint<'_>> = bars
        .iter()
        .map(|bar| MarkPoint {
            at: bar.ts,
            price: Money::new(bar.c),
            chain: &venue_config.chain_history,
        })
        .collect();
    report.portfolio = Some(crate::portfolio::aggregate(&booked)?.summarize(&marks)?);
    Ok(EquityRun {
        manifest,
        decisions,
        trades,
        partials,
        status,
        levels,
        report,
    })
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
struct StatusCtx<'a> {
    manager: &'a PositionManager,
    enabled: bool,
    index: usize,
    at: String,
    trades: &'a [EquityTrade],
    partials: &'a [EquityTrade],
}

/// The reference status table's eight rows at one bar, read from the run
/// state the equity loop already carries. Closed-trade rows (win rate, net
/// P&L) fold the same cycles the report counts, so the table and the metrics
/// cannot disagree; the day rows mark at the curve value the bar closed at.
fn status_of(equity: f64, ctx: &StatusCtx<'_>) -> BacktestStatusView {
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
    let done_trades: Vec<EquityTrade> = ctx
        .trades
        .iter()
        .filter(|trade| trade.exit_bar <= ctx.index)
        .cloned()
        .collect();
    let done_partials: Vec<EquityTrade> = ctx
        .partials
        .iter()
        .filter(|partial| partial.exit_bar <= ctx.index)
        .cloned()
        .collect();
    let cycles = cycles_of(&done_trades, &done_partials);
    let closed = cycles.len();
    let wins = cycles.iter().filter(|cycle| cycle.pnl > 0.0).count();
    let win_rate = if closed > 0 {
        format!("{:.1}%", wins as f64 / closed as f64 * 100.0)
    } else {
        "-".to_owned()
    };
    let net: f64 = cycles.iter().map(|cycle| cycle.pnl).sum();
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
fn push_point(
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
async fn submit_exit_intent(
    intent: ExitIntent,
    manager: &mut PositionManager,
    trades: &mut Vec<EquityTrade>,
    partials: &mut Vec<EquityTrade>,
    cash: &mut f64,
    fees_paid: &mut f64,
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
) {
    let Some(pos) = manager.position() else {
        return;
    };
    let long = pos.long;
    let qty = match intent.quantity {
        ExitQuantity::All => pos.qty,
        ExitQuantity::Units(units) => units.min(pos.qty),
        ExitQuantity::Fraction(fraction) => {
            (f64::from(pos.qty) * fraction.clamp(0.0, 1.0)).floor() as u32
        }
    };
    let price = match intent.price {
        RestingPrice::Market => bar.c.to_f64().unwrap_or(f64::NAN),
        RestingPrice::At(price) => price.as_decimal().to_f64().unwrap_or(f64::NAN),
    };
    if qty == 0 || !price.is_finite() {
        return;
    }
    let reason = intent.reason;
    let Some(quote) = quote_for_bar(bar, venue.config()) else {
        return;
    };
    let touch = if long { quote.bid } else { quote.ask };
    let proposal = Proposal::new()
        .id(format!("equity-{seed}-{index}-x-{reason}"))
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
        .limit_price(limit_price_for_exit(long, price, touch))
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
    let proceeds = fill_price * f64::from(filled_qty);
    if long {
        *cash += proceeds - fee;
    } else {
        *cash -= proceeds + fee;
    }
    let pnl = if long {
        (fill_price - pos.entry_price) * f64::from(filled_qty) - fee
    } else {
        (pos.entry_price - fill_price) * f64::from(filled_qty) - fee
    };
    let trade = EquityTrade {
        long,
        qty: filled_qty,
        entry_price: pos.entry_price,
        entry_bar: pos.entry_bar,
        exit_price: fill_price,
        exit_bar: index,
        pnl,
        bars_held: (index.saturating_sub(pos.entry_bar) + 1) as u64,
        exit_reason: reason.clone(),
    };
    if filled_qty >= pos.qty {
        manager.close();
        manager.note_exit(index, pnl);
        trades.push(trade);
    } else {
        manager.take_units(filled_qty);
        partials.push(trade);
    }
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

#[cfg(test)]
#[path = "../tests/equity.rs"]
mod tests;
