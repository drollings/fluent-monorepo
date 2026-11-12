//! Leg-aware position engine: pure fill math, no venue touch.
//!
//! [`SpreadPosition`] tracks one instrument's exposure through entries,
//! partial reductions, exact closes, and flips-through-zero, with
//! fee-inclusive realised PnL and exit-fee-adjusted unrealised PnL. Every
//! function here is pure `Decimal` math over owned values: no I/O, no clock
//! reads (timestamps arrive on the fill), no order authority.
//!
//! This module migrates nothing yet: [`Position`](crate::order::Position) and
//! its [`apply`](crate::order::Position::apply) stay exactly as they are. The
//! paper/journal migration consumes these functions instead of extending the
//! old path a second time.
//!
//! Math shape (rewritten in venue vocabulary, not copied): volume-weighted
//! entry averaging on increases; realised PnL on the closed quantity net of
//! that fill's fee; unrealised PnL net of the pro-rata exit-fee estimate
//! (`quantity / max * enter-fees`); flips split one fill's fee
//! proportionally between the closing share and the opening share.

use bon::Builder;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

use protopyne_types::{Money, SpreadLegs};

use crate::error::VenueError;
use crate::order::OrderSide;

/// Which side exposure sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Which side exposure sits on.
pub enum PositionSide {
    /// Net long exposure.
    Long,
    /// Net short exposure.
    Short,
}

impl PositionSide {
    /// Side a fill opens from flat: buys open long, sells open short.
    #[must_use]
    pub fn for_entry(side: OrderSide) -> Self {
        match side {
            OrderSide::Buy => Self::Long,
            OrderSide::Sell => Self::Short,
        }
    }

    /// Whether a fill in `side` grows this position (`true`) or works
    /// against it (`false`, a reduction, close, or flip).
    #[must_use]
    pub fn extends(self, side: OrderSide) -> bool {
        matches!(
            (self, side),
            (Self::Long, OrderSide::Buy) | (Self::Short, OrderSide::Sell)
        )
    }

    /// The other side (what a flip opens).
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Long => Self::Short,
            Self::Short => Self::Long,
        }
    }
}

impl fmt::Display for PositionSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Long => f.write_str("long"),
            Self::Short => f.write_str("short"),
        }
    }
}

/// One executed fill as the position engine sees it: instrument, direction,
/// size, price, fee, identity, and time. Timestamps arrive on the fill —
/// the engine never reads a clock.
#[derive(Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
/// One executed fill as the position engine sees it: instrument, direction,
/// size, price, fee, identity, and time. Timestamps arrive on the fill —
/// the engine never reads a clock.
pub struct PositionFill {
    /// Underlying this fill belongs to; must match the position's.
    pub underlying: String,
    /// Fill direction: buys grow long (cover short), sells grow short.
    pub side: OrderSide,
    /// Whole contracts filled; must be positive.
    pub quantity: u32,
    /// Execution price per contract.
    pub price: Money,
    /// Commission on this fill.
    pub fee: Money,
    /// Venue-side trade identifier for audit.
    pub trade_id: String,
    /// Execution time.
    pub at: DateTime<Utc>,
}

/// One instrument's open exposure.
#[derive(Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
/// One instrument's open exposure.
pub struct SpreadPosition {
    /// Underlying this position tracks.
    pub underlying: String,
    /// Net exposure side.
    pub side: PositionSide,
    /// Volume-weighted average entry price across all increasing fills.
    pub price_entry_average: Money,
    /// Current absolute quantity.
    pub quantity_abs: u32,
    /// Maximum absolute quantity reached by entry/increase fills.
    pub quantity_abs_max: u32,
    /// Cumulative realised PnL, fee-inclusive (entry fees debit on open).
    pub pnl_realised: Money,
    /// Estimated unrealised PnL from closing at the last mark, net of the
    /// pro-rata exit-fee estimate. Refreshed on every fill; `ZERO` on open.
    pub pnl_unrealised: Money,
    /// Cumulative fees paid entering or increasing.
    pub fees_enter: Money,
    /// Cumulative fees paid reducing or closing.
    pub fees_exit: Money,
    /// Time of the fill that opened this position.
    pub time_enter: DateTime<Utc>,
    /// Time of the most recent fill.
    pub time_updated: DateTime<Utc>,
    /// Trade identifiers behind this position, in fill order.
    #[builder(default)]
    pub trade_ids: Vec<String>,
    /// Atomic spread this leg belongs to, if any. The math never assigns
    /// it — the commit path tags the structure it executed — but the tag
    /// travels with the position through reductions, closes, and flips.
    pub legs: Option<SpreadLegs>,
}

