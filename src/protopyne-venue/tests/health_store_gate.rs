//! Store-health gate pins: the fourth `may_trade` gate halts entries on a
//! store fault and stays invisible otherwise.
//!
//! The control group comes first on purpose. A gate that fires on a healthy
//! store trains the operator to ignore it; the suite proves silence before
//! it proves refusal — a default reading evaluates exactly as the three
//! pre-existing fields alone did, and each boundary (exactly at the limit)
//! still trades.

use super::*;
use protopyne_store::StoreHealth;

fn healthy() -> HealthState {
    HealthState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        stream_connected: true,
        store: StoreHealth::default(),
        reconciled: true,
        backfilled: true,
    }
}

fn limits() -> HealthLimits {
    HealthLimits::new().build()
}

fn with_store(store: StoreHealth) -> HealthState {
    HealthState { store, ..healthy() }
}

#[test]
fn default_store_reading_trades_exactly_as_before() {
    // Control group: no store observation is no objection. The verdict is
    // `Trade`, the same value the three pre-existing fields produced alone.
    let verdict = may_trade(&healthy(), &limits());
    assert_eq!(verdict, HealthVerdict::Trade);
    assert!(check_trade(&healthy(), &limits(), false).is_ok());
}

#[test]
fn unreachable_store_halts_entries_and_names_the_gate() {
    let state = with_store(StoreHealth {
        reachable: false,
        ..StoreHealth::default()
    });
    match may_trade(&state, &limits()) {
        HealthVerdict::Halt { reasons } => {
            assert!(reasons.iter().any(|r| r.contains("store")), "names the gate, got: {reasons:?}");
        }
        HealthVerdict::Trade => panic!("unreachable store must halt"),
    }
    let err = check_trade(&state, &limits(), false).expect_err("unreachable store refuses entries");
    assert!(matches!(err, VenueError::Health(_)));
}

#[test]
fn stale_watermark_halts_at_the_boundary() {
    // Exactly at the limit still trades; one second past it halts.
    let at_limit = with_store(StoreHealth {
        watermark_age_secs: Some(60),
        ..StoreHealth::default()
    });
    assert_eq!(may_trade(&at_limit, &limits()), HealthVerdict::Trade);

    let past_limit = with_store(StoreHealth {
        watermark_age_secs: Some(61),
        ..StoreHealth::default()
    });
    match may_trade(&past_limit, &limits()) {
        HealthVerdict::Halt { reasons } => assert!(
            reasons.iter().any(|r| r.contains("watermark")),
            "names the gate, got: {reasons:?}"
        ),
        HealthVerdict::Trade => panic!("stale watermark must halt"),
    }
}

#[test]
fn open_gaps_halt_until_repair_closes_them() {
    let gappy = with_store(StoreHealth {
        open_gaps: 1,
        ..StoreHealth::default()
    });
    match may_trade(&gappy, &limits()) {
        HealthVerdict::Halt { reasons } => {
            assert!(reasons.iter().any(|r| r.contains("gap")), "names the gate, got: {reasons:?}");
        }
        HealthVerdict::Trade => panic!("open gaps must halt"),
    }
}

#[test]
fn unobserved_reading_passes_entries() {
    // No observation is no objection: unchanged behavior.
    let state = healthy();
    assert!(!state.store.observed, "the default carries no observation");
    assert_eq!(may_trade(&state, &limits()), HealthVerdict::Trade);
    assert!(check_trade(&state, &limits(), false).is_ok());
    assert!(check_trade(&state, &limits(), true).is_ok());
}

#[test]
fn observed_default_values_pass_and_stay_distinguishable() {
    let observed = StoreHealth {
        observed: true,
        ..StoreHealth::default()
    };
    let state = with_store(observed);
    assert_eq!(may_trade(&state, &limits()), HealthVerdict::Trade);
    assert!(check_trade(&state, &limits(), false).is_ok());
    // Same verdict as the unobserved world, but the observation flag tells
    // the two readings apart downstream.
    assert!(observed.observed);
    assert!(!StoreHealth::default().observed);
}

#[test]
fn observed_unreachable_halts_entries_through_the_authority_gate() {
    let state = with_store(StoreHealth {
        reachable: false,
        observed: true,
        ..StoreHealth::default()
    });
    let err = check_trade(&state, &limits(), false).expect_err("observed outage halts entries");
    assert!(matches!(err, VenueError::Health(_)));
    let report = GateReport::evaluate(&state, &limits());
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].gate, HealthGate::StoreReachable);
    assert_eq!(report.failures[0].gate.class(), GateClass::Authority);
}

#[test]
fn every_failed_gate_is_named_together() {
    let state = HealthState {
        quote_age_secs: 5_000,
        store: StoreHealth {
            reachable: false,
            ..StoreHealth::default()
        },
        ..healthy()
    };
    match may_trade(&state, &limits()) {
        HealthVerdict::Halt { reasons } => {
            assert!(reasons.iter().any(|r| r.contains("quote")), "got: {reasons:?}");
            assert!(reasons.iter().any(|r| r.contains("store")), "got: {reasons:?}");
        }
        HealthVerdict::Trade => panic!("two faults must halt"),
    }
}
