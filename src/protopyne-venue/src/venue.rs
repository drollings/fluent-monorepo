//! The `Venue` trait: one code path for paper now, sandbox later.
//!
//! Backtest implements this same trait over versioned history through the
//! real fill code, so a divergence between paper and backtest is a bug, not
//! a feature. Consumers stay generic over `V: Venue`; there is no paper-only
//! order path.

use crate::error::VenueError;
use crate::order::{
    CorporateEventArgs, CorporateOutcome, Fill, OrderIntent, OrderRecord, OrderSide, Position,
};
use crate::position::SpreadPosition;
use crate::quote::{FeeSchedule, Quote};
use crate::tastytrade::TransportError;
use chrono::{DateTime, Utc};
use fluent_concurrency::io::net::NetCapability;
use protopyne_types::{ChainQuote, ContractId, OptionContract};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Outcome of a pre-submission dry run: margin and sanity checks against
/// current buying power, before anything is journaled as submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DryRunOutcome {
    /// Whether submission may proceed.
    pub allowed: bool,
    /// Committed notional the dry run reserved.
    pub reserved_notional: protopyne_types::Money,
}

/// One cash balance line: an asset and its venue-reported amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceEntry {
    /// Asset or currency code (e.g. `USD`).
    pub asset: String,
    /// Venue-reported amount.
    pub amount: protopyne_types::Money,
}

/// The whole account view at one instant: balances, working orders, recent
/// fills.
///
/// Update semantics are snapshot-replace: receiving a snapshot discards the
/// previous view entirely. An empty snapshot reads as "no account state",
/// never as "unknown" — a venue with nothing to report returns the default.
/// Incremental changes arrive as [`AccountEvent`]s, never as partial
/// snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    /// Cash balances by asset.
    #[serde(default)]
    pub balances: Vec<BalanceEntry>,
    /// Working orders.
    #[serde(default)]
    pub open_orders: Vec<OrderRecord>,
    /// Recent fills.
    #[serde(default)]
    pub recent_trades: Vec<Fill>,
}

impl AccountSnapshot {
    /// `true` when every leg is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.balances.is_empty() && self.open_orders.is_empty() && self.recent_trades.is_empty()
    }
}

/// One incremental account update.
///
/// Update semantics are upsert: each event carries one entry keyed by its
/// natural key — orders by client ID, balances by asset — replacing any
/// prior entry under that key. Trades are append-only: each [`Fill`](crate::order::Fill)
/// is a new execution, never an update to an earlier one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountEvent {
    /// One balance line changed; replaces the line for the same asset.
    BalanceUpdated(BalanceEntry),
    /// One order changed; replaces the order with the same client ID.
    OrderUpdated(OrderRecord),
    /// One new execution; appended, never merged.
    Trade(Fill),
}

/// One executed spread leg: which contract filled, in which direction,
/// how many contracts, at what slipped price, for what commission.
/// Journaled per leg (see `LEG_FILL_KIND`) so replay and reconciliation see
/// the same atomic package the venue executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegExecution {
    /// Contract this leg filled in, as its OCC symbol.
    pub contract: ContractId,
    /// Fill direction: buys open long legs, sells open short legs.
    pub side: OrderSide,
    /// Contracts filled on this leg.
    pub quantity: u32,
    /// Slipped execution price per contract.
    pub price: protopyne_types::Money,
    /// Commission on this leg.
    pub fee: protopyne_types::Money,
}

/// Per-leg market data for spread evaluation. The paper venue owns no feed;
/// callers inject the source (tests stub it, the transport will serve it).
/// A spread leg without a quote refuses fail-closed — legs never fall back
/// to the single-underlying quote, which would misprice them.
///
/// The time-parameterized [`leg_quote_at`](Self::leg_quote_at) is the
/// history-capable read: a source over point-in-time chains answers the
/// newest quote at-or-before `at`, so a replay prices legs as of the bar it
/// decides. The default refuses (`None`): a partial history source that
/// serves [`chain_snapshot`](Self::chain_snapshot) but no leg read must not
/// silently price the newest quote as if it were the bar's. A newest-only
/// source opts into the newest read explicitly through
/// [`newest_ok`](Self::newest_ok) — the opt-in, not the silence, is what a
/// live fill prices.
pub trait LegQuoteSource: Send + Sync {
    /// Newest quote for `contract`, or `None` when this source has none.
    fn leg_quote(&self, contract: &OptionContract) -> Option<Quote>;

