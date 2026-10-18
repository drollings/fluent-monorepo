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
//! per-leg market data the prototype has no chain feed for, and the server
//! path cannot express legs on an intent at all — so under
//! [`ChainPolicy::RequireChains`] the session screens the structure before
//! the shared path could book it, refusing fail-closed with the missing feed
//! named. Singles price off the bar-derived touch and fill honestly, exactly
//! as in batch.
//!
//! Batch divergence, stated once: batch backtest passes
//! [`ChainPolicy::Unscreened`] and still books multi-leg-labeled proposals as
//! single legs (pinned legacy behavior); interactive replay passes
//! [`ChainPolicy::RequireChains`]. The two agree on every unscreened run
//! decision-for-decision (see
//! `stepping_to_completion_reproduces_the_batch_run`).

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive as _;

use protopyne_strategy::{
    BreakerState, Grant, GrantUsage, ProposalSide, ProposeInput, RiskEngine, Strategy,
    StructureKind,
};
use protopyne_types::{Bar, Money};
use protopyne_venue::{Journal, LegQuoteSource, PaperConfig, Recorder, Venue};

use crate::backtest::{BacktestConfig, BacktestVenue};
use crate::error::ServerError;
use crate::runner::{decide_and_submit, skipped_decision, BarDecision, BarHealth, RunnerConfig};

/// Appended to the skip reason of an unsubmitted multi-leg decision. The
/// venue's own refusal stays ahead of it verbatim; this names the missing
/// collaborator (the chain feed) the venue cannot name. Crate-visible so the
/// gateway flags chain-gated verdicts beside the reason, never by parsing it.
pub(crate) const CHAIN_GATE_NOTE: &str =
    "options spreads need an option-chain feed (none configured)";

/// Whether a proposal structure needs per-leg market data to price: every
/// multi-leg shape. Singles price off the bar-derived touch, exactly as in
/// batch. Crate-visible so the shared decide path screens the same shapes
/// the session screens.
pub(crate) fn needs_chain_data(structure: StructureKind) -> bool {
    matches!(
        structure,
        StructureKind::Vertical | StructureKind::Calendar | StructureKind::Condor
    )
}

/// Whether an installed chain source covers `underlying` at `at`: any held
/// quote at-or-before the bar. A coverage fact about the producer's input
/// (validity), never a verdict on the trade — the grant tiers still judge
/// economics unchanged. An empty snapshot refuses; a stale one still
/// selects, and the fill policy's age bound refuses it per leg.
fn chain_covers(chain: Option<&dyn LegQuoteSource>, underlying: &str, at: DateTime<Utc>) -> bool {
    chain.is_some_and(|source| !source.chain_snapshot(underlying, at).is_empty())
}

/// Whether a session may flow multi-leg proposals to the venue without
/// chain data.
///
/// Batch backtest passes [`ChainPolicy::Unscreened`] and preserves its pinned
/// legacy: multi-leg labels book as single legs, exactly as the historical
/// loop booked them. Interactive replay passes
/// [`ChainPolicy::RequireChains`] and refuses them up front. The divergence
/// is deliberate and documented here — not a drift — until a chain feed lets
/// both paths price legs.
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

/// One replayable run over pinned bars, advanced a bar per [`step`](Self::step).
///
/// Borrows the strategy, risk engine, grant, venue, and bars: the batch loops
/// lend all five, and an interactive driver lends the same set across frames.
/// Owns only the run state (grant usage, breaker, closes prefix, cursor,
/// decisions). The journal is ephemeral and scoped to the session — a replay
/// never writes the live paper journal.
pub struct ReplaySession<'a, V> {
    strategy: &'a Strategy,
    risk: &'a RiskEngine,
    grant: &'a Grant,
    usage: GrantUsage,
    breakers: BreakerState,
    venue: &'a V,
    quote_config: BacktestConfig,
    runner_config: RunnerConfig,
    chains: ChainPolicy,
    journal: Journal,
    bars: &'a [Bar],
    closes: Vec<f64>,
    cursor: usize,
    decisions: Vec<BarDecision>,
    seed: u64,
    skipped_warmup: u64,
}

