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
//! - Expiry rule: the nearest expiry at-or-after `dte_days` from `as_of`,
//!   refusing one that overshoots the request's `max_dte_days` window.
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

use crate::proposal::{Direction, StructureKind};

/// Pinned leg-selection policy. Built through the builder so construction
/// reads like configuration and stays forward-compatible; the policy itself
/// lives in the documented rules above, frozen by tests.
#[derive(Debug, Clone, Copy, Default, bon::Builder)]
#[builder(start_fn = new)]
pub struct LegSelector {}

/// How the near-the-money anchor strike is chosen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StrikePolicy {
    /// The listed strike closest to the underlying touch.
    #[default]
    NearestToTouch,
    /// The listed strike whose absolute delta is closest to `delta`.
    DeltaTarget {
        /// Target absolute delta in `(0, 1]`.
        delta: Decimal,
    },
}

/// One directional selection request. The caller supplies the touch and the
/// time slice; the selector never reads a clock.
#[derive(Debug, Clone, Copy)]
pub struct LegRequest<'a> {
    /// Requested structure.
    pub structure: StructureKind,
    /// Direction of exposure.
    pub direction: Direction,
    /// Credit intent: the near-the-money anchor is the short leg.
    pub credit: bool,
    /// Anchor strike policy.
    pub strike_policy: StrikePolicy,
    /// Minimum days to expiry.
    pub dte_days: u32,
    /// Upper bound on days to expiry, when the caller knows one (the
    /// authority's window). `None` leaves selection unbounded; the nearest
    /// listed expiry is then chosen whatever it is.
    pub max_dte_days: Option<u32>,
    /// Underlying root.
    pub underlying: &'a str,
    /// Underlying touch used by the nearest-to-touch policy.
    pub touch: Money,
    /// Selection date for the DTE rule.
    pub as_of: NaiveDate,
}

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
    /// The nearest listed expiry clears the requested minimum but overshoots
    /// the request's window: nothing listed fits.
    #[error(
        "nearest listed expiry is {nearest_dte} days out, past the \
             {max_dte_days}-day maximum"
    )]
    ExpiryBeyondMaxDte {
        /// Days to the nearest listed expiry.
        nearest_dte: u32,
        /// Requested maximum days to expiry.
        max_dte_days: u32,
    },
    /// The eligible expiries list too few strikes for this structure.
    #[error("no suitable strikes: {0}")]
    NoSuitableStrikes(String),
    /// The candidate legs failed spread validation (e.g. a naked short).
    #[error("invalid spread: {0}")]
    InvalidSpread(String),
    /// No underlying touch is available to anchor a strike.
    #[error("no underlying touch to anchor strike selection")]
    NoTouch,
    /// The delta policy found no quote carrying greeks; no heuristic
    /// fallback is allowed.
    #[error("delta-target selection needs greeks and none are present")]
    MissingGreeks,
    /// The requested delta target sits outside `(0, 1]`.
    #[error("delta target {0} outside (0, 1]")]
    InvalidDeltaTarget(Decimal),
}

impl LegSelector {
    /// Select legs with the default policy: long direction, debit intent,
    /// nearest-to-touch strikes.
    pub fn select_legs(
        &self,
        structure: StructureKind,
        dte_days: u32,
        underlying: &str,
        touch: Money,
        as_of: NaiveDate,
        quotes: &[ChainQuote],
    ) -> Result<SpreadLegs, LegSelectRefusal> {
        self.select_directional(
            &LegRequest {
                structure,
                direction: Direction::Long,
                credit: false,
                strike_policy: StrikePolicy::NearestToTouch,
                dte_days,
                max_dte_days: None,
                underlying,
                touch,
                as_of,
            },
            quotes,
        )
    }

    /// Select legs for a directional, policy-driven request over the
    /// at-or-before `quotes`. This is the one selection implementation;
    /// [`select_legs`](Self::select_legs) delegates here.
    pub fn select_directional(
        &self,
        request: &LegRequest<'_>,
        quotes: &[ChainQuote],
    ) -> Result<SpreadLegs, LegSelectRefusal> {
        if let StrikePolicy::DeltaTarget { delta } = request.strike_policy {
            if delta <= Decimal::ZERO || delta > Decimal::ONE {
                return Err(LegSelectRefusal::InvalidDeltaTarget(delta));
            }
        }
        let ours: Vec<&ChainQuote> = quotes
            .iter()
            .filter(|quote| quote.contract.underlying == request.underlying)
            .collect();
        if ours.is_empty() {
            return Err(LegSelectRefusal::EmptySnapshot);
        }
        Self::choose(request, &ours)
    }