    /// Whether the newest quote may stand in for the bar's quote. `false`
    /// unless the source declares otherwise: history-shaped sources answer
    /// time-parameterized reads instead.
    fn newest_ok(&self) -> bool {
        false
    }

    /// Newest quote for `contract` at-or-before `at`, or `None` when this
    /// source holds none. Historical sources override this; a source that
    /// leaves the default answers newest only when it opted in through
    /// [`newest_ok`](Self::newest_ok), and refuses otherwise.
    fn leg_quote_at(&self, contract: &OptionContract, at: DateTime<Utc>) -> Option<Quote> {
        if self.newest_ok() {
            self.leg_quote(contract)
        } else {
            let _ = at;
            None
        }
    }

    /// Whether this source holds any quote for `underlying` at-or-before
    /// `at`: the cheap coverage predicate behind the chain screen. Sources
    /// answer without cloning the window; the default reads the snapshot,
    /// so every existing source keeps its answer.
    fn covers(&self, underlying: &str, at: DateTime<Utc>) -> bool {
        !self.chain_snapshot(underlying, at).is_empty()
    }

    /// Point-in-time snapshot for `underlying`: every held quote stamped
    /// at-or-before `at`, in source order. Leg selection reads this — the
    /// caller never sees the future, so selection cannot borrow it either.
    /// Defaults to empty (a newest-only source cannot serve history
    /// honestly); historical sources override. Object-safe: no generics,
    /// no `Self` returns.
    fn chain_snapshot(&self, underlying: &str, at: DateTime<Utc>) -> Vec<ChainQuote> {
        let _ = (underlying, at);
        Vec::new()
    }
}

/// One authenticated chain-snapshot fetch over a venue transport. The only
/// thing the server's chain fetcher needs from a venue: given credentials,
/// return the chain window. Transports implement this; callers stay generic,
/// so no server, view, or terminal code names a venue's concrete transport.
/// A future `IbAdapter` implements this same surface over `EClientSocket`.
pub trait ChainSnapshotSource: Send + Sync {
    /// Fetch one chain snapshot for `underlying`.
    fn fetch_chain_snapshot<'a>(
        &'a self,
        net: &'a NetCapability,
        underlying: &'a str,
        token: &'a str,
    ) -> ChainSnapshotFuture<'a>;
}

/// Boxed future for [`ChainSnapshotSource`]: object-safe like the rest of
/// the venue boundary.
pub type ChainSnapshotFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ChainQuote>, TransportError>> + Send + 'a>>;

/// One code path for every venue. Implementations are `Send + Sync` so fan-out
/// pools can share them; interior mutability lives inside each venue.
///
/// The async methods return boxed futures (not `impl Future`) so the trait
/// is object-safe: routing holds `Arc<dyn Venue>` handles without a
/// per-venue `match`. Callers still `.await` them exactly as before.
///
/// The read methods come in sync/async pairs: the sync method is the
/// contract (tests, mirrors, sync callers), the async method is the
/// production path. Async defaults delegate to the sync method, so every
/// implementor and test double compiles unchanged; venues whose reads dial
/// override the async method and answer natively, so the async submission
/// protocol never bridges runtimes.
pub trait Venue: Send + Sync {
    /// Check buying power and order sanity without touching the market.
    /// Pure against the venue's current books; never mutates.
    fn dry_run(&self, intent: &OrderIntent, quote: &Quote) -> Result<DryRunOutcome, VenueError>;

