//! Calibration of `ScoreMatrix::router_default` bands and weights.
//!
//! Measurement only: production constants are frozen by this suite. The
//! corpus has three disjoint groups:
//!
//! - (a) must-route: one case per route that the current bands must match;
//! - (b) control: cases in the completeness gap (0.5, 0.7) that must match
//!   NOTHING (clean exhaustion), including high-coherence cases that prove
//!   confident-but-out-of-scope inputs are still rejected;
//! - (c) near-boundary: epsilon cases around each band edge whose flip
//!   behavior is reported, not gated.
//!
//! Axis declaration: bands gate on task-value (`completeness`, `risk`),
//! never on producer confidence (`coherence`). The `low_coherence_rigor`
//! case pins this: coherence 0.1 with in-band task-value still matches,
//! and the control cases pin the converse: coherence 0.95 with out-of-band
//! task-value still matches nothing. A band hit must never be read as a
//! confidence success, and a high coherence must never excuse a band miss.
//! The separate `ThresholdGate` coherence check (classifier stage) is out
//! of scope here; this suite measures the matrix only.
//!
//! Accounting (stated up front; corpus too small for formal
//! precision/recall): recall = must-route matches / 3, bar 3/3;
//! control false positives, bar exactly 0 (hard gate).

use super::*;
use crate::score_matrix::RouterRouteKind;

fn scores(coherence: f64, complexity: f64, completeness: f64, risk: f64) -> HashMap<String, f64> {
    HashMap::from([
        ("coherence".into(), coherence),
        ("complexity".into(), complexity),
        ("completeness".into(), completeness),
        ("risk".into(), risk),
    ])
}

fn matched_routes(m: &ScoreMatrix, s: &HashMap<String, f64>) -> Vec<String> {
    m.resolve(s).iter().map(|r| r.route_name.clone()).collect()
}

#[test]
fn must_route_group_recall_is_three_of_three() {
    let m = ScoreMatrix::router_default();
    // Plan: low completeness, any risk (no risk band on plan).
    assert_eq!(
        matched_routes(&m, &scores(0.9, 0.5, 0.3, 0.9)),
        vec![RouterRouteKind::Plan.as_str().to_string()],
    );
    // Rigor: high completeness, high risk.
    assert_eq!(
        matched_routes(&m, &scores(0.9, 0.5, 0.8, 0.6)),
        vec![RouterRouteKind::Rigor.as_str().to_string()],
    );
    // Local: high completeness, low risk.
    assert_eq!(
        matched_routes(&m, &scores(0.9, 0.5, 0.8, 0.2)),
        vec![RouterRouteKind::Local.as_str().to_string()],
    );
}

#[test]
fn control_group_matches_nothing_even_when_confident() {
    let m = ScoreMatrix::router_default();
    // Completeness gap (0.5, 0.7): no route's bands cover it, at any risk.
    for (completeness, risk) in [(0.6, 0.5), (0.6, 0.0), (0.6, 1.0), (0.55, 0.9)] {
        // Coherence 0.95: confident-but-out-of-scope must still be rejected.
        let matched = matched_routes(&m, &scores(0.95, 0.5, completeness, risk));
        assert!(
            matched.is_empty(),
            "control case (completeness={completeness}, risk={risk}) must match nothing, got {matched:?}"
        );
    }
}

#[test]
fn band_matching_is_coherence_invariant() {
    let m = ScoreMatrix::router_default();
    // Same in-band task-value, producer self-doubt swept low→high: the
    // match set must not move. Confidence is not a band input.
    let low = matched_routes(&m, &scores(0.1, 0.5, 0.8, 0.6));
    let high = matched_routes(&m, &scores(0.95, 0.5, 0.8, 0.6));
    assert_eq!(low, high);
    assert_eq!(low, vec![RouterRouteKind::Rigor.as_str().to_string()]);
}

#[test]
fn near_boundary_flip_behavior_is_reported() {
    let m = ScoreMatrix::router_default();
    // Plan completeness edge at 0.5 (inclusive): 0.49 in, 0.51 out.
    assert_eq!(
        matched_routes(&m, &scores(0.9, 0.5, 0.49, 0.9)),
        vec![RouterRouteKind::Plan.as_str().to_string()]
    );
    assert!(
        matched_routes(&m, &scores(0.9, 0.5, 0.51, 0.9)).is_empty(),
        "0.51 completeness with risk 0.9 falls in the control gap"
    );
    // Rigor/local completeness edge at 0.7 (inclusive): 0.69 out, 0.71 in.
    assert!(matched_routes(&m, &scores(0.9, 0.5, 0.69, 0.6)).is_empty());
    assert_eq!(
        matched_routes(&m, &scores(0.9, 0.5, 0.71, 0.6)),
        vec![RouterRouteKind::Rigor.as_str().to_string()]
    );
    // Rigor/local risk split at 0.4 (inclusive both sides): 0.39 local,
    // 0.41 rigor.
    assert_eq!(
        matched_routes(&m, &scores(0.9, 0.5, 0.8, 0.39)),
        vec![RouterRouteKind::Local.as_str().to_string()]
    );
    assert_eq!(
        matched_routes(&m, &scores(0.9, 0.5, 0.8, 0.41)),
        vec![RouterRouteKind::Rigor.as_str().to_string()]
    );
}
