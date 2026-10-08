use super::*;
use crate::proposal::{Direction, Proposal, ProposalSide, StructureKind};
use chrono::{TimeZone, Utc};
use protopyne_types::{Money, Qty};
use rust_decimal::Decimal;

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
        .state(crate::proposal::ProposalState::Proposed)
        .created_at(Utc::now())
        .build()
}

fn grant() -> Grant {
    Grant::new()
        .id("g1".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical, StructureKind::NakedShort])
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(10)
                .max_contracts(100)
                .max_loss(money(5_000))
                .max_notional(money(100_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.5)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(money(1_000))
                .max_spread_width(money(50))
                .min_dte_days(7)
                .max_dte_days(60)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money(10_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

fn context() -> (GrantUsage, GrantMarket, BreakerState) {
    (
        GrantUsage::new().build(),
        GrantMarket::new()
            .quote_age_secs(5)
            .spread_width(money(10))
            .build(),
        BreakerState::new().build(),
    )
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn deny_reasons(decision: GrantDecision) -> Vec<String> {
    match decision {
        GrantDecision::Allow => panic!("expected Deny, got Allow"),
        GrantDecision::Deny { reasons } => reasons,
    }
}

#[test]
fn valid_defined_risk_vertical_is_allowed() {
    let (usage, market, breakers) = context();
    let decision = evaluate_grant(&proposal(), &grant(), &usage, &market, &breakers, now());
    assert!(decision.is_allow(), "unexpected denial: {decision:?}");
}

#[test]
fn revoked_and_expired_grants_authorize_nothing() {
    let (usage, market, breakers) = context();
    let revoked = grant().revoked_copy();
    assert!(!revoked.is_live(now()));
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &revoked,
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("revoked")));

    let after = Utc.with_ymd_and_hms(2028, 1, 1, 0, 0, 0).unwrap();
    assert!(!grant().is_live(after));
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        after,
    ));
    assert!(reasons.iter().any(|r| r.contains("window")));
}

#[test]
fn scope_mismatches_deny() {
    let (usage, market, breakers) = context();
    let mut p = proposal();
    p.strategy = "other".to_owned();
    let reasons = deny_reasons(evaluate_grant(&p, &grant(), &usage, &market, &breakers, now()));
    assert!(reasons.iter().any(|r| r.contains("scope")));

    let mut p = proposal();
    p.side = ProposalSide::Roll;
    let reasons = deny_reasons(evaluate_grant(&p, &grant(), &usage, &market, &breakers, now()));
    assert!(reasons.iter().any(|r| r.contains("scope")));
}

#[test]
fn exhausted_budgets_deny() {
    let (_, market, breakers) = context();
    let usage = GrantUsage::new().used_trades(10).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("trade budget")));

    let usage = GrantUsage::new().used_contracts(99).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("contract budget")));

    let usage = GrantUsage::new().used_loss(money(4_900)).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("max-loss budget")));

    let usage = GrantUsage::new().used_notional(money(99_500)).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("notional budget")));
}

#[test]
fn weak_signals_and_bad_trades_deny() {
    let (usage, market, breakers) = context();
    let mut p = proposal();
    p.conviction = 0.1;
    let reasons = deny_reasons(evaluate_grant(&p, &grant(), &usage, &market, &breakers, now()));
    assert!(reasons.iter().any(|r| r.contains("conviction")));

    let mut p = proposal();
    p.activity_score = 0.1;
    let reasons = deny_reasons(evaluate_grant(&p, &grant(), &usage, &market, &breakers, now()));
    assert!(reasons.iter().any(|r| r.contains("activity")));

    let mut p = proposal();
    p.max_loss = money(5_000);
    let reasons = deny_reasons(evaluate_grant(&p, &grant(), &usage, &market, &breakers, now()));
    assert!(reasons.iter().any(|r| r.contains("per-trade")));

    let wide = GrantMarket::new()
        .quote_age_secs(5)
        .spread_width(money(500))
        .build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &wide,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("spread width")));

    let mut p = proposal();
    p.dte_days = 2;
    let reasons = deny_reasons(evaluate_grant(&p, &grant(), &usage, &market, &breakers, now()));
    assert!(reasons.iter().any(|r| r.contains("DTE")));
}

#[test]
fn naked_short_is_excluded_even_when_in_scope() {
    let (usage, market, breakers) = context();
    let mut p = proposal();
    p.structure = StructureKind::NakedShort;
    let reasons = deny_reasons(evaluate_grant(&p, &grant(), &usage, &market, &breakers, now()));
    assert!(reasons.iter().any(|r| r.contains("naked")));
}

#[test]
fn stale_quotes_and_tripped_breakers_deny() {
    let (usage, _, breakers) = context();
    let stale = GrantMarket::new()
        .quote_age_secs(600)
        .spread_width(money(10))
        .build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &stale,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("stale")));

    let (_, market, _) = context();
    let tripped = BreakerState::new()
        .tripped(true)
        .trip_reason("loss limit".to_owned())
        .build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &tripped,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("breaker")));

    let errors = BreakerState::new().consecutive_errors(10).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &errors,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("consecutive-error")));
}

#[test]
fn evaluation_is_deterministic() {
    let (usage, market, breakers) = context();
    let first = evaluate_grant(&proposal(), &grant(), &usage, &market, &breakers, now());
    let second = evaluate_grant(&proposal(), &grant(), &usage, &market, &breakers, now());
    assert_eq!(first, second);
}

#[test]
fn tighten_accepts_restriction_and_refuses_loosening() {
    let tight_budgets = GrantBudgets::new()
        .max_trades(5)
        .max_contracts(50)
        .max_loss(money(2_000))
        .max_notional(money(50_000))
        .build();
    let tightened = grant()
        .tighten(GrantPatch::new().budgets(tight_budgets).build())
        .expect("tighter budgets");
    assert_eq!(tightened.budgets.max_trades, 5);

    let loose_budgets = GrantBudgets::new()
        .max_trades(50)
        .max_contracts(500)
        .max_loss(money(50_000))
        .max_notional(money(500_000))
        .build();
    assert!(
        grant()
            .tighten(GrantPatch::new().budgets(loose_budgets).build())
            .is_err(),
        "raising a budget must need a new grant"
    );

    let loose_signal = SignalThresholds::new()
        .min_conviction(0.1)
        .min_activity_score(0.1)
        .build();
    assert!(
        grant()
            .tighten(GrantPatch::new().signal(loose_signal).build())
            .is_err(),
        "lowering a signal threshold must need a new grant"
    );

    let later = Utc.with_ymd_and_hms(2028, 1, 1, 0, 0, 0).unwrap();
    assert!(
        grant()
            .tighten(GrantPatch::new().valid_until(later).build())
            .is_err(),
        "extending the window must need a new grant"
    );

    let earlier = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
    let shortened = grant()
        .tighten(GrantPatch::new().valid_until(earlier).build())
        .expect("shorter window");
    assert_eq!(shortened.valid_until, earlier);
}
