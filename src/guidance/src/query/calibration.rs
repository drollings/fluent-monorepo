//! Calibration corpora for the query decision points.
//!
//! Producer confidence (a score gap) may only order work or abstain; only a
//! task-value check (citation verifies, gate passes) certifies. Control
//! cases below must never fire: a single control fire blocks promotion even
//! when the sweep is clean.

use fluent_llm::decision::{control_fires, evaluate_point, gate_for_point, CaseProvenance, ScoredCase};

fn provenance() -> CaseProvenance {
    CaseProvenance::human("guidance-calibration", "2026-11-09")
}

/// Homonym trap: same symbol name in two files must not resolve unique.
#[must_use]
pub fn homonym_corpus() -> Vec<ScoredCase> {
    let provenance = provenance();
    vec![
        ScoredCase { score: 0.95, expected: true, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.90, expected: true, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.30, expected: false, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.96, expected: false, control: true, provenance },
    ]
}

/// Cosmetic diff: whitespace-only change must not nominate commentary.
#[must_use]
pub fn cosmetic_corpus() -> Vec<ScoredCase> {
    let provenance = provenance();
    vec![
        ScoredCase { score: 0.88, expected: true, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.20, expected: false, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.91, expected: false, control: true, provenance },
    ]
}

/// Keyword-only query: must not route to the vector rung alone.
#[must_use]
pub fn keyword_only_corpus() -> Vec<ScoredCase> {
    let provenance = provenance();
    vec![
        ScoredCase { score: 0.85, expected: true, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.25, expected: false, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.93, expected: false, control: true, provenance },
    ]
}

/// Sweep one corpus over thresholds, composing the shared harness.
#[must_use]
pub fn sweep(
    cases: &[ScoredCase],
    thresholds: &[f64],
) -> Vec<(f64, common_core::calibration::CalibrationReport)> {
    evaluate_point(cases, thresholds)
}

/// Promotion gate: shared gate plus zero control fires plus nonzero recall.
#[must_use]
pub fn passes_gate(cases: &[ScoredCase], threshold: f64) -> bool {
    gate_for_point(cases, threshold)
}

/// Control fires at a threshold: must be zero for promotion.
#[must_use]
pub fn control_fire_count(cases: &[ScoredCase], threshold: f64) -> usize {
    control_fires(cases, threshold)
}

#[cfg(test)]
#[path = "../../tests/threshold_calibration.rs"]
mod calibration_tests;
