//! Gate-by-intent truth table: every health gate failing alone, routed as
//! an entry and as an exit.
//!
//! The behavior half pins today's contract through the legacy entry point:
//! a healthy world routes both intents, each single fault refuses entries
//! while naming its gate, and exits route past every fault. The taxonomy
//! half pins the same table through the classified report: each single
//! fault evaluates to exactly its own gate with its documented class.

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

/// One failing gate at a time, each paired with the word its refusal must
/// name and with its classified gate and class.
fn single_faults() -> Vec<(&'static str, HealthState, HealthGate, GateClass)> {
    vec![
        (
            "quote",
            HealthState {
                quote_age_secs: 61,
                ..healthy()
            },
            HealthGate::QuoteAge,
            GateClass::Integrity,
        ),
        (
            "clock",
            HealthState {
                clock_skew_secs: 6,
                ..healthy()
            },
            HealthGate::ClockSkew,
            GateClass::Integrity,
        ),
        (
            "stream",
            HealthState {
                stream_connected: false,
                ..healthy()
            },
            HealthGate::StreamState,
            GateClass::Integrity,
        ),
        (
            "store",
            HealthState {
                store: StoreHealth {
                    reachable: false,
                    ..StoreHealth::default()
                },
                ..healthy()
            },
            HealthGate::StoreReachable,
            GateClass::Authority,
        ),
        (
            "watermark",
            HealthState {
                store: StoreHealth {
                    watermark_age_secs: Some(61),
                    ..StoreHealth::default()
                },
                ..healthy()
            },
            HealthGate::StoreWatermarkAge,
            GateClass::Integrity,
        ),
        (
            "gap",
            HealthState {
                store: StoreHealth {
                    open_gaps: 1,
                    ..StoreHealth::default()
                },
                ..healthy()
            },
            HealthGate::StoreGaps,
            GateClass::Integrity,
        ),
        (
            "reconcil",
            HealthState {
                reconciled: false,
                ..healthy()
            },
            HealthGate::Reconcile,
            GateClass::Integrity,
        ),
        (
            "backfill",
            HealthState {
                backfilled: false,
                ..healthy()
            },
            HealthGate::Backfill,
            GateClass::Integrity,
        ),
    ]
}

#[test]
fn healthy_world_routes_both_intents() {
    let (state, limits) = (healthy(), limits());
    assert!(check_trade(&state, &limits, false).is_ok());
    assert!(check_trade(&state, &limits, true).is_ok());
    assert!(check_route(&state, &limits, RouteIntent::Entry).is_ok());
    assert!(check_route(&state, &limits, RouteIntent::Exit).is_ok());
}

#[test]
fn entries_refuse_each_single_fault_and_name_the_gate() {
    let limits = limits();
    for (keyword, state, _, _) in single_faults() {
        let Err(err) = check_trade(&state, &limits, false) else {
            panic!("{keyword} fault must refuse entries")
        };
        match err {
            VenueError::Health(reason) => {
                assert!(
                    reason.contains(keyword),
                    "{keyword} fault must name its gate, got: {reason}"
                );
            }
            other => panic!("{keyword} fault must halt with a health reason, got: {other:?}"),
        }
    }
}

#[test]
fn exits_route_past_every_single_fault() {
    let limits = limits();
    for (keyword, state, _, _) in single_faults() {
        assert!(
            check_trade(&state, &limits, true).is_ok(),
            "{keyword} fault must never strand an exit"
        );
        assert!(
            check_route(&state, &limits, RouteIntent::Exit).is_ok(),
            "{keyword} fault must never strand an exit"
        );
    }
}

#[test]
fn each_single_fault_reports_exactly_its_gate_and_class() {
    let limits = limits();
    for (keyword, state, gate, class) in single_faults() {
        let report = GateReport::evaluate(&state, &limits);
        assert_eq!(
            report.verdict,
            HealthVerdict::Halt {
                reasons: report
                    .failures
                    .iter()
                    .map(|failure| failure.reason.clone())
                    .collect(),
            },
            "{keyword} fault must halt"
        );
        assert_eq!(
            report.failures.len(),
            1,
            "{keyword} fault must fail exactly one gate, got: {:?}",
            report.failures
        );
        let failure = &report.failures[0];
        assert_eq!(failure.gate, gate, "{keyword} fault misattributed");
        assert_eq!(failure.gate.class(), class, "{keyword} fault misclassified");
        assert!(
            failure.reason.contains(keyword),
            "{keyword} failure must name its gate, got: {}",
            failure.reason
        );
    }
}

#[test]
fn routed_entries_carry_the_passing_report() {
    let report =
        check_route(&healthy(), &limits(), RouteIntent::Entry).expect("healthy entries route");
    assert_eq!(report.verdict, HealthVerdict::Trade);
    assert!(report.failures.is_empty());
}

#[test]
fn routed_exits_record_no_evaluation() {
    // Exits bypass every gate, including the store fault that refuses
    // entries: the report records the routing outcome, not an evaluation.
    let (_, state, _, _) = single_faults()
        .into_iter()
        .find(|(keyword, _, _, _)| *keyword == "store")
        .expect("store fault row exists");
    let report = check_route(&state, &limits(), RouteIntent::Exit).expect("exits route");
    assert_eq!(report.verdict, HealthVerdict::Trade);
    assert!(report.failures.is_empty());
}

#[test]
fn refused_entries_agree_legacy_and_route() {
    // The route entry point refuses with the exact legacy error: same
    // variant, same message, built from the same failure reasons.
    let limits = limits();
    for (keyword, state, _, _) in single_faults() {
        let legacy = check_trade(&state, &limits, false).expect_err("entries refuse");
        let routed = check_route(&state, &limits, RouteIntent::Entry).expect_err("entries refuse");
        assert_eq!(
            format!("{legacy:?}"),
            format!("{routed:?}"),
            "{keyword} refusal must be identical on both entry points"
        );
    }
}
