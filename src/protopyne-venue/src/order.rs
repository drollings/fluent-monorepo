//! Order vocabulary: intents, lifecycle records, fills, positions, events.
//!
//! Shapes follow the barter order-state pattern (active vs. terminal states)
//! with one prototype rule added: every intent carries a deterministic
//! client ID derived from its proposal, so a retry is always a lookup, never
//! a second order.

use bon::Builder;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

use protopyne_types::{Money, Qty, SpreadLegs};

/// Deterministic client ID for an order attempt: the proposal id plus the
/// attempt counter. Attempt 0 is the first try; any retry reuses its own ID
/// so the journal can tell attempts apart while the venue dedups each one.
#[must_use]
pub fn client_id_for(proposal_id: &str, attempt: u32) -> String {
    format!("{proposal_id}#{attempt}")
}

/// Which side an order takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderSide {
    /// Buy at a limit (or better).
    Buy,
    /// Sell at a limit (or better).
    Sell,
}

impl fmt::Display for OrderSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Buy => f.write_str("buy"),
            Self::Sell => f.write_str("sell"),
        }
    }
}

/// How long a working order stays open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Cancelled automatically at the session close.
    #[default]
    Day,
    /// Works until filled or explicitly cancelled.
    GoodTilCancelled,
    /// Fill immediately, in whole or in part, then cancel the rest.
    ImmediateOrCancel,
}

impl fmt::Display for TimeInForce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Day => f.write_str("day"),
            Self::GoodTilCancelled => f.write_str("gtc"),
            Self::ImmediateOrCancel => f.write_str("ioc"),
        }
    }
}

/// What the trader wants: one limit order described before any venue touch.
#[derive(Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct OrderIntent {
    /// Deterministic client ID (see [`client_id_for`]).
    pub client_id: String,
    /// Proposal this order executes.
    pub proposal_id: String,
    /// Strategy attribution.
    pub strategy: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Buy or sell.
    pub side: OrderSide,
    /// Whole contracts (or shares).
    pub quantity: u32,
    /// Limit price per contract/share.
    pub limit_price: Money,
    /// Multi-leg spread legs. `None` is the legacy single-underlying intent:
    /// the flat fields above describe the whole order and existing builder
    /// call sites compile unchanged. `Some` attaches an atomic spread whose
    /// legs submit as one multi-leg order, never leg by leg.
    pub legs: Option<SpreadLegs>,
    /// How long a working order stays open. Defaults to [`TimeInForce::Day`];
    /// `#[serde(default)]` keeps pre-extension journal rows readable.
    #[builder(default)]
    #[serde(default)]
    pub time_in_force: TimeInForce,
}

/// Lifecycle of one order, from journaled intent to terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderStatus {
    /// Journaled, not yet sent.
    Pending,
    /// Sent, acknowledgement outstanding.
    Submitted,
    /// Venue acknowledged, working.
    Acknowledged,
    /// Working, partially filled; the venue may still change it.
    Partial,
    /// Terminal: fully filled.
    Filled,
    /// Terminal: venue cancelled or rejected the remainder.
    Cancelled,
    /// Terminal: venue refused the order.
    Rejected,
    /// Terminal: the venue withdrew the unfilled remainder (e.g. a day
    /// order past the close).
    Expired,
    /// Terminal: the venue accepted the order then failed to open or work
    /// it — distinct from [`Self::Rejected`], which never got that far.
    OpenFailed,
}

impl OrderStatus {
    /// Whether the venue may still change this order.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.to_active().is_some()
    }

    /// Project this status onto the active typestate, or `None` when
    /// terminal. This is the single home of the active/terminal predicate:
    /// [`is_active`](Self::is_active) delegates here, so no second
    /// active-set can drift apart from it.
    #[must_use]
    pub fn to_active(self) -> Option<ActiveOrderState> {
        match self {
            Self::Pending | Self::Submitted => Some(ActiveOrderState::OpenInFlight),
            Self::Acknowledged | Self::Partial => Some(ActiveOrderState::Open),
            Self::Filled
            | Self::Cancelled
            | Self::Rejected
            | Self::Expired
            | Self::OpenFailed => None,
        }
    }
}

/// The active side of the order typestate: what an order looks like while
/// the venue may still change it. Terminal states have no representation
/// here by construction — [`OrderStatus::to_active`] returns `None` for
/// them, so matching on this type proves liveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActiveOrderState {
    /// Sent, acknowledgement outstanding.
    OpenInFlight,
    /// Venue acknowledged and working (including partially filled).
    Open,
    /// Cancel requested, confirmation outstanding.
    CancelInFlight,
}

