use super::*;
use crate::proposal::{Direction, Proposal, ProposalSide, ProposalState, StructureKind};
use chrono::Utc;
use protopyne_types::{Greeks, Money, Qty};
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
        facts: PortfolioFacts::unobserved(),
    }
}

fn limit_spread() -> OrderShape {
    OrderShape::new(true, true)
}

#[test]
fn healthy_proposal_mints_a_bound_token() {
    let engine = RiskEngine::new(RiskLimits::default());
    let approved = engine
        .check(&proposal(), limit_spread(), &healthy())
        .expect("healthy");
    assert_eq!(approved.proposal_id(), "p1");
}

#[test]
fn kill_switch_and_reconciliation_halt_everything() {
    let engine = RiskEngine::new(RiskLimits::new().kill_switch(true).build());
    let refused = engine
        .check(&proposal(), limit_spread(), &healthy())
        .expect_err("killed");
    assert!(refused.reasons.contains(&RiskReason::KillSwitch));

    let engine = RiskEngine::new(RiskLimits::default());
    let halted = RiskState {
        reconciliation_healthy: false,
        ..healthy()
    };
    let refused = engine
        .check(&proposal(), limit_spread(), &halted)
        .expect_err("halted");
    assert!(refused.reasons.contains(&RiskReason::ReconciliationHalt));
}

#[test]
fn caps_refuse_oversize_proposals() {
    let engine = RiskEngine::new(RiskLimits::default());

    let mut p = proposal();
    p.quantity = Qty::new(Decimal::from(500));
    let refused = engine
        .check(&p, limit_spread(), &healthy())
        .expect_err("too many");
    assert!(refused.reasons.contains(&RiskReason::OverContractCap));

    let mut p = proposal();
    p.max_loss = money(50_000);
    let refused = engine
        .check(&p, limit_spread(), &healthy())
        .expect_err("too much loss");
    assert!(refused.reasons.contains(&RiskReason::OverLossCap));

    let mut p = proposal();
    p.notional = money(5_000_000);
    let refused = engine
        .check(&p, limit_spread(), &healthy())
        .expect_err("too much notional");
    assert!(refused.reasons.contains(&RiskReason::OverNotionalCap));
}

#[test]
fn staleness_and_skew_refuse() {
    let engine = RiskEngine::new(RiskLimits::default());
    let stale = RiskState {
        quote_age_secs: 5_000,
        ..healthy()
    };
    let refused = engine
        .check(&proposal(), limit_spread(), &stale)
        .expect_err("stale");
    assert!(refused.reasons.contains(&RiskReason::StaleQuote));

    let skewed = RiskState {
        clock_skew_secs: 500,
        ..healthy()
    };
    let refused = engine
        .check(&proposal(), limit_spread(), &skewed)
        .expect_err("skewed");
    assert!(refused.reasons.contains(&RiskReason::ClockSkew));
}

#[test]
fn spreads_require_limit_orders_but_stock_may_use_market() {
    let engine = RiskEngine::new(RiskLimits::default());
    let market_spread = OrderShape::new(true, false);
    let refused = engine
        .check(&proposal(), market_spread, &healthy())
        .expect_err("market spread");
    assert!(refused.reasons.contains(&RiskReason::MarketOrderForSpread));

    let market_single = OrderShape::new(false, false);
    assert!(engine.check(&proposal(), market_single, &healthy()).is_ok());
}

#[test]
fn fractional_quantities_fail_closed() {
    let engine = RiskEngine::new(RiskLimits::default());
    let mut p = proposal();
    p.quantity = Qty::new(Decimal::new(25, 1));
    let refused = engine
        .check(&p, limit_spread(), &healthy())
        .expect_err("fractional");
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
        RiskReason::OverOpenRisk,
        RiskReason::OverNetDelta,
        RiskReason::InsufficientBuyingPower,
        RiskReason::PortfolioFactsUnavailable,
        RiskReason::Unrepresentable,
    ];
    assert_eq!(reasons.len(), 13);
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
    let output = doubled.execute(&ctx).expect("double executes");
    assert!(output.message.contains("double"));
}

