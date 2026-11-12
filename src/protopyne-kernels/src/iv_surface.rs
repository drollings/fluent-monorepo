//! One-expiry implied-volatility surface: fit over a chain snapshot.
//!
//! The fit inverts mid-prices through [`implied_vol`] at each listed strike
//! and returns the per-strike IVs with a documented interpolation rule
//! (linear in strike between fitted points, flat past the wings). It refuses
//! — with the shortfall or the contract named — when the chain is
//! under-determined: fewer than three distinct strikes, mixed expiries, an
//! expired or dateless expiry, or a crossed quote.

use chrono::NaiveDate;
use rust_decimal::prelude::ToPrimitive as _;

use protopyne_types::{ChainQuote, OptionKind};

use crate::error::KernelError;
use crate::greeks::{implied_vol, positive_finite};

/// Minimum distinct strikes that determine a surface: level, slope, and one
/// over-determining point.
pub const MIN_SURFACE_STRIKES: usize = 3;

/// One fitted surface point: the strike and its implied volatility.
#[derive(Debug, Clone, Copy, PartialEq)]
/// One fitted surface point: the strike and its implied volatility.
pub struct IvPoint {
    /// Strike price per share.
    pub strike: f64,
    /// Implied volatility at this strike.
    pub iv: f64,
}

/// An implied-volatility surface at one expiry: per-strike IVs ascending by
/// strike, with linear interpolation between points and flat wings.
#[derive(Debug, Clone, PartialEq)]
/// An implied-volatility surface at one expiry: per-strike IVs ascending by
/// strike, with linear interpolation between points and flat wings.
pub struct IvSurface {
    /// Expiry every fitted quote names.
    pub expiry: NaiveDate,
    /// Spot the fit was taken against.
    pub spot: f64,
    /// Rate the fit was taken against.
    pub rate: f64,
    /// Fitted points, ascending by strike.
    pub points: Vec<IvPoint>,
}

impl IvSurface {
    /// Interpolate the surface at `strike`: linear between the bracketing
    /// fitted points, flat (nearest wing) outside the fitted range. A
    /// single-point surface cannot interpolate — fitting refuses those, so
    /// this never sees one.
    #[must_use]
    pub fn iv_at(&self, strike: f64) -> f64 {
        let mut below: Option<&IvPoint> = None;
        for point in &self.points {
            if point.strike >= strike {
                return match below {
                    None => point.iv,
                    Some(lower) => {
                        let span = point.strike - lower.strike;
                        if span <= 0.0 {
                            return lower.iv;
                        }
                        let weight = (strike - lower.strike) / span;
                        lower.iv + weight * (point.iv - lower.iv)
                    }
                };
            }
            below = Some(point);
        }
        below.map_or(f64::NAN, |point| point.iv)
    }
}

/// Days per year for expiry-to-years conversion (ACT/365, stated not fitted).
pub const DAYS_PER_YEAR: f64 = 365.0;

/// Fit the surface over `quotes` — every quote at the same expiry — against
/// `spot` and `rate` as of `as_of`.
///
/// Each strike contributes one point: when a strike lists both a call and a
/// put, the call prices the point (calls carry the cleaner borrow-free
/// bound at the wings). Refuses when the chain is under-determined or
/// dishonest: the shortfall or the offending contract is named.
pub fn fit_surface(
    quotes: &[ChainQuote],
    spot: f64,
    rate: f64,
    as_of: NaiveDate,
) -> Result<IvSurface, KernelError> {
    if !(positive_finite(spot) && rate.is_finite()) {
        return Err(KernelError::InvalidInput(format!(
            "surface fit needs a positive finite spot and a finite rate, got {spot}/{rate}"
        )));
    }
    let expiry = quotes.first().map(|quote| quote.contract.expiry);
    let Some(expiry) = expiry else {
        return Err(KernelError::InvalidInput(
            "surface fit needs at least one quote".to_owned(),
        ));
    };
    if quotes.iter().any(|quote| quote.contract.expiry != expiry) {
        return Err(KernelError::InvalidInput(format!(
            "surface fit needs one expiry, got quotes past {expiry}"
        )));
    }
    let days = (expiry - as_of).num_days();
    if days <= 0 {
        return Err(KernelError::InvalidInput(format!(
            "surface fit needs a live expiry, {expiry} is not past {as_of}"
        )));
    }
    let days = u32::try_from(days).map_err(|_| {
        KernelError::InvalidInput(format!("expiry {expiry} is absurdly far past {as_of}"))
    })?;
    let years = f64::from(days) / DAYS_PER_YEAR;
    // One point per strike, calls preferred where both sides list.
    let mut by_strike: Vec<(f64, &ChainQuote)> = Vec::new();
    for quote in quotes {
        let strike = quote
            .contract
            .strike
            .as_decimal()
            .to_f64()
            .filter(|strike| *strike > 0.0 && strike.is_finite())
            .ok_or_else(|| {
                KernelError::InvalidInput(format!(
                    "surface fit needs a positive finite strike, got {}",
                    quote.contract.occ_symbol()
                ))
            })?;
        match by_strike
            .iter_mut()
            .find(|(listed, _)| (*listed - strike).abs() <= f64::EPSILON)
        {
            Some(slot) => {
                if quote.contract.kind == OptionKind::Call {
                    slot.1 = quote;
                }
            }
            None => by_strike.push((strike, quote)),
        }
    }
    if by_strike.len() < MIN_SURFACE_STRIKES {
        return Err(KernelError::InvalidInput(format!(
            "surface under-determined: {} distinct strikes, need {}",
            by_strike.len(),
            MIN_SURFACE_STRIKES
        )));
    }
    by_strike.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut points = Vec::with_capacity(by_strike.len());
    for (strike, quote) in by_strike {
        let bid = protopyne_types::bar_leg_as_f64(quote.bid.as_decimal());
        let ask = protopyne_types::bar_leg_as_f64(quote.ask.as_decimal());
        if bid.is_nan() || ask.is_nan() {
            return Err(KernelError::InvalidInput(format!(
                "quote on {} has a non-finite market: bid {bid} ask {ask}",
                quote.contract.occ_symbol()
            )));
        }
        if bid > ask {
            return Err(KernelError::InvalidInput(format!(
                "crossed quote on {}: bid {bid} above ask {ask}",
                quote.contract.occ_symbol()
            )));
        }
        let mid = f64::midpoint(bid, ask);
        let iv =
            implied_vol(mid, quote.contract.kind, spot, strike, years, rate).map_err(|_| {
                KernelError::InvalidInput(format!(
                    "no volatility reprices {} at mid {mid}",
                    quote.contract.occ_symbol()
                ))
            })?;
        points.push(IvPoint { strike, iv });
    }
    Ok(IvSurface {
        expiry,
        spot,
        rate,
        points,
    })
}

#[cfg(test)]
#[path = "../tests/iv_surface.rs"]
mod tests;