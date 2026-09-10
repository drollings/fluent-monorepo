//! P3 freshness-matrix tests: `fresh` / `possibly_stale` ×
//! `background` / `wait` / `off` — every cell names its trigger, wait,
//! and serve behavior.

use crate::freshness::{Freshness, FreshnessGate, FreshnessMode};
use std::cell::Cell;

struct Calls {
    triggered: Cell<usize>,
    waited: Cell<usize>,
    served: Cell<Option<Freshness>>,
}

impl Calls {
    fn new() -> Self {
        Self {
            triggered: Cell::new(0),
            waited: Cell::new(0),
            served: Cell::new(None),
        }
    }

    fn run(&self, gate: FreshnessGate, indexed: u64, dirty: u64) -> &'static str {
        gate.serve(
            indexed,
            dirty,
            || {
                self.triggered.set(self.triggered.get() + 1);
            },
            || {
                self.waited.set(self.waited.get() + 1);
                dirty
            },
            |freshness| {
                self.served.set(Some(freshness.clone()));
                "served"
            },
        )
    }
}

#[test]
fn fresh_serves_without_trigger_or_wait_in_every_mode() {
    for mode in [FreshnessMode::Background, FreshnessMode::Wait, FreshnessMode::Off] {
        let calls = Calls::new();
        let gate = FreshnessGate::new(mode);
        assert_eq!(calls.run(gate, 7, 7), "served");
        assert_eq!(calls.triggered.get(), 0, "{mode:?}");
        assert_eq!(calls.waited.get(), 0, "{mode:?}");
        assert_eq!(
            calls.served.take(),
            Some(Freshness::Fresh { revision: 7 }),
            "{mode:?}"
        );
    }
}

#[test]
fn stale_off_serves_stale_without_trigger_or_wait() {
    let calls = Calls::new();
    let gate = FreshnessGate::new(FreshnessMode::Off);
    assert_eq!(calls.run(gate, 7, 9), "served");
    assert_eq!(calls.triggered.get(), 0);
    assert_eq!(calls.waited.get(), 0);
    assert_eq!(
        calls.served.take(),
        Some(Freshness::PossiblyStale {
            indexed_revision: 7,
            dirty_revision: 9,
        })
    );
}

#[test]
fn stale_background_triggers_reconcile_then_serves_stale() {
    let calls = Calls::new();
    let gate = FreshnessGate::new(FreshnessMode::Background);
    assert_eq!(calls.run(gate, 7, 9), "served");
    assert_eq!(calls.triggered.get(), 1);
    assert_eq!(calls.waited.get(), 0);
    assert_eq!(
        calls.served.take(),
        Some(Freshness::PossiblyStale {
            indexed_revision: 7,
            dirty_revision: 9,
        })
    );
}

#[test]
fn stale_wait_triggers_waits_then_serves_fresh() {
    let calls = Calls::new();
    let gate = FreshnessGate::new(FreshnessMode::Wait);
    assert_eq!(calls.run(gate, 7, 9), "served");
    assert_eq!(calls.triggered.get(), 1);
    assert_eq!(calls.waited.get(), 1);
    assert_eq!(
        calls.served.take(),
        Some(Freshness::Fresh { revision: 9 })
    );
}
