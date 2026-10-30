//! Simple long/short backtest: the reference Pine semantics with no
//! proposal, grant, risk-engine, journal, or venue-submission layers.
//!
//! Each bar runs signal → size → fill at the close-derived touch (entries
//! and market exits) or at the resting stop/limit (touched exits), then
//! books cash, fees, trades, and the equity curve. One position at a time.
//! Stops, TP zones, breakeven, trailing, exhaustion, signal exits,
//! cooldowns, daily limits, and session flattens all ride the shared
//! [`PositionManager`], so management matches the reference bar for bar.
//!
//! What this path deliberately does NOT do (the documented difference from
//! `run_equity_backtest`):
//! - no `Proposal` lifecycle, no `decide_proposal`/`submit_decided`;
//! - no `RiskEngine` gates and no `evaluate_grant` merit/budget checks;
//! - no `Recorder`/`Journal` writes in the loop, and no venue `submit`
//!   either: fills price directly off the bar (touch, stop, or limit) with
//!   the shared 3 bps commission schedule, so the legacy path's wrong-side
//!   exit fills (exits submitted through the entry-shaped order helper)
//!   cannot recur;
//! - per-bar gate verdicts on fill decisions read pass-through true with
//!   empty reasons and no lifecycle transitions (see `fill_decision`);
//!   the report's fill assumptions state the absence instead of inventing
//!   verdicts;
//! - commission follows the reference: both the entry and the exit fill pay
//!   3 bps of notional out of cash, so per-cycle P&L matches
//!   `strategy.netprofit`. (The legacy path counted the entry fee in the
//!   fee total but never moved it out of cash.)
//! - a daily-loss breach latches the rest-of-day halt the reference
//!   mandates; the legacy path flattens but re-enters after cooldown.

use std::sync::Arc;

use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use protopyne_kernels::lite::lite_legs;
use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
use protopyne_strategy::position::{entry_stop_distance, PositionManager, PositionSpec};
use protopyne_strategy::signals_lite::{IntegrationLiteSignal, LiteIndicatorParams};
use protopyne_strategy::slots::exit::{ExitIntent, ExitQuantity, RestingPrice};
use protopyne_strategy::{
    Direction, EquitySizer, MarketData, MarketView, PortfolioFacts, ProposalSide, SignalConfig,
    Sizing as _, StructureKind,
};
use protopyne_types::{session::TradingSession, Bar, Money, Timeframe};
use protopyne_view::BacktestStatusView;

use crate::backtest::{quote_for_bar, BacktestConfig};
use crate::equity::{
    cycles_of, mark_equity, money, push_point, ClosedTally, EquityRun, EquityTrade, LevelPoint,
    StatusCtx,
};
use crate::error::ServerError;
use crate::marks::MarkPoint;
use crate::report::{absent_tabs, build_report};
use crate::runner::{skipped_decision, BarDecision, RunnerConfig};

/// Label booked under the portfolio fold's grant slot. The simple path arms
/// no grant; the slot is attribution-only, so it names the path instead of
/// inventing an authorizer.
const SIMPLE_GRANT_LABEL: &str = "simple";

/// One simple backtest run, named: every policy the loop reads, stated once.
pub struct SimpleBacktestRequest<'a> {
    indicator: &'a LiteIndicatorParams,
    strategy: &'a IntegrationLiteStrategyConfig,
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
pub struct SimpleBacktestRequestBuilder<'a> {
    indicator: Option<&'a LiteIndicatorParams>,
    strategy: Option<&'a IntegrationLiteStrategyConfig>,
    venue_config: Option<&'a BacktestConfig>,
    runner_config: Option<&'a RunnerConfig>,
    signal_name: Option<&'a str>,
    underlying: Option<&'a str>,
    timeframe: Option<Timeframe>,
    bars: Option<&'a [Bar]>,
    seed: Option<u64>,
    initial_equity: Option<f64>,
}

macro_rules! simple_setter {
    ($name:ident, $field:ident, $ty:ty) => {
        #[doc = concat!("Set the request's `", stringify!($field), "` policy.")]
        #[must_use]
        pub fn $name(mut self, value: $ty) -> Self {
            self.$field = Some(value);
            self
        }
    };
}

