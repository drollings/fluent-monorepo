//! Threshold calibration: must-fire and must-NOT-fire sets per point.
//!
//! Shadow disagreement never flips the deterministic result while the
//! registry is seed-empty; control fires block promotion.

use crate::query::calibration::{cosmetic_corpus, control_fire_count, homonym_corpus, keyword_only_corpus, passes_gate, sweep};
use crate::query::decision_points::{authorize_point, relevance_gate, route_sufficiency, staleness_eligible, sufficiency_spec, Sufficiency};

#[test]
fn homonym_control_blocks_high_threshold() {
    let cases = homonym_corpus();
    assert_eq!(control_fire_count(&cases, 0.9), 1);
    assert!(!passes_gate(&cases, 0.9));
}

#[test]
fn cosmetic_control_blocks_staleness_promotion() {
    let cases = cosmetic_corpus();
    assert_eq!(control_fire_count(&cases, 0.9), 1);
    assert!(!passes_gate(&cases, 0.9));
    assert!(!staleness_eligible(false, "fn f() {}", "fn  f() {}"));
    assert!(!staleness_eligible(true, "fn f() {}", "fn  f() {}"));
    assert!(staleness_eligible(true, "fn f() {}", "fn g() {}"));
}

#[test]
fn keyword_control_blocks_vector_only_routing() {
    let cases = keyword_only_corpus();
    assert_eq!(control_fire_count(&cases, 0.9), 1);
    assert!(!passes_gate(&cases, 0.9));
}

#[test]
fn shadow_disagreement_keeps_deterministic_result() {
    let status = authorize_point("intent", "test-fingerprint", 1);
    assert!(status.is_shadow_only());
    let (route, _) = route_sufficiency(0, false, None, &sufficiency_spec().thresholds());
    assert_eq!(route, Sufficiency::Unanswerable);
    let kept = relevance_gate(&[("a", 0.9), ("b", 0.4)], 0.5);
    assert_eq!(kept, vec!["a".to_string()]);
}

#[test]
fn sweep_reports_precision_recall_per_threshold() {
    let cases = homonym_corpus();
    let reports = sweep(&cases, &[0.5, 0.9]);
    assert_eq!(reports.len(), 2);
    for (_, report) in reports {
        assert!(report.precision >= 0.0 && report.precision <= 1.0);
        assert!(report.recall >= 0.0 && report.recall <= 1.0);
    }
}
