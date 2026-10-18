//! Leg selection: turn a multi-leg structure into concrete spread legs.
//!
//! [`LegSelector`] is pure over its inputs — no clock, no I/O. The caller
//! slices time: `quotes` must already hold only quotes at-or-before the bar
//! being decided, so selection can never borrow the future (a replay that
//! priced off later quotes would fill better than live possibly could).
//! The selector reads presence, never prices: crossed books still select,
//! and the fill policy refuses them per leg.
//!
//! Pinned policy, frozen by the fixture matrix before it prices anything:
//!
//! - Expiry rule: the nearest expiry at-or-after `dte_days` from `as_of`.
//! - Strike rule: the listed strikes closest to the underlying `touch`.
//! - Vertical: bull call spread, long the greatest strike at-or-below the
//!   touch (or the lowest listed when all sit above), short the next strike
//!   up, both on the front expiry.
//! - Calendar: one strike (closest to the touch, present on both expiries),
//!   long the next expiry out, short the front.
//! - Condor: iron condor on the front expiry — long put, short put below
//!   the touch, short call, long call at-or-above it, low strike first.
//!
//! Refusals name data inadequacy (a validity fact about the producer's
//! input, never a verdict on the trade): empty snapshots, no expiry
//! clearing DTE, or too few listed strikes all refuse instead of guessing.
//! Naked-short rejection stays in exactly one place —
//! [`SpreadLegs::try_new`](protopyne_types::SpreadLegs) — and surfaces here
//! as [`LegSelectRefusal::InvalidSpread`].

use std::collections::BTreeSet;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use thiserror::Error;

use protopyne_types::{
    ChainQuote, LegSide, Money, OptionContract, OptionKind, SpreadLeg, SpreadLegs,
};

use crate::proposal::StructureKind;

/// Pinned leg-selection policy. Built through the builder so construction
/// reads like configuration and stays forward-compatible; the policy itself
/// lives in the documented rules above, frozen by tests.
#[derive(Debug, Clone, Copy, Default, bon::Builder)]
#[builder(start_fn = new)]
pub struct LegSelector {}

/// Why leg selection refused: inadequate data for the request, never a
/// claim about the trade's economics.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum LegSelectRefusal {
    /// Single-leg proposals need no leg selection.
    #[error("single-leg proposals need no leg selection")]
    NotApplicable,
    /// No quotes for this underlying in the snapshot.
    #[error("no chain quotes for this underlying in the snapshot")]
    EmptySnapshot,
    /// No listed expiry sits at-or-after the requested DTE.
    #[error("no listed expiry at-or-after {dte_days} days")]
    NoExpiryForDte {
        /// Requested minimum days to expiry.
        dte_days: u32,
    },
    /// The eligible expiries list too few strikes for this structure.
    #[error("no suitable strikes: {0}")]
    NoSuitableStrikes(String),
    /// The candidate legs failed spread validation (e.g. a naked short).
    #[error("invalid spread: {0}")]
    InvalidSpread(String),
}

impl LegSelector {
    /// Select concrete legs for `structure` over the at-or-before `quotes`.
    pub fn select_legs(
        &self,
        structure: StructureKind,
        dte_days: u32,
        underlying: &str,
        touch: Money,
        as_of: NaiveDate,
        quotes: &[ChainQuote],
    ) -> Result<SpreadLegs, LegSelectRefusal> {
        let ours: Vec<&ChainQuote> = quotes
            .iter()
            .filter(|quote| quote.contract.underlying == underlying)
            .collect();
        if ours.is_empty() {
            return Err(LegSelectRefusal::EmptySnapshot);
        }
        match structure {
            StructureKind::Single => Err(LegSelectRefusal::NotApplicable),
            StructureKind::NakedShort => {
                // Expressible but outside the defined-risk envelope: let the
                // one spread validator refuse it, never a second check here.
                let nearest = nearest_contract(&ours, touch).expect("snapshot nonempty");
                SpreadLegs::try_new(vec![SpreadLeg::new()
                    .contract(nearest.clone())
                    .side(LegSide::Short)
                    .build()])
                .map_err(|refused| LegSelectRefusal::InvalidSpread(refused.to_string()))
            }
            StructureKind::Vertical => {
                let front = front_expiry(&ours, dte_days, as_of)?;
                let grid = strike_grid(&ours, front, OptionKind::Call);
                let long_strike = greatest_below_or_lowest(&grid, touch).ok_or_else(|| {
                    LegSelectRefusal::NoSuitableStrikes(format!(
                        "no listed call strikes on {front}"
                    ))
                })?;
                let short_strike = smallest_above(&grid, &long_strike).ok_or_else(|| {
                    LegSelectRefusal::NoSuitableStrikes(format!(
                        "no call strike above {long_strike} on {front}"
                    ))
                })?;
                Self::validated(vec![
                    leg(
                        contract_of(&ours, front, &long_strike, OptionKind::Call),
                        LegSide::Long,
                    ),
                    leg(
                        contract_of(&ours, front, &short_strike, OptionKind::Call),
                        LegSide::Short,
                    ),
                ])
            }
            StructureKind::Calendar => {
                let front = front_expiry(&ours, dte_days, as_of)?;
                let back = ours
                    .iter()
                    .map(|quote| quote.contract.expiry)
                    .filter(|expiry| *expiry > front)
                    .min()
                    .ok_or_else(|| {
                        LegSelectRefusal::NoSuitableStrikes(format!(
                            "no listed expiry after front {front}"
                        ))
                    })?;
                let front_grid = strike_grid(&ours, front, OptionKind::Call);
                let back_set: BTreeSet<Decimal> = strike_grid(&ours, back, OptionKind::Call)
                    .into_iter()
                    .collect();
                let strike = front_grid
                    .into_iter()
                    .filter(|strike| back_set.contains(strike))
                    .min_by(|a, b| distance(*a, touch).cmp(&distance(*b, touch)).then(a.cmp(b)))
                    .ok_or_else(|| {
                        LegSelectRefusal::NoSuitableStrikes(format!(
                            "no common call strike on {front} and {back}"
                        ))
                    })?;
                Self::validated(vec![
                    leg(
                        contract_of(&ours, back, &strike, OptionKind::Call),
                        LegSide::Long,
                    ),
                    leg(
                        contract_of(&ours, front, &strike, OptionKind::Call),
                        LegSide::Short,
                    ),
                ])
            }
            StructureKind::Condor => {
                let front = front_expiry(&ours, dte_days, as_of)?;
                let mut below = strike_grid(&ours, front, OptionKind::Put)
                    .into_iter()
                    .filter(|strike| money(*strike) < touch)
                    .collect::<Vec<_>>();
                below.sort_by(|a, b| b.cmp(a));
                let mut above = strike_grid(&ours, front, OptionKind::Call)
                    .into_iter()
                    .filter(|strike| money(*strike) >= touch)
                    .collect::<Vec<_>>();
                above.sort();
                if below.len() < 2 || above.len() < 2 {
                    return Err(LegSelectRefusal::NoSuitableStrikes(format!(
                        "condor needs two put strikes below and two call strikes \
                         at-or-above the touch on {front}"
                    )));
                }
                Self::validated(vec![
                    leg(
                        contract_of(&ours, front, &below[1], OptionKind::Put),
                        LegSide::Long,
                    ),
                    leg(
                        contract_of(&ours, front, &below[0], OptionKind::Put),
                        LegSide::Short,
                    ),
                    leg(
                        contract_of(&ours, front, &above[0], OptionKind::Call),
                        LegSide::Short,
                    ),
                    leg(
                        contract_of(&ours, front, &above[1], OptionKind::Call),
                        LegSide::Long,
                    ),
                ])
            }
        }
    }

