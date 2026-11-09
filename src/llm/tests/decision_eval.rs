//! Threshold calibration harness tests (tests-first).

use common_core::golden::compare_series;
use fluent_llm::decision::{
    control_fires, evaluate_point, gate_for_point, CaseProvenance, ScoredCase,
};

fn provenance() -> CaseProvenance {
    CaseProvenance::human("eval-fixture", "2026-11-08")
}

fn scored(score: f64, expected: bool) -> ScoredCase {
    ScoredCase {
        score,
        expected,
        control: false,
        provenance: provenance(),
    }
}

fn control(score: f64) -> ScoredCase {
    ScoredCase {
        score,
        expected: false,
        control: true,
        provenance: provenance(),
    }
}

fn fixture_cases() -> Vec<ScoredCase> {
    let mut cases: Vec<ScoredCase> =
        vec![0.99, 0.97, 0.95, 0.93, 0.91, 0.85, 0.83, 0.81, 0.79, 0.77]
            .into_iter()
            .map(|s| scored(s, true))
            .collect();
    cases.extend(
        vec![0.3, 0.25, 0.2, 0.15, 0.1, 0.35, 0.3, 0.2, 0.1, 0.05]
            .into_iter()
            .map(|s| scored(s, false)),
    );
    cases
}

#[test]
fn sweep_reproduces_pinned_tables() {
    let cases = fixture_cases();
    let reports = evaluate_point(&cases, &[0.5, 0.8, 0.9]);
    let precision: Vec<Option<f64>> = reports.iter().map(|(_, r)| Some(r.precision)).collect();
    let recall: Vec<Option<f64>> = reports.iter().map(|(_, r)| Some(r.recall)).collect();
    let p = compare_series(&precision, &[Some(1.0), Some(1.0), Some(1.0)], 1e-12);
    assert!(p.matches, "precision drift: {:?}", p.mismatches);
    let r = compare_series(&recall, &[Some(1.0), Some(0.8), Some(0.5)], 1e-12);
    assert!(r.matches, "recall drift: {:?}", r.mismatches);
}

#[test]
fn control_group_zero_fires_at_shipped_threshold() {
    let mut cases = fixture_cases();
    // Must-NOT-fire controls: hostile excerpts, insufficient evidence.
    cases.push(control(0.4));
    cases.push(control(0.35));
    cases.push(control(0.1));
    assert_eq!(control_fires(&cases, 0.9), 0);
    assert!(gate_for_point(&cases, 0.9));
}

#[test]
fn never_firing_threshold_cannot_pass() {
    // Vacuous precision (no positive predictions scores 1.0) with zero
    // recall must not pass: a threshold that never fires proves nothing.
    let cases = vec![
        scored(0.1, true),
        scored(0.2, true),
        scored(0.05, false),
        scored(0.1, false),
    ];
    assert!(!gate_for_point(&cases, 0.9));
}

#[test]
fn hot_control_blocks_the_gate() {
    let mut cases = fixture_cases();
    cases.push(control(0.95));
    assert_eq!(control_fires(&cases, 0.9), 1);
    assert!(!gate_for_point(&cases, 0.9));
}