#[test]
fn caps_read_exact_decimals() {
    // A configured cap means exactly what it says: 19.99 is 19.99, not the
    // binary retain artifact (19.989999...), which would refuse a proposal
    // booked at exactly the cap.
    let limits = RiskLimits::new()
        .max_loss_per_trade(19.99)
        .max_notional(19.99)
        .build();
    let exact: Decimal = "19.99".parse().expect("decimal");
    assert_eq!(limits.loss_cap(), Some(exact));
    assert_eq!(limits.notional_cap(), Some(exact));

    let engine = RiskEngine::new(limits);
    let mut at_cap = proposal();
    at_cap.max_loss = Money::new(exact);
    at_cap.notional = Money::new(exact);
    assert!(
        engine.check(&at_cap, limit_spread(), &healthy()).is_ok(),
        "at-cap proposals pass"
    );

    let mut over_cap = proposal();
    over_cap.max_loss = Money::new(exact + Decimal::from(1));
    let refused = engine
        .check(&over_cap, limit_spread(), &healthy())
        .expect_err("over-cap refuses");
    assert!(refused.reasons.contains(&RiskReason::OverLossCap));
}

#[test]
fn over_bound_quote_age_and_clock_skew_refuse_at_construction() {
    for (quote_age, skew) in [(3_601, 5), (0, 5), (60, 3_601), (60, 0)] {
        let limits = RiskLimits::new()
            .quote_max_age_secs(quote_age)
            .clock_skew_secs(skew)
            .build();
        assert!(
            limits.validate().is_err(),
            "quote {quote_age}/skew {skew} refuses at construction"
        );
        let text = serde_json::to_string(&limits).expect("limits serialize");
        let back: RiskLimits = serde_json::from_str(&text).expect("limits deserialize");
        assert!(
            back.validate().is_err(),
            "the serde path refuses too: quote {quote_age}/skew {skew}"
        );
        let engine = RiskEngine::new(limits);
        let refused = engine
            .check(&proposal(), limit_spread(), &healthy())
            .expect_err("an invalid engine fails closed");
        assert!(refused.reasons.contains(&RiskReason::Unrepresentable));
    }
    let edge = RiskLimits::new()
        .quote_max_age_secs(3_600)
        .clock_skew_secs(1)
        .build();
    assert!(
        edge.validate().is_ok(),
        "the declared bounds still construct"
    );
}

#[test]
fn unrepresentable_cap_fails_closed_at_construction() {
    for cap in [f64::NAN, f64::INFINITY, f64::MAX] {
        let limits = RiskLimits::new().max_loss_per_trade(cap).build();
        assert_eq!(limits.loss_cap(), None, "the cap has no decimal reading");
        let engine = RiskEngine::new(limits);
        let refused = engine
            .check(&proposal(), limit_spread(), &healthy())
            .expect_err("no reading fails closed");
        assert!(refused.reasons.contains(&RiskReason::Unrepresentable));
    }
    let limits = RiskLimits::new().max_notional(f64::MAX).build();
    assert_eq!(
        limits.notional_cap(),
        None,
        "the cap has no decimal reading"
    );
    let engine = RiskEngine::new(limits);
    let refused = engine
        .check(&proposal(), limit_spread(), &healthy())
        .expect_err("no reading fails closed");
    assert!(refused.reasons.contains(&RiskReason::Unrepresentable));
}

#[test]
fn engine_field_edits_reparse_the_stored_caps() {
    use fluent_wvr::FieldAccess as _;
    let mut engine = RiskEngine::new(RiskLimits::default());
    assert!(engine
        .check(&proposal(), limit_spread(), &healthy())
        .is_ok());
    engine
        .set_field("max_loss_per_trade", "100")
        .expect("cap edits");
    let refused = engine
        .check(&proposal(), limit_spread(), &healthy())
        .expect_err("the edited cap binds");
    assert!(refused.reasons.contains(&RiskReason::OverLossCap));
}

