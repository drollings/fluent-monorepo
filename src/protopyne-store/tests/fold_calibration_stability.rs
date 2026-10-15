//! The fold-calibration harness is deterministic: measuring the same goldens
//! twice yields identical tallies, and reloading the goldens agrees.
//!
//! A calibration whose numbers wobble between runs cannot freeze anything —
//! the recorded agreement would be one sample among many. This gate pins the
//! harness itself, so the recorded figures are facts about the fold rather
//! than about the run that happened to record them.

use crate::fold::test_fold::{FoldTally, load_goldens};

#[test]
fn repeated_runs_yield_identical_tallies() {
    let goldens = load_goldens();
    assert_eq!(
        FoldTally::measure(&goldens),
        FoldTally::measure(&goldens),
        "the tally must not depend on the run"
    );
}

#[test]
fn repeated_golden_loads_agree() {
    let first = load_goldens();
    let second = load_goldens();
    assert_eq!(first.boundaries.len(), second.boundaries.len());
    assert_eq!(first.complete_run.bars, second.complete_run.bars);
    assert_eq!(
        first.complete_run.total_bars,
        second.complete_run.total_bars
    );
    assert_eq!(first.agreement.comparable, second.agreement.comparable);
    assert_eq!(first.agreement.matched, second.agreement.matched);
}