impl SpreadPosition {
    /// Open from one entry fill.
    fn open(fill: &PositionFill) -> Self {
        Self::new()
            .underlying(fill.underlying.clone())
            .side(PositionSide::for_entry(fill.side))
            .price_entry_average(fill.price)
            .quantity_abs(fill.quantity)
            .quantity_abs_max(fill.quantity)
            .pnl_realised(Money::new(-fill.fee.as_decimal()))
            .pnl_unrealised(Money::zero())
            .fees_enter(fill.fee)
            .fees_exit(Money::zero())
            .time_enter(fill.at)
            .time_updated(fill.at)
            .trade_ids(vec![fill.trade_id.clone()])
            .maybe_legs(None)
            .build()
    }

    /// Re-mark unrealised PnL at `price`.
    fn refresh_unrealised(&mut self, price: Money) {
        self.pnl_unrealised = calculate_pnl_unrealised(
            self.side,
            self.price_entry_average,
            self.quantity_abs,
            self.quantity_abs_max,
            self.fees_enter,
            price,
        );
    }
}

/// The closed-position record left behind by an exact close or a flip.
#[derive(Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
/// The closed-position record left behind by an exact close or a flip.
pub struct PositionExited {
    /// Underlying that closed.
    pub underlying: String,
    /// Side the closed position sat on.
    pub side: PositionSide,
    /// Volume-weighted entry the realised PnL measured against.
    pub price_entry_average: Money,
    /// Maximum absolute quantity the position reached while open.
    pub quantity_abs_max: u32,
    /// Final fee-inclusive realised PnL.
    pub pnl_realised: Money,
    /// Cumulative entry fees.
    pub fees_enter: Money,
    /// Cumulative exit fees.
    pub fees_exit: Money,
    /// Time of the opening fill.
    pub time_enter: DateTime<Utc>,
    /// Time of the closing fill.
    pub time_exit: DateTime<Utc>,
    /// Trade identifiers behind the closed position, in fill order.
    #[builder(default)]
    pub trade_ids: Vec<String>,
    /// Spread tag the closed position carried, if any.
    pub legs: Option<SpreadLegs>,
}

impl PositionExited {
    /// Freeze a fully reduced position into its closed record.
    fn from_closed(position: SpreadPosition) -> Self {
        Self::new()
            .underlying(position.underlying)
            .side(position.side)
            .price_entry_average(position.price_entry_average)
            .quantity_abs_max(position.quantity_abs_max)
            .pnl_realised(position.pnl_realised)
            .fees_enter(position.fees_enter)
            .fees_exit(position.fees_exit)
            .time_enter(position.time_enter)
            .time_exit(position.time_updated)
            .trade_ids(position.trade_ids)
            .maybe_legs(position.legs)
            .build()
    }
}

/// Volume-weighted average entry price when a fill joins existing exposure:
/// `(current_value + fill_value) / (current_quantity + fill_quantity)`.
/// Both-zero in means zero out — no division-by-zero, never `NaN`.
#[must_use]
/// Volume-weighted average entry price when a fill joins existing exposure:
/// `(current_value + fill_value) / (current_quantity + fill_quantity)`.
/// Both-zero in means zero out — no division-by-zero, never `NaN`.
pub fn calculate_price_entry_average(
    current_average: Money,
    current_quantity: u32,
    trade_price: Money,
    trade_quantity: u32,
) -> Money {
    let total = Decimal::from(current_quantity) + Decimal::from(trade_quantity);
    if total.is_zero() {
        return Money::zero();
    }
    Money::new(
        (current_average.as_decimal() * Decimal::from(current_quantity)
            + trade_price.as_decimal() * Decimal::from(trade_quantity))
            / total,
    )
}

