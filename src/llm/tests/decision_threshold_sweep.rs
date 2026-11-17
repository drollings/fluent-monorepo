//! Decision-threshold sweep calibration (method pin, synthetic labeled set).
//!
//! Producer confidence vs task-value (§1 of the crate calibration doc): this
//! pins the SWEEP METHOD (sweep → `GateBars` → lock), not production
//! numbers. Production thresholds earn trust only on labeled production
//! cases plus a must-not-fire control group. The synthetic set below is
//! confidence-correlated by construction so the sweep has a passing
//! operating point; the control group (all-degraded inputs) must never
//! `Act` at any swept threshold.

use common_core::calibration::{default_thresholds, sweep_thresholds, GateBars};
use fluent_llm::decision::{decide_verdict, Decision, ThresholdSet, Verdict};

struct Case {
    confidence: f64,
    correct: bool,
}

fn correlated_corpus() -> Vec<Case> {
    // 10 should-fire cases (high confidence) + 10 should-not-fire (low).
    // Separated by construction: the sweep must find a SHIPPING-passing bar.
    let mut cases: Vec<Case> = (0..10)
        .map(|i| Case {
            confidence: 0.75 + f64::from(i) * 0.02,
            correct: true,
        })
        .collect();
    cases.extend((0..10).map(|i| Case {
        confidence: 0.05 + f64::from(i) * 0.04,
        correct: false,
    }));
    cases
}

fn decision_at(confidence: f64, degraded: bool) -> Decision {
    let other = 1.0 - confidence;
    Decision::from_distribution(
        "A",
        vec![("A".to_string(), confidence), ("B".to_string(), other)],
        "m".to_string(),
        1,
        0,
        false,
    )
    .map(|mut d| {
        d.degraded = degraded;
        d
    })
    .expect("distribution")
}

#[test]
fn sweep_finds_shipping_passing_point_on_correlated_set() {
    let cases = correlated_corpus();
    let reports = sweep_thresholds(
        &cases,
        |c| c.confidence,
        |c| c.correct,
        &default_thresholds(),
    );
    assert_eq!(reports.len(), 21);
    let passing: Vec<f64> = reports
        .iter()
        .filter(|(_, r)| r.passes_gate_at(GateBars::SHIPPING))
        .map(|(t, _)| *t)
        .collect();
    assert!(
        !passing.is_empty(),
        "correlated set must admit a SHIPPING point"
    );
    let full = reports
        .iter()
        .find(|(t, _)| (*t - 0.5).abs() < 1e-9)
        .expect("0.5 grid point")
        .1
        .clone();
    assert_eq!(full.support, 20);
    assert!(full.passes_gate_at(GateBars::SHIPPING));
}

#[test]
fn all_degraded_inputs_never_act_at_any_swept_threshold() {
    // Must-not-fire control: degraded (uncalibrated fallback) decisions are
    // capped below Act for every threshold on the grid.
    for t in default_thresholds() {
        let thresholds = ThresholdSet {
            act: t.max(0.5),
            review: 0.0,
        };
        for conf in [0.0, 0.5, 0.99, 1.0] {
            let d = decision_at(conf, true);
            assert_ne!(
                decide_verdict(&d, &thresholds),
                Verdict::Act,
                "degraded conf={conf} must never Act (act={})",
                thresholds.act
            );
        }
    }
}
