use super::*;
use crate::error::VenueError;
use crate::order::{OrderIntent, OrderRecord};
use crate::quote::Quote;
use crate::venue::{DryRunOutcome, Venue};
use chrono::{NaiveDate, TimeZone, Utc};
use protopyne_strategy::{
    Direction, OrderShape, Proposal, ProposalSide, ProposalState, RiskEngine, RiskReason,
    RiskState, StructureKind,
};
use protopyne_types::{LegSide, Money, OptionContract, OptionKind, Qty, SpreadLeg, SpreadLegs};
use rust_decimal::Decimal;
use std::sync::Arc;

fn money(text: &str) -> Money {
    text.parse().expect("money")
}

fn at(day: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, day, 12, 0, 0).unwrap()
}

fn contract(kind: OptionKind, strike: i64) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(NaiveDate::from_ymd_opt(2026, 7, 17).expect("expiry"))
        .strike(Money::new(Decimal::from(strike)))
        .kind(kind)
        .build()
}

fn leg(contract: &OptionContract, side: LegSide) -> SpreadLeg {
    SpreadLeg::new()
        .contract(contract.clone())
        .side(side)
        .build()
}

fn spread(legs: Vec<SpreadLeg>) -> SpreadLegs {
    SpreadLegs::try_new(legs).expect("valid spread")
}

fn position(legs: &SpreadLegs, side: PositionSide, price: &str, quantity: u32) -> SpreadPosition {
    SpreadPosition::new()
        .underlying("SPY".to_owned())
        .side(side)
        .price_entry_average(money(price))
        .quantity_abs(quantity)
        .quantity_abs_max(quantity)
        .pnl_realised(Money::zero())
        .pnl_unrealised(Money::zero())
        .fees_enter(Money::zero())
        .fees_exit(Money::zero())
        .time_enter(at(1))
        .time_updated(at(1))
        .maybe_legs(Some(legs.clone()))
        .build()
}

fn proposal() -> Proposal {
    Proposal::new()
        .id("p1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(1)))
        .limit_price(money("1"))
        .max_loss(money("10"))
        .notional(money("100"))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .state(ProposalState::Proposed)
        .created_at(at(1))
        .build()
}

fn state(facts: PortfolioFacts) -> RiskState {
    RiskState {
        quote_age_secs: 1,
        clock_skew_secs: 0,
        reconciliation_healthy: true,
        facts,
    }
}

/// Minimal venue: serves a supplied per-contract book, balances, and chain.
struct BookStub {
    positions: Option<Vec<SpreadPosition>>,
    balances: Vec<crate::venue::BalanceEntry>,
    chain: Option<Arc<dyn crate::venue::LegQuoteSource>>,
}

impl BookStub {
    fn empty() -> Self {
        Self {
            positions: Some(Vec::new()),
            balances: Vec::new(),
            chain: None,
        }
    }
}

impl Venue for BookStub {
    fn dry_run(&self, _intent: &OrderIntent, _quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        Ok(DryRunOutcome {
            allowed: true,
            reserved_notional: Money::zero(),
        })
    }

    fn submit<'a>(
        &'a self,
        _intent: OrderIntent,
        _quote: &'a Quote,
        _now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>,
    > {
        Box::pin(async { Err(VenueError::Invalid("stub has no venue".to_owned())) })
    }

    fn query<'a>(
        &'a self,
        _client_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<OrderRecord>> + Send + 'a>> {
        Box::pin(async { None })
    }

    fn cancel<'a>(
        &'a self,
        _client_id: &'a str,
        _now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>,
    > {
        Box::pin(async { Err(VenueError::Invalid("stub has no venue".to_owned())) })
    }

    fn positions(&self) -> Vec<crate::order::Position> {
        Vec::new()
    }

    fn open_spread_positions(&self) -> Option<Vec<SpreadPosition>> {
        self.positions.clone()
    }

    fn chain_source(&self) -> Option<Arc<dyn crate::venue::LegQuoteSource>> {
        self.chain.clone()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        Vec::new()
    }

    fn apply_corporate_event(
        &self,
        _action: &crate::order::CorporateEventArgs,
    ) -> Result<crate::order::CorporateOutcome, VenueError> {
        Err(VenueError::Invalid(
            "stub has no corporate actions".to_owned(),
        ))
    }

    fn fetch_balances(&self) -> Vec<crate::venue::BalanceEntry> {
        self.balances.clone()
    }
}

