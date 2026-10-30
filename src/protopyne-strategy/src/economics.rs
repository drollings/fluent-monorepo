//! Spread economics: one pure pricer for a defined-risk option package.
//!
//! The input legs are already validated by [`SpreadLegs`]; this module
//! re-validates at the boundary and prices against at-or-before quotes the
//! caller sliced. `net_per_unit` is signed (positive debit, negative credit)
//! and every amount is per spread unit in quote units; fees are per contract
//! and `qty` scales the capital requirement. Refusals reuse the shared
//! defined-risk vocabulary — a naked short is never priced.

use rust_decimal::Decimal;
use thiserror::Error;

use protopyne_types::{
    ChainQuote, ContractId, LegSide, Money, OptionKind, Qty, SpreadLeg, SpreadLegs,
};

use crate::proposal::StructureKind;

/// Which side of the package an order takes. Debit packages buy, credit
/// packages sell; the runner maps this to the venue's order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PackageSide {
    /// Pay the package (a debit).
    Buy,
    /// Receive the package (a credit).
    Sell,
}

impl std::fmt::Display for PackageSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Buy => f.write_str("buy"),
            Self::Sell => f.write_str("sell"),
        }
    }
}

/// Why a package could not be priced. Structural refusals carry the shared
/// [`protopyne_types::TypesError`] rather than a second taxonomy.
#[derive(Debug, Error)]
pub enum EconRefusal {
    /// The leg set failed the shared defined-risk validation.
    #[error("{0}")]
    DefinedRisk(#[from] protopyne_types::TypesError),
    /// No at-or-before quote exists for a leg's contract.
    #[error("no quote for {0}")]
    MissingQuote(ContractId),
    /// The leg's newest quote is crossed, so it has no valid mid.
    #[error("crossed quote for {0}")]
    CrossedQuote(ContractId),
    /// A package must cover at least one contract.
    #[error("quantity must be positive")]
    ZeroQuantity,
    /// A negative commission would hide cost in the max-loss bound.
    #[error("fee per contract must be non-negative")]
    NegativeFee,
    /// The leg set has no defined-risk identity this pricer can bound.
    #[error("unsupported leg set for defined-risk economics: {0}")]
    UnsupportedShape(String),
    /// A credit package with no strike width has exposure this pricer
    /// cannot bound.
    #[error("unbounded credit package: {0}")]
    UnboundedRisk(String),
}

/// Priced economics for one package, per spread unit unless noted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpreadEconomics {
    /// Signed package price per unit: positive is a debit, negative a credit.
    pub net_per_unit: Money,
    /// Executable width of the whole package: the price a taker crosses
    /// between the package bid and its ask. The grant's `max_spread_width`
    /// reads this, never the underlying's bar spread.
    pub bid_ask_width: Money,
    /// Distance between the defining strikes, `None` for calendars/singles.
    pub package_width: Option<Money>,
    /// Worst-case loss per unit including fees.
    pub max_loss_per_unit: Money,
    /// Worst-case loss for `qty` units.
    pub capital_requirement: Money,
    /// Order side implied by the sign of the net.
    pub side: PackageSide,
    /// Signed package limit at mid.
    pub mid_limit: Money,
    /// Always `true` on a returned price: unbounded sets refuse instead.
    pub bounded: bool,
}

impl SpreadEconomics {
    /// Signed package limit at mid, widened by `tolerance_bps` on the adverse
    /// side: a buyer pays up, a seller accepts less credit. The tolerance is
    /// the operator's worst acceptable price, not a fill-model estimate.
    #[must_use]
    pub fn limit_with_tolerance(&self, tolerance_bps: u32) -> Money {
        let amount = self.mid_limit.as_decimal();
        let rate = Decimal::from(tolerance_bps) / Decimal::from(10_000);
        let tolerance = amount.abs() * rate;
        Money::new(match self.side {
            PackageSide::Buy => amount + tolerance,
            PackageSide::Sell => amount - tolerance,
        })
    }
}

/// One unit's economics: the per-unit price, loss, width, and side a
/// sizing policy and the containment caps read. A share's unit is its price
/// and the caller's stated per-trade worst case; a package's unit comes from
/// the one pricer at a single unit, so sizing never sees a quantity-scaled
/// number it could mistake for a per-unit one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnitEconomics {
    /// Signed price per unit: a package's net debit/credit, a share's price.
    pub price_per_unit: Money,
    /// Worst-case loss for one unit, fees included.
    pub max_loss_per_unit: Money,
    /// Executable width per unit (the package bid/ask cross, zero for shares).
    pub bid_ask_width: Money,
    /// Order side implied by the unit's sign.
    pub side: PackageSide,
}