/// Realised PnL from closing `closed_quantity` at `closed_price`: the
/// quantity-scaled distance from entry, net of the closing fee. Long gains
/// when the close prints above entry; short gains when it prints below.
#[must_use]
/// Realised PnL from closing `closed_quantity` at `closed_price`: the
/// quantity-scaled distance from entry, net of the closing fee. Long gains
/// when the close prints above entry; short gains when it prints below.
pub fn calculate_pnl_realised(
    side: PositionSide,
    entry_average: Money,
    closed_quantity: u32,
    closed_price: Money,
    closed_fee: Money,
) -> Money {
    let closed_value = Decimal::from(closed_quantity) * closed_price.as_decimal();
    let entry_value = Decimal::from(closed_quantity) * entry_average.as_decimal();
    Money::new(match side {
        PositionSide::Long => closed_value - entry_value - closed_fee.as_decimal(),
        PositionSide::Short => entry_value - closed_value - closed_fee.as_decimal(),
    })
}

/// Estimated unrealised PnL from closing the whole open quantity at `price`,
/// net of the pro-rata exit-fee estimate (`quantity / max * enter-fees`):
/// the enter fees bought a `max`-sized position, so the per-quantity ratio
/// prices the round trip home. A zero `quantity_abs_max` (only observable on
/// values that bypassed [`update_from_fill`]) estimates zero fees rather
/// than dividing by zero.
#[must_use]
/// Estimated unrealised PnL from closing the whole open quantity at `price`,
/// net of the pro-rata exit-fee estimate (`quantity / max * enter-fees`):
/// the enter fees bought a `max`-sized position, so the per-quantity ratio
/// prices the round trip home. A zero `quantity_abs_max` (only observable on
/// values that bypassed [`update_from_fill`]) estimates zero fees rather
/// than dividing by zero.
pub fn calculate_pnl_unrealised(
    side: PositionSide,
    entry_average: Money,
    quantity_abs: u32,
    quantity_abs_max: u32,
    fees_enter: Money,
    price: Money,
) -> Money {
    let approx_exit_fees = if quantity_abs_max == 0 {
        Decimal::ZERO
    } else {
        Decimal::from(quantity_abs) / Decimal::from(quantity_abs_max) * fees_enter.as_decimal()
    };
    let current_value = Decimal::from(quantity_abs) * price.as_decimal();
    let entry_value = Decimal::from(quantity_abs) * entry_average.as_decimal();
    Money::new(match side {
        PositionSide::Long => current_value - entry_value - approx_exit_fees,
        PositionSide::Short => entry_value - current_value - approx_exit_fees,
    })
}

