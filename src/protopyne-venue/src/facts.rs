//! `PortfolioFacts` producer over a venue's own book: one source of truth,
//! never a recomputation.
//!
//! [`VenueBookFacts`] reads the open per-contract spread positions the venue
//! already tracks, prices each package through the shared
//! [`spread_economics`](protopyne_strategy::spread_economics) pricer (there
//! is no second pricer), and reports buying power and net delta from the
//! venue's balance and chain surfaces. `observed` is `true` only when every
//! component a configured aggregate cap needs was actually available, so a
//! missing book, balance, or greeks feed refuses rather than reading an
//! invented zero.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use protopyne_strategy::{spread_economics, PortfolioFacts, PortfolioFactsSource, RiskLimits};
use protopyne_types::{ChainQuote, Greeks, LegSide, Money, Qty};
use rust_decimal::Decimal;

use crate::position::{PositionSide, SpreadPosition};
use crate::venue::{LegQuoteSource, Venue};

/// The one [`PortfolioFactsSource`] the live session installs: observed
/// aggregate caps read the venue's book, balances, and chain.
pub struct VenueBookFacts {
    venue: Arc<dyn Venue>,
    limits: RiskLimits,
}

impl VenueBookFacts {
    /// Bind a producer to the venue whose book it observes and the limits
    /// whose configured caps decide what must be observed.
    #[must_use]
    pub fn new(venue: Arc<dyn Venue>, limits: RiskLimits) -> Self {
        Self { venue, limits }
    }

    /// Observe the facts at one instant. Deterministic when the venue's
    /// chain source is point-in-time; the sync trait entry point passes the
    /// caller's clock.
    #[must_use]
    pub fn facts_at(&self, at: DateTime<Utc>) -> PortfolioFacts {
        let positions = self.venue.open_spread_positions();
        let packages = positions.as_deref().and_then(open_packages);

        let open_risk = packages
            .as_ref()
            .and_then(|packages| open_risk_of(packages));
        let net_greeks = packages
            .as_ref()
            .and_then(|packages| net_greeks_of(packages, self.venue.chain_source().as_deref(), at));
        let buying_power = self.buying_power();

        let observed = (self.limits.max_open_risk.is_none() || open_risk.is_some())
            && (self.limits.max_abs_net_delta.is_none() || net_greeks.is_some())
            && (self.limits.min_buying_power.is_none() || buying_power.is_some());

        PortfolioFacts {
            observed,
            open_risk: open_risk.unwrap_or_else(Money::zero),
            net_greeks,
            buying_power,
            // The venue book reports no equity line: a fraction-of-net-liq
            // policy sizes zero until an equity source is attached.
            net_liq: None,
            // Nor an equity peak or current reading: a configured drawdown
            // cap refuses as unobserved until one is attached.
            equity_peak: None,
            equity_now: None,
        }
    }

    /// Available buying power: the venue's `USD` balance line, or `None`
    /// when it reports none.
    fn buying_power(&self) -> Option<Money> {
        self.venue
            .fetch_balances()
            .into_iter()
            .find(|entry| entry.asset == "USD")
            .map(|entry| entry.amount)
    }
}

impl PortfolioFactsSource for VenueBookFacts {
    fn facts(&self) -> PortfolioFacts {
        self.facts_at(Utc::now())
    }
}

/// One open package: its structure, its held unit count, and each leg paired
/// with the per-contract position that holds it.
struct OpenPackage<'a> {
    legs: &'a protopyne_types::SpreadLegs,
    units: u32,
    pairs: Vec<(&'a protopyne_types::SpreadLeg, &'a SpreadPosition)>,
}

