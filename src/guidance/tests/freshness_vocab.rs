//! Freshness vocabulary: pending, stale, and dependent-stale plus
//! strict refusal. Pending is producer state (an open revision the
//! pipeline doubts); dependent-stale is task value (a dependency
//! changed while this file looks clean). Strict refuses either.

use crate::freshness::{Freshness, FreshnessGate, FreshnessMode};
use std::cell::Cell;

struct Calls {
    triggered: Cell<usize>,
    waited: Cell<usize>,
}

impl Calls {
    fn new() -> Self {
        Self {
            triggered: Cell::new(0),
            waited: Cell::new(0),
        }
    }
}

#[test]
fn pending_is_distinct_from_stale() {
    let gate = FreshnessGate::new(FreshnessMode::Background);
    let pending = gate.check_extended(7, 7, Some(9), false);
    assert_eq!(pending, Freshness::Pending { revision: 9 });
    assert_ne!(
        pending,
        Freshness::PossiblyStale {
            indexed_revision: 7,
            dirty_revision: 9,
        }
    );
}

#[test]
fn dependent_stale_fires_while_own_hash_is_clean() {
    let gate = FreshnessGate::new(FreshnessMode::Background);
    assert_eq!(
        gate.check_extended(7, 7, None, true),
        Freshness::DependentStale {
            indexed_revision: 7,
            dirty_revision: 7,
        }
    );
    assert_eq!(
        gate.check_extended(7, 7, None, false),
        Freshness::Fresh { revision: 7 }
    );
}

#[test]
fn inflight_beats_dependent_signal() {
    let gate = FreshnessGate::new(FreshnessMode::Background);
    assert_eq!(
        gate.check_extended(7, 7, Some(8), true),
        Freshness::Pending { revision: 8 }
    );
}

#[test]
fn stale_without_inflight_is_unchanged() {
    let gate = FreshnessGate::new(FreshnessMode::Background);
    assert_eq!(
        gate.check_extended(7, 9, None, false),
        Freshness::PossiblyStale {
            indexed_revision: 7,
            dirty_revision: 9,
        }
    );
}

#[test]
fn background_serves_pending_after_trigger() {
    let calls = Calls::new();
    let gate = FreshnessGate::new(FreshnessMode::Background);
    let out: Result<&str, _> = gate.try_serve(
        Freshness::Pending { revision: 9 },
        || {
            calls.triggered.set(calls.triggered.get() + 1);
        },
        || {
            calls.waited.set(calls.waited.get() + 1);
            9
        },
        |freshness| {
            assert_eq!(freshness, Freshness::Pending { revision: 9 });
            "served-pending"
        },
    );
    assert_eq!(out, Ok("served-pending"));
    assert_eq!(calls.triggered.get(), 1);
}

#[test]
fn strict_refuses_pending_stale_and_dependent_stale() {
    let gate = FreshnessGate::new(FreshnessMode::Strict);
    for freshness in [
        Freshness::Pending { revision: 9 },
        Freshness::PossiblyStale {
            indexed_revision: 7,
            dirty_revision: 9,
        },
        Freshness::DependentStale {
            indexed_revision: 7,
            dirty_revision: 7,
        },
    ]
    .into_iter()
    .map(|freshness| {
        let refused: Result<&str, _> = gate.try_serve(
            freshness.clone(),
            || {},
            || 9,
            |_| "served",
        );
        (freshness, refused)
    }) {
        assert!(freshness.1.is_err(), "{:?} must refuse under strict", freshness.0);
    }
}

#[test]
fn strict_serves_fresh_and_default_serves_labelled() {
    let strict = FreshnessGate::new(FreshnessMode::Strict);
    let ok: Result<&str, _> = strict.try_serve(
        Freshness::Fresh { revision: 7 },
        || {},
        || 7,
        |_| "served",
    );
    assert_eq!(ok, Ok("served"));
    for mode in [
        FreshnessMode::Background,
        FreshnessMode::Wait,
        FreshnessMode::Off,
    ] {
        let gate = FreshnessGate::new(mode);
        let out: &str = gate.serve(
            7,
            7,
            || {},
            || 7,
            |freshness| {
                assert_eq!(freshness, Freshness::Fresh { revision: 7 });
                "served-fresh"
            },
        );
        assert_eq!(out, "served-fresh", "{mode:?}");
    }
}

#[test]
fn helpers_name_the_axis() {
    assert!(Freshness::Pending { revision: 9 }.is_pending());
    assert!(!Freshness::Fresh { revision: 9 }.is_pending());
    assert!(
        Freshness::DependentStale {
            indexed_revision: 7,
            dirty_revision: 7,
        }
        .is_dependent_stale()
    );
    assert!(!Freshness::Fresh { revision: 7 }.is_dependent_stale());
    assert!(Freshness::Fresh { revision: 7 }.is_fresh());
    assert!(!Freshness::Pending { revision: 9 }.is_fresh());
}