impl ActiveOrderState {
    /// Collapse back onto the status vocabulary. A cancel request has no
    /// status of its own — the venue has confirmed nothing yet — so it
    /// reports the last confirmed working state.
    #[must_use]
    pub fn to_inactive(self) -> OrderStatus {
        match self {
            Self::OpenInFlight => OrderStatus::Submitted,
            Self::Open | Self::CancelInFlight => OrderStatus::Acknowledged,
        }
    }

    /// Request cancellation. Only a sent or working order converts; an order
    /// already cancelling returns `None` instead of panicking or stacking a
    /// second request.
    #[must_use]
    pub fn to_request_cancel(self) -> Option<Self> {
        match self {
            Self::OpenInFlight | Self::Open => Some(Self::CancelInFlight),
            Self::CancelInFlight => None,
        }
    }
}

/// One executed fill: quantity at one average price plus commission.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct Fill {
    /// Contracts (or shares) filled.
    pub quantity: u32,
    /// Average execution price.
    pub price: Money,
    /// Commission paid.
    pub fee: Money,
}

/// The journaled and venue-side record of one order.
#[derive(Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct OrderRecord {
    /// The original intent.
    pub intent: OrderIntent,
    /// Current lifecycle state.
    pub status: OrderStatus,
    /// Contracts filled so far.
    pub filled_quantity: u32,
    /// Average fill price, once partially filled or better.
    pub avg_price: Option<Money>,
    /// Commission paid so far.
    pub fees_paid: Money,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

impl OrderRecord {
    /// New record straight from an intent, before submission.
    #[must_use]
    pub fn pending(intent: OrderIntent, now: DateTime<Utc>) -> Self {
        Self::new()
            .intent(intent)
            .status(OrderStatus::Pending)
            .filled_quantity(0)
            .maybe_avg_price(None)
            .fees_paid(Money::zero())
            .updated_at(now)
            .build()
    }

    /// Apply a fill, keeping a quantity-weighted average price.
    pub fn apply_fill(&mut self, fill: Fill, now: DateTime<Utc>) {
        let prior = Decimal::from(self.filled_quantity);
        let added = Decimal::from(fill.quantity);
        let total = prior + added;
        if total > Decimal::ZERO {
            let prior_notional = self
                .avg_price
                .map_or(Decimal::ZERO, |p| p.as_decimal() * prior);
            let next = (prior_notional + fill.price.as_decimal() * added) / total;
            self.avg_price = Some(Money::new(next));
        }
        self.filled_quantity += fill.quantity;
        self.fees_paid = Money::new(self.fees_paid.as_decimal() + fill.fee.as_decimal());
        self.updated_at = now;
    }
}

/// Net position in one underlying/structure pair.
#[derive(Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct Position {
    /// Underlying symbol.
    pub underlying: String,
    /// Signed contracts (or shares); negative is short.
    pub quantity: i64,
    /// Average entry price.
    pub avg_price: Money,
}

impl Position {
    /// Apply a signed fill (`+` buy, `-` sell) at `price`.
    ///
    /// Average-cost accounting: adds in the same direction average into the
    /// existing basis, partial closes against the position keep the original
    /// basis, and only a fill that flips the position through zero re-anchors
    /// the basis at the fill price — the old side is closed and a new side
    /// is opened at this print.
    pub fn apply(&mut self, signed_qty: i64, price: Money) {
        let next = self.quantity + signed_qty;
        if next == 0 {
            self.quantity = 0;
            return;
        }
        if self.quantity == 0 || self.quantity.signum() == signed_qty.signum() {
            let basis = Decimal::from(self.quantity.abs())
                * self.avg_price.as_decimal()
                + Decimal::from(signed_qty.abs()) * price.as_decimal();
            let denom = Decimal::from(next.abs());
            self.avg_price = Money::new(basis / denom);
        } else if next.signum() != self.quantity.signum() {
            self.avg_price = price;
        }
        self.quantity = next;
    }
}

/// Explicit corporate-action and expiry events. The prototype applies them as
/// whole-position adjustments with journaled reasons — never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CorporateEvent {
    /// Contracts expired worthless; position closes at zero.
    Expiry,
    /// Short legs assigned; prototype closes the position at intrinsic zero
    /// and journals the assignment for human review.
    Assignment,
    /// Cash dividend per contract/share.
    Dividend(Money),
    /// Forward split factor as new shares per old share (e.g. 2 for a
    /// 2-for-1 split). A zero factor is malformed and must be refused by the
    /// venue, never reinterpreted as a no-op; reverse splits have no
    /// representation here and are likewise a typed refusal, not arithmetic.
    Split(u32),
}

/// Quantity held as [`Qty`] for strategy-facing math.
#[must_use]
pub fn qty_of(contracts: u32) -> Qty {
    Qty::new(Decimal::from(contracts))
}

#[cfg(test)]
#[path = "../tests/order.rs"]
mod tests;
