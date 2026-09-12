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

// --- M12.1 characterization: revision-matrix edges (verbatim outputs) ---

#[test]
fn m12_dirty_behind_indexed_is_fresh() {
    // `dirty <= indexed` → Fresh: a dirty watermark older than the index
    // carries no work (clock moved backward or watermark reused).
    for mode in [FreshnessMode::Background, FreshnessMode::Wait, FreshnessMode::Off] {
        let calls = Calls::new();
        let gate = FreshnessGate::new(mode);
        assert_eq!(calls.run(gate, 9, 7), "served");
        assert_eq!(calls.triggered.get(), 0, "{mode:?}");
        assert_eq!(calls.waited.get(), 0, "{mode:?}");
        assert_eq!(
            calls.served.take(),
            Some(Freshness::Fresh { revision: 9 }),
            "{mode:?}"
        );
    }
}

#[test]
fn m12_zero_revisions_are_fresh() {
    let gate = FreshnessGate::new(FreshnessMode::Background);
    assert_eq!(
        gate.check(0, 0),
        Freshness::Fresh { revision: 0 }
    );
    // Boundary: dirty exactly one ahead is already possibly stale.
    assert_eq!(
        gate.check(0, 1),
        Freshness::PossiblyStale {
            indexed_revision: 0,
            dirty_revision: 1,
        }
    );
}

// --- M12.2 home tests: skew table + tiebreak (deterministic, no fs) ---

#[test]
fn m12_mtime_skew_partition() {
    use crate::freshness::{mtime_skew, MtimeSkew};
    use std::time::{Duration, SystemTime};
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let at = |secs: u64, nanos: u32| base + Duration::new(secs, nanos);
    // Equal mtimes: evidence-free, ambiguous (legacy window read true).
    assert_eq!(mtime_skew(base, base), MtimeSkew::Ambiguous);
    // Source strictly older: no work.
    assert_eq!(
        mtime_skew(at(0, 0), base - Duration::from_secs(2)),
        MtimeSkew::SourceOlder
    );
    // Sub-second skew either side of zero stays inside the window.
    assert_eq!(
        mtime_skew(base, base + Duration::from_millis(999)),
        MtimeSkew::Ambiguous
    );
    // Exact 1s boundary: nanos decide (0 → window, 1 → stale).
    assert_eq!(
        mtime_skew(base, base + Duration::new(1, 0)),
        MtimeSkew::Ambiguous
    );
    assert_eq!(
        mtime_skew(base, base + Duration::new(1, 1)),
        MtimeSkew::SourceNewer
    );
    // Beyond the window: stale.
    assert_eq!(
        mtime_skew(base, base + Duration::new(1, 1_000_000)),
        MtimeSkew::SourceNewer
    );
    assert_eq!(
        mtime_skew(base, base + Duration::from_secs(60)),
        MtimeSkew::SourceNewer
    );
}

#[test]
fn m12_hash_tiebreak_at_home() {
    use crate::freshness::hash_decides_fresh;
    assert!(hash_decides_fresh("abc123", Some("abc123")));
    assert!(!hash_decides_fresh("def456", Some("abc123")));
    assert!(!hash_decides_fresh("abc123", None));
    assert!(!hash_decides_fresh("", None));
}