    fn validated(legs: Vec<SpreadLeg>) -> Result<SpreadLegs, LegSelectRefusal> {
        SpreadLegs::try_new(legs)
            .map_err(|refused| LegSelectRefusal::InvalidSpread(refused.to_string()))
    }
}

fn money(strike: Decimal) -> Money {
    Money::new(strike)
}

fn distance(strike: Decimal, touch: Money) -> Decimal {
    (strike - touch.as_decimal()).abs()
}

/// Nearest listed expiry at-or-after `dte_days` from `as_of`.
fn front_expiry(
    quotes: &[&ChainQuote],
    dte_days: u32,
    as_of: NaiveDate,
) -> Result<NaiveDate, LegSelectRefusal> {
    quotes
        .iter()
        .map(|quote| quote.contract.expiry)
        .filter(|expiry| expiry.signed_duration_since(as_of).num_days() >= i64::from(dte_days))
        .min()
        .ok_or(LegSelectRefusal::NoExpiryForDte { dte_days })
}

/// Sorted listed strikes for one expiry and kind.
fn strike_grid(quotes: &[&ChainQuote], expiry: NaiveDate, kind: OptionKind) -> Vec<Decimal> {
    let mut grid: Vec<Decimal> = quotes
        .iter()
        .filter(|quote| quote.contract.expiry == expiry && quote.contract.kind == kind)
        .map(|quote| quote.contract.strike.as_decimal())
        .collect();
    grid.sort();
    grid.dedup();
    grid
}

/// Greatest strike at-or-below the touch, else the lowest listed.
fn greatest_below_or_lowest(grid: &[Decimal], touch: Money) -> Option<Decimal> {
    grid.iter()
        .filter(|strike| money(**strike) <= touch)
        .max()
        .or_else(|| grid.first())
        .copied()
}

/// Smallest listed strike strictly above `strike`.
fn smallest_above(grid: &[Decimal], strike: &Decimal) -> Option<Decimal> {
    grid.iter().filter(|listed| *listed > strike).min().copied()
}

/// Contract identity behind a selected grid point.
fn contract_of(
    quotes: &[&ChainQuote],
    expiry: NaiveDate,
    strike: &Decimal,
    kind: OptionKind,
) -> OptionContract {
    quotes
        .iter()
        .find(|quote| {
            quote.contract.expiry == expiry
                && quote.contract.strike.as_decimal() == *strike
                && quote.contract.kind == kind
        })
        .expect("grid points come from the snapshot")
        .contract
        .clone()
}

fn leg(contract: OptionContract, side: LegSide) -> SpreadLeg {
    SpreadLeg::new().contract(contract).side(side).build()
}

/// Closest contract to the touch, ties to the lower strike.
fn nearest_contract(quotes: &[&ChainQuote], touch: Money) -> Option<OptionContract> {
    quotes
        .iter()
        .min_by(|a, b| {
            distance(a.contract.strike.as_decimal(), touch)
                .cmp(&distance(b.contract.strike.as_decimal(), touch))
                .then(
                    a.contract
                        .strike
                        .as_decimal()
                        .cmp(&b.contract.strike.as_decimal()),
                )
        })
        .map(|quote| quote.contract.clone())
}

#[cfg(test)]
#[path = "../tests/legs.rs"]
mod tests;
