//! Threshold calibration harness (no new sweep math).
//!
//! Every per-point threshold earns gate/cache trust through the shared
//! `common_core::calibration` harness: `sweep_thresholds` over labeled cases,
//! `passes_gate` (precision ≥ 0.90, FPR ≤ 0.05), plus a must-NOT-fire control
//! group that blocks the gate on any fire. Tables render and persist via the
//! shared artifact helpers — no second schema.

/// One labeled case: detector `score` (producer confidence) against ground
/// truth `expected` (task-value correctness). `control` marks must-NOT-fire
/// cases (hostile excerpts, insufficient evidence): they enter sweeps as
/// negatives and additionally gate promotion on zero fires. Provenance
/// travels with the case so a corpus can prove its labels are human-family.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// Threshold calibration harness (no new sweep math).
/// 
/// Every per-point threshold earns gate/cache trust through the shared
/// `common_core::calibration` harness: `sweep_thresholds` over labeled cases,
/// `passes_gate` (precision ≥ 0.90, FPR ≤ 0.05), plus a must-NOT-fire control
/// group that blocks the gate on any fire. Tables render and persist via the
/// shared artifact helpers — no second schema.
/// One labeled case: detector `score` (producer confidence) against ground
/// truth `expected` (task-value correctness). `control` marks must-NOT-fire
/// cases (hostile excerpts, insufficient evidence): they enter sweeps as
/// negatives and additionally gate promotion on zero fires. Provenance
/// travels with the case so a corpus can prove its labels are human-family.
pub struct ScoredCase {
    pub score: f64,
    pub expected: bool,
    pub control: bool,
    pub provenance: super::registry::CaseProvenance,
}

/// Sweep `thresholds` over `cases`, composing the shared harness.
///
/// ```rust
/// use fluent_llm::decision::{evaluate_point, CaseProvenance, ScoredCase};
///
/// let provenance = CaseProvenance::human("example", "2026-11-08");
/// let cases = vec![
///     ScoredCase { score: 0.9, expected: true, control: false, provenance: provenance.clone() },
///     ScoredCase { score: 0.1, expected: false, control: false, provenance },
/// ];
/// let reports = evaluate_point(&cases, &[0.5]);
/// assert_eq!(reports.len(), 1);
/// ```
#[must_use]
/// Sweep `thresholds` over `cases`, composing the shared harness.
/// 
/// ```rust
/// use fluent_llm::decision::{evaluate_point, CaseProvenance, ScoredCase};
/// 
/// let provenance = CaseProvenance::human("example", "2026-11-08");
/// let cases = vec![
/// ScoredCase { score: 0.9, expected: true, control: false, provenance: provenance.clone() },
/// ScoredCase { score: 0.1, expected: false, control: false, provenance },
/// ];
/// let reports = evaluate_point(&cases, &[0.5]);
/// assert_eq!(reports.len(), 1);
/// ```
pub fn evaluate_point(
    cases: &[ScoredCase],
    thresholds: &[f64],
) -> Vec<(f64, common_core::calibration::CalibrationReport)> {
    common_core::calibration::sweep_thresholds(cases, |c| c.score, |c| c.expected, thresholds)
}

/// Control fires at `threshold`: control cases scoring at or above it.
///
/// ```rust
/// use fluent_llm::decision::{control_fires, CaseProvenance, ScoredCase};
///
/// let provenance = CaseProvenance::human("example", "2026-11-08");
/// let cases = vec![
///     ScoredCase { score: 0.95, expected: false, control: true, provenance },
/// ];
/// assert_eq!(control_fires(&cases, 0.9), 1);
/// ```
#[must_use]
/// Control fires at `threshold`: control cases scoring at or above it.
/// 
/// ```rust
/// use fluent_llm::decision::{control_fires, CaseProvenance, ScoredCase};
/// 
/// let provenance = CaseProvenance::human("example", "2026-11-08");
/// let cases = vec![
/// ScoredCase { score: 0.95, expected: false, control: true, provenance },
/// ];
/// assert_eq!(control_fires(&cases, 0.9), 1);
/// ```
pub fn control_fires(cases: &[ScoredCase], threshold: f64) -> usize {
    cases
        .iter()
        .filter(|c| c.control && c.score >= threshold)
        .count()
}

/// Promotion gate: the shared `passes_gate` at `threshold` AND zero control
/// fires AND nonzero recall. A confident-but-wrong control blocks trust even
/// when the sweep is clean — confidence never stands in for correctness —
/// and a never-firing threshold (vacuous precision, zero recall) proves
/// nothing, so it cannot pass either.
///
/// ```rust
/// use fluent_llm::decision::{gate_for_point, CaseProvenance, ScoredCase};
///
/// let provenance = CaseProvenance::human("example", "2026-11-08");
/// let cases = vec![
///     ScoredCase { score: 0.9, expected: true, control: false, provenance: provenance.clone() },
///     ScoredCase { score: 0.1, expected: false, control: false, provenance },
/// ];
/// assert!(gate_for_point(&cases, 0.5));
/// ```
#[must_use]
/// Promotion gate: the shared `passes_gate` at `threshold` AND zero control
/// fires AND nonzero recall. A confident-but-wrong control blocks trust even
/// when the sweep is clean — confidence never stands in for correctness —
/// and a never-firing threshold (vacuous precision, zero recall) proves
/// nothing, so it cannot pass either.
/// 
/// ```rust
/// use fluent_llm::decision::{gate_for_point, CaseProvenance, ScoredCase};
/// 
/// let provenance = CaseProvenance::human("example", "2026-11-08");
/// let cases = vec![
/// ScoredCase { score: 0.9, expected: true, control: false, provenance: provenance.clone() },
/// ScoredCase { score: 0.1, expected: false, control: false, provenance },
/// ];
/// assert!(gate_for_point(&cases, 0.5));
/// ```
pub fn gate_for_point(cases: &[ScoredCase], threshold: f64) -> bool {
    let report = common_core::calibration::calibrate_threshold(
        cases,
        |c| c.score,
        |c| c.expected,
        threshold,
    );
    report.passes_gate() && report.recall > 0.0 && control_fires(cases, threshold) == 0
}

/// `render_point_table`: render the sweep as the shared markdown table.
#[must_use]
/// `render_point_table`: render the sweep as the shared markdown table.
pub fn render_point_table(
    reports: &[(f64, common_core::calibration::CalibrationReport)],
) -> String {
    common_core::calibration::render_calibration_table(reports)
}

/// `emit_point_artifact`: persist the sweep to `target/calibration/<name>.md` (best-effort).
pub fn emit_point_artifact(
    name: &str,
    reports: &[(f64, common_core::calibration::CalibrationReport)],
) {
    common_core::calibration::emit_markdown_artifact(name, reports);
}