impl<'a, V: Venue> ReplaySession<'a, V> {
    /// Open a session over `bars`: validates the runner and venue configs
    /// and refuses empty history, so a misconfigured replay fails before the
    /// first bar rather than mid-run. `chains` states up front whether
    /// multi-leg proposals may flow without chain data — batch passes
    /// [`ChainPolicy::Unscreened`], interactive replay passes
    /// [`ChainPolicy::RequireChains`].
    pub fn start(
        strategy: &'a Strategy,
        risk: &'a RiskEngine,
        grant: &'a Grant,
        venue: &'a V,
        quote_config: BacktestConfig,
        runner_config: RunnerConfig,
        chains: ChainPolicy,
        bars: &'a [Bar],
        seed: u64,
    ) -> Result<Self, ServerError> {
        runner_config.validate()?;
        quote_config.validate()?;
        if bars.is_empty() {
            return Err(ServerError::Backtest(
                "history must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            strategy,
            risk,
            grant,
            usage: GrantUsage::new().build(),
            breakers: BreakerState::new().build(),
            venue,
            quote_config,
            runner_config,
            chains,
            journal: Journal::ephemeral(),
            bars,
            closes: Vec::with_capacity(bars.len()),
            cursor: 0,
            decisions: Vec::with_capacity(bars.len()),
            seed,
            skipped_warmup: 0,
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
    pub fn usage(&self) -> &GrantUsage {
        &self.usage
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
                risk: self.risk,
                grant: self.grant,
                usage: &mut self.usage,
                venue: self.venue,
                journal: &self.journal,
                quote_config: &self.quote_config,
                breakers: &mut self.breakers,
                chains: self.chains,
                // Borrowed sessions never install a chain source: batch
                // legacy flows exactly as before.
                chain: None,
                closes: &mut self.closes,
                skipped_warmup: &mut self.skipped_warmup,
                runner_config: &self.runner_config,
                seed: self.seed,
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
    pub risk: &'a RiskEngine,
    pub grant: &'a Grant,
    pub usage: &'a mut GrantUsage,
    pub venue: &'a V,
    pub journal: &'a Journal,
    pub quote_config: &'a BacktestConfig,
    pub breakers: &'a mut BreakerState,
    pub chains: ChainPolicy,
    /// Per-leg market data for selection, when installed. The borrowed
    /// session holds `None` (batch legacy, byte-identical); the owned
    /// driver threads its installed source. Never a second fill path —
    /// fills read the venue's own installed source.
    pub chain: Option<&'a dyn LegQuoteSource>,
    pub closes: &'a mut Vec<f64>,
    pub skipped_warmup: &'a mut u64,
    pub runner_config: &'a RunnerConfig,
    pub seed: u64,
}

/// Advance one bar through close representability, warmup honesty, the
/// chain screen, and
/// [`decide_and_submit`](crate::runner::decide_and_submit) over the closes
/// prefix with rehearsed health. The screen proposes with the same input
/// the shared path will use (`propose` is pure over its inputs, so the two
/// calls agree): a signal refusal skips exactly as the shared path would;
/// under [`ChainPolicy::RequireChains`] a multi-leg structure with no chain
/// coverage for the bar skips on the missing chain feed before anything
/// journals or submits, while a covered bar flows into the shared path,
/// which selects legs as of the bar; anything else flows unchanged. Under
/// [`ChainPolicy::Unscreened`] the screen only reproduces signal refusals
/// and every proposal flows, exactly as the historical loop ran.
pub(crate) async fn advance_bar<V: Venue>(
    advance: BarAdvance<'_, V>,
    index: usize,
    bar: &Bar,
) -> BarDecision {
    let proposal_id = format!("run-{}-{index}", advance.seed);
    let ts = bar.ts.to_rfc3339();
    let Some(close) = bar.c.to_f64() else {
        return skipped_decision(index, ts, proposal_id, "close not representable");
    };
    advance.closes.push(close);
    if advance.closes.len() <= advance.runner_config.warmup_bars {
        *advance.skipped_warmup += 1;
        return skipped_decision(index, ts, proposal_id, "warmup");
    }
    let input = ProposeInput {
        strategy: &advance.runner_config.strategy_name,
        underlying: &advance.runner_config.underlying,
        side: ProposalSide::Entry,
        closes: advance.closes,
        limit_price: Money::new(bar.c),
        max_loss: advance.runner_config.max_loss,
        notional: advance.runner_config.notional,
        dte_days: advance.runner_config.dte_days,
        created_at: bar.ts,
    };
    match advance.strategy.propose(&proposal_id, input) {
        Err(error) => skipped_decision(index, ts, proposal_id, &format!("signal refused: {error}")),
        Ok(proposal)
            if advance.chains == ChainPolicy::RequireChains
                && needs_chain_data(proposal.structure)
                && !chain_covers(advance.chain, &advance.runner_config.underlying, bar.ts) =>
        {
            skipped_decision(index, ts, proposal_id, CHAIN_GATE_NOTE)
        }
        Ok(_) => {
            // One ephemeral recorder view per step over the session-scoped
            // journal: the shared submission protocol runs unchanged, and
            // parallel sessions stay isolated.
            let recorder = Recorder::ephemeral(advance.journal);
            decide_and_submit(
                advance.strategy,
                advance.risk,
                advance.grant,
                advance.usage,
                advance.venue,
                &recorder,
                advance.quote_config,
                advance.breakers,
                BarHealth::rehearsed(),
                index,
                bar,
                advance.closes,
                advance.runner_config,
                advance.seed,
                advance.chain,
            )
            .await
        }
    }
}

/// One owned replayable run over pinned bars, advanced a bar per
/// [`step`](Self::step).
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
/// named — never booked as single legs.
pub struct ReplayDriver {
    strategy: Strategy,
    risk: RiskEngine,
    grant: Grant,
    usage: GrantUsage,
    breakers: BreakerState,
    venue: BacktestVenue,
    quote_config: BacktestConfig,
    runner_config: RunnerConfig,
    journal: Journal,
    closes: Vec<f64>,
    cursor: usize,
    decisions: Vec<BarDecision>,
    seed: u64,
    skipped_warmup: u64,
    /// Installed chain source, retained for leg selection. The venue holds
    /// its own clone for fills; this one serves point-in-time snapshots to
    /// the shared decide path. `None` until installed.
    chain: Option<std::sync::Arc<dyn LegQuoteSource>>,
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
        runner_config.validate()?;
        quote_config.validate()?;
        if bars.is_empty() {
            return Err(ServerError::Backtest(
                "history must not be empty".to_owned(),
            ));
        }
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
        Ok(Self {
            strategy,
            risk,
            grant,
            usage: GrantUsage::new().build(),
            breakers: BreakerState::new().build(),
            venue,
            quote_config,
            runner_config,
            journal: Journal::ephemeral(),
            closes,
            cursor: start_bar,
            decisions: Vec::new(),
            seed,
            skipped_warmup: 0,
            chain: None,
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
    pub fn usage(&self) -> &GrantUsage {
        &self.usage
    }

    /// Legs the session-scoped venue executed for one client order, in fill
    /// order. The terminal reads these as display rows through the gateway;
    /// empty for singles, refusals, and orders the venue never saw.
    #[must_use]
    pub fn executed_legs(&self, client_id: &str) -> Vec<protopyne_venue::LegExecution> {
        use protopyne_venue::Venue as _;
        self.venue.executed_legs(client_id)
    }

    /// Install the per-leg market data source on the session-scoped venue.
    /// The gateway calls this at open with the replay window's chain slice,
    /// when a chain feed is configured — without it, multi-leg intents keep
    /// refusing fail-closed. A covered bar flows past the
    /// [`ChainPolicy::RequireChains`] screen into leg selection; an
    /// uncovered bar keeps the named refusal.
    pub fn set_leg_quotes(&mut self, source: std::sync::Arc<dyn protopyne_venue::LegQuoteSource>) {
        self.venue.set_leg_quotes(std::sync::Arc::clone(&source));
        self.chain = Some(source);
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
                risk: &self.risk,
                grant: &self.grant,
                usage: &mut self.usage,
                venue: &self.venue,
                journal: &self.journal,
                quote_config: &self.quote_config,
                breakers: &mut self.breakers,
                chains: ChainPolicy::RequireChains,
                chain: self.chain.as_deref(),
                closes: &mut self.closes,
                skipped_warmup: &mut self.skipped_warmup,
                runner_config: &self.runner_config,
                seed: self.seed,
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
#[derive(Default)]
pub struct ReplayRegistry {
    sessions: std::collections::HashMap<String, ReplayDriver>,
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
}
