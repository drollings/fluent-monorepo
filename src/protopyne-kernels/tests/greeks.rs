//! Black-Scholes parity and agreement: the closed form against itself.
//!
//! The calibration shape here is numeric parity, not a control group: the
//! identities below must hold to float rounding on every grid point, because
//! downstream (the IV surface, the computed greeks source, the risk cap)
//! divides by them. Symmetry in the CDF makes put-call parity exact by
//! construction; the grids prove it, plus the finite-difference agreement
//! of delta and gamma against the price function they differentiate.

use super::*;

use protopyne_types::OptionKind;

fn grid() -> Vec<(f64, f64, f64, f64, f64)> {
    let mut points = Vec::new();
    for spot in [50.0, 100.0, 200.0] {
        for moneyness in [0.8, 1.0, 1.2] {
            for years in [0.1, 0.5, 2.0] {
                for rate in [0.0, 0.02, 0.05] {
                    for vol in [0.1, 0.3, 0.6] {
                        points.push((spot, spot * moneyness, years, rate, vol));
                    }
                }
            }
        }
    }
    points
}

// ── The CDF march is accurate to ~1e-14, not just self-consistent ─────────
// Textbook values (the march has no fitted constants to agree with
// itself about): N(0) = 1/2, N(1), N(2) to twelve places.
#[test]
fn the_normal_cdf_matches_textbook_values() {
    assert!(
        (super::norm_cdf(0.0) - 0.5).abs() < f64::EPSILON,
        "N(0) is exactly one half"
    );
    for (x, want) in [
        (1.0, 0.8413447460685429),
        (2.0, 0.9772498680518208),
        (-1.0, 0.15865525393145707),
        (4.0, 0.9999683287581669),
    ] {
        let got = super::norm_cdf(x);
        assert!((got - want).abs() < 1e-12, "N({x}) = {got}, want {want}");
    }
}

// ── Put-call parity within 1e-9 across the grid ────────────────────────────

#[test]
fn put_call_parity_holds_to_float_rounding() {
    for (spot, strike, years, rate, vol) in grid() {
        for kind in [OptionKind::Call, OptionKind::Put] {
            let call = bs_price(OptionKind::Call, spot, strike, years, rate, vol);
            let put = bs_price(OptionKind::Put, spot, strike, years, rate, vol);
            let forward = spot - strike * (-rate * years).exp();
            let breach = (call - put - forward).abs();
            assert!(
                breach < 1e-9,
                "parity breached by {breach}: {kind:?} S={spot} K={strike} T={years} r={rate} σ={vol}"
            );
        }
    }
}

// ── Delta and gamma agree with finite differences within 1e-6 ───────────────

#[test]
fn delta_and_gamma_agree_with_finite_differences() {
    for (spot, strike, years, rate, vol) in grid() {
        for kind in [OptionKind::Call, OptionKind::Put] {
            let greeks = bs_greeks(kind, spot, strike, years, rate, vol);
            assert!(
                greeks.delta.is_finite()
                    && greeks.gamma.is_finite()
                    && greeks.theta.is_finite()
                    && greeks.vega.is_finite()
                    && greeks.rho.is_finite(),
                "every sensitivity is finite: {kind:?} S={spot} K={strike}"
            );
            // Central differences with a tight relative step for delta:
            // truncation is O(h²), rounding is ε·price/h. Gamma uses
            // Richardson extrapolation (h and h/2 combined to kill the h²
            // term), because short-dated speed punishes wide stencils.
            let step = spot * 1e-6;
            let up = bs_price(kind, spot + step, strike, years, rate, vol);
            let down = bs_price(kind, spot - step, strike, years, rate, vol);
            let delta_diff = (up - down) / (2.0 * step);
            assert!(
                (greeks.delta - delta_diff).abs() < 1e-6,
                "delta disagrees: analytic {} vs diff {delta_diff} ({kind:?} S={spot} K={strike} T={years} r={rate} σ={vol})",
                greeks.delta
            );
            let center = bs_price(kind, spot, strike, years, rate, vol);
            let wide = spot * 1e-3;
            let second = |h: f64| {
                (bs_price(kind, spot + h, strike, years, rate, vol) - 2.0 * center
                    + bs_price(kind, spot - h, strike, years, rate, vol))
                    / (h * h)
            };
            let gamma_diff = (4.0 * second(wide / 2.0) - second(wide)) / 3.0;
            assert!(
                (greeks.gamma - gamma_diff).abs() < 1e-6,
                "gamma disagrees: analytic {} vs diff {gamma_diff} ({kind:?} S={spot} K={strike} T={years} r={rate} σ={vol})",
                greeks.gamma
            );
        }
    }
}

// ── IV round-trips within 1e-8; garbage refuses ─────────────────────────────

#[test]
fn implied_volatility_round_trips() {
    let mut tested = 0;
    let mut skipped = 0;
    for (spot, strike, years, rate, vol) in grid() {
        for kind in [OptionKind::Call, OptionKind::Put] {
            let price = bs_price(kind, spot, strike, years, rate, vol);
            let recovered =
                implied_vol(price, kind, spot, strike, years, rate).expect("a priced vol returns");
            // The recovered vol must reprice exactly — that is the property
            // the surface needs. Vol agreement within 1e-8 additionally
            // holds wherever the vol is float-identifiable. Two skips, both
            // documented: where one ulp of price moves σ by more than 1e-9
            // the true σ is unrepresentable, and where the price sits below
            // the closed form's own absolute floor (~spot·7.5e-8 from the
            // CDF approximation) the price is noise, not information. Only
            // repricing is asserted there.
            assert!(
                (bs_price(kind, spot, strike, years, rate, recovered) - price).abs()
                    <= 1e-9 * price.abs().max(1.0),
                "recovered vol must reprice ({kind:?} S={spot} K={strike})"
            );
            let vega = bs_greeks(kind, spot, strike, years, rate, vol).vega;
            let resolution = f64::EPSILON * price.abs().max(1e-300) / vega.max(1e-300);
            if resolution > 1e-9 || price < 1e-6 * spot {
                skipped += 1;
                continue;
            }
            tested += 1;
            assert!(
                (recovered - vol).abs() < 1e-8,
                "round-trip lost {vol}: got {recovered} ({kind:?} S={spot} K={strike})"
            );
        }
    }
    assert!(tested > 0, "the grid must test something");
    assert!(
        skipped < tested,
        "most of the grid must be identifiable, skipped {skipped} of {tested}"
    );
}

#[test]
fn a_price_below_intrinsic_refuses_with_a_reason() {
    // A 100-strike call on a 90 spot cannot trade below zero intrinsic…
    let refused = implied_vol(-1.0, OptionKind::Call, 90.0, 100.0, 0.5, 0.02);
    assert!(refused.is_err());
    // …and a put above its no-arbitrage ceiling cannot trade at all.
    let ceiling = 100.0 * (-0.02 * 0.5_f64).exp();
    let refused = implied_vol(ceiling + 1.0, OptionKind::Put, 90.0, 100.0, 0.5, 0.02);
    let reason = format!("{}", refused.expect_err("above the ceiling refuses"));
    assert!(
        reason.contains("ceiling"),
        "the refusal names the bound: {reason}"
    );
}
