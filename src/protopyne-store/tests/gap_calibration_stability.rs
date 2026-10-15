//! The calibration harness is deterministic: measuring the same corpus twice
//! yields identical tallies at every candidate bound.
//!
//! A calibration whose numbers wobble between runs cannot freeze anything —
//! the recorded sweep table would be one sample among many. This gate pins
//! the harness itself, so the recorded measurement is a fact about the
//! classifier rather than about the run that happened to record it.

use crate::coverage::test_calibration::{GapTally, load_corpus};

#[test]
fn repeated_runs_yield_identical_tallies() {
    let corpus = load_corpus();
    for k in 0..=6_u64 {
        assert_eq!(
            GapTally::for_k(&corpus, k),
            GapTally::for_k(&corpus, k),
            "the tally at k={k} must not depend on the run"
        );
    }
}

#[test]
fn repeated_corpus_loads_agree() {
    assert_eq!(load_corpus().positive.len(), load_corpus().positive.len());
    let first = load_corpus();
    let second = load_corpus();
    assert_eq!(first.frozen_max_lag_bars, second.frozen_max_lag_bars);
    assert_eq!(first.measured, second.measured);
}
