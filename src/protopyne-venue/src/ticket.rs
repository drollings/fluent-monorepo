//! Manual ticket workflow: exact human-executed tickets with confirmation.
//!
//! The platform never submits these orders — the operator places them by
//! hand (see VISION §9). The value here is reconciliation of the human's
//! fill against the ticket: issued tickets wait as unconfirmed, the
//! operator's fill report confirms them, and anything the fill disagrees
//! on flags drift. Ticket rows journal like every other state change
//! (journal-first, then mutate), replay rebuilds the waiting set, and the
//! monitor replica skips them so they never read as phantom orders.
//!
//! The ticket shape is [`OrderIntent`](crate::order::OrderIntent) plus an
//! issue stamp — no second order type to drift apart from it. Fill
//! comparison reuses [`journal_net`](crate::reconcile::journal_net) for the
//! confirmed book the read-only import checks against. That import stays
//! behind [`TicketRead`]: a dark adapter reports its failure and changes
//! nothing, so a fragile read never touches ticket state.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::VenueError;
use crate::journal::{Journal, JournalEvent, VenueStorage};
use crate::order::{OrderIntent, OrderSide};
use crate::reconcile::journal_net;
use protopyne_types::Money;

/// Journaled kind for an issued manual ticket. Detail is the ticket as JSON.
pub const TICKET_ISSUED_KIND: &str = "manual_ticket.issued";
/// Journaled kind for a confirmed manual ticket. Detail is the fill as JSON.
pub const TICKET_CONFIRMED_KIND: &str = "manual_ticket.confirmed";

/// One exact human-executed ticket: the intent plus its issue stamp. The
/// ticket ID is the intent's client ID, so tickets and automated orders
/// share one identifier discipline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualTicket {
    /// What the human was asked to place.
    pub intent: OrderIntent,
    /// When the ticket was issued.
    pub issued_at: DateTime<Utc>,
}

impl ManualTicket {
    /// Wrap an intent as a ticket issued at `issued_at`.
    #[must_use]
    pub fn from_intent(intent: OrderIntent, issued_at: DateTime<Utc>) -> Self {
        Self { intent, issued_at }
    }

    /// Ticket ID: the intent's client ID.
    #[must_use]
    pub fn ticket_id(&self) -> &str {
        &self.intent.client_id
    }

    /// Render the exact ticket the human places: side, quantity,
    /// underlying, limit, good-until, ticket ID, and every leg by OCC
    /// symbol. A ticket missing any of these is not exact.
    #[must_use]
    pub fn render(&self) -> String {
        let intent = &self.intent;
        let legs = intent
            .legs
            .as_ref()
            .map(|legs| {
                legs.legs()
                    .iter()
                    .map(|leg| {
                        format!("{} {}x {}", leg.side, leg.ratio, leg.contract.occ_symbol())
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .map(|list| format!(" legs: {list}"))
            .unwrap_or_default();
        format!(
            "{} {} {} @ {} {} [{}]{legs}",
            intent.side,
            intent.quantity,
            intent.underlying,
            intent.limit_price,
            intent.time_in_force,
            intent.client_id,
        )
    }
}

/// The human's fill report for one ticket: contracts filled at one price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedFill {
    /// Contracts the human reports filled.
    pub quantity: u32,
    /// Average price the human reports.
    pub price: Money,
}

/// How a confirmation (or import) disagreed with the ticket book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftKind {
    /// Filled contracts differ from the ticket quantity.
    QuantityMismatch,
    /// Fill price is worse than the ticket limit.
    PriceOutsideLimit,
    /// The read-only import holds a position the confirmed book cannot
    /// explain.
    ImportMismatch,
}

/// One flagged disagreement between tickets, fills, and the import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketDrift {
    /// Ticket (or underlying, for import drift) that disagrees.
    pub ticket_id: String,
    /// How it disagrees.
    pub kind: DriftKind,
    /// Human-readable reason.
    pub detail: String,
}

/// One read-only position line from the human-executed account. Lines are
/// data, never authority: the ticket book never moves on import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedPosition {
    /// Underlying the position tracks.
    pub underlying: String,
    /// Signed contracts held.
    pub quantity: i64,
}

/// Read-only rung over the human-executed account. Failures report —
/// [`TicketTracker::apply_import`] applies nothing on failure, so a dark
/// adapter never touches ticket state.
pub trait TicketRead: Send + Sync {
    /// Current position lines, or the transport failure.
    fn snapshot(&self) -> Result<Vec<ImportedPosition>, VenueError>;
}