#[test]
fn try_new_refuses_out_of_bound_configuration_up_front() {
    assert!(RiskEngine::try_new(RiskLimits::default()).is_ok());
    let edge = RiskLimits::new()
        .quote_max_age_secs(3_600)
        .clock_skew_secs(1)
        .build();
    assert!(
        RiskEngine::try_new(edge).is_ok(),
        "declared edges construct"
    );
    for (quote_age, skew) in [(3_601, 5), (0, 5), (60, 3_601), (60, 0)] {
        let limits = RiskLimits::new()
            .quote_max_age_secs(quote_age)
            .clock_skew_secs(skew)
            .build();
        assert!(
            RiskEngine::try_new(limits).is_err(),
            "quote {quote_age}/skew {skew} refuses at construction, not at first check"
        );
    }
    let bad_cap = RiskLimits::new().max_loss_per_trade(f64::NAN).build();
    assert!(
        RiskEngine::try_new(bad_cap).is_err(),
        "unreadable cap refuses up front"
    );
}

#[test]
fn engine_field_edits_cannot_widen_into_unchecked_bounds() {
    use fluent_wvr::FieldAccess as _;
    let mut engine = RiskEngine::new(RiskLimits::default());
    assert!(engine
        .check(&proposal(), limit_spread(), &healthy())
        .is_ok());
    // The schema path enforces the declared bounds on the edit itself, so an
    // over-bound age never lands in the engine to be caught later.
    let refused = engine
        .set_field("quote_max_age_secs", "99999")
        .expect_err("over-bound edit");
    assert!(
        format!("{refused:?}").contains("3600"),
        "the refusal names the bound: {refused:?}"
    );
    assert!(
        engine
            .check(&proposal(), limit_spread(), &healthy())
            .is_ok(),
        "the refused edit changed nothing"
    );
    let refused = engine
        .set_field("clock_skew_secs", "0")
        .expect_err("under-bound edit");
    assert!(
        format!("{refused:?}").contains("minimum 1"),
        "the refusal names the bound: {refused:?}"
    );
}

#[test]
fn aggregate_caps_are_inert_when_unconfigured() {
    let engine = RiskEngine::new(RiskLimits::default());
    assert!(!engine.limits().has_aggregate_caps());
    assert!(engine
        .check(&proposal(), limit_spread(), &healthy())
        .is_ok());
}

#[test]
fn a_configured_aggregate_cap_refuses_unobserved_facts() {
    let engine = RiskEngine::new(RiskLimits::new().max_open_risk(1_000.0).build());
    let refused = engine
        .check(&proposal(), limit_spread(), &healthy())
        .expect_err("unobserved");
    assert!(
        refused
            .reasons
            .contains(&RiskReason::PortfolioFactsUnavailable),
        "an unobserved cap must fail closed: {:?}",
        refused.reasons
    );
}

#[test]
fn observed_open_risk_over_the_cap_refuses_and_under_it_passes() {
    let engine = RiskEngine::new(RiskLimits::new().max_open_risk(100.0).build());
    let over = RiskState {
        facts: PortfolioFacts::observed(money(150), None, None, None),
        ..healthy()
    };
    assert!(engine
        .check(&proposal(), limit_spread(), &over)
        .expect_err("over cap")
        .reasons
        .contains(&RiskReason::OverOpenRisk));

    let under = RiskState {
        facts: PortfolioFacts::observed(money(40), None, None, None),
        ..healthy()
    };
    assert!(engine.check(&proposal(), limit_spread(), &under).is_ok());
}

#[test]
fn a_nan_net_delta_denies() {
    let engine = RiskEngine::new(RiskLimits::new().max_abs_net_delta(5.0).build());
    let nan_greeks = Greeks::new().delta(f64::NAN).build();
    let state = RiskState {
        facts: PortfolioFacts::observed(money(0), Some(nan_greeks), None, None),
        ..healthy()
    };
    assert!(engine
        .check(&proposal(), limit_spread(), &state)
        .is_err_and(|refused| refused
            .reasons
            .contains(&RiskReason::PortfolioFactsUnavailable)));
}

