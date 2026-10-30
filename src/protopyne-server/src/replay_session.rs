//! Interactive replay session: the batch decide loop, one bar at a time.
//!
//! [`ReplaySession`] owns the per-run state the batch loops used to hold as
//! locals (grant usage, breaker, closes prefix, cursor, decisions, ephemeral
//! journal) and advances it through
//! [`decide_and_submit`](crate::runner::decide_and_submit) — the same shared
//! decision stack batch backtest and the paper feed run. Batch entry points
//! drive a session to completion, so interactive stepping and batch runs
//! cannot diverge: there is one loop body, and the session owns it.
//!
//! Options gate: multi-leg structures (vertical, calendar, condor) need
//! per-leg market data, and prices that data must come from. A batch run
//! carries it in the venue config's chain history; interactive replay
//! installs the gateway-sliced window on its driver, which also selects the
//! chain-derived economics arm — config-supplied economics are the caller's
//! constants and refuse a leg-attached proposal, so a package is only ever
//! booked when the gates checked the structure's own numbers. Under
//! [`ChainPolicy::RequireChains`] the session screens the structure before
//! the shared path could book it, refusing fail-closed with the missing feed
//! named. Singles price off the bar-derived touch and fill honestly, exactly
//! as in batch.
//!
//! Chain-backed batch, stated once: a batch run whose venue config carries
//! chain history threads the venue's own installed source into the session
//! and screens under [`ChainPolicy::RequireChains`], exactly as interactive
//! replay does — covered bars flow, uncovered bars refuse with the feed
//! named. Without chain history the batch keeps its pinned unscreened
//! legacy; the two agree on every unscreened run decision-for-decision (see
//! `stepping_to_completion_reproduces_the_batch_run`).

use chrono::{DateTime, Utc};
use common_core::registry::KeyedRegistry;
use rust_decimal::prelude::ToPrimitive as _;

use protopyne_strategy::{
    ChainSlice, Grant, GrantLedger, GrantUsage, MarketData, MarketView, PortfolioFacts, RiskEngine,
    SignalConfig, SignalInstance, SizingCaps, Strategy, StructureKind,
};
use protopyne_types::{Bar, Timeframe};
use protopyne_venue::{GateReading, Journal, LegQuoteSource, PaperConfig, Quote, Recorder, Venue};

use crate::backtest::{BacktestConfig, BacktestVenue};
use crate::error::ServerError;
use crate::runner::{
    bar_quote, decide_proposal, skipped_decision, submit_decided, BarDecision, BarHealth,
    RunnerConfig,
};

/// Appended to the skip reason of an unsubmitted multi-leg decision. The
/// venue's own refusal stays ahead of it verbatim; this names the missing
/// collaborator (the chain feed) the venue cannot name. Crate-visible so the
/// gateway flags chain-gated verdicts beside the reason, never by parsing it.
pub(crate) const CHAIN_GATE_NOTE: &str =
    "options spreads need an option-chain feed (none configured)";

/// Whether a proposal structure needs per-leg market data to price: every
/// multi-leg shape. Singles price off the bar-derived touch, exactly as in
/// batch. Crate-visible so the shared decide path and the rehearsal
/// composition screen the same shapes the session screens.
pub(crate) fn needs_chain_data(structure: StructureKind) -> bool {
    structure.needs_chain_data()
}

/// Whether an installed chain source covers `underlying` at `at`: any held
/// quote at-or-before the bar. A coverage fact about the producer's input
/// (validity), never a verdict on the trade — the grant tiers still judge
/// economics unchanged. An empty snapshot refuses; a stale one still
/// selects, and the fill policy's age bound refuses it per leg.
fn chain_covers(chain: Option<&dyn LegQuoteSource>, underlying: &str, at: DateTime<Utc>) -> bool {
    chain.is_some_and(|source| source.covers(underlying, at))
}

/// Whether a session may flow multi-leg proposals to the venue without
/// chain data.
///
/// An unscreened batch preserves its pinned legacy: multi-leg labels book
/// as single legs, exactly as the historical loop booked them. Interactive
/// replay passes [`ChainPolicy::RequireChains`], and a batch run whose
/// venue config demands a screen joins it — covered bars flow, uncovered
/// bars refuse naming the missing feed, never booked as single legs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainPolicy {
    /// No structure screen; multi-leg proposals flow to the venue exactly as
    /// the historical loop booked them (batch legacy).
    Unscreened,
    /// Multi-leg proposals flow only when an installed chain source covers
    /// the bar; uncovered bars refuse before anything journals or submits,
    /// naming the missing chain feed (interactive replay).
    RequireChains,
}

/// One position the session's book carries across bars.
#[derive(Debug, Clone, PartialEq)]
pub struct BookPosition {
    /// Underlying.
    pub underlying: String,
    /// Long or short.
    pub long: bool,
    /// Units held.
    pub quantity: u32,
    /// Average entry price.
    pub entry_avg: protopyne_types::Money,
    /// Worst-case loss committed at entry.
    pub max_loss: protopyne_types::Money,
    /// Entry bar.
    pub entry_bar: usize,
    /// Strategy attribution.
    pub strategy: String,
    /// Structure the entry used.
    pub structure: StructureKind,
    /// Original entry proposal id (the exit body keeps identity).
    pub entry_id: String,
}

