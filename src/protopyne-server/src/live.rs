//! Live loop authority and driver: journaled arm + loop-start first, then the
//! one bar body.
//!
//! Boot alone places nothing. [`loop_gate`] folds the journaled authority — an
//! unrevoked [`Grant`] (an arm) *and* a `command.start_loop` row — and
//! [`run_live_loop`] refuses to step a single bar until both exist. Once they
//! do, the loop runs through the same [`advance_bar`](crate::replay_session)
//! body as batch and replay, recording intents and transitions durably through
//! the live journal's permit.

use std::sync::Arc;
use std::time::Duration;

use protopyne_strategy::{Grant, RiskEngine, Strategy};
use protopyne_types::{Bar, Timeframe};
use protopyne_venue::{
    loop_started, replay_grant, Journal, JournalEvent, LiveSession, QuoteSource, VenueStorage,
};

use crate::backtest::BacktestConfig;
use crate::error::ServerError;
use crate::replay_session::ChainPolicy;
use crate::runner::{BarDecision, RunnerConfig};
use crate::trading_loop::TradingLoopExt;

/// Whether the journaled authority permits the loop to start.
#[derive(Debug, Clone, PartialEq)]
pub enum LoopGate {
    /// No live (unrevoked) grant is journaled.
    Unarmed,
    /// A grant is live, but no loop-start command is journaled.
    NotStarted,
    /// Both are journaled: the loop may run under this grant.
    Ready(Box<Grant>),
}

/// Fold the journaled authority. Boot alone is never enough: the operator must
/// arm a grant and then journal an explicit start.
pub fn loop_gate(journal: &Journal, permit: &VenueStorage) -> Result<LoopGate, ServerError> {
    let replayed = journal
        .replay(permit)
        .map_err(|error| ServerError::Service(format!("journal replay refused: {error}")))?;
    Ok(gate_of(&replayed.events))
}

/// The pure fold behind [`loop_gate`], over already-replayed events.
#[must_use]
pub fn gate_of(events: &[JournalEvent]) -> LoopGate {
    match replay_grant(events).filter(|grant| !grant.revoked) {
        None => LoopGate::Unarmed,
        Some(_) if !loop_started(events) => LoopGate::NotStarted,
        Some(grant) => LoopGate::Ready(Box::new(grant)),
    }
}

/// One live loop's inputs: the bar feed, the quote ladder, and the decision
/// stack the session steps.
pub struct LiveLoop<'a> {
    /// Bars the session feeds the loop, in order.
    pub bars: Vec<Bar>,
    /// Quote sources the loop prices against; empty is the rehearsal policy.
    pub sources: Vec<Arc<dyn QuoteSource>>,
    /// Strategy the loop proposes from.
    pub strategy: &'a Strategy,
    /// Mandatory risk engine.
    pub risk: &'a RiskEngine,
    /// Quote synthesis config (fill model) for resolved quotes.
    pub quote_config: &'a BacktestConfig,
    /// Sizing and attribution.
    pub runner_config: &'a RunnerConfig,
    /// Bar spacing of the feed.
    pub timeframe: Timeframe,
    /// Chain policy for the feed.
    pub chains: ChainPolicy,
    /// Pacing between bars; zero runs unpaced.
    pub cadence: Duration,
    /// Seed for deterministic proposal ids.
    pub seed: u64,
}

/// What a start attempt did.
#[derive(Debug)]
pub enum LiveLoopOutcome {
    /// Authority was absent: not one bar was decided.
    Refused(LoopGate),
    /// Authority was present but a supervised producer was absent: not one
    /// bar was decided. Names the missing producers in pipeline order.
    MissingProducers(Vec<String>),
    /// The loop ran to feed exhaustion, returning every bar's verdict.
    Ran(Vec<BarDecision>),
}

/// Start the live loop only when the journaled authority permits it, then run
/// the shared bar body to feed exhaustion. Authority comes first (an
/// unarmed boot reports its gate, never its producers); a missing producer
/// set refuses next, before a single bar is decided. A refusal is a normal
/// outcome — the caller prints it and exits — never an implicit start.
pub async fn run_live_loop(
    live: &mut LiveSession,
    config: LiveLoop<'_>,
) -> Result<LiveLoopOutcome, ServerError> {
    let gate = loop_gate(&live.journal, &live.storage)?;
    let LoopGate::Ready(grant) = gate else {
        return Ok(LiveLoopOutcome::Refused(gate));
    };
    let missing = live.missing_producers();
    if !missing.is_empty() {
        return Ok(LiveLoopOutcome::MissingProducers(missing));
    }
    let mut feed = live.session.spawn_feed(config.bars, 64);
    let decisions = live
        .session
        .run_trading_loop(
            &mut feed,
            config.sources,
            config.strategy,
            config.risk,
            grant.as_ref(),
            config.quote_config,
            config.runner_config,
            config.timeframe,
            config.chains,
            config.cadence,
            config.seed,
            Some((&live.journal, &live.storage)),
        )
        .await?;
    Ok(LiveLoopOutcome::Ran(decisions))
}

#[cfg(test)]
#[path = "../tests/live_loop.rs"]
mod tests;
