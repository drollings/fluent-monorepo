//! Mark selection: chain mid, fill-price fallback, and absent-not-zero.

use super::*;
use crate::common_fixtures::money;
use chrono::TimeZone;
use protopyne_types::{ContractId, OptionContract, OptionKind};

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).unwrap()
}

fn contract(strike: i64) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(chrono::NaiveDate::from_ymd_opt(2026, 1, 16).expect("valid expiry"))
        .strike(money(&strike.to_string()))
        .kind(OptionKind::Call)
        .build()
}

fn leg(contract: &OptionContract, side: OrderSide, quantity: u32, price: &str) -> LegExecution {
    LegExecution {
        contract: ContractId::new(contract.occ_symbol()),
        side,
        quantity,
        price: money(price),
        fee: Money::zero(),
    }
}

fn quote(contract: &OptionContract, bid: &str, ask: &str, timestamp: DateTime<Utc>) -> ChainQuote {
    ChainQuote::new()
        .contract(contract.clone())
        .bid(money(bid))
        .ask(money(ask))
        .timestamp(timestamp)
        .build()
}

fn vertical() -> (OptionContract, OptionContract, Vec<LegExecution>) {
    let long = contract(600);
    let short = contract(610);
    let legs = vec![
        leg(&long, OrderSide::Buy, 1, "2.70"),
        leg(&short, OrderSide::Sell, 1, "1.20"),
    ];
    (long, short, legs)
}

#[test]
fn chain_mid_marks_the_package() {
    let (long, short, legs) = vertical();
    let chain = vec![
        quote(&long, "2.50", "2.70", at(1_000)),
        quote(&short, "1.20", "1.40", at(1_000)),
    ];
    let (mark, basis) = mark_per_unit(&legs, 1, &chain, at(1_001));
    assert_eq!(basis, MarkBasis::ChainMid);
    // Long mid 2.60 minus short mid 1.30 = 1.30 per package unit.
    assert_eq!(mark, Some(money("1.30")));
}

#[test]
fn dark_chain_falls_back_to_the_fill_price() {
    let (_, _, legs) = vertical();
    let (mark, basis) = mark_per_unit(&legs, 1, &[], at(1_001));
    assert_eq!(basis, MarkBasis::FillPrice);
    // Executed long 2.70 minus short 1.20 = 1.50, the package's own basis.
    assert_eq!(mark, Some(money("1.50")));
}

#[test]
fn no_legs_or_no_units_has_no_mark() {
    let (long, _, legs) = vertical();
    let chain = vec![quote(&long, "2.50", "2.70", at(1_000))];
    assert_eq!(
        mark_per_unit(&[], 1, &chain, at(1_001)),
        (None, MarkBasis::Absent),
        "no legs is no position to mark"
    );
    assert_eq!(
        mark_per_unit(&legs, 0, &chain, at(1_001)),
        (None, MarkBasis::Absent),
        "zero package units is no position to mark"
    );
}

#[test]
fn newest_quote_at_or_before_is_used() {
    let (long, short, legs) = vertical();
    let chain = vec![
        quote(&long, "1.00", "1.20", at(1_000)),
        quote(&long, "2.00", "2.20", at(2_000)),
        quote(&short, "0.40", "0.60", at(1_000)),
        quote(&short, "0.80", "1.00", at(2_000)),
    ];
    let (mark, basis) = mark_per_unit(&legs, 1, &chain, at(1_500));
    assert_eq!(basis, MarkBasis::ChainMid);
    // The 1_000 rung wins at 1_500: long mid 1.10 minus short mid 0.50 = 0.60.
    assert_eq!(mark, Some(money("0.60")));
}

#[test]
fn a_crossed_newest_quote_falls_back_rather_than_stalely_reaching_back() {
    let (long, short, legs) = vertical();
    let chain = vec![
        quote(&long, "1.00", "1.20", at(1_000)),
        // Crossed newest quote: no valid mid exists.
        quote(&long, "3.00", "2.00", at(2_000)),
        quote(&short, "0.40", "0.60", at(1_000)),
    ];
    let (mark, basis) = mark_per_unit(&legs, 1, &chain, at(2_000));
    assert_eq!(basis, MarkBasis::FillPrice);
    assert_eq!(mark, Some(money("1.50")));
}

#[test]
fn per_unit_scales_by_package_units() {
    let (long, short, _) = vertical();
    // Two package units: each leg's contracts scale by two.
    let legs = vec![
        leg(&long, OrderSide::Buy, 2, "2.70"),
        leg(&short, OrderSide::Sell, 2, "1.20"),
    ];
    let (mark, basis) = mark_per_unit(&legs, 2, &[], at(1_001));
    assert_eq!(basis, MarkBasis::FillPrice);
    assert_eq!(mark, Some(money("1.50")), "per unit, not per package");
}

#[test]
fn the_basis_note_names_each_basis() {
    assert!(mark_basis_note(MarkBasis::ChainMid).contains("chain mid"));
    assert!(mark_basis_note(MarkBasis::FillPrice).contains("fill price"));
    assert!(mark_basis_note(MarkBasis::Absent).contains("absent"));
}

#[test]
fn preference_orders_chain_mid_over_fill_price_over_absent() {
    assert_eq!(
        MarkBasis::ChainMid.preferred(MarkBasis::FillPrice),
        MarkBasis::ChainMid
    );
    assert_eq!(
        MarkBasis::FillPrice.preferred(MarkBasis::Absent),
        MarkBasis::FillPrice
    );
    assert_eq!(
        MarkBasis::Absent.preferred(MarkBasis::ChainMid),
        MarkBasis::ChainMid
    );
}
