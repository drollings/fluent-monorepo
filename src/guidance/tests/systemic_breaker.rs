//! Systemic breaker signal: one cause failing across distinct items
//! trips and names it; scattered causes never trip the systemic wire.
//! Existing signals (repeat/revert/stagnation) keep their own pins.

use crate::maintenance::{is_breaker_trip, BreakerConfig, LoopBreaker};

fn strict() -> LoopBreaker {
    LoopBreaker::new(BreakerConfig::strict())
}

fn failing(file: &str, sig: &str) -> (String, String) {
    (file.to_string(), sig.to_string())
}

#[test]
fn scattered_causes_never_trip_systemic() {
    let mut breaker = strict();
    // Distinct causes across waves: the systemic wire stays quiet even
    // as items keep failing (other signals may fire on their own).
    for (index, sig) in ["E1", "E2", "E3", "E4"].iter().enumerate() {
        let file = format!("f{index}.rs");
        assert_eq!(
            breaker.observe_with_failing(true, &[failing(&file, sig)]),
            None,
            "scattered cause must not trip systemic"
        );
    }
}

#[test]
fn one_cause_across_items_trips_and_names_it() {
    let mut breaker = strict();
    assert_eq!(
        breaker.observe_with_failing(true, &[failing("a.rs", "E9")]),
        None
    );
    assert_eq!(
        breaker.observe_with_failing(true, &[failing("b.rs", "E9")]),
        None
    );
    let stop = breaker.observe_with_failing(true, &[failing("c.rs", "E9")]);
    let reason = stop.expect("third distinct file, one cause: must trip");
    assert!(
        reason.starts_with("systemic fingerprint"),
        "trip names the cause: {reason}"
    );
    assert!(is_breaker_trip(&reason), "systemic halts like other trips");
}

#[test]
fn same_file_repeating_is_repeat_not_systemic() {
    let mut breaker = strict();
    assert_eq!(
        breaker.observe_with_failing(false, &[failing("a.rs", "E9")]),
        None
    );
    assert_eq!(
        breaker.observe_with_failing(false, &[failing("a.rs", "E9")]),
        None
    );
    // One file failing thrice is the repeat signal, not a systemic one.
    assert_eq!(
        breaker.observe_with_failing(false, &[failing("a.rs", "E9")]),
        Some("repeat failure".to_string())
    );
}

#[test]
fn unattributed_failures_never_trip_systemic() {
    let mut breaker = strict();
    // Empty signatures carry no attributable cause: certified waves with
    // unattributed failures trip nothing on the systemic wire.
    for index in 0..5 {
        let file = format!("f{index}.rs");
        assert_eq!(
            breaker.observe_with_failing(true, &[failing(&file, "")]),
            None,
            "unattributed failures must not trip systemic"
        );
    }
}
