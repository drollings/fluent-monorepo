use super::*;
use fluent_wvr::prelude::*;
use protopyne_types::{Validate, check_for_execute};

#[test]
fn session_config_trait_parity() {
    let good = SessionConfig::default();
    assert!(good.validate().is_ok());
    assert!(Validate::validate(&good).is_ok());
    let bad_workers = SessionConfig::new()
        .workers(0)
        .queue_capacity(64)
        .credit_limit(16)
        .build();
    assert!(bad_workers.validate().is_err());
    assert!(Validate::validate(&bad_workers).is_err());
    let bad_credit = SessionConfig::new()
        .workers(4)
        .queue_capacity(64)
        .credit_limit(0)
        .build();
    assert!(bad_credit.validate().is_err());
    assert!(Validate::validate(&bad_credit).is_err());
}

#[test]
fn invalid_session_config_fails_execute_bridge() {
    let bad = SessionConfig::new()
        .workers(0)
        .queue_capacity(64)
        .credit_limit(16)
        .build();
    assert!(check_for_execute(&bad, "session config invalid").is_err());
    let ctx = WorkContext::default();
    assert!(bad.execute(&ctx).is_err());
    assert!(SessionConfig::default().execute(&ctx).is_ok());
}
