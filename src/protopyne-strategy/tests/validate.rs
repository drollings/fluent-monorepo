use super::*;
use fluent_wvr::prelude::*;
use fluent_wvr::validate::{check_for_execute, Validate};

fn valid_grant() -> Grant {
    // The shared fixture's shape with a named id and a signal floor above zero.
    crate::fixtures::grant_with(|grant| {
        grant.id = "validate-fixture".to_owned();
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
    })
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
    // A revoked grant stays representable (structural validation passes) but
    // is refused at arm time.
    assert!(revoked.validate().is_ok());
    assert!(Validate::validate(&revoked).is_ok());
    assert!(revoked.validate_armable().is_err());
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
