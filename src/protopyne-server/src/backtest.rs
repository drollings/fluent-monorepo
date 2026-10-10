//! Versioned-history venue behind the same trait as paper.
//!
//! [`BacktestVenue`] implements [`Venue`](protopyne_venue::Venue) over a
//! pinned bar history. Fill behavior is not reimplemented here: every dry
//! run, submit, query, cancel, and corporate-action call delegates to an
//! inner [`PaperVenue`](protopyne_venue::PaperVenue), so a divergence between
//! backtest and paper fills is a bug, not a feature. The venue additionally
//! pins its `data_version` and synthesizes two-sided quotes from closed-bar
//! closes with an explicit, reported spread assumption.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use protopyne_types::{Bar, Money};
use protopyne_venue::{AccountEvent, AccountSnapshot, BalanceEntry, CorporateEvent, DryRunOutcome, Fill, LegExecution, OrderIntent, OrderRecord, PaperConfig, PaperVenue, Position, Quote, Venue, VenueError};

use crate::csv::load_bars_csv;
use crate::error::ServerError;

/// How quotes are derived from closed-bar closes in the prototype.
///
/// The mid equals the bar close; the spread is `close * spread_bps / 10_000`
/// split symmetrically around the mid. Sizes are fixed. This is a stated
/// fill assumption surfaced in every report, not a claim about real
/// liquidity.
#[derive(Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize)]
#[builder(start_fn = new)]
pub struct BacktestConfig {
    /// Symbol this history belongs to.
    pub underlying: String,
    /// Pinned history identifier (file hash, dataset tag, ...).
    pub data_version: String,
    /// Spread used to synthesize quotes, in basis points of close.
    #[builder(default = 5)]
    pub quote_spread_bps: u32,
    /// Quoted size on each side of the synthetic touch.
    #[builder(default = 100)]
    pub quote_size: u64,
}

impl protopyne_types::Validate for BacktestConfig {
    type Err = ServerError;
    fn validate(&self) -> Result<(), Self::Err> {
        BacktestConfig::validate(self)
    }
}

