//! Exits survive a store halt: a store outage halts entries and must never
//! strand an open position, and this suite is the proof of that ordering.
//!
//! A store outage halts entries and must never strand an open position. The
//! suite pins both halves of that contract at the gate level: the health
//! gate refuses the entry intent with the store reason while routing the
//! exit intent, and the command-plane twin (`TradingState::check`) agrees
//! intent for intent — the two gates compose instead of contradicting.

use super::*;
use protopyne_store::StoreHealth;

use crate::command::TradingState;

fn store_halted() -> (HealthState, HealthLimits) {
    let state = HealthState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        stream_connected: true,
        store: StoreHealth {
            reachable: false,
            ..StoreHealth::default()
        },
        reconciled: true,
        backfilled: true,
    };
    (state, HealthLimits::new().build())
}

#[test]
fn store_halt_refuses_entries_and_routes_exits() {
    let (state, limits) = store_halted();
    let err = check_trade(&state, &limits, false).expect_err("store halt refuses entries");
    assert!(matches!(err, VenueError::Health(_)));
    assert!(
        check_trade(&state, &limits, true).is_ok(),
        "a store halt must never strand an open position"
    );
}

#[test]
fn health_gate_and_command_gate_agree_intent_for_intent() {
    let (state, limits) = store_halted();
    // The halted book routes exits and refuses entries on both gates.
    let trading = TradingState::ExitOnly;
    assert!(check_trade(&state, &limits, false).is_err());
    assert!(trading.check(false).is_err());
    assert!(check_trade(&state, &limits, true).is_ok());
    assert!(trading.check(true).is_ok());
    // A healthy world routes everything on both gates.
    let healthy = HealthState {
        store: StoreHealth::default(),
        ..state
    };
    assert!(check_trade(&healthy, &limits, false).is_ok());
    assert!(TradingState::Enabled.check(false).is_ok());
}