/// One realized trade booked by the session's book.
///
/// Deliberately not the equity runner's trade record: this book is
/// multi-symbol (hence `underlying`), averages in `Money`, and never books
/// partials, while the equity run is single-symbol, prices in `f64`, and
/// reuses its record for partial fills — see that record's own note for the
/// full field list that forbids the merge.
#[derive(Debug, Clone, PartialEq)]
pub struct BookTrade {
    /// Underlying.
    pub underlying: String,
    /// Long or short.
    pub long: bool,
    /// Units closed.
    pub quantity: u32,
    /// Entry average.
    pub entry_avg: protopyne_types::Money,
    /// Exit average.
    pub exit_avg: protopyne_types::Money,
    /// Realized P&L after fees.
    pub pnl: f64,
    /// Entry bar.
    pub entry_bar: usize,
    /// Exit bar.
    pub exit_bar: usize,
    /// Why it closed.
    pub reason: String,
}

/// The session's position book: one open position per underlying plus the
/// realized-trade log. It is fed by the same decisions the venue produced, so
/// the strategy's exit rules and the report read one book.
#[derive(Debug, Clone, Default)]
pub struct PositionBook {
    positions: Vec<BookPosition>,
    trades: Vec<BookTrade>,
}

impl PositionBook {
    /// An empty book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open positions.
    #[must_use]
    pub fn positions(&self) -> &[BookPosition] {
        &self.positions
    }

    /// Realized trades in close order.
    #[must_use]
    pub fn trades(&self) -> &[BookTrade] {
        &self.trades
    }

    /// Whether the book is flat.
    #[must_use]
    pub fn is_flat(&self) -> bool {
        self.positions.is_empty()
    }

    /// Position views for the strategy's loss rules.
    #[must_use]
    pub fn views(&self) -> Vec<protopyne_strategy::PositionView<'_>> {
        self.positions
            .iter()
            .map(|position| protopyne_strategy::PositionView {
                underlying: position.underlying.clone(),
                side: if position.long {
                    protopyne_strategy::Direction::Long
                } else {
                    protopyne_strategy::Direction::Short
                },
                quantity: position.quantity,
                entry_avg: position.entry_avg,
                max_loss: position.max_loss,
                dte_days: None,
                legs: None,
                events: None,
            })
            .collect()
    }

    /// Apply one filled decision to the book. An entry opens/averages in;
    /// an exit reduces or closes and books the realized trade.
    pub fn apply(
        &mut self,
        decision: &BarDecision,
        proposal: Option<&protopyne_strategy::Proposal>,
        index: usize,
        reason: &str,
    ) {
        if !decision.submitted || decision.filled_quantity == 0 {
            return;
        }
        let (Some(direction), Some(price)) = (decision.direction, decision.fill_price) else {
            return;
        };
        let qty = decision.filled_quantity;
        let long = direction == protopyne_strategy::Direction::Long;
        let exit = matches!(decision.side, Some(protopyne_strategy::ProposalSide::Exit));
        if exit {
            let Some(position) = self
                .positions
                .iter_mut()
                .find(|position| position.underlying == decision.underlying)
            else {
                return;
            };
            let closed = qty.min(position.quantity);
            let sign = if position.long { 1.0 } else { -1.0 };
            let pnl = (price.as_decimal().to_f64().unwrap_or(0.0)
                - position.entry_avg.as_decimal().to_f64().unwrap_or(0.0))
                * f64::from(closed)
                * sign
                - decision
                    .fee
                    .map_or(0.0, |fee| fee.as_decimal().to_f64().unwrap_or(0.0));
            self.trades.push(BookTrade {
                underlying: position.underlying.clone(),
                long: position.long,
                quantity: closed,
                entry_avg: position.entry_avg,
                exit_avg: price,
                pnl,
                entry_bar: position.entry_bar,
                exit_bar: index,
                reason: if reason.is_empty() {
                    "exit".to_owned()
                } else {
                    reason.to_owned()
                },
            });
            position.quantity = position.quantity.saturating_sub(closed);
            if position.quantity == 0 {
                self.positions
                    .retain(|candidate| candidate.underlying != decision.underlying);
            }
        } else {
            let entry_avg = price;
            let max_loss =
                proposal.map_or(protopyne_types::Money::zero(), |proposal| proposal.max_loss);
            let strategy = proposal.map_or_else(String::new, |proposal| proposal.strategy.clone());
            let structure = proposal.map_or(StructureKind::Single, |proposal| proposal.structure);
            let entry_id = proposal.map_or_else(
                || decision.proposal_id.clone(),
                |proposal| proposal.id.clone(),
            );
            if let Some(position) = self.positions.iter_mut().find(|position| {
                position.underlying == decision.underlying && position.long == long
            }) {
                let total = position.quantity.saturating_add(qty);
                if total > 0 {
                    let weighted = position.entry_avg.as_decimal()
                        * rust_decimal::Decimal::from(position.quantity)
                        + entry_avg.as_decimal() * rust_decimal::Decimal::from(qty);
                    position.entry_avg =
                        protopyne_types::Money::new(weighted / rust_decimal::Decimal::from(total));
                    position.quantity = total;
                }
            } else {
                self.positions.push(BookPosition {
                    underlying: decision.underlying.clone(),
                    long,
                    quantity: qty,
                    entry_avg,
                    max_loss,
                    entry_bar: index,
                    strategy,
                    structure,
                    entry_id,
                });
            }
        }
    }

    /// The first exit rule's proposal for the open book at this bar, when
    /// the strategy has exits and a rule fires.
    #[must_use]
    pub fn exit_proposal(
        &self,
        strategy: &Strategy,
        bar: &Bar,
        index: usize,
        seed: u64,
        quote: &Quote,
    ) -> Option<protopyne_strategy::Proposal> {
        if !strategy.has_exits() {
            return None;
        }
        let mark = protopyne_strategy::MarkContext {
            basis: protopyne_types::MarkBasis::FillPrice,
            mark: Some(protopyne_types::Money::new(bar.c)),
        };
        for view in self.views() {
            let actions = strategy.review_exits(&view, &mark);
            let Some(action) = actions.into_iter().next() else {
                continue;
            };
            let position = self
                .positions
                .iter()
                .find(|position| position.underlying == view.underlying)?;
            let entry = protopyne_strategy::Proposal::new()
                .id(position.entry_id.clone())
                .strategy(position.strategy.clone())
                .underlying(position.underlying.clone())
                .direction(view.side)
                .side(protopyne_strategy::ProposalSide::Entry)
                .structure(position.structure)
                .quantity(protopyne_types::Qty::new(rust_decimal::Decimal::from(
                    position.quantity,
                )))
                .limit_price(position.entry_avg)
                .max_loss(position.max_loss)
                .notional(protopyne_types::Money::zero())
                .dte_days(0)
                .conviction(1.0)
                .activity_score(1.0)
                .created_at(bar.ts)
                .build();
            let intent =
                protopyne_strategy::slots::exit::ExitIntent::close(action.kind, action.reason);
            let touch = if view.side == protopyne_strategy::Direction::Long {
                quote.bid
            } else {
                quote.ask
            };
            let id = format!("run-{seed}-{index}-exit");
            return Some(strategy.exit_proposal_named(
                &entry,
                protopyne_types::Qty::new(rust_decimal::Decimal::from(position.quantity)),
                &intent,
                touch,
                bar.ts,
                &id,
            ));
        }
        None
    }
}

