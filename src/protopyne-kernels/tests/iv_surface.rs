//! Surface precision and refusal: the fit against a known constant vol.
//!
//! A synthetic chain generated from a known constant-vol surface must be
//! recovered within tolerance (precision), and every under-determined or
//! dishonest chain must refuse with the shortfall or the contract named —
//! never a smoothed invention.

use super::*;

use chrono::{NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;

use crate::greeks::bs_price;
use protopyne_types::{ChainQuote, Money, OptionContract, OptionKind};

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
}

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn contract(strike: &str, kind: OptionKind) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(date(2026, 12, 18))
        .strike(money(strike))
        .kind(kind)
        .build()
}

fn quote(contract: OptionContract, mid: f64) -> ChainQuote {
    // A tight symmetric market around the model mid, quoted to nine decimals:
    // two-decimal rounding would move the mid by up to half a cent, which is
    // ~1e-4 of vol at these vegas — coarser than the 1e-9 recovery pin.
    let half = 0.01;
    ChainQuote::new()
        .contract(contract)
        .bid(money(&format!("{:.9}", mid - half)))
        .ask(money(&format!("{:.9}", mid + half)))
        .timestamp(Utc.with_ymd_and_hms(2026, 6, 18, 14, 30, 0).unwrap())
        .build()
}

fn chain(vol: f64, spot: f64, rate: f64, as_of: NaiveDate) -> Vec<ChainQuote> {
    let expiry = date(2026, 12, 18);
    let days = u32::try_from((expiry - as_of).num_days()).expect("live expiry");
    let years = f64::from(days) / DAYS_PER_YEAR;
    ["90", "95", "100", "105", "110"]
        .into_iter()
        .map(|strike| {
            let strike_f = strike.parse::<f64>().expect("numeric strike");
            let mid = bs_price(OptionKind::Call, spot, strike_f, years, rate, vol);
            quote(contract(strike, OptionKind::Call), mid)
        })
        .collect()
}

// ── A known constant-vol surface is recovered within tolerance ──────────────

#[test]
fn a_constant_vol_surface_is_recovered_within_tolerance() {
    let as_of = date(2026, 6, 18);
    let quotes = chain(0.2, 100.0, 0.01, as_of);
    let surface =
        fit_surface(&quotes, 100.0, 0.01, as_of).expect("a five-strike chain determines a surface");
    assert_eq!(surface.points.len(), 5);
    for point in &surface.points {
        assert!(
            (point.iv - 0.2).abs() < 1e-9,
            "strike {} recovered {} instead of 0.2",
            point.strike,
            point.iv
        );
    }
    // Interpolation holds the interior and pins the wings flat.
    assert!((surface.iv_at(100.0) - 0.2).abs() < 1e-9);
    assert!((surface.iv_at(50.0) - surface.points[0].iv).abs() < 1e-12);
    assert!(
        (surface.iv_at(200.0) - surface.points[4].iv).abs() < 1e-12,
        "past the wings the surface holds flat, never extrapolates"
    );
}

// ── Under-determined and dishonest chains refuse ────────────────────────────

#[test]
fn fewer_than_three_strikes_refuses_naming_the_shortfall() {
    let as_of = date(2026, 6, 18);
    let mut quotes = chain(0.2, 100.0, 0.01, as_of);
    quotes.truncate(2);
    let reason = format!(
        "{}",
        fit_surface(&quotes, 100.0, 0.01, as_of).expect_err("two strikes refuse")
    );
    assert!(
        reason.contains('2') && reason.contains('3'),
        "the refusal names the shortfall: {reason}"
    );
}

#[test]
fn a_crossed_quote_refuses_naming_the_contract() {
    let as_of = date(2026, 6, 18);
    let mut quotes = chain(0.2, 100.0, 0.01, as_of);
    let crossed = ChainQuote::new()
        .contract(contract("105", OptionKind::Call))
        .bid(money("3.00"))
        .ask(money("2.00"))
        .timestamp(Utc.with_ymd_and_hms(2026, 6, 18, 14, 30, 0).unwrap())
        .build();
    quotes[3] = crossed;
    let reason = format!(
        "{}",
        fit_surface(&quotes, 100.0, 0.01, as_of).expect_err("crossed refuses")
    );
    assert!(
        reason.contains("105") || reason.contains("C00105000"),
        "the refusal names the contract: {reason}"
    );
}

#[test]
fn a_mixed_expiry_chain_refuses() {
    let as_of = date(2026, 6, 18);
    let mut quotes = chain(0.2, 100.0, 0.01, as_of);
    quotes[0].contract.expiry = date(2027, 1, 15);
    assert!(
        fit_surface(&quotes, 100.0, 0.01, as_of).is_err(),
        "two expiries are not one surface"
    );
}
