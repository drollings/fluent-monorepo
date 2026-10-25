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
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use protopyne_store::{CandleStore, ReadRequest as TapeReadRequest, SpanPlan, TapeKey};
use protopyne_types::{Bar, ChainQuote, Money};
use protopyne_venue::{
    AccountEvent, AccountSnapshot, BalanceEntry, CorporateEvent, CorporateEventArgs,
    CorporateOutcome, DryRunOutcome, Fill, LegExecution, LegQuoteSource, OrderIntent, OrderRecord,
    PaperConfig, PaperVenue, Position, Quote, Venue, VenueError,
};

use crate::error::ServerError;

/// `skip_serializing_if` requires a `&bool` predicate; the type is `Copy`.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

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
    /// Corporate-action tape consumed at the bar whose timestamp first
    /// reaches each event's effective instant. Empty keeps configs (and
    /// manifests) byte-identical; non-empty is pinned in the run config.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corporate_actions: Vec<CorporateEventArgs>,
    /// Point-in-time option-chain history the run prices legs off. Empty
    /// keeps configs (and manifests) byte-identical; non-empty installs a
    /// [`HistoricalChainSource`](crate::chain_history::HistoricalChainSource)
    /// on the venue for fills and threads it into the session for
    /// point-in-time selection, and the report pins its coverage.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain_history: Vec<ChainQuote>,
    /// Screen multi-leg proposals on chain coverage in batch: covered bars
    /// flow, uncovered bars refuse naming the missing feed. Unset keeps the
    /// historical unscreened legacy byte-identical. Set without
    /// [`chain_history`](Self::chain_history) refuses the run before the
    /// first bar — a screen that cannot screen is a misconfiguration,
    /// never a quiet all-skip run.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_chains: bool,
}

impl fluent_wvr::validate::Validate for BacktestConfig {
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
            return Err(ServerError::Backtest(
                "data_version must be pinned".to_owned(),
            ));
        }
        if self.quote_spread_bps > 10_000 {
            return Err(ServerError::Backtest(
                "quote_spread_bps must not exceed 10_000 (100% of close)".to_owned(),
            ));
        }
        for action in &self.corporate_actions {
            if action.underlying.trim().is_empty() {
                return Err(ServerError::Backtest(
                    "corporate action must name its underlying".to_owned(),
                ));
            }
            if let Some(contract) = &action.contract {
                if contract.underlying != action.underlying {
                    return Err(ServerError::Backtest(format!(
                        "corporate action contract {} belongs to {}, not {}",
                        contract.occ_symbol(),
                        contract.underlying,
                        action.underlying
                    )));
                }
            }
            match action.event {
                CorporateEvent::Split(0) => {
                    return Err(ServerError::Backtest(
                        "split factor must be new shares per old share, got 0".to_owned(),
                    ));
                }
                CorporateEvent::Assignment | CorporateEvent::Exercise
                    if action.contract.is_none() =>
                {
                    return Err(ServerError::Backtest(format!(
                        "{} needs the identified option contract",
                        action.event.label()
                    )));
                }
                CorporateEvent::Dividend if action.amount.is_none() => {
                    return Err(ServerError::Backtest(
                        "dividend needs the cash amount per share".to_owned(),
                    ));
                }
                _ => {}
            }
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

/// Where a run's bars come from: a file set, a caller-supplied tape, or
/// a durable store.
///
/// The files arm carries CSV or Parquet candidates in fallback order and
/// walks them through the one candidate ladder (a missing or unreadable file
/// skips to the next; any parse or structural refusal stops the walk).
/// The dynamic arm carries an in-memory tape (synthetic fixtures, a
/// live-captured session, a second symbol with no file) with the same
/// fail-closed shape — an empty tape refuses just like an empty candidate
/// set. The tape arm
/// plans and reads a span through a [`CandleStore`], the same read path a
/// chart or a replay uses; it is a **substrate arm**, not a third parsing
/// path, and the store stays outside [`BacktestVenue`]. Every arm loads
/// through the one [`load_history`] entry point, so file, dynamic, and stored
/// histories replay through the identical decide path.
#[derive(Debug, Clone)]
pub enum HistorySource {
    /// File candidates in fallback order (`.csv` or `.parquet` by extension).
    Files(Vec<std::path::PathBuf>),
    /// Caller-supplied bars, replayed as given.
    Dynamic(Vec<Bar>),
    /// A planned read of a durable tape through a [`CandleStore`]. The `key`
    /// names the tape the caller believes it is reading; the read refuses if
    /// the store serves a different tape, so a stale key cannot silently
    /// replay the wrong series.
    Tape {
        /// The tape the caller intends to read.
        key: TapeKey,
        /// The span, spacing, and fold policy to read it with.
        request: TapeReadRequest,
    },
}

/// Bars loaded from a history source, with the planner provenance the tape
/// arm carries. The file and dynamic arms have no planner to name, so their
/// `provenance` is empty; a run over them pins exactly as it did before tape
/// data existed.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedHistory {
    /// Committed bars, ascending by `ts`.
    pub bars: Vec<Bar>,
    /// How every served span was produced, ascending. Empty for the file and
    /// dynamic arms.
    pub provenance: Vec<SpanPlan>,
}