/// One replayable run over pinned bars, advanced a bar per [`step`](Self::step).
///
/// Borrows the strategy, risk engine, grant, venue, and bars: the batch loops
/// lend all five, and an interactive driver lends the same set across frames.
/// Owns only the run state (grant usage, breaker, closes prefix, cursor,
/// decisions). The journal is ephemeral and scoped to the session — a replay
/// never writes the live paper journal.
pub struct ReplaySession<'a, V> {
    strategy: &'a Strategy,
    signal: Box<dyn SignalInstance>,
    risk: &'a RiskEngine,
    ledger: GrantLedger,
    venue: &'a V,
    quote_config: BacktestConfig,
    runner_config: RunnerConfig,
    chains: ChainPolicy,
    /// Installed chain source for point-in-time selection and the coverage
    /// screen, when the caller threads one. Batch threads the venue's own
    /// installed source; the owned driver threads its installed source.
    /// `None` keeps the historical unscreened legacy byte-identical.
    chain: Option<std::sync::Arc<dyn LegQuoteSource>>,
    journal: Journal,
    bars: &'a [Bar],
    timeframe: Timeframe,
    closes: Vec<f64>,
    cursor: usize,
    decisions: Vec<BarDecision>,
    seed: u64,
    skipped_warmup: u64,
    book: PositionBook,
    corporate: Vec<protopyne_venue::CorporateEventArgs>,
    corporate_cursor: usize,
}

