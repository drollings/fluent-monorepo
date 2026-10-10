//! Execution routing registry: which venue handle serves each symbol.
//!
//! One registry replaces the session's `Arc<PaperVenue>` binding. Handles
//! are `Arc<dyn Venue>` — no `enum Venue` plus per-site `match` may be
//! introduced (the open-closed violation this module exists to prevent).
//! Routing is an exact match on the underlying over the registry's scoped
//! index; `safe_submit` is untouched and remains the only submission path —
//! routing only selects which handle each call reaches.
//!
//! Routing rules, in order:
//!
//! 1. A registry with no routes is a default: the single venue serves every
//!    symbol (`VenueRegistry::single` behaves identically to the bare
//!    venue).
//! 2. With routes, the routed venue serves its symbols first; retryable
//!    failures fail over across the remaining venues in registration order
//!    through [`first_accept_in_order`].
//! 3. Terminal refusals stop the chain: a merit refusal at one venue never
//!    fans out into a second order elsewhere.
//! 4. Symbols with no route refuse fail-closed with a reason naming the
//!    symbol. Lookups (`query`) simply miss.
//! 5. Every submit is query-first across the chain: resubmitting a client
//!    ID after a re-route returns the original record, never a second
//!    order.
//!
//! [`first_accept_in_order`]: fluent_concurrency::ladder::first_accept_in_order

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use fluent_concurrency::ladder::first_accept_in_order;
use protopyne_types::{SymKey, SymbolIndex};

use crate::error::VenueError;
use crate::order::{CorporateEvent, Fill, OrderIntent, OrderRecord, Position};
use crate::quote::Quote;
use crate::venue::{AccountEvent, BalanceEntry, DryRunOutcome, LegExecution, Venue};

/// Routing table plus fallback chain, behind one lock (held only across
/// synchronous map work, never across an `await`).
struct RegistryInner {
    /// Scoped interner: route keys never leak into JSON/journal rows.
    index: SymbolIndex,
    /// Exact-match routes in registration order; a later route for the same
    /// symbol replaces the earlier one (re-routing is explicit).
    routes: Vec<(SymKey, Arc<dyn Venue>)>,
    /// Fallback chain in registration order; every routed venue joins it.
    /// Never empty: [`VenueRegistry::single`] is the only constructor.
    venues: Vec<Arc<dyn Venue>>,
}

/// Which venue handle serves each symbol. Shared by `Arc`; routes are added
/// through `&self` so the session can own one registry while tests re-route
/// it.
pub struct VenueRegistry {
    inner: Mutex<RegistryInner>,
}

impl VenueRegistry {
    /// One venue serving every symbol: byte-identical to the bare venue, so
    /// existing callers move without behavior change.
    #[must_use]
    pub fn single(venue: Arc<dyn Venue>) -> Self {
        Self {
            inner: Mutex::new(RegistryInner {
                index: SymbolIndex::new(),
                routes: Vec::new(),
                venues: vec![venue],
            }),
        }
    }

    /// Route `underlying` to `venue`. A later route for the same symbol
    /// replaces the earlier one. The venue joins the fallback chain on first
    /// registration. Empty symbols refuse fail-closed.
    pub fn add_route(&self, underlying: &str, venue: Arc<dyn Venue>) -> Result<(), VenueError> {
        if underlying.is_empty() {
            return Err(VenueError::Invalid("route symbol must not be empty".to_owned()));
        }
        let mut inner = common_core::sync::lock(&self.inner);
        let key = inner.index.intern(underlying).map_err(|e| {
            VenueError::Invalid(format!("route symbol {underlying} refused: {e}"))
        })?;
        if let Some(slot) = inner.routes.iter_mut().find(|(route, _)| *route == key) {
            slot.1 = Arc::clone(&venue);
        } else {
            inner.routes.push((key, Arc::clone(&venue)));
        }
        if !inner.venues.iter().any(|known| Arc::ptr_eq(known, &venue)) {
            inner.venues.push(venue);
        }
        Ok(())
    }

    /// Ordered chain for `underlying`: the routed venue first, then the rest
    /// of the chain in registration order. `None` when routes exist but none
    /// matches (refuse); a routeless registry defaults every symbol to its
    /// single venue.
    fn chain_for(&self, underlying: &str) -> Option<Vec<Arc<dyn Venue>>> {
        let inner = common_core::sync::lock(&self.inner);
        if inner.routes.is_empty() {
            return Some(inner.venues.clone());
        }
        let key = inner.index.lookup(underlying)?;
        let routed = inner
            .routes
            .iter()
            .find(|(route, _)| *route == key)
            .map(|(_, venue)| Arc::clone(venue))?;
        let mut chain = vec![routed];
        for venue in &inner.venues {
            if !chain.iter().any(|known| Arc::ptr_eq(known, venue)) {
                chain.push(Arc::clone(venue));
            }
        }
        Some(chain)
    }

