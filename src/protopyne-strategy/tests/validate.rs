use super::*;
use chrono::{TimeZone, Utc};
use fluent_wvr::prelude::*;
use protopyne_types::{check_for_execute, Money, Validate};
use rust_decimal::Decimal;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn valid_grant() -> Grant {
    Grant::new()
        .id("validate-fixture".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
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
                .max_trades(1_000)
                .max_contracts(10_000)
                .max_loss(money(1_000_000))
                .max_notional(money(100_000_000))
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
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

#[test]
fn risk_limits_trait_parity() {
    let good = RiskLimits::default();
    assert!(good.validate().is_ok());
    assert!(Validate::validate(&good).is_ok());
    let bad = RiskLimits::new()
        .max_contracts_per_trade(0)
        .max_loss_per_trade(500.0)
        .max_notional(50_000.0)
        .quote_max_age_secs(60)
        .clock_skew_secs(5)
        .limit_only_spreads(true)
        .kill_switch(false)
        .build();
    assert!(bad.validate().is_err());
    assert!(Validate::validate(&bad).is_err());
    let bad_loss = RiskLimits::new()
        .max_contracts_per_trade(10)
        .max_loss_per_trade(f64::INFINITY)
        .max_notional(50_000.0)
        .quote_max_age_secs(60)
        .clock_skew_secs(5)
        .limit_only_spreads(true)
        .kill_switch(false)
        .build();
    assert!(bad_loss.validate().is_err());
    assert!(Validate::validate(&bad_loss).is_err());
}

#[test]
fn invalid_limits_fail_execute_bridge() {
    let bad = RiskLimits::new()
        .max_contracts_per_trade(0)
        .max_loss_per_trade(500.0)
        .max_notional(50_000.0)
        .quote_max_age_secs(60)
        .clock_skew_secs(5)
        .limit_only_spreads(true)
        .kill_switch(false)
        .build();
    assert!(check_for_execute(&bad, "risk limits invalid").is_err());
    let engine = RiskEngine::new(bad);
    let ctx = WorkContext::default();
    assert!(engine.execute(&ctx).is_err());
    let good_engine = RiskEngine::new(RiskLimits::default());
    assert!(good_engine.execute(&ctx).is_ok());
}

#[test]
fn grant_validate_covers_service_cases() {
    let good = valid_grant();
    assert!(good.validate().is_ok());
    assert!(Validate::validate(&good).is_ok());
    let mut revoked = valid_grant();
    revoked.revoked = true;
    assert!(revoked.validate().is_err());
    assert!(Validate::validate(&revoked).is_err());
    // Empty window.
    let mut empty_window = valid_grant();
    empty_window.valid_until = empty_window.valid_from;
    assert!(empty_window.validate().is_err());
    assert!(Validate::validate(&empty_window).is_err());
    // Empty scope.
    let mut empty_scope = valid_grant();
    empty_scope.scope.strategies.clear();
    assert!(empty_scope.validate().is_err());
    assert!(Validate::validate(&empty_scope).is_err());
}