impl<'a, V: Venue> ReplaySession<'a, V> {
    /// Open a session over `bars`: validates the runner and venue configs
    /// and refuses empty history, so a misconfigured replay fails before the
    /// first bar rather than mid-run. `chains` states up front whether
    /// multi-leg proposals may flow without chain data — batch passes
    /// [`ChainPolicy::Unscreened`] for its pinned legacy unless the venue
    /// config demands a screen, interactive replay passes
    /// [`ChainPolicy::RequireChains`]. `chain` is the point-in-time source
    /// the shared body selects legs from, exactly as the owned driver
    /// threads its installed source; `None` keeps the historical unscreened
    /// legacy byte-identical.
    pub fn start(
        strategy: &'a Strategy,
        risk: &'a RiskEngine,
        grant: &'a Grant,
        venue: &'a V,
        quote_config: BacktestConfig,
        runner_config: RunnerConfig,
        chains: ChainPolicy,
        chain: Option<std::sync::Arc<dyn LegQuoteSource>>,
        bars: &'a [Bar],
        seed: u64,
    ) -> Result<Self, ServerError> {
        crate::backtest::ensure_bars_for_session(&runner_config, &quote_config, bars)?;
        let timeframe = Timeframe::infer_or_d1(bars);
        // One instance for the whole run: concurrent sessions never share
        // signal state, and a refusal to bind fails before the first bar.
        let signal = strategy
            .bind(&SignalConfig::new(timeframe))
            .map_err(|e| ServerError::Backtest(format!("signal bind refused: {e}")))?;
        let corporate = quote_config.corporate_actions.clone();
        Ok(Self {
            strategy,
            signal,
            risk,
            ledger: GrantLedger::new(grant.clone()),
            venue,
            quote_config,
            runner_config,
            chains,
            chain,
            journal: Journal::ephemeral(),
            bars,
            timeframe,
            closes: Vec::with_capacity(bars.len()),
            cursor: 0,
            decisions: Vec::with_capacity(bars.len()),
            seed,
            skipped_warmup: 0,
            book: PositionBook::new(),
            corporate,
            corporate_cursor: 0,
        })
    }

    /// Bars in the replay window.
    #[must_use]
    pub fn total_bars(&self) -> usize {
        self.bars.len()
    }

    /// Index of the next bar [`step`](Self::step) will decide.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Decisions recorded so far, in replay order.
    #[must_use]
    pub fn decisions(&self) -> &[BarDecision] {
        &self.decisions
    }

    /// Warmup bars skipped so far.
    #[must_use]
    pub fn skipped_warmup(&self) -> u64 {
        self.skipped_warmup
    }

    /// Grant budget usage accumulated so far.
    #[must_use]
    pub fn usage(&self) -> GrantUsage {
        self.ledger.usage()
    }

    /// Decide the next bar through the shared decision stack and record the
    /// verdict, or `None` when the cursor stands past the last bar. The body
    /// is the batch loop body exactly (close representability, warmup
    /// honesty, then [`decide_and_submit`](crate::runner::decide_and_submit)
    /// over the closes prefix with rehearsed health), with one replay-only
    /// screen ahead of it: a multi-leg proposal is refused before the shared
    /// path could book it as a single leg, naming the missing chain feed.
    /// Singles flow through untouched, so stepping a session to completion
    /// reproduces a single-leg batch run decision-for-decision.
    pub async fn step(&mut self) -> Option<BarDecision> {
        let index = self.cursor;
        let bar = self.bars.get(index)?;
        self.cursor += 1;
        let decision = advance_bar(
            BarAdvance {
                strategy: self.strategy,
                signal: self.signal.as_mut(),
                risk: self.risk,
                ledger: &self.ledger,
                venue: self.venue,
                journal: &self.journal,
                permit: None,
                quote_config: &self.quote_config,
                chains: self.chains,
                // The session's own installed source, exactly as the owned
                // driver threads its installed source: one code path, no
                // fork. `None` is the unscreened legacy.
                chain: self.chain.as_deref(),
                bars: self.bars,
                timeframe: self.timeframe,
                closes: &mut self.closes,
                skipped_warmup: &mut self.skipped_warmup,
                book: &mut self.book,
                runner_config: &self.runner_config,
                seed: self.seed,
                corporate: &self.corporate,
                corporate_cursor: &mut self.corporate_cursor,
                // The replay bodies synthesize quotes from the bar, exactly
                // as the batch loops did.
                quote: BarQuote::Synthesized,
                health: BarHealth::rehearsed(),
                facts: None,
                // Rehearsal declares the producer gates irrelevant at
                // construction: no reading, no halt, byte-identical bars.
                gate: None,
            },
            index,
            bar,
        )
        .await;
        self.decisions.push(decision.clone());
        Some(decision)
    }
}

#[cfg(test)]
#[path = "../tests/replay_session.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/replay_driver.rs"]
mod driver_tests;

