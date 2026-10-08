//! The `Venue` trait: one code path for paper now, sandbox later.
//!
//! Backtest implements this same trait over versioned history through the
//! real fill code, so a divergence between paper and backtest is a bug, not
//! a feature. Consumers stay generic over `V: Venue`; there is no paper-only
//! order path.

use crate::error::VenueError;
use crate::order::{CorporateEvent, OrderIntent, OrderRecord, Position};
use crate::quote::Quote;
use std::future::Future;

/// Outcome of a pre-submission dry run: margin and sanity checks against
/// current buying power, before anything is journaled as submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DryRunOutcome {
    /// Whether submission may proceed.
    pub allowed: bool,
    /// Committed notional the dry run reserved.
    pub reserved_notional: protopyne_types::Money,
}

/// One code path for every venue. Implementations are `Send + Sync` so fan-out
/// pools can share them; interior mutability lives inside each venue.
pub trait Venue: Send + Sync {
    /// Check buying power and order sanity without touching the market.
    /// Pure against the venue's current books; never mutates.
    fn dry_run(&self, intent: &OrderIntent, quote: &Quote) -> Result<DryRunOutcome, VenueError>;

    /// Submit an intent. Idempotent per client ID: resubmitting a known ID
    /// returns the existing record, never a second order.
    fn submit(
        &self,
        intent: OrderIntent,
        quote: &Quote,
        now: chrono::DateTime<chrono::Utc>,
    ) -> impl Future<Output = Result<OrderRecord, VenueError>> + Send;

    /// Look up an order by its deterministic client ID.
    fn query(
        &self,
        client_id: &str,
    ) -> impl Future<Output = Option<OrderRecord>> + Send;

    /// Cancel a working order. Terminal orders are returned unchanged.
    fn cancel(
        &self,
        client_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> impl Future<Output = Result<OrderRecord, VenueError>> + Send;

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
