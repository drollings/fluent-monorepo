//! Arm-screen worst-case projection: a booted chain derives a computed bound,
//! and anything unpriceable falls back to the budget echo.

use super::*;
use chrono::{TimeZone, Utc};
use protopyne_strategy::fixtures::grant_with;
use protopyne_strategy::{worst_case_for, GrantBudgets, WorstCaseBasis};
use protopyne_types::{Bar, ChainQuote, OptionContract, OptionKind};
use rust_decimal::Decimal;

fn money(text: &str) -> Money {
    Money::new(text.parse().expect("money"))
}

fn stamp(secs: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).unwrap()
}

fn bar() -> Bar {
    let close = Decimal::from(100);
    Bar::new()
        .ts(stamp(1_777_000_000))
        .o(close)
        .h(close)
        .l(close)
        .c(close)
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

fn contract(kind: OptionKind, strike: &str) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(chrono::NaiveDate::from_ymd_opt(2026, 7, 31).expect("valid expiry"))
        .strike(strike.parse().expect("strike"))
        .kind(kind)
        .build()
}

fn quote(kind: OptionKind, strike: &str, price: &str) -> ChainQuote {
    ChainQuote::new()
        .contract(contract(kind, strike))
        .bid(price.parse().expect("bid"))
        .ask(price.parse().expect("ask"))
        .timestamp(stamp(1_777_000_000))
        .build()
}

fn grant(structures: Vec<StructureKind>, directions: Vec<Direction>) -> Grant {
    // The shared fixture's shape with the worst-case suite's tighter budgets.
    grant_with(|grant| {
        grant.id = "worst-case".to_owned();
        grant.scope.structures = structures;
        grant.scope.directions = directions;
        grant.budgets = GrantBudgets::new()
            .max_trades(10)
            .max_contracts(40)
            .max_loss(Money::from(1_000_000))
            .max_notional(Money::from(1_000_000))
            .build();
        grant.trade.min_dte_days = 7;
        grant.trade.max_dte_days = 60;
        grant.portfolio.max_open_risk = Money::from(10_000);
        grant.valid_from = chrono::DateTime::from_timestamp(1_767_225_600, 0).expect("valid stamp");
        grant.valid_until =
            chrono::DateTime::from_timestamp(1_798_761_600, 0).expect("valid stamp");
    })
}

fn condor_chain() -> Vec<ChainQuote> {
    vec![
        quote(OptionKind::Put, "90", "0.50"),
        quote(OptionKind::Put, "95", "1.00"),
        quote(OptionKind::Call, "105", "1.00"),
        quote(OptionKind::Call, "110", "0.50"),
    ]
}

#[test]
fn a_priced_condor_becomes_a_computed_bound() {
    let grant = grant(
        vec![StructureKind::Condor],
        vec![Direction::Long, Direction::Short],
    );
    let units = chain_units(&grant, &condor_chain(), &bar());
    assert_eq!(
        units.len(),
        1,
        "the condor prices for every required variant"
    );
    assert_eq!(units[0].0, StructureKind::Condor);
    assert_eq!(units[0].1.max_loss, money("4"), "5-wide wing less 1 credit");

    let computed = worst_case_for(
        &grant,
        &grant.scope.structures,
        |structure| {
            units
                .iter()
                .find(|entry| entry.0 == structure)
                .map(|entry| entry.1)
        },
        structure_legs,
    );
    assert_eq!(computed.basis, WorstCaseBasis::ChainDerived);
    assert_eq!(computed.max_trades, 10);
    assert_eq!(computed.max_contracts, 40);
    assert_eq!(computed.max_loss, money("40"));
    assert_eq!(computed.max_notional, money("40"));
}

#[test]
fn a_chain_less_grant_falls_back_to_the_budget_echo() {
    let grant = grant(vec![StructureKind::Condor], vec![Direction::Long]);
    assert!(chain_units(&grant, &[], &bar()).is_empty());
    let computed = worst_case_for(&grant, &grant.scope.structures, |_| None, structure_legs);
    assert_eq!(computed.basis, WorstCaseBasis::BudgetEcho);
    assert_eq!(computed.max_loss, grant.budgets.max_loss);
    assert_eq!(computed.max_trades, grant.budgets.max_trades);
}

#[test]
fn a_structure_with_an_unpriceable_variant_is_omitted() {
    // Only calls are listed: the credit (put) variant refuses, so the
    // vertical cannot be bounded and the bound stays an echo.
    let grant = grant(vec![StructureKind::Vertical], vec![Direction::Long]);
    let calls = vec![
        quote(OptionKind::Call, "100", "3.00"),
        quote(OptionKind::Call, "105", "1.20"),
        quote(OptionKind::Call, "110", "0.80"),
    ];
    assert!(chain_units(&grant, &calls, &bar()).is_empty());
}
