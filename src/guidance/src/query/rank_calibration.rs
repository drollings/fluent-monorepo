//! Calibration for rank and citation heuristics: homonym traps, no-caller
//! traps, and citation-validity floors.
//!
//! Producer confidence (a high lexical score on an absent symbol) never
//! certifies; only task-value checks (grounded excerpts, verified
//! citations) do. Control cases below must never fire: a single control
//! fire blocks promotion even when the sweep is clean.

use fluent_llm::decision::{CaseProvenance, ScoredCase};

use super::calibration::{control_fire_count, passes_gate};

fn provenance() -> CaseProvenance {
    CaseProvenance::human("guidance-rank-calibration", "2026-11-09")
}

/// No-caller traps: absent symbols that lexically resemble indexed ones
/// score high on producer confidence but must never certify. Positives
/// are unique indexed symbols.
#[must_use]
pub fn no_caller_trap_corpus() -> Vec<ScoredCase> {
    let provenance = provenance();
    vec![
        ScoredCase { score: 0.95, expected: true, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.90, expected: true, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.30, expected: false, control: false, provenance: provenance.clone() },
        ScoredCase { score: 0.94, expected: false, control: true, provenance: provenance.clone() },
        ScoredCase { score: 0.91, expected: false, control: true, provenance },
    ]
}

/// Promotion gate over the trap corpus at one threshold.
#[must_use]
pub fn trap_gate_passes(threshold: f64) -> bool {
    passes_gate(&no_caller_trap_corpus(), threshold)
}

/// Control fires at one threshold: must be zero for promotion.
#[must_use]
pub fn trap_control_fires(threshold: f64) -> usize {
    control_fire_count(&no_caller_trap_corpus(), threshold)
}

/// Citation precision: share of envelope citations resolving to grounded
/// stages. Task-value axis — the floor promotion must clear.
#[must_use]
pub fn citation_precision(verified: usize, total: usize) -> f64 {
    if total == 0 {
        return 1.0;
    }
    verified as f64 / total as f64
}

#[cfg(test)]
#[path = "../../tests/envelope_calibration.rs"]
mod envelope_calibration_tests;
