use super::*;
use protopyne_store::StoreHealth;

fn healthy() -> HealthState {
    HealthState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        stream_connected: true,
        store: StoreHealth::default(),
    }
}

fn limits() -> HealthLimits {
    HealthLimits::new().build()
}

#[test]
fn fresh_connected_state_may_trade() {
    let verdict = may_trade(&healthy(), &limits());
    assert!(verdict.may_trade());
    assert!(check_trade(&healthy(), &limits(), false).is_ok());
}

#[test]
fn each_gate_fails_with_its_reason() {
    let stale = HealthState {
        quote_age_secs: 5_000,
        ..healthy()
    };
    match may_trade(&stale, &limits()) {
        HealthVerdict::Halt { reasons } => assert!(reasons.iter().any(|r| r.contains("quote"))),
        HealthVerdict::Trade => panic!("stale must halt"),
    }

    let skewed = HealthState {
        clock_skew_secs: 500,
        ..healthy()
    };
    assert!(!may_trade(&skewed, &limits()).may_trade());

    let dark = HealthState {
        stream_connected: false,
        ..healthy()
    };
    let err = check_trade(&dark, &limits(), false).expect_err("dark");
    assert!(matches!(err, VenueError::Health(_)));
}