fn two_verticals() -> (SpreadLegs, SpreadLegs, Vec<SpreadPosition>) {
    let short_put = contract(OptionKind::Put, 100);
    let long_put = contract(OptionKind::Put, 95);
    let first = spread(vec![
        leg(&short_put, LegSide::Short),
        leg(&long_put, LegSide::Long),
    ]);

    let short_call = contract(OptionKind::Call, 110);
    let long_call = contract(OptionKind::Call, 115);
    let second = spread(vec![
        leg(&short_call, LegSide::Short),
        leg(&long_call, LegSide::Long),
    ]);

    let positions = vec![
        position(&first, PositionSide::Short, "3.00", 2),
        position(&first, PositionSide::Long, "1.20", 2),
        position(&second, PositionSide::Short, "2.50", 1),
        position(&second, PositionSide::Long, "1.00", 1),
    ];
    (first, second, positions)
}

fn facts_for(stub: BookStub, limits: RiskLimits) -> PortfolioFacts {
    VenueBookFacts::new(Arc::new(stub), limits).facts_at(at(2))
}

#[test]
fn two_open_verticals_sum_their_committed_max_loss() {
    let (_, _, positions) = two_verticals();
    let stub = BookStub {
        positions: Some(positions),
        ..BookStub::empty()
    };
    let facts = facts_for(stub, RiskLimits::new().max_open_risk(20.0).build());

    assert!(facts.observed);
    // (5.00 width - 1.80 credit) * 2 units + (5.00 - 1.50) * 1 unit.
    assert_eq!(facts.open_risk, money("9.90"));
    assert_eq!(facts.net_delta, None);
}

#[test]
fn a_configured_greek_cap_without_a_chain_source_is_unobserved() {
    let (_, _, positions) = two_verticals();
    let stub = BookStub {
        positions: Some(positions),
        ..BookStub::empty()
    };
    let limits = RiskLimits::new().max_abs_net_delta(1.0).build();
    let facts = facts_for(stub, limits.clone());

    assert!(!facts.observed);
    assert_eq!(facts.net_delta, None);
    let engine = RiskEngine::new(limits);
    let refused = engine
        .check(&proposal(), OrderShape::new(true, true), &state(facts))
        .expect_err("unobserved greeks refuse");
    assert!(refused
        .reasons
        .contains(&RiskReason::PortfolioFactsUnavailable));
}

#[test]
fn an_empty_book_observes_zero_open_risk() {
    let limits = RiskLimits::new().max_open_risk(1.0).build();
    let facts = facts_for(BookStub::empty(), limits.clone());

    assert!(facts.observed);
    assert_eq!(facts.open_risk, Money::zero());
    let engine = RiskEngine::new(limits);
    engine
        .check(&proposal(), OrderShape::new(true, true), &state(facts))
        .expect("zero risk is under the cap");
}

#[test]
fn buying_power_comes_from_the_venue_balance() {
    let stub = BookStub {
        positions: Some(Vec::new()),
        balances: vec![crate::venue::BalanceEntry {
            asset: "USD".to_owned(),
            amount: money("250.00"),
        }],
        chain: None,
    };
    let limits = RiskLimits::new().min_buying_power(100.0).build();
    let facts = facts_for(stub, limits);

    assert!(facts.observed);
    assert_eq!(facts.buying_power, Some(money("250.00")));
}

#[test]
fn a_position_without_a_structure_tag_is_unobserved() {
    let mut orphan = position(
        &spread(vec![leg(&contract(OptionKind::Call, 100), LegSide::Long)]),
        PositionSide::Long,
        "2.00",
        1,
    );
    orphan.legs = None;
    let stub = BookStub {
        positions: Some(vec![orphan]),
        ..BookStub::empty()
    };
    let facts = facts_for(stub, RiskLimits::new().max_open_risk(1.0).build());

    assert!(!facts.observed);
}