/// Fold one fill into an optional open position.
///
/// - `None` + fill opens a new position (no closed record).
/// - Same-direction fills increase: entry re-averages, fees debit realised.
/// - Opposite-direction fills reduce, exactly close, or flip through zero:
///   a flip splits the fill's fee proportionally between the closing share
///   and the opening share, so the two shares sum to the fill fee exactly.
///
/// Refusals (never panics): zero-quantity fills, and fills for a different
/// underlying than the open position.
pub fn update_from_fill(
    current: Option<SpreadPosition>,
    fill: &PositionFill,
) -> Result<(Option<SpreadPosition>, Option<PositionExited>), VenueError> {
    if fill.quantity == 0 {
        return Err(VenueError::Invalid(
            "fill quantity must be positive".to_owned(),
        ));
    }
    let mut position = match current {
        None => return Ok((Some(SpreadPosition::open(fill)), None)),
        Some(held) => {
            if held.underlying != fill.underlying {
                return Err(VenueError::Invalid(format!(
                    "position in {} cannot absorb a fill for {}",
                    held.underlying, fill.underlying
                )));
            }
            held
        }
    };
    position.trade_ids.push(fill.trade_id.clone());

    if position.side.extends(fill.side) {
        position.price_entry_average = calculate_price_entry_average(
            position.price_entry_average,
            position.quantity_abs,
            fill.price,
            fill.quantity,
        );
        position.quantity_abs = position
            .quantity_abs
            .checked_add(fill.quantity)
            .ok_or_else(|| VenueError::Invalid("position quantity overflow".to_owned()))?;
        position.quantity_abs_max = position.quantity_abs_max.max(position.quantity_abs);
        position.pnl_realised =
            Money::new(position.pnl_realised.as_decimal() - fill.fee.as_decimal());
        position.fees_enter = Money::new(position.fees_enter.as_decimal() + fill.fee.as_decimal());
        position.time_updated = fill.at;
        position.refresh_unrealised(fill.price);
        return Ok((Some(position), None));
    }

    if fill.quantity < position.quantity_abs {
        let realised = calculate_pnl_realised(
            position.side,
            position.price_entry_average,
            fill.quantity,
            fill.price,
            fill.fee,
        );
        position.pnl_realised =
            Money::new(position.pnl_realised.as_decimal() + realised.as_decimal());
        position.quantity_abs -= fill.quantity;
        position.fees_exit = Money::new(position.fees_exit.as_decimal() + fill.fee.as_decimal());
        position.time_updated = fill.at;
        position.refresh_unrealised(fill.price);
        return Ok((Some(position), None));
    }

    if fill.quantity == position.quantity_abs {
        let realised = calculate_pnl_realised(
            position.side,
            position.price_entry_average,
            fill.quantity,
            fill.price,
            fill.fee,
        );
        position.pnl_realised =
            Money::new(position.pnl_realised.as_decimal() + realised.as_decimal());
        position.fees_exit = Money::new(position.fees_exit.as_decimal() + fill.fee.as_decimal());
        position.time_updated = fill.at;
        position.quantity_abs = 0;
        position.refresh_unrealised(fill.price);
        return Ok((None, Some(PositionExited::from_closed(position))));
    }

    let leftover = fill.quantity - position.quantity_abs;
    let total = Decimal::from(fill.quantity);
    let fee_exit_share =
        Money::new(fill.fee.as_decimal() * Decimal::from(position.quantity_abs) / total);
    let fee_enter_next = Money::new(fill.fee.as_decimal() * Decimal::from(leftover) / total);
    let realised = calculate_pnl_realised(
        position.side,
        position.price_entry_average,
        position.quantity_abs,
        fill.price,
        fee_exit_share,
    );
    position.pnl_realised = Money::new(position.pnl_realised.as_decimal() + realised.as_decimal());
    position.fees_exit = Money::new(position.fees_exit.as_decimal() + fee_exit_share.as_decimal());
    position.time_updated = fill.at;
    position.quantity_abs = 0;
    position.refresh_unrealised(fill.price);
    let exited = PositionExited::from_closed(position);
    let next = SpreadPosition::new()
        .underlying(fill.underlying.clone())
        .side(exited.side.opposite())
        .price_entry_average(fill.price)
        .quantity_abs(leftover)
        .quantity_abs_max(leftover)
        .pnl_realised(Money::new(-fee_enter_next.as_decimal()))
        .pnl_unrealised(Money::zero())
        .fees_enter(fee_enter_next)
        .fees_exit(Money::zero())
        .time_enter(fill.at)
        .time_updated(fill.at)
        .trade_ids(vec![fill.trade_id.clone()])
        .maybe_legs(exited.legs.clone())
        .build();
    Ok((Some(next), Some(exited)))
}

#[cfg(test)]
#[path = "../tests/position.rs"]
mod tests;