impl<'a> SimpleBacktestRequestBuilder<'a> {
    simple_setter!(indicator, indicator, &'a LiteIndicatorParams);
    simple_setter!(strategy, strategy, &'a IntegrationLiteStrategyConfig);
    simple_setter!(venue_config, venue_config, &'a BacktestConfig);
    simple_setter!(runner_config, runner_config, &'a RunnerConfig);
    simple_setter!(signal_name, signal_name, &'a str);
    simple_setter!(underlying, underlying, &'a str);
    simple_setter!(timeframe, timeframe, Timeframe);
    simple_setter!(bars, bars, &'a [Bar]);
    simple_setter!(seed, seed, u64);
    simple_setter!(initial_equity, initial_equity, f64);

    /// Build the named request. A missing policy refuses naming itself —
    /// no positional default fills in.
    pub fn build(self) -> Result<SimpleBacktestRequest<'a>, ServerError> {
        macro_rules! take {
            ($field:ident) => {
                self.$field.ok_or_else(|| {
                    ServerError::Backtest(format!(
                        "simple backtest request refuses: no {} was set",
                        stringify!($field)
                    ))
                })?
            };
        }
        Ok(SimpleBacktestRequest {
            indicator: take!(indicator),
            strategy: take!(strategy),
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

    /// Build and run the request through the simple decision stack.
    pub fn run(self) -> Result<EquityRun, ServerError> {
        self.build()?.run()
    }
}

impl<'a> SimpleBacktestRequest<'a> {
    /// Start a named simple backtest request.
    #[must_use]
    pub fn builder() -> SimpleBacktestRequestBuilder<'a> {
        SimpleBacktestRequestBuilder::default()
    }

    /// Run the named request through the simple decision stack.
    pub fn run(self) -> Result<EquityRun, ServerError> {
        run_simple_backtest(
            self.indicator,
            self.strategy,
            self.venue_config,
            self.runner_config,
            self.signal_name,
            self.underlying,
            self.timeframe,
            self.bars,
            self.seed,
            self.initial_equity,
        )
    }
}

/// Run the simple decision stack over closed bars.
///
/// The signal proposes, the equity policy sizes, and the order fills at the
/// quoted touch (entries and market exits) or the resting stop/limit
/// (touched exits) — pure arithmetic over the bar, with no gates and no
/// venue round-trip between the signal and the fill. Management (stops, TP
/// zones, breakeven, trailing, exhaustion, signal exits, cooldowns, daily
/// limits, session flatten) rides the runner-owned position manager beside
/// it.
#[allow(clippy::too_many_arguments)]
pub fn run_simple_backtest(
    indicator: &LiteIndicatorParams,
    strategy: &IntegrationLiteStrategyConfig,
    venue_config: &BacktestConfig,
    runner_config: &RunnerConfig,
    signal_name: &str,
    underlying: &str,
    timeframe: Timeframe,
    bars: &[Bar],
    seed: u64,
    initial_equity: f64,
) -> Result<EquityRun, crate::error::ServerError> {
    crate::backtest::ensure_bars_for_session(runner_config, venue_config, bars)?;
    let signal = IntegrationLiteSignal::new(indicator.clone(), strategy.clone())
        .map_err(|e| ServerError::Backtest(format!("signal refused its dials: {e}")))?;
    let session = TradingSession::parse(&indicator.session)
        .map_err(|e| ServerError::Backtest(format!("malformed session: {e}")))?;
    // The reference strategy commissions a percentage of notional (0.03 %),
    // not a flat per-share charge — the shared schedule, applied here to
    // both legs out of cash.
    let fees = protopyne_venue::FeeSchedule::new()
        .per_contract(Money::zero())
        .percent_bps(3)
        .build();
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
        bars,
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
    let mut tally = ClosedTally::default();
    let mut cash = initial_equity;
    let mut fees_paid = 0.0;
    let mut open_fees = 0.0;
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
            decisions.push(skipped_decision(
                index,
                bar.ts.to_rfc3339(),
                format!("simple-{seed}-{index}"),
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
                    tally: &tally,
                },
            );
            continue;
        };
        closes.push(close);
        let equity_now = mark_equity(cash, manager.position(), close);
        manager.begin_bar(bar, close, equity_now);
        let proposal_id = |tag: &str| format!("simple-{seed}-{index}-{tag}");

        // Warmup honesty first: no decision before every leg is seeded.
        if closes.len() <= warmup.max(runner_config.warmup_bars) {
            decisions.push(skipped_decision(
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
                    tally: &tally,
                },
            );
            continue;
        }

        let atr = legs.atr_risk[index].unwrap_or(f64::NAN);

        // --- Exits before entries, in the documented order: session flatten,
        // daily-loss flatten (which latches the rest-of-day halt), then the
        // rule pass.
        if manager.position().is_some() {
            if let Some(reason) = manager.forced_flatten() {
                if reason == "daily-loss" {
                    manager.latch_day_halt();
                }
                if let Some(quote) = quote_for_bar(bar, venue_config) {
                    fill_exit(
                        &ExitIntent::close(reason, reason),
                        &mut manager,
                        &mut trades,
                        &mut partials,
                        &mut tally,
                        &mut cash,
                        &mut fees_paid,
                        &mut open_fees,
                        &mut decisions,
                        &mut fills,
                        &mut filled_notional,
                        &fees,
                        &quote,
                        signal_name,
                        underlying,
                        bar,
                        index,
                        &proposal_id("x"),
                    );
                }
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
                        tally: &tally,
                    },
                );
                continue;
            }
            if let Some(tape) =
                protopyne_strategy::tape::Tape::new(&bars[..=index], &legs, &closes, index)
            {
                if let Some(quote) = quote_for_bar(bar, venue_config) {
                    for intent in manager.review_exits(&tape, bars, index) {
                        fill_exit(
                            &intent,
                            &mut manager,
                            &mut trades,
                            &mut partials,
                            &mut tally,
                            &mut cash,
                            &mut fees_paid,
                            &mut open_fees,
                            &mut decisions,
                            &mut fills,
                            &mut filled_notional,
                            &fees,
                            &quote,
                            signal_name,
                            underlying,
                            bar,
                            index,
                            &proposal_id("x"),
                        );
                    }
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
                    // A reversal closes the old side first, then opens the
                    // new side only if that close filled.
                    if let Some(open) = manager.position() {
                        if open.long == (out.direction == Direction::Long) {
                            decisions.push(skipped_decision(
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
                                    tally: &tally,
                                },
                            );
                            continue;
                        }
                        if let Some(quote) = quote_for_bar(bar, venue_config) {
                            fill_exit(
                                &ExitIntent::close("reverse", "reverse"),
                                &mut manager,
                                &mut trades,
                                &mut partials,
                                &mut tally,
                                &mut cash,
                                &mut fees_paid,
                                &mut open_fees,
                                &mut decisions,
                                &mut fills,
                                &mut filled_notional,
                                &fees,
                                &quote,
                                signal_name,
                                underlying,
                                bar,
                                index,
                                &proposal_id("reverse"),
                            );
                        }
                        if manager.position().is_some() {
                            decisions.push(skipped_decision(
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
                                    tally: &tally,
                                },
                            );
                            continue;
                        }
                    }
                    let Some(quote) = quote_for_bar(bar, venue_config) else {
                        decisions.push(skipped_decision(
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
                                tally: &tally,
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
                        .min(u64::from(u32::MAX) as f64) as u32;
                    if qty > 0 {
                        let touch = if long { quote.ask } else { quote.bid };
                        // Entries fill at the quoted touch, commissioning 3
                        // bps of notional through the shared schedule. The
                        // touch always clears (it is the fill price by
                        // construction), so no refusal path exists here.
                        let price = touch.as_decimal().to_f64().unwrap_or(close);
                        let Some(fee) = fees.fee_for_notional(
                            Money::new(touch.as_decimal() * Decimal::from(qty)),
                            qty,
                        ) else {
                            decisions.push(skipped_decision(
                                index,
                                bar.ts.to_rfc3339(),
                                proposal_id("entry"),
                                "fee overflow",
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
                                    tally: &tally,
                                },
                            );
                            continue;
                        };
                        let fee_f64 = fee.as_decimal().to_f64().unwrap_or(0.0);
                        proposals += 1;
                        fills += 1;
                        filled_notional += touch.as_decimal() * Decimal::from(qty);
                        fees_paid += fee_f64;
                        // Both legs pay commission out of cash, as the
                        // reference does; the entry leg accrues to the open
                        // position and lands in the closing slice's P&L.
                        if long {
                            cash -= price * f64::from(qty) + fee_f64;
                        } else {
                            cash += price * f64::from(qty) - fee_f64;
                        }
                        open_fees += fee_f64;
                        manager.open(long, qty, price, index, atr, bars);
                        manager.note_entry();
                        decisions.push(fill_decision(
                            out.direction,
                            ProposalSide::Entry,
                            out.conviction,
                            qty,
                            touch,
                            fee,
                            touch,
                            signal_name,
                            underlying,
                            &proposal_id("entry"),
                            index,
                            bar,
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
                                tally: &tally,
                            },
                        );
                        continue;
                    }
                    decisions.push(skipped_decision(
                        index,
                        bar.ts.to_rfc3339(),
                        proposal_id("entry"),
                        "signal fired but sizing left no quantity",
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
                            tally: &tally,
                        },
                    );
                    continue;
                }
            }
            decisions.push(skipped_decision(
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
                    tally: &tally,
                },
            );
        } else {
            // Reversals disarmed and a position open: the curve still marks,
            // and the table still reports — every bar carries both, so the
            // two never drift apart by configuration.
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
                    tally: &tally,
                },
            );
        }
    }

    let mut report = build_report(
        manifest.clone(),
        vec![
            "entries fill at the quoted touch; market exits fill at the touch, resting stops and targets fill at their own level when the bar touches it"
                .to_owned(),
            "stops and targets rest as explicit prices; trailing and breakeven only tighten"
                .to_owned(),
            "a bar that touches both the stop and a target resolves stop-first (the exchange-conservative both-touched rule)"
                .to_owned(),
            "commissions are 3 basis points of notional, both legs, paid out of cash (the reference percentage schedule)"
                .to_owned(),
            "quotes are synthesized from the bar close with a symmetric spread; the reference assumes one tick of slippage, this path models none beyond the spread"
                .to_owned(),
            "no risk engine, grant, journal, venue submission, or proposal evaluation on this path; the position manager's cooldown, daily, and session gates still apply"
                .to_owned(),
        ],
        vec![
            format!("simple backtest over {} bars", bars.len()),
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
    // that belonged to the same entry. The closing slice already carries the
    // entry leg's commission (see `fill_exit`), so the fold needs no
    // further adjustment.
    report.trades = Some(crate::report::TradesAnalysis::of(&cycles_of(
        &trades, &partials,
    )));
    report.unavailable_tabs = absent_tabs();
    // Portfolio aggregation over the same fills the report counts, folded
    // once by the shared aggregator. The grant slot names the path: no
    // grant authorized these fills.
    let booked = crate::runner::booked_fills_of(
        &decisions,
        crate::portfolio::PAPER_PORTFOLIO,
        SIMPLE_GRANT_LABEL,
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

/// A fill decision on a path with no gates to evaluate.
///
/// The wire shape is the shared [`BarDecision`] so runs, views, and the
/// portfolio fold keep working unchanged; the gate verdicts read
/// pass-through true with empty reasons and no lifecycle transitions
/// because there is no risk engine, grant, or journal to have produced
/// them. The report's fill assumptions state the absence — a fill here
/// must never be read as a grant-covered one.
#[allow(clippy::too_many_arguments)]
fn fill_decision(
    direction: Direction,
    side: ProposalSide,
    conviction: f64,
    qty: u32,
    price: Money,
    fee: Money,
    touch: Money,
    signal_name: &str,
    underlying: &str,
    proposal_id: &str,
    bar_index: usize,
    bar: &Bar,
) -> BarDecision {
    BarDecision {
        bar_index,
        ts: bar.ts.to_rfc3339(),
        proposal_id: proposal_id.to_owned(),
        direction: Some(direction),
        side: Some(side),
        structure: Some(StructureKind::Single),
        strategy: signal_name.to_owned(),
        underlying: underlying.to_owned(),
        conviction,
        risk_pass: true,
        risk_reasons: Vec::new(),
        grant_merit: true,
        grant_merit_reasons: Vec::new(),
        grant_authorized: true,
        grant_authorization_reasons: Vec::new(),
        submitted: true,
        limit_price: price,
        checked_limit: touch,
        submitted_quantity: qty,
        filled_quantity: qty,
        fill_price: Some(price),
        fee: Some(fee),
        legs: Vec::new(),
        transitions: Vec::new(),
        lifecycle_error: None,
        skipped: None,
    }
}

/// Fill one exit intent off the bar: no proposal, no gates, no journal, no
/// venue. A resting price (`At`) fills at its own level — the intent only
/// exists because the bar touched it — while a market intent fills at the
/// touch. An unquotable bar carries the position to the next bar, which
/// retries; resting orders persist, as in the reference. A full close folds
/// the entry leg's commission (accrued in `open_fees`) into the closing
/// slice so cycle P&L matches the reference's both-legs commission.
#[allow(clippy::too_many_arguments)]
fn fill_exit(
    intent: &ExitIntent,
    manager: &mut PositionManager,
    trades: &mut Vec<EquityTrade>,
    partials: &mut Vec<EquityTrade>,
    tally: &mut ClosedTally,
    cash: &mut f64,
    fees_paid: &mut f64,
    open_fees: &mut f64,
    decisions: &mut Vec<BarDecision>,
    fills: &mut u64,
    filled_notional: &mut Decimal,
    fees: &protopyne_venue::FeeSchedule,
    quote: &protopyne_venue::Quote,
    signal_name: &str,
    underlying: &str,
    bar: &Bar,
    index: usize,
    proposal_id: &str,
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
    if qty == 0 {
        return;
    }
    let touch = if long { quote.bid } else { quote.ask };
    // Touched rest fills at its level; market closes fill at the touch.
    let fill = match intent.price {
        RestingPrice::At(price) => price,
        RestingPrice::Market => touch,
    };
    let Some(fee) = fees.fee_for_notional(Money::new(fill.as_decimal() * Decimal::from(qty)), qty)
    else {
        decisions.push(skipped_decision(
            index,
            bar.ts.to_rfc3339(),
            proposal_id.to_owned(),
            "fee overflow",
        ));
        return;
    };
    let reason = intent.reason.clone();
    let fill_price = fill.as_decimal().to_f64().unwrap_or(f64::NAN);
    if !fill_price.is_finite() {
        return;
    }
    let fee_f64 = fee.as_decimal().to_f64().unwrap_or(0.0);
    *fees_paid += fee_f64;
    *filled_notional += fill.as_decimal() * Decimal::from(qty);
    let proceeds = fill_price * f64::from(qty);
    if long {
        *cash += proceeds - fee_f64;
    } else {
        *cash -= proceeds + fee_f64;
    }
    // Per-fill P&L carries its own leg's commission; the entry leg's
    // commission joins at the full close so the cycle, not the slice, pays
    // it — matching the reference's per-trade net.
    let mut pnl = if long {
        (fill_price - pos.entry_price) * f64::from(qty) - fee_f64
    } else {
        (pos.entry_price - fill_price) * f64::from(qty) - fee_f64
    };
    let trade = EquityTrade {
        long,
        qty,
        entry_price: pos.entry_price,
        entry_bar: pos.entry_bar,
        exit_price: fill_price,
        exit_bar: index,
        pnl: 0.0,
        bars_held: (index.saturating_sub(pos.entry_bar) + 1) as u64,
        exit_reason: reason.clone(),
    };
    if qty >= pos.qty {
        pnl -= *open_fees;
        *open_fees = 0.0;
        manager.close();
        manager.note_exit(index, pnl);
        let mut cycle_pnl = pnl;
        for partial in partials.iter() {
            if partial.entry_bar == trade.entry_bar && partial.long == trade.long {
                cycle_pnl += partial.pnl;
            }
        }
        tally.note_close(cycle_pnl);
        trades.push(EquityTrade { pnl, ..trade });
    } else {
        manager.take_units(qty);
        partials.push(EquityTrade { pnl, ..trade });
    }
    *fills += 1;
    decisions.push(fill_decision(
        if long {
            Direction::Long
        } else {
            Direction::Short
        },
        ProposalSide::Exit,
        1.0,
        qty,
        fill,
        fee,
        touch,
        signal_name,
        underlying,
        proposal_id,
        index,
        bar,
    ));
}

#[cfg(test)]
#[path = "../tests/simple_backtest.rs"]
mod tests;