/// Everything one bar advance borrows, stated once so the borrowed session
/// and the owned driver advance through the same body: there is one loop
/// body, and this struct carries it.
pub(crate) struct BarAdvance<'a, V> {
    pub strategy: &'a Strategy,
    /// This run's signal instance: stepped once per bar, never shared.
    pub signal: &'a mut dyn SignalInstance,
    pub risk: &'a RiskEngine,
    pub ledger: &'a GrantLedger,
    pub venue: &'a V,
    pub journal: &'a Journal,
    /// Durable permit for the journal, when the caller owns one: the live
    /// loop records through it; batch and replay pass `None` and stay
    /// ephemeral.
    pub permit: Option<&'a protopyne_venue::VenueStorage>,
    pub quote_config: &'a BacktestConfig,
    pub chains: ChainPolicy,
    /// Per-leg market data for selection, when installed. The session holds
    /// whatever its caller threaded (the batch run's own installed source,
    /// or nothing for the unscreened legacy); the owned driver threads its
    /// installed source. Never a second fill path — fills read the venue's
    /// own installed source.
    pub chain: Option<&'a dyn LegQuoteSource>,
    /// The full history window: each bar's [`MarketData`] is the prefix
    /// ending at the bar being decided, so a signal never sees the future.
    pub bars: &'a [Bar],
    /// Spacing of [`bars`](Self::bars), inferred once for the run.
    pub timeframe: Timeframe,
    pub closes: &'a mut Vec<f64>,
    pub skipped_warmup: &'a mut u64,
    /// The run's open positions and realized trades.
    pub book: &'a mut PositionBook,
    pub runner_config: &'a RunnerConfig,
    pub seed: u64,
    /// Corporate-action tape in effective order; each event applies at the
    /// first bar whose timestamp reaches it.
    pub corporate: &'a [protopyne_venue::CorporateEventArgs],
    pub corporate_cursor: &'a mut usize,
    /// This bar's quote policy: synthesized from the bar (the rehearsal
    /// default), a quote the source ladder resolved, or a dark ladder.
    pub quote: BarQuote,
    /// Observed producer-health for this bar. The rehearsal default states
    /// quotes are synthesized at the bar close over one clock; a live driver
    /// fills it from the resolved quote and the clock.
    pub health: BarHealth,
    /// Observed portfolio facts for the aggregate gates, when the caller
    /// owns a producer. The live loop reads the session's producer every
    /// bar; batch and replay pass `None` and run exactly as before.
    pub facts: Option<PortfolioFacts>,
    /// The session's producer-confidence reading, when the caller owns a
    /// session. The live loop passes one every bar; batch and replay pass
    /// `None` and run exactly as before (declared irrelevant at
    /// construction). A trip skips the bar with a named refusal after the
    /// signal advances, so exits still route through the submission path.
    pub gate: Option<GateReading>,
}

/// Where one bar's quote comes from, so the same bar body serves the batch
/// loops, the replay driver, and the live loop without forking.
pub(crate) enum BarQuote {
    /// Synthesize from the closed bar (the rehearsal default, byte-identical
    /// to the batch loops).
    Synthesized,
    /// Price against a quote the source ladder resolved.
    Resolved(Quote),
    /// Quote sources were configured but every one was dark: the body skips
    /// the bar after the signal advances, never inventing a price.
    Dark,
}

