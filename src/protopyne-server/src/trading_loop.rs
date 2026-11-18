//! The live trading loop: a session-driven, feed-fed driver that advances the
//! *same* per-bar body the replay drivers use.
//!
//! Implemented as an extension trait on the venue's [`TradingSession`] so the
//! call reads `session.run_trading_loop(...)` while the bar body
//! ([`advance_bar`]) stays in this crate: the venue owns the session, its
//! venue, the feed, and quote resolution; the server owns the decision stack.
//! One body, three drivers (batch, replay, live) — a divergence between them
//! is a bug, never a modeling choice.
//!
//! Quote policy: an empty `sources` list is the rehearsal policy (each bar's
//! quote is synthesized from its close, byte-identical to the batch loops); a
//! configured ladder prices against the newest quote a source resolved, with
//! the quote's age becoming the producer-health reading; a dark ladder skips
//! the bar after the signal advances, never inventing a price.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use fluent_wvr::validate::Validate;
use protopyne_strategy::{Grant, GrantLedger, RiskEngine, SignalConfig, Strategy};
use protopyne_types::{Bar, Timeframe};
use protopyne_venue::{GateReading, Journal, QuoteSource, TradingSession};

use crate::backtest::BacktestConfig;
use crate::error::ServerError;
use crate::operator::execute_operator_commands;
use crate::replay_session::{advance_bar, BarAdvance, BarQuote, ChainPolicy};
use crate::runner::{bar_quote, BarDecision, BarHealth, RunnerConfig};

/// Feed-driven counterpart of the replay drivers. Implemented here rather than
/// in the venue because the bar body is a server type; the method still reads
/// as a session method at the call site.
#[allow(async_fn_in_trait)]
pub trait TradingLoopExt {
    /// Drive one loop to completion: consume `feed` to exhaustion, resolve a
    /// quote per bar, advance the shared body, and pace to `cadence`. Returns
    /// every bar's verdict, in feed order. A `cadence` of zero runs unpaced.
    #[allow(clippy::too_many_arguments)]
    async fn run_trading_loop(
        &self,
        feed: &mut mpsc::Receiver<Bar>,
        sources: Vec<Arc<dyn QuoteSource>>,
        strategy: &Strategy,
        risk: &RiskEngine,
        grant: &Grant,
        quote_config: &BacktestConfig,
        runner_config: &RunnerConfig,
        timeframe: Timeframe,
        chains: ChainPolicy,
        cadence: Duration,
        seed: u64,
        authority: Option<(&Journal, &protopyne_venue::VenueStorage)>,
    ) -> Result<Vec<BarDecision>, ServerError>;
}