/// Group the venue's per-contract book into open packages. Returns `None`
/// when the book cannot be read honestly: an untagged position, mixed leg
/// ratios, a quantity that is not an exact multiple of a leg ratio, or
/// positions that do not line up one-to-one with the structure's legs.
fn open_packages(positions: &[SpreadPosition]) -> Option<Vec<OpenPackage<'_>>> {
    let mut grouped: Vec<(&protopyne_types::SpreadLegs, Vec<&SpreadPosition>)> = Vec::new();
    for position in positions {
        let legs = position.legs.as_ref()?;
        match grouped.iter_mut().find(|(known, _)| *known == legs) {
            Some((_, members)) => members.push(position),
            None => grouped.push((legs, vec![position])),
        }
    }

    let mut packages = Vec::with_capacity(grouped.len());
    for (legs, members) in grouped {
        let ratio = legs.legs().first()?.ratio;
        if legs.legs().iter().any(|leg| leg.ratio != ratio) {
            return None;
        }
        let mut used = vec![false; members.len()];
        let mut pairs = Vec::with_capacity(legs.len());
        let mut units: Option<u32> = None;
        for leg in legs.legs() {
            let want_long = leg.side == LegSide::Long;
            let index = members.iter().enumerate().position(|(index, member)| {
                !used[index] && (member.side == PositionSide::Long) == want_long
            })?;
            used[index] = true;
            let member = members[index];
            if member.quantity_abs % ratio != 0 {
                return None;
            }
            let leg_units = member.quantity_abs / ratio;
            match units {
                Some(previous) if previous != leg_units => return None,
                _ => units = Some(leg_units),
            }
            pairs.push((leg, member));
        }
        if pairs.len() != legs.len() {
            return None;
        }
        packages.push(OpenPackage {
            legs,
            units: units?,
            pairs,
        });
    }
    Some(packages)
}

/// Sum each open package's committed max loss, priced at its entry averages
/// through the shared pricer. `None` when any package is outside the
/// defined-risk shapes that pricer bounds.
fn open_risk_of(packages: &[OpenPackage<'_>]) -> Option<Money> {
    let mut total = Decimal::ZERO;
    for package in packages {
        let quotes: Vec<ChainQuote> = package
            .pairs
            .iter()
            .map(|(leg, member)| {
                ChainQuote::new()
                    .contract(leg.contract.clone())
                    .bid(member.price_entry_average)
                    .ask(member.price_entry_average)
                    .timestamp(member.time_enter)
                    .build()
            })
            .collect();
        let priced = spread_economics(
            package.legs,
            &quotes,
            Qty::new(Decimal::from(package.units)),
            Money::zero(),
        )
        .ok()?;
        total += priced.capital_requirement.as_decimal();
    }
    Some(Money::new(total))
}

/// Net portfolio delta from the chain source's point-in-time greeks. `None`
/// when there is no chain source or any open leg lacks greeks for its
/// contract.
/// Net book greeks: the signed, ratio- and unit-scaled sum of every leg's
/// Greeks. Portfolio sensitivities aggregate linearly, so the book's delta
/// is the signed sum of the legs' deltas — and likewise for gamma, theta,
/// and vega. Any leg without supplied greeks unobserves the whole book:
/// absence is not a zero.
fn net_greeks_of(
    packages: &[OpenPackage<'_>],
    chain: Option<&dyn LegQuoteSource>,
    at: DateTime<Utc>,
) -> Option<Greeks> {
    let chain = chain?;
    let mut snapshots: HashMap<String, Vec<ChainQuote>> = HashMap::new();
    let mut delta = 0.0_f64;
    let mut gamma = 0.0_f64;
    let mut theta = 0.0_f64;
    let mut vega = 0.0_f64;
    for package in packages {
        for (leg, _) in &package.pairs {
            let underlying = &leg.contract.underlying;
            let snapshot = snapshots
                .entry(underlying.clone())
                .or_insert_with(|| chain.chain_snapshot(underlying, at));
            let newest = snapshot
                .iter()
                .filter(|quote| quote.contract == leg.contract)
                .max_by_key(|quote| quote.timestamp)?;
            let leg_greeks = newest.greeks.as_ref()?;
            let sign = if leg.side == LegSide::Long { 1.0 } else { -1.0 };
            let scale = sign * f64::from(leg.ratio) * f64::from(package.units);
            delta += scale * leg_greeks.delta;
            gamma += scale * leg_greeks.gamma;
            theta += scale * leg_greeks.theta;
            vega += scale * leg_greeks.vega;
        }
    }
    [delta, gamma, theta, vega]
        .iter()
        .all(|value| value.is_finite())
        .then(|| {
            Greeks::new()
                .delta(delta)
                .gamma(gamma)
                .theta(theta)
                .vega(vega)
                .build()
        })
}

#[cfg(test)]
#[path = "../tests/facts.rs"]
mod tests;