#[test]
fn a_buying_power_floor_refuses_when_unmet() {
    let engine = RiskEngine::new(RiskLimits::new().min_buying_power(500.0).build());
    let short = RiskState {
        facts: PortfolioFacts::observed(money(0), None, Some(money(100)), None),
        ..healthy()
    };
    assert!(engine
        .check(&proposal(), limit_spread(), &short)
        .expect_err("under floor")
        .reasons
        .contains(&RiskReason::InsufficientBuyingPower));

    let covered = RiskState {
        facts: PortfolioFacts::observed(money(0), None, Some(money(5_000)), None),
        ..healthy()
    };
    assert!(engine.check(&proposal(), limit_spread(), &covered).is_ok());
}

#[test]
fn try_new_refuses_aggregate_caps_without_a_producer() {
    assert!(RiskEngine::try_new(RiskLimits::default()).is_ok());
    assert!(RiskEngine::try_new(RiskLimits::new().max_open_risk(100.0).build()).is_err());
}

// ── The delta cap reads supplied greeks; nothing supplied refuses ──────────

fn greeks_with_delta(delta: f64) -> Greeks {
    Greeks::new().delta(delta).build()
}

#[test]
fn a_supplied_delta_breach_denies_with_over_net_delta() {
    let engine = RiskEngine::new(RiskLimits::new().max_abs_net_delta(5.0).build());
    let state = RiskState {
        facts: PortfolioFacts::observed(money(0), Some(greeks_with_delta(5.5)), None, None),
        ..healthy()
    };
    assert!(engine
        .check(&proposal(), limit_spread(), &state)
        .is_err_and(|refused| refused.reasons.contains(&RiskReason::OverNetDelta)));
    let state = RiskState {
        facts: PortfolioFacts::observed(money(0), Some(greeks_with_delta(-5.5)), None, None),
        ..healthy()
    };
    assert!(engine
        .check(&proposal(), limit_spread(), &state)
        .is_err_and(|refused| refused.reasons.contains(&RiskReason::OverNetDelta)));
}

#[test]
fn no_greeks_source_refuses_as_unavailable() {
    let engine = RiskEngine::new(RiskLimits::new().max_abs_net_delta(5.0).build());
    // No source attached: the facts carry no greeks, so the configured cap
    // refuses rather than passing a book it cannot see.
    let state = RiskState {
        facts: PortfolioFacts::observed(money(0), None, None, None),
        ..healthy()
    };
    assert!(engine
        .check(&proposal(), limit_spread(), &state)
        .is_err_and(|refused| refused
            .reasons
            .contains(&RiskReason::PortfolioFactsUnavailable)));
}

// ── A computed source wraps the kernels closed form ─────────────────────────

#[test]
fn a_computed_source_prices_greeks_like_the_closed_form() {
    use chrono::NaiveDate;
    use protopyne_types::{OptionContract, OptionKind};

    let as_of = NaiveDate::from_ymd_opt(2026, 6, 18).expect("valid test date");
    let source = ComputedGreeks {
        spot: 100.0,
        rate: 0.02,
        volatility: 0.25,
        as_of,
    };
    let contract = OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(NaiveDate::from_ymd_opt(2026, 12, 18).expect("valid test date"))
        .strike(money(100))
        .kind(OptionKind::Call)
        .build();
    let supplied = source.greeks(&contract).expect("a live contract prices");
    let years =
        f64::from(u32::try_from((contract.expiry - as_of).num_days()).expect("live expiry"))
            / 365.0;
    let direct = protopyne_kernels::bs_greeks(OptionKind::Call, 100.0, 100.0, years, 0.02, 0.25);
    assert!((supplied.delta - direct.delta).abs() < 1e-12);
    assert!((supplied.gamma - direct.gamma).abs() < 1e-12);
    // An expired contract has no greeks — absence, never a number.
    let mut dead = contract.clone();
    dead.expiry = as_of;
    assert!(source.greeks(&dead).is_none());
    // Garbage inputs refuse rather than miscompute.
    let wild = ComputedGreeks {
        spot: f64::NAN,
        ..source
    };
    assert!(wild.greeks(&contract).is_none());
}