/// Advance one bar through close representability, warmup honesty, the
/// chain screen, and the shared submission body over the closes prefix with
/// rehearsed health. The run's own signal instance decides the bar exactly
/// once: a refusal skips; under [`ChainPolicy::RequireChains`] a multi-leg
/// structure with no chain coverage for the bar skips on the missing chain
/// feed before anything journals or submits, while a covered bar flows into
/// the shared body, which selects legs as of the bar. Under
/// [`ChainPolicy::Unscreened`] every proposal flows, exactly as the
/// historical loop ran.
pub(crate) async fn advance_bar<V: Venue>(
    advance: BarAdvance<'_, V>,
    index: usize,
    bar: &Bar,
) -> BarDecision {
    let proposal_id = format!("run-{}-{index}", advance.seed);
    let ts = bar.ts.to_rfc3339();
    // Corporate actions land before this bar decides: the book a proposal is
    // gated against is the post-action book, and a refusal names itself.
    if let Err(reason) = apply_corporate_actions(
        advance.venue,
        advance.journal,
        advance.corporate,
        advance.corporate_cursor,
        bar.ts,
    ) {
        return skipped_decision(index, ts, proposal_id, &reason);
    }
    let Some(close) = bar.c.to_f64() else {
        return skipped_decision(index, ts, proposal_id, "close not representable");
    };
    advance.closes.push(close);
    if advance.closes.len() <= advance.runner_config.warmup_bars {
        *advance.skipped_warmup += 1;
        return skipped_decision(index, ts, proposal_id, "warmup");
    }
    // Exits run before any entry and before the signal is asked for one. A
    // bar that closes the book records the exit decision; the next bar asks
    // the signal fresh, exactly as the equity runner manages a position
    // before it considers an entry.
    let quote = match &advance.quote {
        // Rehearsal policy: the quote is synthesized from the closed bar,
        // exactly as the batch loops priced it.
        BarQuote::Synthesized => match bar_quote(bar, advance.quote_config) {
            Ok(quote) => quote,
            Err(reason) => return skipped_decision(index, ts, proposal_id, reason),
        },
        // Live policy: price against the quote a source actually resolved.
        BarQuote::Resolved(quote) => *quote,
        // Every configured source was dark. The bar fails closed rather than
        // inventing a price (the signal still advanced above).
        BarQuote::Dark => {
            return skipped_decision(index, ts, proposal_id, "no quote from any source")
        }
    };
    // One recorder view per step over the session-scoped journal: the shared
    // submission protocol runs unchanged, and parallel sessions stay
    // isolated. A live loop supplies its durable permit, so the same body
    // records authority-bearing rows.
    let recorder = match advance.permit {
        Some(permit) => Recorder::durable(advance.journal, permit),
        None => Recorder::ephemeral(advance.journal),
    };
    if let Some(exit) =
        advance
            .book
            .exit_proposal(advance.strategy, bar, index, advance.seed, &quote)
    {
        let kind = exit.id.clone();
        let decision = submit_decided(
            exit,
            quote,
            advance.risk,
            advance.ledger,
            advance.venue,
            &recorder,
            advance.health,
            advance.facts,
            index,
            bar,
            advance.runner_config,
        )
        .await;
        advance.book.apply(&decision, None, index, &kind);
        return decision;
    }
    // The view's bars are the prefix ending at this bar: a signal sees the
    // same past the caller's closes prefix does, never the future.
    let window = advance.bars.get(..=index).unwrap_or(advance.bars);
    // The chain-derived arm needs the point-in-time chain in the view; the
    // config-supplied arm keeps the chainless view its pinned legacy used.
    let snapshot = if advance.runner_config.economics.is_config_supplied() {
        None
    } else {
        advance
            .chain
            .map(|source| source.chain_snapshot(&advance.runner_config.underlying, bar.ts))
    };
    let chain = match snapshot.as_ref() {
        Some(quotes) => match ChainSlice::new(quotes, bar.ts, &advance.runner_config.underlying) {
            Ok(slice) => Some(slice),
            Err(error) => {
                return skipped_decision(
                    index,
                    ts,
                    proposal_id,
                    &format!("chain slice refused: {error}"),
                );
            }
        },
        None => None,
    };
    let data = MarketData {
        bars: window,
        chain,
        dte_days: advance.runner_config.dte_days,
        timeframe: advance.timeframe,
    };
    let view = MarketView {
        data: &data,
        closes: advance.closes,
        as_of: bar.ts,
    };
    // Containment threads the risk engine's per-trade cap and the observed
    // facts the bar carries, so the proposal is already contained before
    // the gates judge it.
    let proposal = match decide_proposal(
        advance.strategy,
        advance.signal,
        &proposal_id,
        bar,
        &view,
        advance.runner_config,
        advance.venue.fee_schedule().per_contract,
        Some(advance.ledger.grant().trade.max_dte_days),
        SizingCaps {
            risk: Some(advance.risk.limits()),
            facts: advance.facts.as_ref(),
        },
    ) {
        Ok(proposal) => proposal,
        Err(error) => {
            // One missing-feed vocabulary under the screen policy: a bar the
            // window does not cover cannot carry a package, so a proposal
            // refused for want of the feed names the feed — the same note the
            // screen below records, whichever way the refusal came.
            if advance.chains == ChainPolicy::RequireChains
                && !chain_covers(advance.chain, &advance.runner_config.underlying, bar.ts)
            {
                return skipped_decision(index, ts, proposal_id, CHAIN_GATE_NOTE);
            }
            return skipped_decision(index, ts, proposal_id, &format!("signal refused: {error}"));
        }
    };
    if advance.chains == ChainPolicy::RequireChains
        && needs_chain_data(proposal.structure)
        && !chain_covers(advance.chain, &advance.runner_config.underlying, bar.ts)
    {
        return skipped_decision(index, ts, proposal_id, CHAIN_GATE_NOTE);
    }
    // Producer-confidence halt: the signal advanced above (its state stays
    // in sync across the trip), but no entry flows while any attached gate
    // refuses. Exits route through the submission path, never through this
    // bar. Batch and replay never set the gate, so their bytes cannot move.
    if let Some(gate) = &advance.gate {
        if !gate.allows_entries() {
            let reason = gate.entry_refusal_reason().expect("a halt names its axis");
            return skipped_decision(index, ts, proposal_id, &reason);
        }
    }
    let decision = submit_decided(
        proposal.clone(),
        quote,
        advance.risk,
        advance.ledger,
        advance.venue,
        &recorder,
        advance.health,
        advance.facts,
        index,
        bar,
        advance.runner_config,
    )
    .await;
    advance.book.apply(&decision, Some(&proposal), index, "");
    decision
}

