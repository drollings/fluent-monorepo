//! Black-Scholes pricing and sensitivities: one home for the closed form.
//!
//! All `f64` — never money. Prices feed calibration and the IV surface, and
//! a computed greeks source converts into [`Greeks`](protopyne_types::Greeks)
//! at the boundary. Units follow the [`Greeks`](protopyne_types::Greeks)
//! contract: theta per *day*, vega per *point* (1.0) of implied volatility,
//! rho per 1.0 of rate.
//!
//! The normal CDF is a fixed-order Taylor march of `dN/dx = φ(x)` (order 16,
//! step 0.25): every coefficient comes from the φ recurrence, so there are
//! no fitted constants to mistranscribe, and truncation sits near 1e-18 per
//! step. Symmetry is enforced (`N(−x) = 1 − N(x)` exactly), so put-call
//! parity holds to float rounding rather than to an approximation error.

use protopyne_types::OptionKind;

use crate::error::KernelError;

/// Positive and finite: the domain of spots, strikes, times, and vols.
pub(crate) fn positive_finite(value: f64) -> bool {
    value > 0.0 && value.is_finite()
}

/// Bounded bisection iterations for [`implied_vol`]: the explicit cap the
/// refusal names.
pub const IMPLIED_VOL_MAX_ITER: u32 = 200;

/// Floor of the bisection bracket: a price at intrinsic value recovers a
/// volatility near zero, never exactly zero (which would divide by zero).
pub const IMPLIED_VOL_MIN: f64 = 1e-6;

/// Ceiling of the bisection bracket before doubling takes over.
pub const IMPLIED_VOL_START_MAX: f64 = 2.0;

/// One over √2π: the normal density's constant.
const INV_SQRT_2PI: f64 = 0.3989422804014327;

/// Taylor order of the CDF march: the remainder per step is far below float
/// rounding, so the march is exact to ~1e-15.
const CDF_ORDER: usize = 16;

/// March step: small enough that the order-16 remainder vanishes, large
/// enough that the farthest queried point needs dozens of steps, not
/// thousands.
const CDF_STEP: f64 = 0.25;

/// Standard normal density.
fn norm_pdf(x: f64) -> f64 {
    (-0.5 * x * x).exp() * INV_SQRT_2PI
}

/// Standard normal CDF: symmetry is enforced exactly (`N(−x) = 1 − N(x)`),
/// so put-call parity holds to float rounding.
fn norm_cdf(x: f64) -> f64 {
    if x < 0.0 {
        1.0 - norm_cdf_pos(-x)
    } else {
        norm_cdf_pos(x)
    }
}

/// Standard normal CDF for `x >= 0`: march `dN/dx = φ(x)` from `N(0) = 0.5`
/// in fixed steps, each step a Taylor polynomial whose coefficients come
/// from the density recurrence `φ^{(k+1)} = −x·φ^{(k)} − k·φ^{(k−1)}`.
fn norm_cdf_pos(x: f64) -> f64 {
    let mut total = 0.5;
    let mut at = 0.0;
    while at < x {
        let h = (x - at).min(CDF_STEP);
        // p[k] = φ^{(k)}(at), so N^{(k+1)}(at) = p[k].
        let mut older = 0.0;
        let mut newer = norm_pdf(at);
        let mut step_sum = newer * h;
        let mut h_pow = h;
        let mut fact = 1.0;
        for k in 1..CDF_ORDER {
            let current = -at * newer - (k - 1) as f64 * older;
            h_pow *= h;
            fact *= (k + 1) as f64;
            step_sum += current * h_pow / fact;
            older = newer;
            newer = current;
        }
        total += step_sum;
        at += h;
    }
    total
}

/// Black-Scholes sensitivities. Units match
/// [`Greeks`](protopyne_types::Greeks): theta per day, vega per point of
/// volatility; rho per 1.0 of rate has no [`Greeks`](protopyne_types::Greeks)
/// field and is dropped at the conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BsGreeks {
    /// Change in value per $1 move in the underlying.
    pub delta: f64,
    /// Change in delta per $1 move in the underlying.
    pub gamma: f64,
    /// Change in value per day of time decay.
    pub theta: f64,
    /// Change in value per point (1.0) of implied volatility.
    pub vega: f64,
    /// Change in value per 1.0 of rate.
    pub rho: f64,
}

impl From<BsGreeks> for protopyne_types::Greeks {
    /// Drop rho: the wire type carries the four sensitivities the risk cap
    /// reads, and rho has no field there.
    fn from(greeks: BsGreeks) -> Self {
        Self::new()
            .delta(greeks.delta)
            .gamma(greeks.gamma)
            .theta(greeks.theta)
            .vega(greeks.vega)
            .build()
    }
}

