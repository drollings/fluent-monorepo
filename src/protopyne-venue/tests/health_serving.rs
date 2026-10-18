//! Serving-path gating: the command plane's answer AND the health gate over
//! the one fact the serving path observes.
//!
//! The serving path never sees quotes, clocks, or stream state — those are
//! the live feed's facts — so the verdict builds the pruned no-objection
//! state around the store reading alone. Entries need both halves; exits
//! stay the command plane's answer, which always routes.

use super::*;
use protopyne_store::StoreHealth;

use crate::command::TradingState;

fn healthy_reading() -> StoreHealth {
    StoreHealth {
        observed: true,
        ..StoreHealth::default()
    }
}

fn outage_reading() -> StoreHealth {
    StoreHealth {
        reachable: false,
        observed: true,
        ..StoreHealth::default()
    }
}

#[test]
fn serving_verdict_passes_healthy_and_unobserved_and_halts_on_outage() {
    assert_eq!(serving_verdict(healthy_reading()), HealthVerdict::Trade);
    assert_eq!(
        serving_verdict(StoreHealth::default()),
        HealthVerdict::Trade,
        "no observation is no objection"
    );
    match serving_verdict(outage_reading()) {
        HealthVerdict::Halt { reasons } => {
            assert!(
                reasons.iter().any(|r| r.contains("store")),
                "got: {reasons:?}"
            );
        }
        HealthVerdict::Trade => panic!("an observed outage must halt"),
    }
}

#[test]
fn serving_entries_need_an_enabled_book_and_a_passing_verdict() {
    let cases = [
        (TradingState::Enabled, healthy_reading(), true),
        (TradingState::Enabled, StoreHealth::default(), true),
        (TradingState::Enabled, outage_reading(), false),
        (TradingState::ExitOnly, healthy_reading(), false),
        (TradingState::ExitOnly, StoreHealth::default(), false),
        (TradingState::ExitOnly, outage_reading(), false),
        (TradingState::Disabled, healthy_reading(), false),
        (TradingState::Disabled, StoreHealth::default(), false),
        (TradingState::Disabled, outage_reading(), false),
    ];
    for (trading, reading, expected) in cases {
        assert_eq!(
            serving_allows_entries(trading, reading),
            expected,
            "serving entries under {trading:?} with {reading:?}"
        );
    }
}