    fn choose(
        request: &LegRequest<'_>,
        ours: &[&ChainQuote],
    ) -> Result<SpreadLegs, LegSelectRefusal> {
        match request.structure {
            StructureKind::Single => Err(LegSelectRefusal::NotApplicable),
            StructureKind::NakedShort => {
                // Expressible but outside the defined-risk envelope: let the
                // one spread validator refuse it, never a second check here.
                let nearest = nearest_contract(ours, request.touch).expect("snapshot nonempty");
                SpreadLegs::try_new(vec![SpreadLeg::new()
                    .contract(nearest.clone())
                    .side(LegSide::Short)
                    .build()])
                .map_err(|refused| LegSelectRefusal::InvalidSpread(refused.to_string()))
            }
            StructureKind::Vertical => Self::vertical(request, ours),
            StructureKind::Calendar => Self::calendar(request, ours),
            StructureKind::Condor => Self::condor(request, ours),
        }
    }

    /// The anchor is the near-the-money leg; the wing sits one strike farther
    /// out. Debit intents buy the anchor, credit intents sell it.
    fn vertical(
        request: &LegRequest<'_>,
        ours: &[&ChainQuote],
    ) -> Result<SpreadLegs, LegSelectRefusal> {
        let front = front_expiry(ours, request.dte_days, request.max_dte_days, request.as_of)?;
        let kind = match (request.direction, request.credit) {
            (Direction::Long, false) | (Direction::Short, true) => OptionKind::Call,
            (Direction::Long, true) | (Direction::Short, false) => OptionKind::Put,
        };
        let grid = strike_grid(ours, front, kind);
        let anchor = match request.strike_policy {
            StrikePolicy::NearestToTouch => greatest_below_or_lowest(&grid, request.touch),
            StrikePolicy::DeltaTarget { delta } => {
                delta_strike(ours, front, kind, delta).map(Some)?
            }
        }
        .ok_or_else(|| {
            LegSelectRefusal::NoSuitableStrikes(format!("no listed {kind} strikes on {front}"))
        })?;
        let wing = match kind {
            OptionKind::Call => smallest_above(&grid, &anchor),
            OptionKind::Put => greatest_below(&grid, &anchor),
        }
        .ok_or_else(|| {
            LegSelectRefusal::NoSuitableStrikes(format!(
                "no {kind} strike beyond {anchor} on {front}"
            ))
        })?;
        let anchor_side = if request.credit {
            LegSide::Short
        } else {
            LegSide::Long
        };
        let wing_side = match anchor_side {
            LegSide::Long => LegSide::Short,
            LegSide::Short => LegSide::Long,
        };
        Self::validated(vec![
            leg(contract_of(ours, front, &anchor, kind), anchor_side),
            leg(contract_of(ours, front, &wing, kind), wing_side),
        ])
    }