    /// Async dry-run read: the submission protocol's path. Defaults to the
    /// sync method; venues over network truth override and dial directly.
    fn dry_run_async<'a>(
        &'a self,
        intent: &'a OrderIntent,
        quote: &'a Quote,
    ) -> Pin<Box<dyn Future<Output = Result<DryRunOutcome, VenueError>> + Send + 'a>> {
        let outcome = self.dry_run(intent, quote);
        Box::pin(std::future::ready(outcome))
    }

    /// Commission schedule this venue applies. Defaults to zero so a venue
    /// without a fee model stays honest rather than inventing a charge;
    /// paper and backtest return their configured schedule.
    fn fee_schedule(&self) -> FeeSchedule {
        FeeSchedule::new()
            .per_contract(protopyne_types::Money::zero())
            .build()
    }

    /// Submit an intent. Idempotent per client ID: resubmitting a known ID
    /// returns the existing record, never a second order.
    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        quote: &'a Quote,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>>;

    /// Look up an order by its deterministic client ID.
    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<OrderRecord>> + Send + 'a>>;

    /// Cancel a working order. Terminal orders are returned unchanged.
    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>>;

    /// Current venue-truth positions.
    fn positions(&self) -> Vec<Position>;

    /// Open per-contract spread positions, when the venue tracks them.
    /// `None` (the default) means the venue cannot report them, so an
    /// aggregate open-risk cap fails closed rather than reading an invented
    /// zero.
    fn open_spread_positions(&self) -> Option<Vec<SpreadPosition>> {
        None
    }

    /// The option-chain quote source attached to this venue, when one
    /// exists. Aggregate greek caps read their point-in-time greeks here;
    /// without a source those caps report unobserved.
    fn chain_source(&self) -> Option<Arc<dyn LegQuoteSource>> {
        None
    }

    /// Current venue-truth orders.
    fn orders(&self) -> Vec<OrderRecord>;

    /// Apply one explicit corporate action to venue truth. Returns what the
    /// venue booked (closed option legs, generated stock leg, dividend cash,
    /// affected contracts, and whether reconciliation is due); the caller
    /// journals it. Assignment/exercise never delete the net position.
    fn apply_corporate_event(
        &self,
        action: &CorporateEventArgs,
    ) -> Result<CorporateOutcome, VenueError>;

    /// Whole account view assembled from the fetch methods below. The
    /// default composes the three fetches, so an implementor that overrides
    /// one rung carries it through the snapshot without further code.
    /// Venues with no account surface keep the empty default untouched.
    fn account_snapshot(&self) -> AccountSnapshot {
        AccountSnapshot {
            balances: self.fetch_balances(),
            open_orders: self.fetch_open_orders(),
            recent_trades: self.fetch_trades(),
        }
    }

    /// Async whole-account read: the async path. Defaults to the sync
    /// snapshot; venues over network truth override and dial directly.
    fn account_snapshot_async<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = AccountSnapshot> + Send + 'a>> {
        let snapshot = self.account_snapshot();
        Box::pin(std::future::ready(snapshot))
    }

    /// Pending incremental account events, drained by the caller. Empty by
    /// default: venues without an account stream report nothing rather than
    /// an error, and reconciliation-grade truth stays on [`positions`](Self::positions)
    /// and [`orders`](Self::orders).
    fn account_stream(&self) -> Vec<AccountEvent> {
        Vec::new()
    }

    /// Cash balances. Empty by default.
    fn fetch_balances(&self) -> Vec<BalanceEntry> {
        Vec::new()
    }

    /// Async cash-balance read. Defaults to the mirror; venues over network
    /// truth override and dial directly.
    fn fetch_balances_async<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Vec<BalanceEntry>> + Send + 'a>> {
        let balances = self.fetch_balances();
        Box::pin(std::future::ready(balances))
    }

    /// Working orders as the account reports them. Empty by default.
    fn fetch_open_orders(&self) -> Vec<OrderRecord> {
        Vec::new()
    }

    /// Async working-order read. Defaults to the mirror.
    fn fetch_open_orders_async<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Vec<OrderRecord>> + Send + 'a>> {
        let orders = self.fetch_open_orders();
        Box::pin(std::future::ready(orders))
    }

    /// Recent fills as the account reports them. Empty by default.
    fn fetch_trades(&self) -> Vec<Fill> {
        Vec::new()
    }

    /// Async recent-fill read. Defaults to the mirror.
    fn fetch_trades_async<'a>(&'a self) -> Pin<Box<dyn Future<Output = Vec<Fill>> + Send + 'a>> {
        let trades = self.fetch_trades();
        Box::pin(std::future::ready(trades))
    }

    /// Legs executed for `client_id` by the most recent spread fill, in leg
    /// order. Empty by default and always empty for single-leg intents: only
    /// venues that execute multi-leg packages override this, so the safe
    /// submission path journals leg rows exactly when legs executed.
    fn executed_legs(&self, _client_id: &str) -> Vec<LegExecution> {
        Vec::new()
    }
}

/// Where quotes come from. The session resolves each decision through a
/// primary source with a stale-quote fallback ladder, so one dead feed fails
/// closed instead of trading blind.
pub trait QuoteSource: Send + Sync {
    /// Newest quote for `underlying`, or `None` when this source has none.
    fn latest(&self, underlying: &str) -> Option<Quote>;

    /// Source name for audit (which rung served the quote).
    fn name(&self) -> &str;
}

#[cfg(test)]
#[path = "../tests/venue.rs"]
mod tests;