impl LoadedHistory {
    /// A load with no planner provenance (the file and dynamic arms).
    #[must_use]
    pub fn untaped(bars: Vec<Bar>) -> Self {
        Self {
            bars,
            provenance: Vec::new(),
        }
    }
}

/// Load bars from any source arm. Pure dispatch over the established loaders
/// and the store's own read — no third parsing path may hide here.
///
/// The file and dynamic arms ignore `store`; the tape arm requires it and
/// refuses when it is absent, naming the missing collaborator rather than
/// falling back to another source. An empty tape — of any arm — refuses just
/// like an empty candidate set.
pub fn load_history(
    source: &HistorySource,
    store: Option<&dyn CandleStore>,
) -> Result<LoadedHistory, ServerError> {
    match source {
        HistorySource::Files(candidates) => Ok(LoadedHistory::untaped(
            crate::history::load_bars_candidates(candidates)?,
        )),
        HistorySource::Dynamic(bars) => {
            if bars.is_empty() {
                return Err(ServerError::Backtest(
                    "history must not be empty".to_owned(),
                ));
            }
            Ok(LoadedHistory::untaped(bars.clone()))
        }
        HistorySource::Tape { key, request } => {
            let store = store.ok_or_else(|| {
                ServerError::Backtest("tape history needs a store to read from".to_owned())
            })?;
            let series = store
                .read(request)
                .map_err(|error| ServerError::Backtest(format!("tape read refused: {error}")))?;
            if series.key != *key {
                return Err(ServerError::Backtest(format!(
                    "tape read served {:?} for the requested {key:?}",
                    series.key
                )));
            }
            if series.bars.is_empty() {
                return Err(ServerError::Backtest(
                    "history must not be empty".to_owned(),
                ));
            }
            Ok(LoadedHistory {
                bars: series.bars,
                provenance: series.provenance,
            })
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
            return Err(ServerError::Backtest(
                "history must not be empty".to_owned(),
            ));
        }
        let mut inner = PaperVenue::new(paper);
        // The run's own chain facts become its fill source: legs price
        // through the time-parameterized read as of each submission's bar,
        // exactly as an installed replay source would.
        if !config.chain_history.is_empty() {
            inner.set_leg_quotes(Arc::new(crate::chain_history::HistoricalChainSource::new(
                config.chain_history.clone(),
            )));
        }
        Ok(Self {
            config,
            bars,
            inner,
        })
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

    /// Dividend cash booked per underlying by the inner paper book.
    #[must_use]
    pub fn booked_dividends(&self) -> std::collections::HashMap<String, Money> {
        self.inner.booked_dividends()
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

    /// Install the per-leg market data source for spread evaluation on the
    /// inner paper venue. Without a source, multi-leg intents refuse
    /// fail-closed; with one, legs price through the time-parameterized
    /// read as of each submission's bar.
    pub fn set_leg_quotes(&mut self, source: Arc<dyn LegQuoteSource>) {
        self.inner.set_leg_quotes(source);
    }
}

impl Venue for BacktestVenue {
    fn dry_run(&self, intent: &OrderIntent, quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        self.inner.dry_run(intent, quote)
    }

    fn fee_schedule(&self) -> protopyne_venue::FeeSchedule {
        self.inner.fee_schedule()
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

    fn open_spread_positions(&self) -> Option<Vec<protopyne_venue::SpreadPosition>> {
        self.inner.open_spread_positions()
    }

    fn chain_source(&self) -> Option<Arc<dyn protopyne_venue::LegQuoteSource>> {
        self.inner.chain_source()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        self.inner.orders()
    }
    fn apply_corporate_event(
        &self,
        action: &CorporateEventArgs,
    ) -> Result<CorporateOutcome, VenueError> {
        self.inner.apply_corporate_event(action)
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

#[cfg(test)]
#[path = "../tests/history_source_tape.rs"]
mod history_source_tape_tests;

#[cfg(test)]
#[path = "../tests/history_source_backward_compatible.rs"]
mod history_source_backward_compatible_tests;

#[cfg(all(test, feature = "pine"))]
#[path = "../tests/common/mtf.rs"]
mod common_mtf;

#[cfg(all(test, feature = "pine"))]
#[path = "../tests/mtf_fanout.rs"]
mod mtf_fanout_tests;

#[cfg(all(test, feature = "pine"))]
#[path = "../tests/common/golden.rs"]
mod common_golden;

#[cfg(all(test, feature = "pine"))]
#[path = "../tests/pine_integration_lite.rs"]
mod pine_integration_lite_tests;

#[cfg(all(test, feature = "pine"))]
#[path = "../tests/pine_task_indicators.rs"]
mod pine_task_indicators_tests;

#[cfg(all(test, feature = "pine"))]
#[path = "../tests/pine_integration_lite_strategy.rs"]
mod pine_integration_lite_strategy_tests;