    /// Calendars stay debit: long the back expiry, short the front. Direction
    /// picks the option kind; a credit calendar has no bounded width.
    fn calendar(
        request: &LegRequest<'_>,
        ours: &[&ChainQuote],
    ) -> Result<SpreadLegs, LegSelectRefusal> {
        let front = front_expiry(ours, request.dte_days, request.max_dte_days, request.as_of)?;
        let back = ours
            .iter()
            .map(|quote| quote.contract.expiry)
            .filter(|expiry| *expiry > front)
            .min()
            .ok_or_else(|| {
                LegSelectRefusal::NoSuitableStrikes(format!("no listed expiry after front {front}"))
            })?;
        let kind = match request.direction {
            Direction::Long => OptionKind::Call,
            Direction::Short => OptionKind::Put,
        };
        let front_grid = strike_grid(ours, front, kind);
        let back_set: BTreeSet<Decimal> = strike_grid(ours, back, kind).into_iter().collect();
        let common: Vec<Decimal> = front_grid
            .into_iter()
            .filter(|strike| back_set.contains(strike))
            .collect();
        let strike = match request.strike_policy {
            StrikePolicy::NearestToTouch => common.into_iter().min_by(|a, b| {
                distance(*a, request.touch)
                    .cmp(&distance(*b, request.touch))
                    .then(a.cmp(b))
            }),
            StrikePolicy::DeltaTarget { delta } => {
                let mut best: Option<(Decimal, Decimal)> = None;
                for strike in &common {
                    let Some(quote) = ours.iter().find(|quote| {
                        quote.contract.expiry == back
                            && quote.contract.kind == kind
                            && quote.contract.strike.as_decimal() == *strike
                    }) else {
                        continue;
                    };
                    let Some(greeks) = quote.greeks else {
                        continue;
                    };
                    let Some(distance) = delta_distance(greeks.delta, delta) else {
                        continue;
                    };
                    if is_better_delta(distance, *strike, best) {
                        best = Some((distance, *strike));
                    }
                }
                best.map(|(_, strike)| strike)
            }
        }
        .ok_or_else(|| {
            LegSelectRefusal::NoSuitableStrikes(format!(
                "no common {kind} strike on {front} and {back}"
            ))
        })?;
        Self::validated(vec![
            leg(contract_of(ours, back, &strike, kind), LegSide::Long),
            leg(contract_of(ours, front, &strike, kind), LegSide::Short),
        ])
    }

    /// Iron condor: short the inner strikes, long the wings. Credit is the
    /// canonical shape; direction does not change the symmetric package.
    fn condor(
        request: &LegRequest<'_>,
        ours: &[&ChainQuote],
    ) -> Result<SpreadLegs, LegSelectRefusal> {
        let front = front_expiry(ours, request.dte_days, request.max_dte_days, request.as_of)?;
        let mut below: Vec<&ChainQuote> = ours
            .iter()
            .copied()
            .filter(|quote| {
                quote.contract.expiry == front
                    && quote.contract.kind == OptionKind::Put
                    && quote.contract.strike < request.touch
            })
            .collect();
        below.sort_by_key(|quote| std::cmp::Reverse(quote.contract.strike));
        let mut above: Vec<&ChainQuote> = ours
            .iter()
            .copied()
            .filter(|quote| {
                quote.contract.expiry == front
                    && quote.contract.kind == OptionKind::Call
                    && quote.contract.strike >= request.touch
            })
            .collect();
        above.sort_by_key(|quote| quote.contract.strike);
        if below.len() < 2 || above.len() < 2 {
            return Err(LegSelectRefusal::NoSuitableStrikes(format!(
                "condor needs two put strikes below and two call strikes \
                 at-or-above the touch on {front}"
            )));
        }
        let put_index = match request.strike_policy {
            StrikePolicy::NearestToTouch => 0,
            StrikePolicy::DeltaTarget { delta } => delta_index(&below, delta)?,
        };
        let call_index = match request.strike_policy {
            StrikePolicy::NearestToTouch => 0,
            StrikePolicy::DeltaTarget { delta } => delta_index(&above, delta)?,
        };
        if below.len() < put_index + 2 || above.len() < call_index + 2 {
            return Err(LegSelectRefusal::NoSuitableStrikes(format!(
                "condor delta anchor {put_index}/{call_index} leaves no wing on {front}"
            )));
        }
        Self::validated(vec![
            leg(below[put_index + 1].contract.clone(), LegSide::Long),
            leg(below[put_index].contract.clone(), LegSide::Short),
            leg(above[call_index].contract.clone(), LegSide::Short),
            leg(above[call_index + 1].contract.clone(), LegSide::Long),
        ])
    }

    fn validated(legs: Vec<SpreadLeg>) -> Result<SpreadLegs, LegSelectRefusal> {
        SpreadLegs::try_new(legs)
            .map_err(|refused| LegSelectRefusal::InvalidSpread(refused.to_string()))
    }
}

fn money(strike: Decimal) -> Money {
    Money::new(strike)
}

/// Distance between a quote's absolute delta and a decimal target.
fn delta_distance(delta: f64, target: Decimal) -> Option<Decimal> {
    Decimal::from_f64_retain(delta.abs()).map(|delta| (delta - target).abs())
}

