use super::*;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use std::any::TypeId;

fn dec(s: &str) -> Decimal {
    s.parse().expect("valid test decimal")
}

fn money(s: &str) -> Money {
    Money::new(dec(s))
}

fn call_contract() -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(NaiveDate::from_ymd_opt(2026, 1, 16).expect("valid date"))
        .strike(money("600"))
        .kind(OptionKind::Call)
        .build()
}

fn put_contract() -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(NaiveDate::from_ymd_opt(2026, 1, 16).expect("valid date"))
        .strike(money("590"))
        .kind(OptionKind::Put)
        .build()
}

fn long_leg() -> SpreadLeg {
    SpreadLeg::new()
        .contract(call_contract())
        .side(LegSide::Long)
        .ratio(1)
        .build()
}

fn short_leg() -> SpreadLeg {
    SpreadLeg::new()
        .contract(put_contract())
        .side(LegSide::Short)
        .ratio(1)
        .build()
}

#[test]
fn single_short_leg_is_refused_as_naked() {
    let err = SpreadLegs::try_new(vec![short_leg()]).expect_err("naked short must refuse");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("naked") || msg.contains("defined-risk") || msg.contains("defined risk"),
        "refusal must name the naked-short reason, got: {err}"
    );
}

#[test]
fn empty_spread_is_refused() {
    let err = SpreadLegs::try_new(vec![]).expect_err("0 legs must refuse");
    assert!(!err.to_string().is_empty());
}

#[test]
fn five_leg_spread_is_refused() {
    let legs = vec![
        long_leg(),
        long_leg(),
        short_leg(),
        short_leg(),
        long_leg(),
    ];
    let err = SpreadLegs::try_new(legs).expect_err("5 legs must refuse");
    assert!(!err.to_string().is_empty());
}

#[test]
fn all_short_multileg_is_refused_as_naked() {
    let err = SpreadLegs::try_new(vec![short_leg(), short_leg()])
        .expect_err("all-short spread must refuse");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("naked") || msg.contains("defined-risk") || msg.contains("defined risk"),
        "refusal must name the naked-short reason, got: {err}"
    );
}

#[test]
fn vertical_spread_constructs() {
    let spread = SpreadLegs::try_new(vec![long_leg(), short_leg()]).expect("vertical must build");
    assert_eq!(spread.len(), 2);
    assert!(!spread.is_empty());
}

#[test]
fn greeks_survive_serde_round_trip() {
    let quote = ChainQuote::new()
        .contract(call_contract())
        .bid(money("2.50"))
        .ask(money("2.70"))
        .bid_size(10_u32)
        .ask_size(12_u32)
        .open_interest(1_500_u64)
        .maybe_iv(Some(0.22))
        .maybe_greeks(Some(
            Greeks::new()
                .delta(0.55)
                .gamma(0.02)
                .theta(-0.03)
                .vega(0.11)
                .build(),
        ))
        .timestamp(chrono::Utc::now())
        .build();
    let json = serde_json::to_string(&quote).expect("serialize chain quote");
    let back: ChainQuote = serde_json::from_str(&json).expect("deserialize chain quote");
    assert_eq!(back, quote);
    let greeks = back.greeks.expect("greeks survive round-trip");
    assert!((greeks.delta - 0.55).abs() < 1e-12);
    assert!((greeks.gamma - 0.02).abs() < 1e-12);
    assert!((greeks.theta - (-0.03)).abs() < 1e-12);
    assert!((greeks.vega - 0.11).abs() < 1e-12);
}

/// `ContractId` and `PortfolioId` are distinct opaque handles: mixing them
/// fails to compile (`let _: ContractId = PortfolioId::new("x")` does not
/// build), so this test only pins the runtime-visible consequence — the
/// types are not interchangeable even by `TypeId`.
#[test]
fn contract_and_portfolio_ids_are_distinct_types() {
    assert_ne!(TypeId::of::<ContractId>(), TypeId::of::<PortfolioId>());
    let contract = ContractId::new("SPY-260116-C600");
    let portfolio = PortfolioId::new("SPY-260116-C600");
    assert_eq!(contract.as_str(), portfolio.as_str());
    fn accepts_contract(_: ContractId) {}
    accepts_contract(contract);
}

#[test]
fn occ_identifier_matches_golden_vector() {
    assert_eq!(call_contract().occ_symbol(), "SPY   260116C00600000");
    assert_eq!(put_contract().occ_symbol(), "SPY   260116P00590000");
}