/// One selected instrument: its shape, legs, expiry, and unit economics.
/// The sizing policy and the containment caps read the unit; the proposal
/// carries the capital the sized quantity commits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnitSelection {
    /// Structure to propose.
    pub kind: StructureKind,
    /// Concrete legs, when selected from a chain.
    pub legs: Option<SpreadLegs>,
    /// Days to the selected expiry.
    pub dte_days: u32,
    /// The unit the policy sizes and the caps contain.
    pub unit: UnitEconomics,
    /// The one-pricer's package economics at a single unit, when the unit
    /// was chain-priced. The proposal rescales only the capital requirement
    /// to the sized quantity; every other field is already per-unit, so the
    /// rescaled economics read exactly as a sized pricing would.
    pub economics: Option<SpreadEconomics>,
}

impl SpreadEconomics {
    /// The per-unit economics of this priced package. Every field but the
    /// capital requirement is already per-unit (the pricer scales only the
    /// requirement by quantity), so the unit is exact at any quantity.
    #[must_use]
    pub fn unit(&self) -> UnitEconomics {
        UnitEconomics {
            price_per_unit: self.net_per_unit,
            max_loss_per_unit: self.max_loss_per_unit,
            bid_ask_width: self.bid_ask_width,
            side: self.side,
        }
    }
}

/// Classified bound for a leg set.
enum Shape {
    /// A vertical or condor: width times ratio bounds the short side.
    Width { width: Decimal, ratio: Decimal },
    /// A single long option or a calendar: the debit is the bound.
    DebitPremium,
}

/// Price `legs` against `quotes` (already sliced to at-or-before) for `qty`
/// units at `fee_per_contract` per leg contract.
pub fn spread_economics(
    legs: &SpreadLegs,
    quotes: &[ChainQuote],
    qty: Qty,
    fee_per_contract: Money,
) -> Result<SpreadEconomics, EconRefusal> {
    legs.validate()?;
    if qty.as_decimal() <= Decimal::ZERO {
        return Err(EconRefusal::ZeroQuantity);
    }
    if fee_per_contract.as_decimal() < Decimal::ZERO {
        return Err(EconRefusal::NegativeFee);
    }
    let mut net = Decimal::ZERO;
    let mut net_bid = Decimal::ZERO;
    let mut net_ask = Decimal::ZERO;
    let mut fees = Decimal::ZERO;
    for leg in legs.legs() {
        let quote = quote_for(leg, quotes)?;
        let ratio = Decimal::from(leg.ratio);
        let sign = match leg.side {
            LegSide::Long => Decimal::ONE,
            LegSide::Short => Decimal::NEGATIVE_ONE,
        };
        net += sign * quote.midpoint.as_decimal() * ratio;
        net_bid += sign * quote.bid.as_decimal() * ratio;
        net_ask += sign * quote.ask.as_decimal() * ratio;
        fees += fee_per_contract.as_decimal() * ratio;
    }
    let shape = classify(legs.legs())?;
    let package_width = match &shape {
        Shape::Width { width, ratio } => Some(Money::new(*width * *ratio)),
        Shape::DebitPremium => None,
    };
    let bound = match shape {
        Shape::Width { width, ratio } => {
            if net > Decimal::ZERO {
                net
            } else {
                width * ratio + net
            }
        }
        Shape::DebitPremium => {
            if net < Decimal::ZERO {
                return Err(EconRefusal::UnboundedRisk(
                    "credit package has no strike width".to_owned(),
                ));
            }
            net
        }
    };
    let max_loss = Money::new(bound + fees);
    let side = if net < Decimal::ZERO {
        PackageSide::Sell
    } else {
        PackageSide::Buy
    };
    Ok(SpreadEconomics {
        net_per_unit: Money::new(net),
        bid_ask_width: Money::new((net_ask - net_bid).abs()),
        package_width,
        max_loss_per_unit: max_loss,
        capital_requirement: Money::new(max_loss.as_decimal() * qty.as_decimal()),
        side,
        mid_limit: Money::new(net),
        bounded: true,
    })
}