impl BacktestConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), ServerError> {
        if self.underlying.trim().is_empty() {
            return Err(ServerError::Backtest("underlying must be named".to_owned()));
        }
        if self.data_version.trim().is_empty() {
            return Err(ServerError::Backtest("data_version must be pinned".to_owned()));
        }
        if self.quote_spread_bps > 10_000 {
            return Err(ServerError::Backtest(
                "quote_spread_bps must not exceed 10_000 (100% of close)".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Synthesize a two-sided quote for `bar` under `config`.
///
/// Returns `None` (fail closed, bar skipped with a reason) when the close is
/// non-positive or the derived touch would be crossed.
#[must_use]
pub fn quote_for_bar(bar: &Bar, config: &BacktestConfig) -> Option<Quote> {
    if config.quote_spread_bps > 10_000 {
        return None;
    }
    if bar.c <= Decimal::ZERO {
        return None;
    }
    let basis = Decimal::from(10_000);
    let spread = bar.c * Decimal::from(config.quote_spread_bps) / basis;
    let half = spread / Decimal::from(2);
    let bid = bar.c - half;
    let ask = bar.c + half;
    if bid <= Decimal::ZERO || ask <= bid {
        return None;
    }
    Some(
        Quote::new()
            .bid(Money::new(bid))
            .ask(Money::new(ask))
            .bid_size(config.quote_size)
            .ask_size(config.quote_size)
            .ts(bar.ts)
            .build(),
    )
}

/// Load the first history file that parses, trying `candidates` in order.
///
/// A missing/unreadable file is skipped; a file with a bad header stops the
/// walk (reconfiguration is required, not another guess); a bad row is
/// skipped to the next candidate. Exhaustion without a usable file is an
/// error. Composes the shared first-accept combinator rather than
/// hand-rolling the fallback loop.
pub fn load_bars_with_fallback(candidates: &[PathBuf]) -> Result<Vec<Bar>, ServerError> {
    let outcome = fluent_concurrency::ladder::first_accept_in_order_sync(
        candidates.iter().cloned(),
        |path: PathBuf| match load_bars_csv(path.as_path()) {
            Ok(bars) => Ok(Some(bars)),
            Err(ServerError::BadRow { .. } | ServerError::Io(_)) => Ok(None),
            Err(other) => Err(other),
        },
        |e: &ServerError| matches!(e, ServerError::BadHeader(_)),
    );
    match outcome {
        Ok(Some(bars)) => Ok(bars),
        Ok(None) => Err(ServerError::Backtest("no candidate history file parsed".to_owned())),
        Err(e) => Err(e),
    }
}

/// Load history from exactly one path (no fallback).
pub fn load_bars_single(path: &Path) -> Result<Vec<Bar>, ServerError> {
    load_bars_csv(path)
}

/// Where a run's bars come from: a CSV file set or a caller-supplied tape.
///
/// The CSV arm keeps the exact `load_bars_with_fallback` semantics
/// (first-usable-wins, `BadHeader` stops, `BadRow`/`Io` skips); the dynamic
/// arm carries an in-memory tape (synthetic fixtures, a live-captured
/// session, a second symbol with no CSV) with the same fail-closed shape —
/// an empty tape refuses just like an empty candidate set. The harness loads
/// either arm interchangeably, so file and dynamic histories replay through
/// the identical decide path.
#[derive(Debug, Clone)]
pub enum HistorySource {
    /// CSV candidates in fallback order.
    Csv(Vec<PathBuf>),
    /// Caller-supplied bars, replayed as given.
    Dynamic(Vec<Bar>),
}

/// Load bars from either source arm. Pure dispatch over the two established
/// loaders — no third parsing path may hide here.
pub fn load_history(source: &HistorySource) -> Result<Vec<Bar>, ServerError> {
    match source {
        HistorySource::Csv(candidates) => load_bars_with_fallback(candidates),
        HistorySource::Dynamic(bars) => {
            if bars.is_empty() {
                return Err(ServerError::Backtest("history must not be empty".to_owned()));
            }
            Ok(bars.clone())
        }
    }
}

/// A venue over pinned history sharing paper's fill code.
///
/// Construction pins `(bars, data_version)` together; every [`Venue`] call
/// delegates to the inner paper venue. History accessors expose the version
/// and bar range for the run manifest.
pub struct BacktestVenue {
    config: BacktestConfig,
    bars: Vec<Bar>,
    inner: PaperVenue,
}

impl BacktestVenue {
    /// Pin `bars` to `config` over explicitly tuned paper fills. The tuning
    /// (fees, slippage, liquidity filter) is a stated assumption, not a
    /// hidden default: callers that want today's values pass
    /// `PaperConfig::new().build()` and say so.
    pub fn new(
        config: BacktestConfig,
        bars: Vec<Bar>,
        paper: PaperConfig,
    ) -> Result<Self, ServerError> {
        config.validate()?;
        if bars.is_empty() {
            return Err(ServerError::Backtest("history must not be empty".to_owned()));
        }
        Ok(Self { config, bars, inner: PaperVenue::new(paper) })
    }

    /// Borrow the venue configuration (pin + quote assumptions).
    #[must_use]
    pub fn config(&self) -> &BacktestConfig {
        &self.config
    }

    /// Pinned history identifier for the run manifest.
    #[must_use]
    pub fn data_version(&self) -> &str {
        &self.config.data_version
    }

    /// Borrow the pinned bar history.
    #[must_use]
    pub fn bars(&self) -> &[Bar] {
        &self.bars
    }

    /// First bar timestamp, for the manifest range.
    #[must_use]
    pub fn first_ts(&self) -> DateTime<Utc> {
        self.bars[0].ts
    }

    /// Last bar timestamp, for the manifest range.
    #[must_use]
    pub fn last_ts(&self) -> DateTime<Utc> {
        self.bars[self.bars.len() - 1].ts
    }
}

impl Venue for BacktestVenue {
    fn dry_run(&self, intent: &OrderIntent, quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        self.inner.dry_run(intent, quote)
    }

    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        quote: &'a Quote,
        now: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        self.inner.submit(intent, quote, now)
    }

    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<OrderRecord>> + Send + 'a>> {
        self.inner.query(client_id)
    }

    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        now: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        self.inner.cancel(client_id, now)
    }

    fn positions(&self) -> Vec<Position> {
        self.inner.positions()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        self.inner.orders()
    }

    fn apply_corporate_event(
        &self,
        underlying: &str,
        event: CorporateEvent,
        now: DateTime<Utc>,
    ) -> Result<String, VenueError> {
        self.inner.apply_corporate_event(underlying, event, now)
    }

    fn account_snapshot(&self) -> AccountSnapshot {
        self.inner.account_snapshot()
    }

    fn account_stream(&self) -> Vec<AccountEvent> {
        self.inner.account_stream()
    }

    fn fetch_balances(&self) -> Vec<BalanceEntry> {
        self.inner.fetch_balances()
    }

    fn fetch_open_orders(&self) -> Vec<OrderRecord> {
        self.inner.fetch_open_orders()
    }

    fn fetch_trades(&self) -> Vec<Fill> {
        self.inner.fetch_trades()
    }

    fn executed_legs(&self, client_id: &str) -> Vec<LegExecution> {
        self.inner.executed_legs(client_id)
    }
}

#[cfg(test)]
#[path = "../tests/backtest.rs"]
mod tests;
