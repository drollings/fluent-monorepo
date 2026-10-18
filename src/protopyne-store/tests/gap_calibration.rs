//! The freshness-calibration gate: the frozen bound is green, the sweep
//! table matches the recorded measurement, and the code default equals the
//! frozen bound.
//!
//! The corpus carries both halves — six tapes that genuinely require repair
//! and five healthy tapes that must be left alone. Green is recall `1.0`
//! with control false-fire `0.0` at the frozen bound, mirroring the
//! kernels' parity-dashboard gate. The recorded sweep table freezes the
//! whole measurement, not just the frozen point, so a later change that
//! shifts any candidate's numbers fails here instead of silently re-freezing.

use super::*;
use crate::coverage::test_calibration::{load_corpus, GapTally};

/// Render the measured sweep table for assertion messages: one line per
/// candidate bound, so a drift names every candidate that moved.
fn render_table(corpus: &crate::coverage::test_calibration::Corpus) -> String {
    use std::fmt::Write as _;
    let mut table = String::from("k | recall | control_false_fire\n");
    for k in 0..=6_u64 {
        let tally = GapTally::for_k(corpus, k);
        writeln!(
            table,
            "{k} | {:.4} | {:.4}",
            tally.recall(),
            tally.false_fire_rate()
        )
        .expect("writing to a String never fails");
    }
    table
}

#[test]
fn corpus_has_the_recorded_shape() {
    let corpus = load_corpus();
    assert_eq!(
        corpus.positive.len(),
        6,
        "six must-fire cases: a dropped positive inflates recall"
    );
    assert_eq!(
        corpus.control.len(),
        5,
        "five must-not-fire cases: a dropped control hides false fires"
    );
}

#[test]
fn frozen_threshold_is_green() {
    let corpus = load_corpus();
    let frozen = corpus.frozen_max_lag_bars;
    let tally = GapTally::for_k(&corpus, frozen);
    assert!(
        (tally.recall() - 1.0).abs() < f64::EPSILON,
        "recall at the frozen bound {frozen} must be 1.0, got {:.4}:\n{}",
        tally.recall(),
        render_table(&corpus)
    );
    assert!(
        tally.false_fire_rate().abs() < f64::EPSILON,
        "control false-fire at the frozen bound {frozen} must be 0.0:\n{}",
        render_table(&corpus)
    );
    assert!(
        tally.is_green(),
        "the frozen bound must satisfy the green predicate:\n{}",
        render_table(&corpus)
    );
    for row in &tally.rows {
        assert_eq!(
            row.actual, row.expected,
            "case '{}' must produce its recorded verdict at the frozen bound",
            row.name
        );
    }
    for row in tally
        .rows
        .iter()
        .filter(|row| row.actual == crate::coverage::test_calibration::ExpectClass::Torn)
    {
        let case = corpus
            .positive
            .iter()
            .chain(corpus.control.iter())
            .find(|case| case.name == row.name)
            .expect("every row names a recorded case");
        assert_eq!(
            Some(row.holes),
            case.expect_holes,
            "case '{}' must report its recorded hole count",
            row.name
        );
    }
}

#[test]
fn sweep_table_matches_the_recorded_measurement() {
    let corpus = load_corpus();
    let computed = render_table(&corpus);
    for k in 0..=6_u64 {
        let tally = GapTally::for_k(&corpus, k);
        let recorded = corpus
            .measured
            .get(&k.to_string())
            .unwrap_or_else(|| panic!("no recorded measurement for k={k}:\n{computed}"));
        assert!(
            (tally.recall() - recorded.recall).abs() < 1e-12,
            "recall drifted at k={k}: computed {:.4}, recorded {:.4}:\n{computed}",
            tally.recall(),
            recorded.recall
        );
        assert!(
            (tally.false_fire_rate() - recorded.control_false_fire).abs() < 1e-12,
            "control false-fire drifted at k={k}: computed {:.4}, recorded {:.4}:\n{computed}",
            tally.false_fire_rate(),
            recorded.control_false_fire
        );
    }
}

#[test]
fn code_default_equals_the_frozen_bound() {
    let corpus = load_corpus();
    assert_eq!(
        FreshnessPolicy::new().build().max_lag_bars,
        corpus.frozen_max_lag_bars,
        "the builder default and the frozen measurement are one value read \
         from two places: they must agree"
    );
}