/// The newest at-or-before quote for this leg's contract, with its mid
/// resolved once: a crossed book has no valid mid and refuses here.
fn quote_for(leg: &SpreadLeg, quotes: &[ChainQuote]) -> Result<ResolvedQuote, EconRefusal> {
    let quote = quotes
        .iter()
        .filter(|quote| quote.contract == leg.contract)
        .max_by_key(|quote| quote.timestamp)
        .ok_or_else(|| EconRefusal::MissingQuote(leg.contract.contract_id()))?;
    let midpoint = quote
        .midpoint()
        .ok_or_else(|| EconRefusal::CrossedQuote(leg.contract.contract_id()))?;
    Ok(ResolvedQuote {
        bid: quote.bid,
        ask: quote.ask,
        midpoint,
    })
}

/// One leg's book and its resolved mid.
struct ResolvedQuote {
    bid: Money,
    ask: Money,
    midpoint: Money,
}

/// Recognize the defined-risk shapes this pricer bounds. Ratios must match
/// within a package so a strike width scales the whole position.
/// The pricing shape of a validated leg package: what the pricer needs
/// (width, premium) rather than what selection needed (grid, delta).
/// Runs *after* [`SpreadLegs::try_new`](protopyne_types::SpreadLegs::try_new) —
/// the defined-risk authority stays there; this maps validated legs to
/// pricing shapes and never re-decides validity (M5.4: sequential stages,
/// not duplicate shape knowledge).
fn classify(legs: &[SpreadLeg]) -> Result<Shape, EconRefusal> {
    match legs {
        [_] => Ok(Shape::DebitPremium),
        [left, right] => {
            let same_ratio = left.ratio == right.ratio;
            let opposite = left.side != right.side;
            let same_kind = left.contract.kind == right.contract.kind;
            if !(same_ratio && opposite && same_kind) {
                return Err(unsupported(
                    "two legs must share a kind and ratio and oppose each other",
                ));
            }
            let same_expiry = left.contract.expiry == right.contract.expiry;
            let same_strike = left.contract.strike == right.contract.strike;
            if same_expiry && !same_strike {
                let width =
                    (left.contract.strike.as_decimal() - right.contract.strike.as_decimal()).abs();
                Ok(Shape::Width {
                    width,
                    ratio: Decimal::from(left.ratio),
                })
            } else if !same_expiry && same_strike {
                Ok(Shape::DebitPremium)
            } else {
                Err(unsupported(
                    "two legs must be a same-expiry vertical or a same-strike calendar",
                ))
            }
        }
        [first, second, third, fourth] => {
            let all = [first, second, third, fourth];
            let ratio = all[0].ratio;
            if all.iter().any(|leg| leg.ratio != ratio) {
                return Err(unsupported("condor legs must share one ratio"));
            }
            let expiry = all[0].contract.expiry;
            if all.iter().any(|leg| leg.contract.expiry != expiry) {
                return Err(unsupported("condor legs must share one expiry"));
            }
            let mut puts = all
                .iter()
                .filter(|leg| leg.contract.kind == OptionKind::Put);
            let mut calls = all
                .iter()
                .filter(|leg| leg.contract.kind == OptionKind::Call);
            let (put_left, put_right) = (puts.next(), puts.next());
            let (call_left, call_right) = (calls.next(), calls.next());
            let (Some(put_left), Some(put_right), Some(call_left), Some(call_right)) =
                (put_left, put_right, call_left, call_right)
            else {
                return Err(unsupported("a condor holds two puts and two calls"));
            };
            let width = |left: &SpreadLeg, right: &SpreadLeg, long_below: bool| {
                let (long, short) = match (left.side, right.side) {
                    (LegSide::Long, LegSide::Short) => (left, right),
                    (LegSide::Short, LegSide::Long) => (right, left),
                    _ => return Err(unsupported("a condor leg pair must oppose sides")),
                };
                let long_strike = long.contract.strike.as_decimal();
                let short_strike = short.contract.strike.as_decimal();
                if (long_strike < short_strike) != long_below || long_strike == short_strike {
                    return Err(unsupported(
                        "condor wings must sit outside the short strikes",
                    ));
                }
                Ok((long_strike - short_strike).abs())
            };
            let put_width = width(put_left, put_right, true)?;
            let call_width = width(call_left, call_right, false)?;
            Ok(Shape::Width {
                width: put_width.max(call_width),
                ratio: Decimal::from(ratio),
            })
        }
        _ => Err(unsupported(
            "only 1, 2, or 4-leg defined-risk packages price",
        )),
    }
}

fn unsupported(reason: &str) -> EconRefusal {
    EconRefusal::UnsupportedShape(reason.to_owned())
}

#[cfg(test)]
#[path = "../tests/economics.rs"]
mod tests;