impl TradingLoopExt for TradingSession {
    async fn run_trading_loop(
        &self,
        feed: &mut mpsc::Receiver<Bar>,
        sources: Vec<Arc<dyn QuoteSource>>,
        strategy: &Strategy,
        risk: &RiskEngine,
        grant: &Grant,
        quote_config: &BacktestConfig,
        runner_config: &RunnerConfig,
        timeframe: Timeframe,
        chains: ChainPolicy,
        cadence: Duration,
        seed: u64,
        authority: Option<(&Journal, &protopyne_venue::VenueStorage)>,
    ) -> Result<Vec<BarDecision>, ServerError> {
        runner_config.validate()?;
        quote_config.validate()?;
        // One signal instance for the whole loop, bound once: a stateful
        // signal advances exactly once per bar, exactly as the replay drivers.
        let mut signal = strategy
            .bind(&SignalConfig::new(timeframe))
            .map_err(|error| ServerError::Backtest(format!("signal bind refused: {error}")))?;
        let ledger = GrantLedger::new(grant.clone());
        // A live caller supplies its durable journal + permit; a rehearsal
        // loop owns a private ephemeral one and records exactly as before.
        let fallback;
        let (journal, permit) = if let Some((journal, permit)) = authority {
            (journal, Some(permit))
        } else {
            fallback = Journal::ephemeral();
            (&fallback, None)
        };
        let corporate = quote_config.corporate_actions.clone();
        let mut corporate_cursor = 0usize;
        // The bar history grows as the feed delivers: each bar's `MarketData`
        // is the prefix ending at it, so a signal never sees a future bar.
        let mut history: Vec<Bar> = Vec::new();
        let mut closes: Vec<f64> = Vec::new();
        let mut skipped_warmup = 0u64;
        let mut book = crate::replay_session::PositionBook::new();
        let mut decisions = Vec::new();
        // Last executed operator-command seq: audit rows carry their
        // command's seq, so a restart never re-executes a command.
        let mut commands_done: i64 = 0;
        while let Some(bar) = feed.recv().await {
            history.push(bar.clone());
            let index = history.len() - 1;
            // The session's supervised units run alongside the loop: stream
            // health, trading state, and the attached reconcile/backfill
            // sensors observe into the same handles the reading below
            // consumes, so a trip gates this bar. Failures cancel through
            // the batch graph; the loop reads the gates, never the summary.
            // A session with no sensors runs an empty batch here, exactly as
            // before.
            let _ = self.run_bar(Vec::new()).await;
            // The session's producer-confidence reading, assembled fresh
            // every bar from the handles it owns: a trip halts entries below
            // while exits still route through the submission path.
            let reading = self.gate_reading();
            let (quote, health) =
                loop_quote(&sources, &runner_config.underlying, &bar, &reading).await;
            // Operator commands journaled since the last bar execute here,
            // outside the entry gate: a halted book still closes and cancels
            // while entries refuse. A dark bar holds commands over (no price
            // to rest closes at) instead of failing them.
            if let Some((journal, permit)) = authority {
                let command_quote = match &quote {
                    BarQuote::Synthesized => bar_quote(&bar, quote_config).ok(),
                    BarQuote::Resolved(resolved) => Some(*resolved),
                    BarQuote::Dark => None,
                };
                if let Some(command_quote) = command_quote {
                    let command_state = protopyne_strategy::RiskState {
                        quote_age_secs: health.quote_age_secs,
                        clock_skew_secs: 0,
                        reconciliation_healthy: health.reconciliation_healthy,
                        facts: self.portfolio_facts().map_or_else(
                            protopyne_strategy::PortfolioFacts::unobserved,
                            |producer| producer.facts(),
                        ),
                    };
                    commands_done = execute_operator_commands(
                        self,
                        journal,
                        permit,
                        &ledger,
                        risk,
                        &command_state,
                        &command_quote,
                        bar.ts,
                        commands_done,
                    )
                    .await
                    .map_err(|error| {
                        ServerError::Service(format!("operator command refused: {error}"))
                    })?;
                }
            }
            let decision = advance_bar(
                BarAdvance {
                    strategy,
                    signal: signal.as_mut(),
                    risk,
                    ledger: &ledger,
                    venue: self.registry().as_ref(),
                    journal,
                    permit,
                    quote_config,
                    chains,
                    chain: None,
                    bars: &history,
                    timeframe,
                    closes: &mut closes,
                    skipped_warmup: &mut skipped_warmup,
                    book: &mut book,
                    runner_config,
                    seed,
                    corporate: &corporate,
                    corporate_cursor: &mut corporate_cursor,
                    quote,
                    health,
                    // The session's producer, when one is attached: the
                    // aggregate gates read the book every bar. No producer
                    // means no reading, exactly as before.
                    facts: self.portfolio_facts().map(|producer| producer.facts()),
                    gate: Some(reading),
                },
                index,
                &bar,
            )
            .await;
            decisions.push(decision);
            if !cadence.is_zero() {
                // NOT-A-BACKOFF (M15): pacing cadence, not retry backoff.
                tokio::time::sleep(cadence).await;
            }
        }
        Ok(decisions)
    }
}

/// Resolve this bar's quote policy and health reading. No configured sources
/// is the rehearsal policy; a ladder that answers prices against its newest
/// quote with the quote's age as the health fact; a dark ladder fails closed.
/// The reconciliation bit is sourced from the session's reading, never
/// invented: a trip reports unhealthy here while the bar-level halt below
/// carries the named refusal.
async fn loop_quote(
    sources: &[Arc<dyn QuoteSource>],
    underlying: &str,
    bar: &Bar,
    reading: &GateReading,
) -> (BarQuote, BarHealth) {
    if sources.is_empty() {
        return (BarQuote::Synthesized, BarHealth::rehearsed());
    }
    match TradingSession::resolve_quote(sources.to_vec(), underlying).await {
        Some((quote, _source)) => {
            let age = (bar.ts - quote.ts).num_seconds().max(0) as u64;
            (
                BarQuote::Resolved(quote),
                BarHealth {
                    quote_age_secs: age,
                    clock_skew_secs: 0,
                    reconciliation_healthy: reading.reconcile.allows_entry,
                },
            )
        }
        None => (BarQuote::Dark, BarHealth::rehearsed()),
    }
}

#[cfg(test)]
#[path = "../tests/trading_loop.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/live_loop_baseline.rs"]
mod live_loop_baseline_tests;
