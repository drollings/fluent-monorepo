//! Option-mark selection: where a package's per-unit mark comes from, and the
//! honest answer when none exists.
//!
//! The preference order is deterministic, so it is a stated assumption rather
//! than a calibrated heuristic: a point-in-time chain mid when every leg
//! answers ([`MarkBasis::ChainMid`]), else the package's own executed prices
//! ([`MarkBasis::FillPrice`]), else nothing ([`MarkBasis::Absent`]). An
//! absent mark reports unrealized as `None` — the absence of a mark is never a
//! mark of zero.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use protopyne_types::{ChainQuote, Money};
use protopyne_venue::{LegExecution, OrderSide};

pub use protopyne_types::MarkBasis;

/// The report's assumption line naming the run's mark basis. Absence is named
/// explicitly so no consumer reads an unmarked book as a zero mark.
#[must_use]
pub fn mark_basis_note(basis: MarkBasis) -> String {
    match basis {
        MarkBasis::ChainMid => {
            "unrealized marks option legs from the point-in-time chain mid".to_owned()
        }
        MarkBasis::FillPrice => "unrealized marks from the position's own fill price where no \
             chain mid is available (legless runs keep the underlying-close proxy)"
            .to_owned(),
        MarkBasis::Absent => "no mark available for some positions: unrealized is reported \
             absent, never zero"
            .to_owned(),
    }
}

/// The per-unit mark of a *position*: `legs` describing one package, held
/// `package_units` times over.
///
/// [`mark_per_unit`] divides the position's value by its package count, so
/// the legs must describe the whole position; this scales one package's legs
/// to the size and delegates, so callers holding a book's legs never scale
/// them by hand.
#[must_use]
pub fn mark_position(
    legs: &[LegExecution],
    package_units: u32,
    chain: &[ChainQuote],
    at: DateTime<Utc>,
) -> (Option<Money>, MarkBasis) {
    let scaled: Vec<LegExecution> = legs
        .iter()
        .map(|leg| LegExecution {
            contract: leg.contract.clone(),
            side: leg.side,
            quantity: leg.quantity.saturating_mul(package_units),
            price: leg.price,
            fee: leg.fee,
        })
        .collect();
    mark_per_unit(&scaled, package_units, chain, at)
}

/// One point-in-time mark for a fold: the tape price a legless book marks
/// against, plus the instant and the run's chain a legged book needs (its
/// legs have no share price to ride the tape on).
#[derive(Debug, Clone, Copy)]
pub struct MarkPoint<'a> {
    /// When the mark applies.
    pub at: DateTime<Utc>,
    /// The tape price (a bar close) legless books mark against.
    pub price: Money,
    /// The run's chain quotes; empty when the run held none, which leaves a
    /// legged book on its own fill prices.
    pub chain: &'a [ChainQuote],
}

impl MarkPoint<'_> {
    /// A tape-only mark over an empty chain: the shape every chainless run
    /// passes.
    #[must_use]
    pub fn tape(at: DateTime<Utc>, price: Money) -> Self {
        Self {
            at,
            price,
            chain: &[],
        }
    }
}

/// The per-package-unit mark for `legs` at `at`, and its basis.
///
/// `legs` are one package's executed legs (`quantity` is already scaled by its
/// ratio); `package_units` is the package count the position books. A
/// [`MarkBasis::ChainMid`] mark needs a non-crossed quote at-or-before `at`
/// for every leg; any miss falls back to the package's own executed prices; an
/// empty leg set or zero units has no mark at all.
#[must_use]
pub fn mark_per_unit(
    legs: &[LegExecution],
    package_units: u32,
    chain: &[ChainQuote],
    at: DateTime<Utc>,
) -> (Option<Money>, MarkBasis) {
    if legs.is_empty() || package_units == 0 {
        return (None, MarkBasis::Absent);
    }
    let units = Decimal::from(package_units);
    let mid = legs.iter().try_fold(Decimal::ZERO, |sum, leg| {
        let price = newest_mid(chain, leg.contract.as_str(), at)?;
        Some(sum + signed_flow(leg, price.as_decimal()))
    });
    if let Some(total) = mid {
        return (Some(Money::new(total / units)), MarkBasis::ChainMid);
    }
    let fill = legs.iter().fold(Decimal::ZERO, |sum, leg| {
        sum + signed_flow(leg, leg.price.as_decimal())
    });
    (Some(Money::new(fill / units)), MarkBasis::FillPrice)
}

/// Signed leg notional: buys pay, sells receive, scaled by leg contracts.
fn signed_flow(leg: &LegExecution, price: Decimal) -> Decimal {
    let sign = match leg.side {
        OrderSide::Buy => Decimal::ONE,
        OrderSide::Sell => Decimal::NEGATIVE_ONE,
    };
    sign * price * Decimal::from(leg.quantity)
}

/// The newest non-crossed mid for `occ` at-or-before `at`, when one exists. A
/// crossed newest quote yields `None` rather than an older, staler one: the
/// leg mark fails closed to the fill-price fallback.
fn newest_mid(chain: &[ChainQuote], occ: &str, at: DateTime<Utc>) -> Option<Money> {
    chain
        .iter()
        .filter(|quote| quote.contract.occ_symbol() == occ && quote.timestamp <= at)
        .max_by_key(|quote| quote.timestamp)
        .and_then(ChainQuote::midpoint)
}

#[cfg(test)]
#[path = "../tests/marks.rs"]
mod tests;
