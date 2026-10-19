use super::*;
use fluent_wvr::prelude::*;
use protopyne_types::{check_for_execute, Validate};

#[test]
fn session_config_trait_parity() {
    let good = SessionConfig::default();
    assert!(good.validate().is_ok());
    assert!(Validate::validate(&good).is_ok());
    let bad_timeout = SessionConfig::new().shutdown_timeout_secs(0).build();
    assert!(bad_timeout.validate().is_err());
    assert!(Validate::validate(&bad_timeout).is_err());
}

#[test]
fn invalid_session_config_fails_execute_bridge() {
    let bad = SessionConfig::new().shutdown_timeout_secs(0).build();
    assert!(check_for_execute(&bad, "session config invalid").is_err());
    let ctx = WorkContext::default();
    assert!(bad.execute(&ctx).is_err());
    assert!(SessionConfig::default().execute(&ctx).is_ok());
}