fn distance(strike: Decimal, touch: Money) -> Decimal {
    (strike - touch.as_decimal()).abs()
}

/// Nearest listed expiry at-or-after `dte_days` from `as_of`, refusing one
/// that overshoots `max_dte_days`: no listed date inside the window is a
/// data inadequacy, not a reason to trade the nearest date on the board.
fn front_expiry(
    quotes: &[&ChainQuote],
    dte_days: u32,
    max_dte_days: Option<u32>,
    as_of: NaiveDate,
) -> Result<NaiveDate, LegSelectRefusal> {
    let (nearest_dte, nearest) = quotes
        .iter()
        .map(|quote| (dte_of(quote.contract.expiry, as_of), quote.contract.expiry))
        .filter(|(dte, _)| *dte >= dte_days)
        .min_by_key(|(dte, expiry)| (*dte, *expiry))
        .ok_or(LegSelectRefusal::NoExpiryForDte { dte_days })?;
    if let Some(max) = max_dte_days.filter(|max| nearest_dte > *max) {
        return Err(LegSelectRefusal::ExpiryBeyondMaxDte {
            nearest_dte,
            max_dte_days: max,
        });
    }
    Ok(nearest)
}

/// Days from `as_of` to `expiry`, floored at zero for an already-passed
/// date and saturated for an unrepresentable distance.
pub(crate) fn dte_of(expiry: NaiveDate, as_of: NaiveDate) -> u32 {
    u32::try_from(expiry.signed_duration_since(as_of).num_days().max(0)).unwrap_or(u32::MAX)
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

/// Greatest listed strike strictly below `strike`.
fn greatest_below(grid: &[Decimal], strike: &Decimal) -> Option<Decimal> {
    grid.iter().filter(|listed| *listed < strike).max().copied()
}

/// Strike on `expiry`/`kind` whose absolute delta is closest to `target`.
/// Quotes without greeks are skipped; if none carry greeks the request
/// refuses rather than falling back to the touch.
/// Whether `(distance, strike)` beats the running best: strictly closer
/// wins; exact-distance ties go to the lower strike. The one home for the
/// three delta loops — `delta_strike`, `delta_index`, and the calendar arm
/// (M3.3). `nearest_contract` (touch metric) and `contract_of` (identity
/// lookup) are different queries and stay separate.
fn is_better_delta(distance: Decimal, strike: Decimal, best: Option<(Decimal, Decimal)>) -> bool {
    best.is_none_or(|(best_distance, best_strike)| {
        distance < best_distance || (distance == best_distance && strike < best_strike)
    })
}

fn delta_strike(
    quotes: &[&ChainQuote],
    expiry: NaiveDate,
    kind: OptionKind,
    target: Decimal,
) -> Result<Decimal, LegSelectRefusal> {
    let mut best: Option<(Decimal, Decimal)> = None;
    for quote in quotes {
        if quote.contract.expiry != expiry || quote.contract.kind != kind {
            continue;
        }
        let Some(greeks) = quote.greeks else {
            continue;
        };
        let Some(distance) = delta_distance(greeks.delta, target) else {
            continue;
        };
        let strike = quote.contract.strike.as_decimal();
        if is_better_delta(distance, strike, best) {
            best = Some((distance, strike));
        }
    }
    best.map(|(_, strike)| strike)
        .ok_or(LegSelectRefusal::MissingGreeks)
}

/// Index of the quote whose absolute delta is closest to `target`, ties to
/// the lower strike. Refuses when no quote carries greeks.
fn delta_index(quotes: &[&ChainQuote], target: Decimal) -> Result<usize, LegSelectRefusal> {
    let mut best: Option<((Decimal, Decimal), usize)> = None;
    for (index, quote) in quotes.iter().enumerate() {
        let Some(greeks) = quote.greeks else {
            continue;
        };
        let Some(distance) = delta_distance(greeks.delta, target) else {
            continue;
        };
        let strike = quote.contract.strike.as_decimal();
        if is_better_delta(distance, strike, best.map(|(pair, _)| pair)) {
            best = Some(((distance, strike), index));
        }
    }
    best.map(|(_, index)| index)
        .ok_or(LegSelectRefusal::MissingGreeks)
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