/// Apply every corporate action due at-or-before `at`, in tape order, and
/// journal what the venue booked. One action row carries the generated stock
/// leg; each closed option leg lands its own [`LEG_FILL_KIND`] row so per-OCC
/// reconciliation sees the close without treating a stock leg as an option.
fn apply_corporate_actions<V: Venue>(
    venue: &V,
    journal: &Journal,
    actions: &[protopyne_venue::CorporateEventArgs],
    cursor: &mut usize,
    at: DateTime<Utc>,
) -> Result<(), String> {
    while let Some(action) = actions.get(*cursor) {
        if action.at > at {
            break;
        }
        let outcome = venue
            .apply_corporate_event(action)
            .map_err(|error| format!("corporate action refused: {error}"))?;
        let client = format!("corporate:{}:{}", action.underlying, *cursor);
        let detail = serde_json::to_string(&(action, &outcome))
            .map_err(|error| format!("corporate action not serializable: {error}"))?;
        journal
            .record_ephemeral(&client, protopyne_venue::CORPORATE_KIND, &detail, action.at)
            .map_err(|error| format!("corporate action journal refused: {error}"))?;
        for (index, leg) in outcome.legs.iter().enumerate() {
            let detail = serde_json::to_string(leg)
                .map_err(|error| format!("corporate leg not serializable: {error}"))?;
            journal
                .record_ephemeral(
                    &format!("{client}#leg{index}"),
                    protopyne_venue::LEG_FILL_KIND,
                    &detail,
                    action.at,
                )
                .map_err(|error| format!("corporate leg journal refused: {error}"))?;
        }
        *cursor += 1;
    }
    Ok(())
}
///
/// The registry-side twin of [`ReplaySession`]: where the session borrows
/// the strategy, risk engine, grant, venue, and bars (lifetimes a registry
/// slot cannot hold), the driver owns all five — the venue pins its own
/// copy of the bars behind an explicitly tuned paper config, so the replay
/// fills through the same paper fills as batch. Run state advances through
/// [`advance_bar`], the same body the session steps, so the two cannot
/// diverge. The chain policy is fixed to
/// [`ChainPolicy::RequireChains`]: a multi-leg proposal flows only when the
/// installed chains cover the bar, and uncovered bars refuse with the feed
/// named — never booked as single legs, and never priced by the config arm.
pub struct ReplayDriver {
    strategy: Strategy,
    signal: Box<dyn SignalInstance>,
    risk: RiskEngine,
    ledger: GrantLedger,
    venue: BacktestVenue,
    quote_config: BacktestConfig,
    runner_config: RunnerConfig,
    journal: Journal,
    closes: Vec<f64>,
    timeframe: Timeframe,
    cursor: usize,
    decisions: Vec<BarDecision>,
    seed: u64,
    skipped_warmup: u64,
    book: PositionBook,
    /// Chain policy for this driver's steps. Always [`ChainPolicy::RequireChains`]:
    /// interactive replay screens multi-leg proposals bar by bar (covered
    /// bars flow past the screen, uncovered bars refuse naming the feed),
    /// while the borrowed [`ReplaySession`] follows its caller's policy.
    /// A field rather than a step-local literal so both steps read their
    /// policy from `self` the same way (M7.2).
    chains: ChainPolicy,
    /// Installed chain source, retained for point-in-time selection. The
    /// venue holds its own clone for fills; this one serves the snapshot the
    /// shared decide path prices legs from. `None` until installed, and its
    /// presence selects the chain-derived economics arm.
    chain: Option<std::sync::Arc<dyn LegQuoteSource>>,
    corporate: Vec<protopyne_venue::CorporateEventArgs>,
    corporate_cursor: usize,
}

impl ReplayDriver {
    /// Open a driver over `bars`: validates both configs and refuses empty
    /// history, so a misconfigured replay fails before the first bar rather
    /// than mid-run. `start_bar` names the first bar the driver decides;
    /// earlier bars feed indicator context only — their representable closes
    /// pre-fill the signal prefix (the batch rule, never a repair), so
    /// signal inputs from `start_bar` on match a batch run over the same
    /// history, with fresh budgets. A `start_bar` past the last bar refuses.
    pub fn start(
        strategy: Strategy,
        risk: RiskEngine,
        grant: Grant,
        quote_config: BacktestConfig,
        runner_config: RunnerConfig,
        bars: Vec<Bar>,
        seed: u64,
        start_bar: usize,
    ) -> Result<Self, ServerError> {
        crate::backtest::ensure_bars_for_session(&runner_config, &quote_config, &bars)?;
        if start_bar > bars.len() {
            return Err(ServerError::Backtest(format!(
                "start_bar {start_bar} is past {} bars",
                bars.len()
            )));
        }
        // The session-scoped venue: today's paper tuning, stated at the call
        // site, with an ephemeral journal the replay alone owns — a replay
        // never writes the live paper journal.
        let venue = BacktestVenue::new(quote_config.clone(), bars, PaperConfig::new().build())?;
        let mut closes = Vec::with_capacity(venue.bars().len());
        for bar in &venue.bars()[..start_bar] {
            if let Some(close) = bar.c.to_f64() {
                closes.push(close);
            }
        }
        let timeframe = Timeframe::infer_or_d1(venue.bars());
        // One instance for the driver's whole session, stepped with the
        // cursor so replay and batch see the same per-run state.
        let signal = strategy
            .bind(&SignalConfig::new(timeframe))
            .map_err(|e| ServerError::Backtest(format!("signal bind refused: {e}")))?;
        let corporate = quote_config.corporate_actions.clone();
        Ok(Self {
            strategy,
            signal,
            risk,
            ledger: GrantLedger::new(grant),
            venue,
            quote_config,
            runner_config,
            journal: Journal::ephemeral(),
            closes,
            timeframe,
            cursor: start_bar,
            decisions: Vec::new(),
            seed,
            skipped_warmup: 0,
            book: PositionBook::new(),
            chain: None,
            chains: ChainPolicy::RequireChains,
            corporate,
            corporate_cursor: 0,
        })
    }