/// Unconfirmed tickets, confirmed fills, drift flags, and the last good
/// import snapshot. In-memory only; durability comes from journaling
/// issue/confirm rows and rebuilding via [`replay`](Self::replay).
#[derive(Debug, Default)]
pub struct TicketTracker {
    open: BTreeMap<String, ManualTicket>,
    confirmed: Vec<(String, i64)>,
    drifts: Vec<TicketDrift>,
    imported: Vec<ImportedPosition>,
}

impl TicketTracker {
    /// An empty tracker: nothing waits, nothing confirmed, no drift.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a ticket: it waits as unconfirmed. A duplicate ID refuses —
    /// one ticket, one wait.
    pub fn issue(
        &mut self,
        intent: OrderIntent,
        issued_at: DateTime<Utc>,
    ) -> Result<&ManualTicket, VenueError> {
        if self.open.contains_key(&intent.client_id) {
            return Err(VenueError::Invalid(format!(
                "ticket {} already waits: refusing a second wait",
                intent.client_id
            )));
        }
        let id = intent.client_id.clone();
        self.open.insert(id.clone(), ManualTicket::from_intent(intent, issued_at));
        Ok(&self.open[&id])
    }

    /// Confirm a ticket against the human's fill report. Unknown IDs
    /// refuse; anything the fill disagrees on flags drift. Confirmed
    /// tickets never wait twice either way.
    pub fn confirm(
        &mut self,
        ticket_id: &str,
        fill: ConfirmedFill,
        _at: DateTime<Utc>,
    ) -> Result<Option<TicketDrift>, VenueError> {
        let ticket = self.open.remove(ticket_id).ok_or_else(|| {
            VenueError::Invalid(format!("unknown ticket {ticket_id}: nothing to confirm"))
        })?;
        let signed = match ticket.intent.side {
            OrderSide::Buy => i64::from(fill.quantity),
            OrderSide::Sell => -i64::from(fill.quantity),
        };
        self.confirmed.push((ticket.intent.underlying.clone(), signed));
        let drift = Self::check_fill(&ticket, &fill).map(|(kind, detail)| {
            let drift =
                TicketDrift { ticket_id: ticket_id.to_owned(), kind, detail };
            self.drifts.push(drift.clone());
            drift
        });
        Ok(drift)
    }

    /// Tickets still waiting for the human's fill report, in ticket order.
    #[must_use]
    pub fn unconfirmed(&self) -> Vec<&ManualTicket> {
        self.open.values().collect()
    }

    /// Flagged disagreements, oldest first.
    #[must_use]
    pub fn drifts(&self) -> &[TicketDrift] {
        &self.drifts
    }

    /// Last good read-only snapshot. A failed import never replaces it.
    #[must_use]
    pub fn imported(&self) -> &[ImportedPosition] {
        &self.imported
    }

    /// Net confirmed contracts per underlying, reusing the reconciliation
    /// accumulation so the two books cannot disagree on arithmetic.
    #[must_use]
    pub fn confirmed_net(&self, underlying: &str) -> i64 {
        journal_net(&self.confirmed).get(underlying).copied().unwrap_or(0)
    }

    /// Journal-first issue: validate, journal the row, then track. A
    /// refused issue journals nothing.
    pub fn issue_journaled(
        &mut self,
        journal: &Journal,
        permit: &VenueStorage,
        intent: OrderIntent,
        now: DateTime<Utc>,
    ) -> Result<&ManualTicket, VenueError> {
        if self.open.contains_key(&intent.client_id) {
            return Err(VenueError::Invalid(format!(
                "ticket {} already waits: refusing a second wait",
                intent.client_id
            )));
        }
        let ticket = ManualTicket::from_intent(intent, now);
        let detail =
            serde_json::to_string(&ticket).map_err(|e| VenueError::Journal(e.to_string()))?;
        journal.record_transition(permit, ticket.ticket_id(), TICKET_ISSUED_KIND, &detail, now)?;
        let id = ticket.ticket_id().to_owned();
        self.open.insert(id.clone(), ticket);
        Ok(&self.open[&id])
    }

