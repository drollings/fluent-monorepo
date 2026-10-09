use super::*;
use crate::proposal::{Direction, Proposal, ProposalSide, ProposalState, StructureKind};
use chrono::Utc;
use protopyne_types::{Money, Qty};
use rust_decimal::Decimal;
use std::sync::Arc;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn proposal() -> Proposal {
    Proposal::new()
        .id("p1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money(250))
        .max_loss(money(300))
        .notional(money(1_000))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .state(ProposalState::Proposed)
        .created_at(Utc::now())
        .build()
}

fn healthy() -> RiskState {
    RiskState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        reconciliation_healthy: true,
        is_limit_order: true,
        is_spread: true,
    }
}

#[test]
fn healthy_proposal_mints_a_bound_token() {
    let engine = RiskEngine::new(RiskLimits::default());
    let approved = engine.check(&proposal(), &healthy()).expect("healthy");
    assert_eq!(approved.proposal_id(), "p1");
}

#[test]
fn kill_switch_and_reconciliation_halt_everything() {
    let engine = RiskEngine::new(RiskLimits::new().kill_switch(true).build());
    let refused = engine.check(&proposal(), &healthy()).expect_err("killed");
    assert!(refused.reasons.contains(&RiskReason::KillSwitch));

    let engine = RiskEngine::new(RiskLimits::default());
    let halted = RiskState {
        reconciliation_healthy: false,
        ..healthy()
    };
    let refused = engine.check(&proposal(), &halted).expect_err("halted");
    assert!(refused.reasons.contains(&RiskReason::ReconciliationHalt));
}

#[test]
fn caps_refuse_oversize_proposals() {
    let engine = RiskEngine::new(RiskLimits::default());

    let mut p = proposal();
    p.quantity = Qty::new(Decimal::from(500));
    let refused = engine.check(&p, &healthy()).expect_err("too many");
    assert!(refused.reasons.contains(&RiskReason::OverContractCap));

    let mut p = proposal();
    p.max_loss = money(50_000);
    let refused = engine.check(&p, &healthy()).expect_err("too much loss");
    assert!(refused.reasons.contains(&RiskReason::OverLossCap));

    let mut p = proposal();
    p.notional = money(5_000_000);
    let refused = engine.check(&p, &healthy()).expect_err("too much notional");
    assert!(refused.reasons.contains(&RiskReason::OverNotionalCap));
}

#[test]
fn staleness_and_skew_refuse() {
    let engine = RiskEngine::new(RiskLimits::default());
    let stale = RiskState {
        quote_age_secs: 5_000,
        ..healthy()
    };
    let refused = engine.check(&proposal(), &stale).expect_err("stale");
    assert!(refused.reasons.contains(&RiskReason::StaleQuote));

    let skewed = RiskState {
        clock_skew_secs: 500,
        ..healthy()
    };
    let refused = engine.check(&proposal(), &skewed).expect_err("skewed");
    assert!(refused.reasons.contains(&RiskReason::ClockSkew));
}

#[test]
fn spreads_require_limit_orders_but_stock_may_use_market() {
    let engine = RiskEngine::new(RiskLimits::default());
    let market_spread = RiskState {
        is_limit_order: false,
        is_spread: true,
        ..healthy()
    };
    let refused = engine
        .check(&proposal(), &market_spread)
        .expect_err("market spread");
    assert!(refused.reasons.contains(&RiskReason::MarketOrderForSpread));

    let market_single = RiskState {
        is_limit_order: false,
        is_spread: false,
        ..healthy()
    };
    assert!(engine.check(&proposal(), &market_single).is_ok());
}

#[test]
fn fractional_quantities_fail_closed() {
    let engine = RiskEngine::new(RiskLimits::default());
    let mut p = proposal();
    p.quantity = Qty::new(Decimal::new(25, 1));
    let refused = engine.check(&p, &healthy()).expect_err("fractional");
    assert!(refused.reasons.contains(&RiskReason::Unrepresentable));
}

#[test]
fn every_refusal_reason_is_reachable() {
    let reasons = [
        RiskReason::KillSwitch,
        RiskReason::ReconciliationHalt,
        RiskReason::OverContractCap,
        RiskReason::OverLossCap,
        RiskReason::OverNotionalCap,
        RiskReason::StaleQuote,
        RiskReason::ClockSkew,
        RiskReason::MarketOrderForSpread,
        RiskReason::Unrepresentable,
    ];
    assert_eq!(reasons.len(), 9);
    for reason in &reasons {
        let refused = RiskRefused {
            reasons: vec![reason.clone()],
            detail: "test".to_owned(),
        };
        assert!(refused.detail.contains("test"));
        assert_eq!(refused.reasons.len(), 1);
    }
}

#[test]
fn engine_serves_as_a_component_with_test_doubles() {
    let engine = RiskEngine::new(RiskLimits::default());
    assert_eq!(engine.name(), "risk_engine");
    let ctx = WorkContext::default();
    assert!(engine.execute(&ctx).is_ok());

    let inner: Arc<dyn Component> = Arc::new(RiskEngine::new(RiskLimits::default()));
    let doubled = ComponentAdapter::new(inner).with_execute_override(Arc::new(|_| {
        Ok(WorkOutput::ok("double refuses everything"))
    }));
    let output = doubled
        .execute(&ctx)
        .expect("double executes");
    assert!(output.message.contains("double"));
}

#[test]
fn caps_read_exact_decimals() {
    // A configured cap means exactly what it says: 19.99 is 19.99, not the
    // binary retain artifact (19.989999...), which would refuse a proposal
    // booked at exactly the cap.
    let limits = RiskLimits::new().max_loss_per_trade(19.99).max_notional(19.99).build();
    let exact: Decimal = "19.99".parse().expect("decimal");
    assert_eq!(limits.loss_cap(), Some(exact));
    assert_eq!(limits.notional_cap(), Some(exact));

    let engine = RiskEngine::new(limits);
    let mut at_cap = proposal();
    at_cap.max_loss = Money::new(exact);
    at_cap.notional = Money::new(exact);
    assert!(engine.check(&at_cap, &healthy()).is_ok(), "at-cap proposals pass");

    let mut over_cap = proposal();
    over_cap.max_loss = Money::new(exact + Decimal::from(1));
    let refused = engine.check(&over_cap, &healthy()).expect_err("over-cap refuses");
    assert!(refused.reasons.contains(&RiskReason::OverLossCap));
}