    /// Bars in the replay window.
    #[must_use]
    pub fn total_bars(&self) -> usize {
        self.venue.bars().len()
    }

    /// Index of the next bar [`step`](Self::step) will decide.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Decisions recorded so far, in replay order.
    #[must_use]
    pub fn decisions(&self) -> &[BarDecision] {
        &self.decisions
    }

    /// Warmup bars skipped so far.
    #[must_use]
    pub fn skipped_warmup(&self) -> u64 {
        self.skipped_warmup
    }

    /// Grant budget usage accumulated so far.
    #[must_use]
    pub fn usage(&self) -> GrantUsage {
        self.ledger.usage()
    }

    /// Install the per-leg market data source on the session-scoped venue.
    /// The gateway calls this at open with the replay window's chain slice,
    /// when a chain feed is configured — without it, multi-leg intents keep
    /// refusing fail-closed.
    ///
    /// Installing a source also selects the chain-derived economics arm: a
    /// covered bar then flows past the [`ChainPolicy::RequireChains`] screen
    /// into a *priced* package, whose own max loss and notional are what the
    /// gates check. Config-supplied economics are the caller's constants and
    /// would refuse a leg-attached proposal, so the arm follows the data. An
    /// uncovered bar keeps the named refusal.
    pub fn set_leg_quotes(&mut self, source: std::sync::Arc<dyn protopyne_venue::LegQuoteSource>) {
        self.venue.set_leg_quotes(std::sync::Arc::clone(&source));
        self.chain = Some(source);
        self.runner_config.economics = protopyne_strategy::EconomicsPolicy::ChainDerived;
    }

    /// Decide the next bar through [`advance_bar`] and record the verdict,
    /// or `None` when the cursor stands past the last bar.
    pub async fn step(&mut self) -> Option<BarDecision> {
        let index = self.cursor;
        let bar = self.venue.bars().get(index)?;
        self.cursor += 1;
        let decision = advance_bar(
            BarAdvance {
                strategy: &self.strategy,
                signal: self.signal.as_mut(),
                risk: &self.risk,
                ledger: &self.ledger,
                venue: &self.venue,
                journal: &self.journal,
                permit: None,
                quote_config: &self.quote_config,
                chains: self.chains,
                chain: self.chain.as_deref(),
                bars: self.venue.bars(),
                timeframe: self.timeframe,
                closes: &mut self.closes,
                skipped_warmup: &mut self.skipped_warmup,
                book: &mut self.book,
                runner_config: &self.runner_config,
                seed: self.seed,
                corporate: &self.corporate,
                corporate_cursor: &mut self.corporate_cursor,
                // The replay bodies synthesize quotes from the bar, exactly
                // as the batch loops did.
                quote: BarQuote::Synthesized,
                health: BarHealth::rehearsed(),
                facts: None,
                // Rehearsal declares the producer gates irrelevant at
                // construction: no reading, no halt, byte-identical bars.
                gate: None,
            },
            index,
            bar,
        )
        .await;
        self.decisions.push(decision.clone());
        Some(decision)
    }

    /// Decide up to `count` bars, stopping at the end of history. Answers
    /// the verdicts decided by this call, in replay order.
    pub async fn step_n(&mut self, count: usize) -> Vec<BarDecision> {
        let mut decided = Vec::new();
        for _ in 0..count {
            match self.step().await {
                Some(decision) => decided.push(decision),
                None => break,
            }
        }
        decided
    }
}

/// Keyed store of live replay drivers behind one mutex.
///
/// One operator, potentially several named slots: `start` replaces whatever
/// a slot holds (an explicit restart, never a merge), `remove` releases the
/// slot, and stepping borrows the driver in place so usage, breakers, and
/// the closes prefix persist across gateway calls.
/// Duplicate policy: replace on `start` (explicit restart, never merge).
/// `ids()` returns sorted order via [`KeyedRegistry`].
#[derive(Default)]
pub struct ReplayRegistry {
    sessions: KeyedRegistry<String, ReplayDriver>,
}

impl ReplayRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open (or replace) the driver in `session_id`'s slot.
    pub fn start(&mut self, session_id: String, driver: ReplayDriver) {
        self.sessions.insert(session_id, driver);
    }

    /// Borrow a live driver, if its slot is occupied.
    #[must_use]
    pub fn session(&self, session_id: &str) -> Option<&ReplayDriver> {
        self.sessions.get(session_id)
    }

    /// Borrow a live driver mutably, if its slot is occupied.
    pub fn session_mut(&mut self, session_id: &str) -> Option<&mut ReplayDriver> {
        self.sessions.get_mut(session_id)
    }

    /// Release a slot; answers whether one was occupied.
    pub fn remove(&mut self, session_id: &str) -> bool {
        self.sessions.remove(session_id).is_some()
    }

    /// Slot ids in sorted order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    /// Number of occupied slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no slot is occupied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}