/// European option value. Garbage in (non-positive spot/strike/volatility,
/// non-positive time, non-finite inputs) propagates as `NaN` — callers that
/// need a refusal validate first ([`implied_vol`] does).
#[must_use]
pub fn bs_price(kind: OptionKind, spot: f64, strike: f64, years: f64, rate: f64, vol: f64) -> f64 {
    let sqrt_t = years.sqrt();
    let d1 = ((spot / strike).ln() + (rate + 0.5 * vol * vol) * years) / (vol * sqrt_t);
    let d2 = d1 - vol * sqrt_t;
    let discount = (-rate * years).exp();
    match kind {
        OptionKind::Call => spot * norm_cdf(d1) - strike * discount * norm_cdf(d2),
        OptionKind::Put => strike * discount * norm_cdf(-d2) - spot * norm_cdf(-d1),
    }
}

/// European option sensitivities. Same domain as [`bs_price`]: garbage
/// propagates as `NaN`.
#[must_use]
pub fn bs_greeks(
    kind: OptionKind,
    spot: f64,
    strike: f64,
    years: f64,
    rate: f64,
    vol: f64,
) -> BsGreeks {
    let sqrt_t = years.sqrt();
    let d1 = ((spot / strike).ln() + (rate + 0.5 * vol * vol) * years) / (vol * sqrt_t);
    let d2 = d1 - vol * sqrt_t;
    let discount = (-rate * years).exp();
    let gamma = norm_pdf(d1) / (spot * vol * sqrt_t);
    let decay = -(spot * norm_pdf(d1) * vol) / (2.0 * sqrt_t) / 365.0;
    let vega = spot * norm_pdf(d1) * sqrt_t;
    match kind {
        OptionKind::Call => BsGreeks {
            delta: norm_cdf(d1),
            gamma,
            theta: decay - rate * strike * discount * norm_cdf(d2) / 365.0,
            vega,
            rho: strike * years * discount * norm_cdf(d2),
        },
        OptionKind::Put => BsGreeks {
            delta: norm_cdf(d1) - 1.0,
            gamma,
            theta: decay + rate * strike * discount * norm_cdf(-d2) / 365.0,
            vega,
            rho: -strike * years * discount * norm_cdf(-d2),
        },
    }
}

/// Implied volatility by bounded bisection: the σ that reprices `price`.
///
/// Refuses (rather than miscomputing) when the inputs are unusable — a
/// negative price, a non-positive spot/strike/time, a price below intrinsic
/// value or above the no-arbitrage ceiling — and when bisection exhausts
/// [`IMPLIED_VOL_MAX_ITER`] without bracketing the price to `1e-12`.
pub fn implied_vol(
    price: f64,
    kind: OptionKind,
    spot: f64,
    strike: f64,
    years: f64,
    rate: f64,
) -> Result<f64, KernelError> {
    if !price.is_finite() || price < 0.0 {
        return Err(KernelError::InvalidInput(format!(
            "implied volatility needs a finite non-negative price, got {price}"
        )));
    }
    if !(positive_finite(spot)
        && positive_finite(strike)
        && positive_finite(years)
        && rate.is_finite())
    {
        return Err(KernelError::InvalidInput(format!(
            "implied volatility needs positive finite spot/strike/time and a finite rate, got {spot}/{strike}/{years}/{rate}"
        )));
    }
    let discount = (-rate * years).exp();
    let (intrinsic, ceiling) = match kind {
        OptionKind::Call => ((spot - strike * discount).max(0.0), spot),
        OptionKind::Put => ((strike * discount - spot).max(0.0), strike * discount),
    };
    if price < intrinsic && intrinsic - price > 1e-12 {
        return Err(KernelError::InvalidInput(format!(
            "price {price} below intrinsic value {intrinsic}: no volatility reprices it"
        )));
    }
    if price > ceiling {
        return Err(KernelError::InvalidInput(format!(
            "price {price} above the no-arbitrage ceiling {ceiling}: no volatility reprices it"
        )));
    }
    let target = price.max(intrinsic);
    let mut low = IMPLIED_VOL_MIN;
    let mut high = IMPLIED_VOL_START_MAX;
    while bs_price(kind, spot, strike, years, rate, high) < target {
        high *= 2.0;
        if high > 20.0 {
            return Err(KernelError::InvalidInput(format!(
                "price {price} above any modeled volatility: no volatility reprices it"
            )));
        }
    }
    // Converge on the bracket width, never on the price residual: far
    // in- or out-of-the-money the price barely moves with σ, so a price
    // tolerance would stop at the wrong volatility. The bracket always
    // shrinks, so the cap below is a backstop, not a path.
    for _ in 0..IMPLIED_VOL_MAX_ITER {
        let mid = f64::midpoint(low, high);
        if bs_price(kind, spot, strike, years, rate, mid) < target {
            low = mid;
        } else {
            high = mid;
        }
        if high - low <= 1e-12 {
            return Ok(f64::midpoint(low, high));
        }
    }
    Err(KernelError::VolNotConverged {
        tried: IMPLIED_VOL_MAX_ITER,
    })
}

#[cfg(test)]
#[path = "../tests/greeks.rs"]
mod tests;