    /// Every venue in registration order (lookups, truth union).
    fn chain_all(&self) -> Vec<Arc<dyn Venue>> {
        common_core::sync::lock(&self.inner).venues.clone()
    }

    /// First record held anywhere in the chain, in registration order.
    async fn query_chain(&self, client_id: &str) -> Option<OrderRecord> {
        first_accept_in_order(
            self.chain_all(),
            |venue: Arc<dyn Venue>| {
                let id = client_id.to_owned();
                async move { Ok::<Option<OrderRecord>, VenueError>(venue.query(&id).await) }
            },
            |_: &VenueError| false,
        )
        .await
        .ok()
        .flatten()
    }

    /// Refusal naming the unrouted symbol.
    fn unrouted(underlying: &str) -> VenueError {
        VenueError::Invalid(format!("no venue routes underlying '{underlying}'"))
    }
}

impl Venue for VenueRegistry {
    fn dry_run(&self, intent: &OrderIntent, quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        let chain = Self::chain_for(self, &intent.underlying)
            .ok_or_else(|| Self::unrouted(&intent.underlying))?;
        chain
            .first()
            .expect("chain never empty")
            .dry_run(intent, quote)
    }

    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        quote: &'a Quote,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            let chain = Self::chain_for(self, &intent.underlying)
                .ok_or_else(|| Self::unrouted(&intent.underlying))?;
            if let Some(known) = self.query_chain(&intent.client_id).await {
                return Ok(known);
            }
            let submitted = first_accept_in_order(
                chain,
                |venue: Arc<dyn Venue>| {
                    let intent = intent.clone();
                    async move { venue.submit(intent, quote, now).await.map(Some) }
                },
                |error: &VenueError| !error.is_retryable(),
            )
            .await;
            match submitted {
                Ok(Some(record)) => Ok(record),
                Ok(None) => Err(VenueError::Invalid(format!(
                    "no venue accepted order {}",
                    intent.client_id
                ))),
                Err(error) => Err(error),
            }
        })
    }

    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<OrderRecord>> + Send + 'a>> {
        Box::pin(async move { self.query_chain(client_id).await })
    }

    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            let cancelled = first_accept_in_order(
                self.chain_all(),
                |venue: Arc<dyn Venue>| {
                    let id = client_id.to_owned();
                    async move {
                        if venue.query(&id).await.is_some() {
                            venue.cancel(&id, now).await.map(Some)
                        } else {
                            Ok(None)
                        }
                    }
                },
                |_: &VenueError| false,
            )
            .await;
            match cancelled {
                Ok(Some(record)) => Ok(record),
                Ok(None) => Err(VenueError::Invalid(format!(
                    "unknown order {client_id}: no venue holds it"
                ))),
                Err(error) => Err(error),
            }
        })
    }

    fn positions(&self) -> Vec<Position> {
        let mut truth = Vec::new();
        for venue in self.chain_all() {
            truth.extend(venue.positions());
        }
        truth
    }

    fn orders(&self) -> Vec<OrderRecord> {
        let mut truth = Vec::new();
        for venue in self.chain_all() {
            truth.extend(venue.orders());
        }
        truth
    }

    fn apply_corporate_event(
        &self,
        underlying: &str,
        event: CorporateEvent,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, VenueError> {
        let chain =
            Self::chain_for(self, underlying).ok_or_else(|| Self::unrouted(underlying))?;
        chain
            .first()
            .expect("chain never empty")
            .apply_corporate_event(underlying, event, now)
    }

    fn fetch_balances(&self) -> Vec<BalanceEntry> {
        let mut balances = Vec::new();
        for venue in self.chain_all() {
            balances.extend(venue.fetch_balances());
        }
        balances
    }

    fn fetch_open_orders(&self) -> Vec<OrderRecord> {
        let mut open = Vec::new();
        for venue in self.chain_all() {
            open.extend(venue.fetch_open_orders());
        }
        open
    }

    fn fetch_trades(&self) -> Vec<Fill> {
        let mut trades = Vec::new();
        for venue in self.chain_all() {
            trades.extend(venue.fetch_trades());
        }
        trades
    }

    fn account_stream(&self) -> Vec<AccountEvent> {
        let mut events = Vec::new();
        for venue in self.chain_all() {
            events.extend(venue.account_stream());
        }
        events
    }

    fn executed_legs(&self, client_id: &str) -> Vec<LegExecution> {
        // First non-empty legs win: single-leg fills report none on every
        // venue (so emptiness is the right answer regardless), while a
        // spread package executes on exactly one venue — query-first submit
        // never double-submits a client ID, so at most one holder exists.
        for venue in self.chain_all() {
            let legs = venue.executed_legs(client_id);
            if !legs.is_empty() {
                return legs;
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
#[path = "../tests/registry.rs"]
mod registry_tests;