    /// Journal-first confirm: validate, journal the row, then confirm. A
    /// refused confirm journals nothing and mutates nothing.
    pub fn confirm_journaled(
        &mut self,
        journal: &Journal,
        permit: &VenueStorage,
        ticket_id: &str,
        fill: ConfirmedFill,
        now: DateTime<Utc>,
    ) -> Result<Option<TicketDrift>, VenueError> {
        if !self.open.contains_key(ticket_id) {
            return Err(VenueError::Invalid(format!(
                "unknown ticket {ticket_id}: nothing to confirm"
            )));
        }
        let detail =
            serde_json::to_string(&fill).map_err(|e| VenueError::Journal(e.to_string()))?;
        journal.record_transition(permit, ticket_id, TICKET_CONFIRMED_KIND, &detail, now)?;
        self.confirm(ticket_id, fill, now)
    }

    /// Rebuild the waiting set from journaled rows: last row per ticket
    /// wins; unparseable rows and every other kind never project. A restart
    /// rebuilds exactly the set the operator left.
    #[must_use]
    pub fn replay(events: &[JournalEvent]) -> Self {
        let mut tracker = Self::new();
        let mut sides: BTreeMap<String, (String, OrderSide)> = BTreeMap::new();
        for event in events {
            if event.kind == TICKET_ISSUED_KIND {
                if let Ok(ticket) = serde_json::from_str::<ManualTicket>(&event.detail) {
                    sides.insert(
                        ticket.ticket_id().to_owned(),
                        (ticket.intent.underlying.clone(), ticket.intent.side),
                    );
                    tracker.open.insert(ticket.ticket_id().to_owned(), ticket);
                }
            } else if event.kind == TICKET_CONFIRMED_KIND {
                if let Ok(fill) = serde_json::from_str::<ConfirmedFill>(&event.detail) {
                    if let Some((underlying, side)) = sides.remove(&event.client_id) {
                        let signed = match side {
                            OrderSide::Buy => i64::from(fill.quantity),
                            OrderSide::Sell => -i64::from(fill.quantity),
                        };
                        tracker.confirmed.push((underlying, signed));
                    }
                    tracker.open.remove(&event.client_id);
                }
            }
        }
        tracker
    }

    /// Apply one read-only import. Failure reports and changes nothing —
    /// open tickets, drifts, and the last good snapshot survive. Success
    /// replaces the snapshot and re-derives import drift from scratch, so
    /// a corrected import clears its own flag instead of banking a second.
    pub fn apply_import(&mut self, read: &dyn TicketRead) -> Result<(), VenueError> {
        let snapshot = read.snapshot()?;
        self.imported = snapshot;
        self.drifts.retain(|drift| drift.kind != DriftKind::ImportMismatch);
        let book = journal_net(&self.confirmed);
        let mut underlyings: Vec<&String> = self.imported.iter().map(|line| &line.underlying).collect();
        underlyings.sort();
        underlyings.dedup();
        for underlying in underlyings {
            let held: i64 =
                self.imported.iter().filter(|line| &line.underlying == underlying).map(|line| line.quantity).sum();
            let confirmed = book.get(underlying).copied().unwrap_or(0);
            if held != confirmed {
                self.drifts.push(TicketDrift {
                    ticket_id: underlying.clone(),
                    kind: DriftKind::ImportMismatch,
                    detail: format!(
                        "import holds {held} {underlying} against {confirmed} confirmed"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Compare one fill against its ticket: quantity must match exactly,
    /// and the price must not be worse than the limit (buys at or under,
    /// sells at or over).
    fn check_fill(ticket: &ManualTicket, fill: &ConfirmedFill) -> Option<(DriftKind, String)> {
        if fill.quantity != ticket.intent.quantity {
            return Some((
                DriftKind::QuantityMismatch,
                format!(
                    "ticket {} wants {} but the fill reports {}",
                    ticket.ticket_id(),
                    ticket.intent.quantity,
                    fill.quantity
                ),
            ));
        }
        let worse = match ticket.intent.side {
            OrderSide::Buy => fill.price.as_decimal() > ticket.intent.limit_price.as_decimal(),
            OrderSide::Sell => fill.price.as_decimal() < ticket.intent.limit_price.as_decimal(),
        };
        if worse {
            return Some((
                DriftKind::PriceOutsideLimit,
                format!(
                    "ticket {} limits at {} but the fill reports {}",
                    ticket.ticket_id(),
                    ticket.intent.limit_price,
                    fill.price
                ),
            ));
        }
        None
    }
}

#[cfg(test)]
#[path = "../tests/ticket.rs"]
mod tests;
