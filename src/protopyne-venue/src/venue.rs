//! The `Venue` trait: one code path for paper now, sandbox later.
//!
//! Backtest implements this same trait over versioned history through the
//! real fill code, so a divergence between paper and backtest is a bug, not
//! a feature. Consumers stay generic over `V: Venue`; there is no paper-only
//! order path.

use crate::error::VenueError;
use crate::order::{CorporateEvent, Fill, OrderIntent, OrderRecord, OrderSide, Position};
use crate::quote::Quote;
use chrono::{DateTime, Utc};
use protopyne_types::{ChainQuote, ContractId, OptionContract};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

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
/// decides. The default delegates to [`leg_quote`](Self::leg_quote), so
/// live (newest-only) sources keep compiling and behaving exactly as before.
pub trait LegQuoteSource: Send + Sync {
    /// Newest quote for `contract`, or `None` when this source has none.
    fn leg_quote(&self, contract: &OptionContract) -> Option<Quote>;

    /// Newest quote for `contract` at-or-before `at`, or `None` when this
    /// source holds none. Historical sources override this; live sources
    /// inherit the newest-only read.
    fn leg_quote_at(&self, contract: &OptionContract, at: DateTime<Utc>) -> Option<Quote> {
        let _ = at;
        self.leg_quote(contract)
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

/// One code path for every venue. Implementations are `Send + Sync` so fan-out
/// pools can share them; interior mutability lives inside each venue.
///
/// The async methods return boxed futures (not `impl Future`) so the trait
/// is object-safe: routing holds `Arc<dyn Venue>` handles without a
/// per-venue `match`. Callers still `.await` them exactly as before.
pub trait Venue: Send + Sync {
    /// Check buying power and order sanity without touching the market.
    /// Pure against the venue's current books; never mutates.
    fn dry_run(&self, intent: &OrderIntent, quote: &Quote) -> Result<DryRunOutcome, VenueError>;

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

    /// Current venue-truth orders.
    fn orders(&self) -> Vec<OrderRecord>;

    /// Apply an explicit corporate-action or expiry event to venue truth.
    /// Journaled by the caller with the returned description.
    fn apply_corporate_event(
        &self,
        underlying: &str,
        event: CorporateEvent,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, VenueError>;

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

    /// Working orders as the account reports them. Empty by default.
    fn fetch_open_orders(&self) -> Vec<OrderRecord> {
        Vec::new()
    }

    /// Recent fills as the account reports them. Empty by default.
    fn fetch_trades(&self) -> Vec<Fill> {
        Vec::new()
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
